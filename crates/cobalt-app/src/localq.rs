//! Local query tabs: SQL over the local tables (results of open tabs, later files) through the
//! in-process engine (`cobalt-local`). A run streams batches into a `ResultSet` through the same
//! events the drivers emit, so the grid, the row cap, cancel, exports and history are the
//! existing code. Design: `docs/design/local_query.md`.

use crate::ops::{self, Ctx};
use crate::session::protocol::Event;
use crate::state::{hash_text, AppState, EditorTab, ResultsTab, RunMode, RunView, RunViewState, ToastKind};
use cobalt_core::TabId;
use cobalt_core::ServerMessage;
use cobalt_local::{Engine, TableSource};
use cobalt_results::{ResultSet, RunState};
use cobalt_store::NewHistoryEntry;
use futures_util::StreamExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

pub const HISTORY_SOURCE: &str = "Local (DataFusion)";
const SNAPSHOT_PREFIX: &str = "local:";

/// A run in flight: the flags the task watches.
#[derive(Clone, Default)]
pub struct LocalRun {
    pub cancel: Arc<AtomicBool>,
    /// Fetch more: `Some(None)` = lift the cap, `Some(Some(n))` = n more rows.
    pub more: Arc<parking_lot::Mutex<Option<Option<u64>>>>,
    pub notify: Arc<tokio::sync::Notify>,
}

/// The per-tab state of a local query tab.
#[derive(Default)]
pub struct LocalTab {
    pub run: Option<LocalRun>,
    /// The next run lifts the row cap ("Run again without the cap").
    pub uncapped_next: bool,
    /// The last run's rewrite notes (shown in the toolbar tooltip).
    pub notes: Vec<String>,
    /// The engine's function names, for completion.
    pub functions: Arc<Vec<String>>,
}

/// A new Local query tab with `text`.
pub fn new_tab(state: &mut AppState, cx: &Ctx, text: Option<String>) -> usize {
    let idx = state.new_tab();
    let functions = Arc::clone(&state.local_functions);
    let t = &mut state.tabs[idx];
    t.local = Some(LocalTab { functions, ..LocalTab::default() });
    if let Some(text) = text {
        t.text = text;
        t.mark_saved();
    }
    t.editor.request_focus = true;
    retitle(t);
    refresh_catalog(state, cx, idx);
    idx
}

/// `Local_n` unless renamed.
pub fn retitle(t: &mut EditorTab) {
    if t.custom_title || t.local.is_none() {
        return;
    }
    t.title = format!("Local_{}", t.untitled_index);
}

/// The engine's tables as a completion catalog for one tab.
pub fn refresh_catalog(state: &mut AppState, cx: &Ctx, idx: usize) {
    let Some(t) = state.tabs.get_mut(idx) else { return };
    if t.local.is_none() {
        return;
    }
    t.catalog = Some(Arc::new(catalog(cx.local)));
}

/// Every local tab's catalog (after the registry changed).
pub fn refresh_all_catalogs(state: &mut AppState, cx: &Ctx) {
    let cat = Arc::new(catalog(cx.local));
    let functions = Arc::clone(&state.local_functions);
    for t in state.tabs.iter_mut() {
        if let Some(l) = t.local.as_mut() {
            t.catalog = Some(Arc::clone(&cat));
            l.functions = Arc::clone(&functions);
        }
    }
}

fn catalog(engine: &Engine) -> cobalt_core::DatabaseCatalog {
    let mut cat = cobalt_core::DatabaseCatalog { database: "local".into(), ..Default::default() };
    for (i, t) in engine.tables().into_iter().enumerate() {
        let id = i as i32 + 1;
        cat.objects.push(cobalt_core::ObjectRef { database: "local".into(), schema: String::new(), name: t.name.clone(), kind: cobalt_core::ObjectKind::Table, object_id: Some(id) });
        cat.columns.insert(id, t.columns.clone());
    }
    cat
}

/// Open a Local query tab on a result set of a tab: `SELECT * FROM <its table> LIMIT 100`, run.
pub fn query_result(state: &mut AppState, cx: &Ctx, idx: usize, set: usize) {
    let Some(tab_id) = state.tabs.get(idx).map(|t| t.id) else { return };
    let name = cx.local.tables().into_iter().find(|t| matches!(&t.source, TableSource::Result { tab, set: s, .. } if *tab == tab_id.to_string() && *s == set)).map(|t| t.name);
    let Some(name) = name else {
        cx.toast(ToastKind::Info, "This result is not registered as a local table (it may still be arriving, or the tab was re-run).");
        return;
    };
    let text = format!("SELECT *\nFROM {}\nLIMIT 100", quote(&name));
    let i = new_tab(state, cx, Some(text));
    state.active_tab = Some(i);
    run(state, cx, i, RunMode::All);
}

/// Quote a table name for the engine when it needs it.
pub fn quote(name: &str) -> String {
    cobalt_sql::completion::quote_for(name, cobalt_sql::Dialect::Local)
}

/// The table name a tab's result set gets: the tab's title, sanitised, with `_2`, `_3`… for
/// later sets, kept unique against other tabs' tables.
pub fn table_name(engine: &Engine, tab: TabId, title: &str, set: usize) -> String {
    let base = {
        let s = cobalt_local::sanitize_name(title.split(" · ").next().unwrap_or(title));
        if s.is_empty() { format!("tab_{}", tab.to_string().chars().take(8).collect::<String>()) } else { s }
    };
    let mut name = if set == 0 { base.clone() } else { format!("{base}_{}", set + 1) };
    let taken = |n: &str| engine.tables().iter().any(|t| t.name == n && !matches!(&t.source, TableSource::Result { tab: tb, .. } if *tb == tab.to_string()));
    let mut suffix = 'b';
    while taken(&name) {
        name = if set == 0 { format!("{base}_{suffix}") } else { format!("{base}_{}_{suffix}", set + 1) };
        suffix = (suffix as u8 + 1) as char;
    }
    name
}

/// Register a result set that just started arriving for a tab.
pub fn register_result(engine: &Engine, tab: TabId, title: &str, rs: &Arc<ResultSet>) {
    if cobalt_driver::is_showplan_result(&rs.columns) {
        return;
    }
    let name = table_name(engine, tab, title, rs.index);
    engine.register_result(&name, Arc::clone(rs), TableSource::Result { tab: tab.to_string(), set: rs.index, title: title.to_string() });
}

// ---------------------------------------------------------------------------------------------
// Running
// ---------------------------------------------------------------------------------------------

/// F5 and friends on a Local tab.
pub fn run(state: &mut AppState, cx: &Ctx, idx: usize, mode: RunMode) {
    let t = &mut state.tabs[idx];
    if t.is_running() {
        return;
    }
    let Some((script, _)) = ops::script_for(t, mode) else { return };
    t.last_run_mode = Some(mode);
    t.pending_run = None;
    let tab = t.id;
    let uncapped = t.local.as_mut().map(|l| std::mem::take(&mut l.uncapped_next)).unwrap_or(false);
    let plan = mode == RunMode::EstimatedPlan;
    let sugar = cx.settings.local.tsql_sugar;
    let mut statements: Vec<(String, Vec<String>)> = crate::sparkq::split_statements(&script).into_iter().map(|s| if sugar { cobalt_local::sugar::rewrite(&s) } else { (s, Vec::new()) }).collect();
    if plan {
        if statements.len() > 1 {
            cx.toast(ToastKind::Info, "Showing the plan of the first statement; select one statement for another.");
        }
        statements.truncate(1);
    }
    if statements.is_empty() {
        return;
    }
    let run_id = cx.session.new_run();
    let mut view = RunView::new(run_id);
    view.script_hash = hash_text(&t.text);
    if cx.settings.history.capture && !plan {
        let mut e = NewHistoryEntry::new(HISTORY_SOURCE.to_string(), script.clone());
        e.tab_id = Some(tab);
        view.history_id = cx.store.add_history(&e).ok();
    }
    t.run = Some(view);
    t.results_visible = true;
    t.results_tab = if plan { ResultsTab::Messages } else { ResultsTab::Results };
    let handle = LocalRun::default();
    if let Some(l) = t.local.as_mut() {
        l.run = Some(handle.clone());
        l.notes = statements.iter().flat_map(|(_, n)| n.iter().cloned()).collect();
    }
    state.history.loaded = false;
    // the tab's previous tables go (its new results replace them)
    cx.local.deregister_tab(&tab.to_string());
    let engine = Arc::clone(cx.local);
    let session = cx.session.emitter();
    let cap = if uncapped { u64::MAX } else { cx.settings.execution.row_cap.max(1) };
    let egui = cx.egui.clone();
    cx.session.emit(Event::RunStarted { tab, run: run_id, batches: statements.len() });
    if plan {
        // the engine's logical and physical plan, as text in Messages
        let (sql, notes) = statements.remove(0);
        cx.session.spawn(async move {
            let started = Instant::now();
            session.emit(Event::BatchStarted { tab, run: run_id, batch: 0, start_line: 1 });
            for n in &notes {
                session.emit(Event::Message { tab, run: run_id, message: info(format!("rewrote {n}")), batch_start_line: 1 });
            }
            let (error, failed) = match engine.explain(&sql).await {
                Ok(text) => {
                    session.emit(Event::Message { tab, run: run_id, message: info(text), batch_start_line: 1 });
                    (None, false)
                }
                Err(e) => {
                    let msg = ServerMessage { number: 0, state: 0, class: 16, message: e, server: Some("local".into()), procedure: None, line: 0, is_error: true };
                    session.emit(Event::Message { tab, run: run_id, message: msg.clone(), batch_start_line: 1 });
                    (Some(msg), true)
                }
            };
            session.emit(Event::BatchDone { tab, run: run_id, batch: 0, error, elapsed: started.elapsed() });
            session.emit(Event::RunDone { tab, run: run_id, cancelled: false, failed, elapsed: started.elapsed(), total_rows: 0 });
            egui.request_repaint();
        });
        return;
    }
    cx.session.spawn(async move {
        let started = Instant::now();
        let mut total_rows: u64 = 0;
        let mut failed = false;
        let mut cancelled = false;
        let mut cap = cap;
        for (i, (sql, notes)) in statements.iter().enumerate() {
            if handle.cancel.load(Ordering::Relaxed) {
                cancelled = true;
                break;
            }
            let batch_started = Instant::now();
            session.emit(Event::BatchStarted { tab, run: run_id, batch: i, start_line: 1 });
            for n in notes {
                session.emit(Event::Message { tab, run: run_id, message: info(format!("rewrote {n}")), batch_start_line: 1 });
            }
            let df = match engine.sql(sql).await {
                Ok(df) => df,
                Err(e) => {
                    let msg = ServerMessage { number: 0, state: 0, class: 16, message: e.clone(), server: Some("local".into()), procedure: None, line: 0, is_error: true };
                    session.emit(Event::Message { tab, run: run_id, message: msg.clone(), batch_start_line: 1 });
                    session.emit(Event::BatchDone { tab, run: run_id, batch: i, error: Some(msg), elapsed: batch_started.elapsed() });
                    failed = true;
                    break;
                }
            };
            let schema = Arc::new(df.schema().as_arrow().clone());
            let columns = cobalt_local::columns_for(&schema);
            let rs = ResultSet::new(i, columns, Arc::clone(engine.budget()), engine.spill_dir().clone());
            let rs_schema = Arc::clone(&rs.schema);
            session.emit(Event::ResultSetStarted { tab, run: run_id, rs: Arc::clone(&rs) });
            let mut rows: u64 = 0;
            let mut error: Option<ServerMessage> = None;
            match df.execute_stream().await {
                Err(e) => {
                    error = Some(ServerMessage { number: 0, state: 0, class: 16, message: friendly(e.to_string()), server: Some("local".into()), procedure: None, line: 0, is_error: true });
                }
                Ok(mut stream) => {
                    let mut pending: Option<arrow::array::RecordBatch> = None;
                    loop {
                        let item: Result<arrow::array::RecordBatch, String> = match pending.take() {
                            Some(b) => Ok(b),
                            None => match stream.next().await {
                                Some(i) => i.map_err(|e| e.to_string()),
                                None => break,
                            },
                        };
                        if handle.cancel.load(Ordering::Relaxed) {
                            cancelled = true;
                            break;
                        }
                        let batch = match item {
                            Ok(b) => b,
                            Err(e) => {
                                error = Some(ServerMessage { number: 0, state: 0, class: 16, message: friendly(e), server: Some("local".into()), procedure: None, line: 0, is_error: true });
                                break;
                            }
                        };
                        let batch = match cobalt_local::conform(batch, &rs_schema) {
                            Ok(b) => b,
                            Err(e) => {
                                error = Some(ServerMessage { number: 0, state: 0, class: 16, message: format!("could not convert a batch: {e}"), server: Some("local".into()), procedure: None, line: 0, is_error: true });
                                break;
                            }
                        };
                        let mut batch = batch;
                        if rows + batch.num_rows() as u64 > cap {
                            // stop exactly at the cap; the remainder waits for "fetch more"
                            let take = (cap - rows) as usize;
                            pending = Some(batch.slice(take, batch.num_rows() - take));
                            batch = batch.slice(0, take);
                        }
                        let n = batch.num_rows() as u64;
                        if let Err(e) = rs.append(batch) {
                            error = Some(ServerMessage { number: 0, state: 0, class: 16, message: e.to_string(), server: Some("local".into()), procedure: None, line: 0, is_error: true });
                            break;
                        }
                        rows += n;
                        egui.request_repaint();
                        if rows >= cap {
                            // the row cap: pause until "fetch more" or cancel
                            rs.set_state(RunState::Paused);
                            session.emit(Event::Paused { tab, run: run_id, index: i });
                            loop {
                                handle.notify.notified().await;
                                if handle.cancel.load(Ordering::Relaxed) {
                                    break;
                                }
                                if let Some(more) = handle.more.lock().take() {
                                    cap = match more {
                                        Some(n) => rows + n,
                                        None => u64::MAX,
                                    };
                                    break;
                                }
                            }
                            if handle.cancel.load(Ordering::Relaxed) {
                                cancelled = true;
                                break;
                            }
                            rs.set_state(RunState::Streaming);
                        }
                    }
                }
            }
            rs.set_state(match (&error, cancelled) {
                (Some(e), _) => RunState::Error { message: e.message.clone() },
                (None, true) => RunState::Cancelled,
                (None, false) => RunState::Complete,
            });
            session.emit(Event::ResultSetDone { tab, run: run_id, index: i, rows });
            total_rows += rows;
            if let Some(e) = &error {
                session.emit(Event::Message { tab, run: run_id, message: e.clone(), batch_start_line: 1 });
                failed = true;
            }
            session.emit(Event::BatchDone { tab, run: run_id, batch: i, error, elapsed: batch_started.elapsed() });
            if failed || cancelled {
                break;
            }
        }
        session.emit(Event::RunDone { tab, run: run_id, cancelled, failed, elapsed: started.elapsed(), total_rows });
        egui.request_repaint();
    });
}

fn info(text: String) -> ServerMessage {
    ServerMessage { number: 0, state: 0, class: 0, message: text, server: Some("local".into()), procedure: None, line: 0, is_error: false }
}

fn friendly(e: String) -> String {
    for prefix in ["Error during planning: ", "SQL error: ", "Schema error: ", "Execution error: ", "Arrow error: ", "External error: "] {
        if let Some(rest) = e.strip_prefix(prefix) {
            return rest.to_string();
        }
    }
    e
}

/// Stop the run; the rows so far stay.
pub fn cancel(state: &mut AppState, _cx: &Ctx, idx: usize) {
    let Some(t) = state.tabs.get_mut(idx) else { return };
    if let Some(r) = &mut t.run {
        if r.is_live() {
            r.state = RunViewState::Cancelling;
        }
    }
    if let Some(h) = t.local.as_ref().and_then(|l| l.run.clone()) {
        h.cancel.store(true, Ordering::Relaxed);
        h.notify.notify_one();
    }
}

/// The capped result's "fetch more" (`None` = the rest).
pub fn fetch_more(state: &mut AppState, _cx: &Ctx, idx: usize, rows: Option<u64>) {
    let Some(t) = state.tabs.get_mut(idx) else { return };
    if let Some(r) = &mut t.run {
        if r.state == RunViewState::Paused {
            r.state = RunViewState::Running;
            r.paused_set = None;
            r.started = Instant::now().checked_sub(r.elapsed).unwrap_or_else(Instant::now);
        }
    }
    if let Some(h) = t.local.as_ref().and_then(|l| l.run.clone()) {
        *h.more.lock() = Some(rows);
        h.notify.notify_one();
    }
}

/// "Run again without the cap".
pub fn rerun_uncapped(state: &mut AppState, cx: &Ctx, idx: usize) {
    let Some(mode) = state.tabs[idx].last_run_mode.filter(|m| *m != RunMode::EstimatedPlan) else { return };
    if let Some(l) = state.tabs[idx].local.as_mut() {
        l.uncapped_next = true;
    }
    run(state, cx, idx, mode);
}

/// The tab's local tables go with it.
pub fn on_tab_closed(cx: &Ctx, t: &EditorTab) {
    cx.local.deregister_tab(&t.id.to_string());
}

/// The snapshot marker so a restored tab comes back as a Local tab.
pub fn snapshot_binding(t: &EditorTab) -> Option<String> {
    t.local.as_ref().map(|_| SNAPSHOT_PREFIX.to_string())
}

pub fn from_snapshot(database: &str) -> Option<LocalTab> {
    database.strip_prefix(SNAPSHOT_PREFIX).map(|_| LocalTab::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_titles() {
        let e = Engine::new(Arc::new(cobalt_results::MemoryBudget::unlimited()), 64 << 20, std::env::temp_dir()).unwrap();
        let t1 = TabId(uuid::Uuid::from_u128(1));
        let t2 = TabId(uuid::Uuid::from_u128(2));
        assert_eq!(table_name(&e, t1, "SQLQuery_1 · cobalt_test", 0), "sqlquery_1");
        assert_eq!(table_name(&e, t1, "SQLQuery_1 · cobalt_test", 1), "sqlquery_1_2");
        assert_eq!(table_name(&e, t2, "orders.sql", 0), "orders_sql");
    }
}
