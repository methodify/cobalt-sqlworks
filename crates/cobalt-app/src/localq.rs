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
    /// A file (or Delta folder) this tab was opened on, for the toolbar's Import.
    pub file: Option<std::path::PathBuf>,
    /// A Diff tab: the two tables, keys, counts per class and the class filter in force.
    pub diff: Option<DiffView>,
}

/// The state of a Diff tab (`cobalt_local::diff`).
#[derive(Clone, Debug)]
pub struct DiffView {
    pub spec: cobalt_local::diff::DiffSpec,
    /// Counts per class for the run they were computed on.
    pub counts: Option<(u64, Vec<(String, u64)>)>,
    /// The class chip in force (a filter on `__change`), or every row.
    pub class: Option<String>,
}

/// A `ModelCatalog` for the Explore builder over a local table: one table, its columns typed
/// for the filter editor, no measures (numeric columns default to Sum).
pub fn local_catalog(engine: &Engine, table: &str) -> Option<cobalt_core::tabular::ModelCatalog> {
    let info = engine.tables().into_iter().find(|t| t.name == table)?;
    Some(crate::exploreq::catalog_from_columns(table, &info.columns))
}

pub(crate) fn tabular_type(t: &cobalt_core::SqlType) -> cobalt_core::tabular::TabularDataType {
    use cobalt_core::tabular::TabularDataType as T;
    use cobalt_core::SqlType as S;
    match t {
        S::Bit => T::Boolean,
        S::TinyInt | S::SmallInt | S::Int | S::BigInt => T::Int64,
        S::Float | S::Real => T::Double,
        S::Decimal { .. } | S::Numeric { .. } | S::Money | S::SmallMoney => T::Decimal,
        S::Date | S::Time { .. } | S::DateTime | S::DateTime2 { .. } | S::SmallDateTime | S::DateTimeOffset { .. } => T::DateTime,
        S::Binary { .. } | S::VarBinary { .. } | S::Image | S::Timestamp => T::Binary,
        S::Char { .. } | S::VarChar { .. } | S::NChar { .. } | S::NVarChar { .. } | S::Text | S::NText | S::Xml | S::Json | S::UniqueIdentifier => T::String,
        _ => T::Unknown,
    }
}

/// The local table of a tab's result set, when registered.
pub fn table_for_result(cx: &Ctx, tab: TabId, set: usize) -> Option<String> {
    cx.local.tables().into_iter().find(|t| matches!(&t.source, TableSource::Result { tab: tb, set: s, .. } if *tb == tab.to_string() && *s == set)).map(|t| t.name)
}

/// A Diff tab: the diff query over `spec`, run, with the class chips on the toolbar.
pub fn new_diff_tab(state: &mut AppState, cx: &Ctx, spec: cobalt_local::diff::DiffSpec) -> Result<usize, String> {
    let tables = cx.local.tables();
    let a = tables.iter().find(|t| t.name == spec.a).ok_or_else(|| format!("no local table named {}", spec.a))?;
    let b = tables.iter().find(|t| t.name == spec.b).ok_or_else(|| format!("no local table named {}", spec.b))?;
    let (text, _) = cobalt_local::diff::to_sql(&spec, &a.columns, &b.columns)?;
    let i = new_tab(state, cx, Some(text));
    {
        let t = &mut state.tabs[i];
        t.title = format!("Diff · {} vs {}", spec.a, spec.b);
        t.custom_title = true;
        if let Some(l) = t.local.as_mut() {
            l.diff = Some(DiffView { spec, counts: None, class: None });
        }
    }
    state.active_tab = Some(i);
    run(state, cx, i, RunMode::All);
    Ok(i)
}

/// The Diff setup dialog, with `a` chosen and the rest guessed.
pub fn open_diff_dialog(state: &mut AppState, cx: &Ctx, a: Option<String>) {
    let tables = cx.local.tables();
    let a = a.or_else(|| tables.first().map(|t| t.name.clone())).unwrap_or_default();
    let b = tables.iter().rev().find(|t| t.name != a).map(|t| t.name.clone()).unwrap_or_default();
    let keys = match (tables.iter().find(|t| t.name == a), tables.iter().find(|t| t.name == b)) {
        (Some(x), Some(y)) => cobalt_local::diff::guess_keys(&x.columns, &y.columns),
        _ => Vec::new(),
    };
    state.dialog = crate::state::Dialog::DiffSetup { a, b, keys, compare: Vec::new() };
}

/// Count the classes of a finished Diff run once (the toolbar shows them).
pub fn update_diff_counts(state: &mut AppState, idx: usize) {
    let fmt = state.formatter.clone();
    let Some(t) = state.tabs.get_mut(idx) else { return };
    let Some(run) = t.run.as_ref() else { return };
    if run.state != RunViewState::Done {
        return;
    }
    let run_id = run.id.0;
    let Some(rs) = run.result_sets.first().map(|v| Arc::clone(&v.rs)) else { return };
    let Some(l) = t.local.as_mut() else { return };
    let Some(d) = l.diff.as_mut() else { return };
    if d.counts.as_ref().map(|c| c.0 == run_id).unwrap_or(false) {
        return;
    }
    let Some(col) = rs.columns.iter().position(|c| c.name == cobalt_local::diff::CHANGE_COLUMN) else { return };
    let mut counts: std::collections::BTreeMap<String, u64> = Default::default();
    for chunk in 0..rs.chunk_count() {
        if let Some(texts) = rs.column_texts(chunk, col, &fmt) {
            for s in texts.iter() {
                *counts.entry(s.to_string()).or_default() += 1;
            }
        }
    }
    let order = ["added", "removed", "changed", "same"];
    let mut v: Vec<(String, u64)> = order.iter().map(|k| (k.to_string(), counts.get(*k).copied().unwrap_or(0))).collect();
    for (k, n) in counts {
        if !order.contains(&k.as_str()) {
            v.push((k, n));
        }
    }
    d.counts = Some((run_id, v));
}

/// Show one class of the diff (a filter on `__change`), or every row.
pub fn diff_class_filter(state: &mut AppState, cx: &Ctx, idx: usize, class: Option<String>) {
    let Some(col) = state.tabs.get(idx).and_then(|t| t.run.as_ref()).and_then(|r| r.result_sets.first()).and_then(|v| v.rs.columns.iter().position(|c| c.name == cobalt_local::diff::CHANGE_COLUMN)) else { return };
    let spec = cobalt_results::ViewSpec { filters: class.iter().map(|c| cobalt_results::ColumnFilter { column: col, op: cobalt_results::FilterOp::Equals(c.clone()) }).collect(), sort: Vec::new() };
    ops::apply_view(state, cx, idx, 0, spec);
    if let Some(d) = state.tabs.get_mut(idx).and_then(|t| t.local.as_mut()).and_then(|l| l.diff.as_mut()) {
        d.class = class;
    }
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
            if let Some(table) = t.explore.as_ref().and_then(|e| e.spec.source.as_ref()).and_then(|s| s.local_table()).map(str::to_string) {
                if let Some(mc) = local_catalog(cx.local, &table) {
                    t.model_catalog = Some(Arc::new(mc));
                }
            }
        }
    }
}

fn catalog(engine: &Engine) -> cobalt_core::DatabaseCatalog {
    let mut cat = cobalt_core::DatabaseCatalog { database: "local".into(), ..Default::default() };
    for (i, t) in engine.tables().into_iter().enumerate() {
        let id = i as i32 + 1;
        // `scratch.x` is schema `scratch`, name `x`: completion quotes each part on its own
        let (schema, name) = match t.name.split_once('.') {
            Some((s, n)) => (s.to_string(), n.to_string()),
            None => (String::new(), t.name.clone()),
        };
        if !schema.is_empty() && !cat.schemas.contains(&schema) {
            cat.schemas.push(schema.clone());
        }
        cat.objects.push(cobalt_core::ObjectRef { database: "local".into(), schema, name, kind: cobalt_core::ObjectKind::Table, object_id: Some(id) });
        cat.columns.insert(id, t.columns.clone());
    }
    cat
}

/// A file (Parquet, CSV, JSON lines, Arrow) or a Delta folder as a local table, with a Local
/// tab over it (`SELECT * FROM <name> LIMIT 1000`, run). Returns the tab index.
pub fn open_file_table(state: &mut AppState, cx: &Ctx, path: &std::path::Path) -> Result<usize, String> {
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "file".into());
    let mut name = cobalt_local::sanitize_name(&stem);
    if name.is_empty() {
        name = "file".into();
    }
    let existing = cx.local.tables().into_iter().find(|t| matches!(&t.source, TableSource::File { path: p } if p == path)).map(|t| t.name);
    let name = match existing {
        Some(n) => n,
        None => {
            let base = name.clone();
            let mut suffix = 2;
            while cx.local.has_table(&name) {
                name = format!("{base}_{suffix}");
                suffix += 1;
            }
            name
        }
    };
    let engine = Arc::clone(cx.local);
    let p = path.to_path_buf();
    let n = name.clone();
    cx.session.handle().block_on(async move { engine.register_file(&n, &p).await })?;
    let text = format!("SELECT *\nFROM {}\nLIMIT 1000", quote(&name));
    let i = new_tab(state, cx, Some(text));
    if let Some(l) = state.tabs[i].local.as_mut() {
        l.file = Some(path.to_path_buf());
    }
    state.tabs[i].title = format!("{} · local", path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or(name.clone()));
    state.tabs[i].custom_title = true;
    state.active_tab = Some(i);
    refresh_all_catalogs(state, cx);
    run(state, cx, i, RunMode::All);
    Ok(i)
}

/// Import the tab's file into a connection: the Import dialog on the active connected tab, or
/// on a new query tab.
pub fn import_file_into_connection(state: &mut AppState, cx: &Ctx, idx: usize) {
    let Some(path) = state.tabs.get(idx).and_then(|t| t.local.as_ref()).and_then(|l| l.file.clone()) else { return };
    let target = state.tabs.iter().position(|t| t.conn.is_connected() && t.notebook.is_none() && t.dashboard.is_none() && t.explore.is_none() && t.spark.is_none() && t.tabular.is_none() && t.local.is_none());
    let target = match target {
        Some(i) => i,
        None => {
            let (profile, db) = state.tabs.iter().find_map(|t| t.profile.as_ref().map(|p| (Some(p.id), t.conn.database().map(str::to_string)))).unwrap_or((None, None));
            ops::new_query_tab(state, cx, profile, db, None, false)
        }
    };
    state.active_tab = Some(target);
    ops::open_import_dialog(state, cx, target, Some(path));
}

/// Persist a result set or a local table as `scratch.<name>` (Parquet in the app data folder),
/// off the UI thread.
pub fn save_scratch(state: &mut AppState, cx: &Ctx, from: crate::state::ScratchFrom, name: String) {
    let engine = Arc::clone(cx.local);
    let tx = cx.fabric_tx.clone();
    let egui = cx.egui.clone();
    match from {
        crate::state::ScratchFrom::Result { tab_index, set } => {
            let Some(rs) = state.tabs.get(tab_index).and_then(|t| t.run.as_ref()).and_then(|r| r.result_sets.get(set)).map(|v| Arc::clone(&v.rs)) else { return };
            if rs.state().is_live() {
                cx.toast(ToastKind::Info, "Wait for the result to finish arriving.");
                return;
            }
            cx.session.spawn(async move {
                let result = engine.save_scratch(&name, rs).await.map(|i| i.name);
                let _ = tx.send(crate::fabric::FabricEvent::LocalScratch { result });
                egui.request_repaint();
            });
        }
        crate::state::ScratchFrom::Table(table) => {
            if cx.local.tables().iter().any(|t| t.name == table && t.live) {
                cx.toast(ToastKind::Info, "Wait for the result to finish arriving.");
                return;
            }
            cx.session.spawn(async move {
                let result = engine.copy_to_scratch(&name, &table).await.map(|i| i.name);
                let _ = tx.send(crate::fabric::FabricEvent::LocalScratch { result });
                egui.request_repaint();
            });
        }
    }
}

/// A lakehouse Delta table read from OneLake with the signed-in token, as a local table, with
/// a Local tab over it once it is open.
pub fn register_onelake(state: &mut AppState, cx: &Ctx, workspace_id: &str, lakehouse_id: &str, rel: &str, label: &str) {
    let Some(slot) = state.fabric.slot else {
        cx.toast(ToastKind::Warning, "Sign in on the Fabric panel first.");
        return;
    };
    let url = format!("abfss://{workspace_id}@onelake.dfs.fabric.microsoft.com/{lakehouse_id}/Tables/{}", rel.trim_matches('/'));
    let name = {
        let base = cobalt_local::sanitize_name(label);
        let mut n = if base.is_empty() { "lakehouse_table".to_string() } else { base.clone() };
        let mut i = 2;
        while cx.local.has_table(&n) && !cx.local.tables().iter().any(|t| t.name == n && matches!(&t.source, TableSource::OneLake { url: u } if *u == url)) {
            n = format!("{base}_{i}");
            i += 1;
        }
        n
    };
    let engine = Arc::clone(cx.local);
    let tx = cx.fabric_tx.clone();
    let egui = cx.egui.clone();
    let resolver = Arc::clone(cx.resolver);
    let tenant = cx.settings.connections.entra_default_tenant.clone();
    let hint = state.fabric.account.as_ref().map(|a| a.username.clone());
    let prompter = ops::UiPrompter { cancel: Arc::new(AtomicBool::new(false)), device: Arc::new(parking_lot::Mutex::new(None)), url: Arc::new(parking_lot::Mutex::new(None)), egui: egui.clone() };
    cx.toast(ToastKind::Info, format!("Opening {label} from OneLake…"));
    cx.session.spawn(async move {
        let result = match ops::onelake_token(resolver, slot, tenant, hint, prompter).await {
            Ok(token) => engine.register_onelake(&name, &url, &token).await.map(|i| i.name),
            Err(e) => Err(e),
        };
        let _ = tx.send(crate::fabric::FabricEvent::LocalOneLake { result });
        egui.request_repaint();
    });
}

/// Open a Local query tab on a result set of a tab: `SELECT * FROM <its table> LIMIT 100`, run.
pub fn query_result(state: &mut AppState, cx: &Ctx, idx: usize, set: usize) {
    let Some(tab_id) = state.tabs.get(idx).map(|t| t.id) else { return };
    let Some(name) = table_for_result(cx, tab_id, set) else {
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
    // plans are not tables, and neither is the empty set a DDL statement answers with
    if rs.columns.is_empty() || cobalt_driver::is_showplan_result(&rs.columns) {
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
    run_on(t, cx, mode);
    state.history.loaded = false;
}

/// Run on any tab that carries a `LocalTab` (a query tab, a dashboard tile's tab).
pub fn run_on(t: &mut EditorTab, cx: &Ctx, mode: RunMode) {
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
    // names with upper-case letters (lakehouse columns are camelCase) resolve case-insensitively
    let known: Vec<String> = if sugar {
        let mut v: Vec<String> = Vec::new();
        for t in cx.local.tables() {
            v.push(t.name.rsplit('.').next().unwrap_or(&t.name).to_string());
            v.extend(t.columns.iter().map(|c| c.name.clone()));
        }
        v.sort();
        v.dedup();
        v
    } else {
        Vec::new()
    };
    let mut statements: Vec<(String, Vec<String>)> = crate::sparkq::split_statements(&script).into_iter().map(|s| if sugar { cobalt_local::sugar::rewrite_with(&s, &known) } else { (s, Vec::new()) }).collect();
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
        engine.sync_from_context().await;
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
