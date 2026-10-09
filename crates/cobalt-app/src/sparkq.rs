//! Spark SQL query tabs: an ordinary query tab (editor, grid, Messages, exports, history)
//! whose connection is the local Spark session. The tab binds a workspace and a default
//! lakehouse the way a notebook does (`NotebookFabric`), runs its statements in its own worker
//! context, and every statement that returns rows becomes a result set of the run.
//! Design: `docs/design/spark_query_tabs.md`.

use crate::kernel::{self, KernelState, RunReq, SqlRun};
use crate::notebook::{self, SparkPrep};
use crate::ops::{self, Ctx};
use crate::state::{fmt_count, fmt_duration, hash_text, AppState, EditorTab, Followup, GridState, MessageLine, NotebookFabric, PendingEdit, ResultSetView, ResultsTab, RunMode, RunView, RunViewState, SparkSqlMode, SparkSqlRun, SparkTab, ToastKind};
use cobalt_core::TabId;
use cobalt_store::NewHistoryEntry;
use serde_json::Value;

/// The `cell_id` a query tab's run carries through the kernel (notebooks use their cell ids).
pub const CELL_ID: &str = "query";
/// Run to File collects everything: the display cap is lifted to this (the `run_code` path on
/// a worker before 0.7.0; `run_sql` streams without a limit).
const NO_CAP: u64 = 2_000_000_000;
/// Rows per streamed Arrow batch (`run_sql` with `stream`, 0.7.0).
const BATCH_ROWS: u64 = 10_000;
/// The grid keeps this many rows of a Run to File (the rest goes to the file only).
const EXPORT_PREVIEW_ROWS: usize = 1_000;
const RECENT_KEY: &str = "spark:recent_lakehouses";
const RECENT_MAX: usize = 12;

/// A lakehouse the Local Spark root of the Servers tree offers (open tabs, pins, recents).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SparkEntry {
    pub workspace_id: String,
    pub workspace_name: String,
    pub lakehouse_id: String,
    pub lakehouse_name: String,
    /// Spark query tabs bound to it right now (not persisted).
    #[serde(skip)]
    pub open: usize,
}

/// What the Servers tree shows under Local Spark.
pub struct SparkRoot {
    pub session: String,
    pub ready: bool,
    pub starting: bool,
    pub signed_in: bool,
    pub entries: Vec<SparkEntry>,
}

/// The binding stored in a tab snapshot's `database` column (`spark:` + JSON).
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
struct Snapshot {
    workspace_id: Option<String>,
    lakehouse_id: Option<String>,
    write_mode: Option<String>,
    workspace_name: String,
    lakehouse_name: Option<String>,
}

/// Workspace and lakehouse display names of a binding, from the Fabric panel's lists when
/// they are loaded.
pub fn names(state: &AppState, b: &NotebookFabric) -> (Option<String>, Option<String>) {
    let ws = state.fabric.workspace(&b.workspace_id).map(|w| w.display_name.clone());
    let lh = b.lakehouse_id.as_ref().and_then(|id| state.fabric.lakehouses(&b.workspace_id).and_then(|v| v.into_iter().find(|(_, i)| i == id).map(|(n, _)| n)));
    (ws, lh)
}

/// Open a Spark SQL tab bound to `binding` (None = plain local Spark). `names` seeds the
/// display names when the Fabric lists are not loaded yet (a restored or remembered tab).
pub fn new_tab(state: &mut AppState, cx: &Ctx, binding: Option<NotebookFabric>, names: Option<(String, Option<String>)>, text: Option<String>) -> usize {
    let idx = state.new_tab();
    let (ws_name, lh_name) = match (&binding, names) {
        (Some(b), seed) => {
            let (ws, lh) = self::names(state, b);
            let (sws, slh) = seed.unwrap_or_default();
            (ws.unwrap_or(sws), lh.or(slh))
        }
        (None, _) => (String::new(), None),
    };
    let t = &mut state.tabs[idx];
    t.spark = Some(SparkTab { binding, workspace_name: ws_name, lakehouse_name: lh_name, sink: None, sql: None, parse_next: false, uncapped_next: false });
    if let Some(text) = text {
        t.text = text;
        t.mark_saved();
    }
    t.editor.request_focus = true;
    retitle(t);
    remember_recent(cx, &state.tabs[idx]);
    notebook::maybe_early_start(state, cx, idx);
    tick(state, cx);
    idx
}

/// `SparkSQL_n · lakehouse` unless the user renamed the tab.
pub fn retitle(t: &mut EditorTab) {
    if t.custom_title {
        return;
    }
    if let Some(s) = &t.spark {
        t.title = match &s.lakehouse_name {
            Some(lh) => format!("SparkSQL_{} · {lh}", t.untitled_index),
            None => format!("SparkSQL_{}", t.untitled_index),
        };
    }
}

/// Bind the tab to another workspace / lakehouse / write mode. The tab's worker context is
/// dropped so the next run creates one with the new default lakehouse.
pub fn set_binding(state: &mut AppState, cx: &Ctx, idx: usize, binding: Option<NotebookFabric>) {
    if binding.is_some() && state.kernel.fabric.is_none() && state.active_tab == Some(idx) {
        let _ = notebook::rebind_if_unused(state);
    }
    let names = binding.as_ref().map(|b| self::names(state, b)).unwrap_or_default();
    let tab = state.tabs[idx].id;
    let Some(s) = state.tabs[idx].spark.as_mut() else { return };
    let changed = s.binding != binding;
    s.binding = binding;
    s.workspace_name = names.0.unwrap_or_default();
    s.lakehouse_name = names.1;
    retitle(&mut state.tabs[idx]);
    remember_recent(cx, &state.tabs[idx]);
    state.kernel.binding_warned.remove(&tab);
    if changed {
        kernel::drop_context(&mut state.kernel, tab);
    }
    if state.lakehouse_pane.selected.is_some() {
        state.lakehouse_pane.selected = None; // re-resolved from the new binding
    }
    state.tabs[idx].catalog = None;
    state.tabs[idx].catalog_database = None;
    tick(state, cx);
}

/// Display names that arrived after the tab was made (Fabric lists loading later).
pub fn refresh_names(state: &mut AppState, idx: usize) {
    let Some(b) = state.tabs[idx].spark.as_ref().and_then(|s| s.binding.clone()) else { return };
    let (ws, lh) = names(state, &b);
    let t = &mut state.tabs[idx];
    let Some(s) = t.spark.as_mut() else { return };
    let mut changed = false;
    if let Some(ws) = ws {
        if s.workspace_name != ws {
            s.workspace_name = ws;
            changed = true;
        }
    }
    if lh.is_some() && s.lakehouse_name != lh {
        s.lakehouse_name = lh;
        changed = true;
    }
    if changed {
        retitle(t);
    }
}

/// F5 and friends on a Spark tab: the text for `mode` runs as Spark SQL in the tab's context.
pub fn run(state: &mut AppState, cx: &Ctx, idx: usize, mode: RunMode) {
    let t = &mut state.tabs[idx];
    if t.is_running() {
        return;
    }
    let Some((script, _)) = ops::script_for(t, mode) else { return };
    t.last_run_mode = Some(mode);
    t.pending_run = None;
    let sql_mode = if mode == RunMode::EstimatedPlan { SparkSqlMode::Plan } else if t.spark.as_ref().map(|s| s.parse_next).unwrap_or(false) { SparkSqlMode::Parse } else { SparkSqlMode::Run };
    match notebook::prepare_spark(state, cx, idx) {
        SparkPrep::Ready { context_lakehouse, register } => submit(state, cx, idx, script, sql_mode, context_lakehouse, register),
        SparkPrep::Wait => state.tabs[idx].pending_run = Some(mode),
        SparkPrep::Abort => {}
    }
}

/// Parse (Shift+Alt+P): `EXPLAIN` every statement of the text; only the outcome is shown.
pub fn parse(state: &mut AppState, cx: &Ctx, idx: usize) {
    if let Some(s) = state.tabs[idx].spark.as_mut() {
        s.parse_next = true;
    }
    run(state, cx, idx, RunMode::All);
    if let Some(s) = state.tabs[idx].spark.as_mut() {
        s.parse_next = false;
    }
}

/// The capped result's "Run again without the cap".
pub fn rerun_uncapped(state: &mut AppState, cx: &Ctx, idx: usize) {
    let Some(mode) = state.tabs[idx].last_run_mode.filter(|m| *m != RunMode::EstimatedPlan) else { return };
    if let Some(s) = state.tabs[idx].spark.as_mut() {
        s.uncapped_next = true;
    }
    run(state, cx, idx, mode);
}

fn submit(state: &mut AppState, cx: &Ctx, idx: usize, script: String, mode: SparkSqlMode, context_lakehouse: Option<String>, register: Vec<Value>) {
    let use_context = state.kernel.has("contexts") || !state.kernel.state.is_ready();
    let want_description = state.kernel.has("job_description") || !state.kernel.state.is_ready();
    let t = &mut state.tabs[idx];
    let tab = t.id;
    let job = if mode == SparkSqlMode::Run { t.pending_export.take() } else { None };
    let uncapped = t.spark.as_mut().map(|s| std::mem::take(&mut s.uncapped_next)).unwrap_or(false);
    let cap = cx.settings.notebooks.spark_row_limit.max(1);
    let no_cap = job.is_some() || uncapped;
    let limit = if no_cap { NO_CAP } else { cap };
    let mut statements = split_statements(&script);
    match mode {
        SparkSqlMode::Plan => {
            // one plan: the first statement of the selection (or the statement under the caret)
            if statements.len() > 1 {
                cx.toast(ToastKind::Info, "Showing the plan of the first statement; select one statement for another.");
            }
            statements.truncate(1);
            statements = statements.into_iter().map(|s| format!("EXPLAIN EXTENDED {s}")).collect();
        }
        SparkSqlMode::Parse => {} // the helper analyzes each statement itself
        SparkSqlMode::Run => {}
    }
    if statements.is_empty() {
        return;
    }
    let script = statements.join(";\n");
    // Parse goes through the helper (analysis only, nothing executed); the rest streams
    let (code, sql) = if mode == SparkSqlMode::Parse {
        let list = format!("[{}]", statements.iter().map(|s| notebook::py_literal(s)).collect::<Vec<_>>().join(", "));
        (format!("__cobalt_parse({list})"), None)
    } else {
        (format!("__cobalt_sql_all({}, {limit})", notebook::py_literal(&script)), Some(SqlRun { statements: statements.clone(), limit: if no_cap || mode != SparkSqlMode::Run { None } else { Some(cap) }, batch_rows: BATCH_ROWS }))
    };
    let run_id = cx.session.new_run();
    let mut view = RunView::new(run_id);
    view.script_hash = hash_text(&t.text);
    view.export_target = job.as_ref().map(|j| j.display_target());
    if cx.settings.history.capture && mode == SparkSqlMode::Run {
        let mut e = NewHistoryEntry::new(format!("Local Spark ({})", cx.settings.spark.profile), script.clone());
        e.database = t.spark.as_ref().and_then(|s| s.lakehouse_name.clone());
        e.tab_id = Some(tab);
        view.history_id = cx.store.add_history(&e).ok();
    }
    t.run = Some(view);
    t.results_visible = true;
    t.results_tab = match mode {
        SparkSqlMode::Run => ResultsTab::Results,
        SparkSqlMode::Plan => ResultsTab::Plan,
        SparkSqlMode::Parse => ResultsTab::Messages,
    };
    let title = t.title.clone();
    let export = job.is_some();
    let sink = job.map(|j| ops::spawn_run_export(state, cx, Some(tab), *j));
    if let Some(s) = state.tabs[idx].spark.as_mut() {
        s.sink = sink;
        s.sql = Some(SparkSqlRun { mode, statements: statements.len(), limit: if no_cap || mode != SparkSqlMode::Run { None } else { Some(cap) }, sets: Default::default(), export, sink_open: None, texts: Default::default(), failed: false });
    }
    let job_description = if want_description { script.lines().map(str::trim).find(|l| !l.is_empty()).map(|l| l.chars().take(80).collect::<String>()) } else { None };
    let context = if use_context { Some(kernel::context_id(tab)) } else { None };
    kernel::run(&mut state.kernel, RunReq { tab, cell_id: CELL_ID.into(), code, sql, context, context_lakehouse, context_name: Some(title), job_description, register });
    state.history.loaded = false;
}

/// Statements of a script: split on `;` outside quotes (`'`, `"`, backticks) and `--` comments;
/// blank ones dropped. The Python helper in the worker splits the same way.
pub fn split_statements(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut quote: Option<char> = None;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if let Some(q) = quote {
            buf.push(c);
            if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '\'' | '"' | '`' => {
                quote = Some(c);
                buf.push(c);
            }
            '-' if chars.peek() == Some(&'-') => {
                buf.push(c);
                for n in chars.by_ref() {
                    buf.push(n);
                    if n == '\n' {
                        break;
                    }
                }
            }
            ';' => {
                let st = buf.trim();
                if !st.is_empty() {
                    out.push(st.to_string());
                }
                buf.clear();
            }
            _ => buf.push(c),
        }
    }
    let st = buf.trim();
    if !st.is_empty() {
        out.push(st.to_string());
    }
    out
}

/// A streamed batch of statement `statement`: into the statement's result set (created on its
/// first batch) and, during Run to File, to the export sink.
pub fn on_sql_batch(state: &mut AppState, idx: usize, statement: usize, bytes: &[u8]) {
    use crate::session::SinkMsg;
    let t = &mut state.tabs[idx];
    let Some(sp) = t.spark.as_mut() else { return };
    let Some(sql) = sp.sql.as_mut() else { return };
    let Some(run) = t.run.as_mut() else { return };
    if !run.is_live() {
        return;
    }
    let (columns, batches) = match notebook::ipc_columns_batches(bytes) {
        Ok(x) => x,
        Err(e) => {
            run.messages.push(msg(format!("Could not read a result batch: {e}"), true));
            return;
        }
    };
    if sql.mode != SparkSqlMode::Run {
        // EXPLAIN answers one `plan` string; an analysis error comes back as that string too
        let text = batches.first().and_then(|b| b.column(0).as_any().downcast_ref::<arrow::array::StringArray>().map(|a| a.value(0).to_string())).unwrap_or_default();
        sql.texts.insert(statement, text);
        return;
    }
    let set_index = match sql.sets.get(&statement) {
        Some(&i) => i,
        None => {
            let i = run.result_sets.len();
            let rs = cobalt_results::ResultSet::new(i, columns, std::sync::Arc::new(cobalt_results::MemoryBudget::unlimited()), std::env::temp_dir());
            if let (true, Some(sink)) = (sql.export, sp.sink.as_ref()) {
                let _ = sink.tx.send(SinkMsg::SetStart { index: i, columns: rs.columns.clone(), schema: rs.schema.clone() });
                sql.sink_open = Some(statement);
            }
            run.result_sets.push(ResultSetView { rs, grid: GridState::default(), is_plan: false, profile: None });
            sql.sets.insert(statement, i);
            i
        }
    };
    let rs = run.result_sets[set_index].rs.clone();
    for b in batches {
        if sql.export {
            // the file gets every row; the grid keeps a preview
            let cast = match notebook::append_preview(&rs, b, EXPORT_PREVIEW_ROWS) {
                Ok(b) => b,
                Err(e) => {
                    run.messages.push(msg(format!("Could not read a result batch: {e}"), true));
                    continue;
                }
            };
            if let Some(sink) = sp.sink.as_ref() {
                if sink.tx.send(SinkMsg::Batch(cast)).is_err() {
                    run.messages.push(msg("The export stopped taking rows.".into(), true));
                }
            }
        } else if let Err(e) = notebook::append_cast(&rs, b) {
            run.messages.push(msg(format!("Could not read a result batch: {e}"), true));
        }
    }
    run.total_rows = run.result_sets.iter().map(|s| s.rs.row_count() as u64).sum();
}

/// Statement `statement` ended: its result set is complete, DML metrics and completion go to
/// Messages, an error ends the run's messages with the compacted Spark text.
pub fn on_sql_statement(state: &mut AppState, idx: usize, statement: usize, result: &Result<Value, String>) {
    use crate::session::SinkMsg;
    let t = &mut state.tabs[idx];
    let Some(sp) = t.spark.as_mut() else { return };
    let Some(sql) = sp.sql.as_mut() else { return };
    let Some(run) = t.run.as_mut() else { return };
    let many = sql.statements > 1;
    let where_ = |n: usize| if many { format!("Statement {}: ", n + 1) } else { String::new() };
    if let Some(&i) = sql.sets.get(&statement) {
        if let Some(v) = run.result_sets.get(i) {
            v.rs.set_state(if result.is_ok() { cobalt_results::RunState::Complete } else { cobalt_results::RunState::Error { message: "failed".into() } });
        }
    }
    if sql.sink_open == Some(statement) {
        sql.sink_open = None;
        if let Some(sink) = sp.sink.as_ref() {
            let _ = sink.tx.send(if result.is_ok() { SinkMsg::SetEnd } else { SinkMsg::Failed(result.as_ref().err().map(|e| kernel::compact_spark_error(e)).unwrap_or_default()) });
        }
    }
    match result {
        Ok(v) => {
            for n in v.get("notices").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
                run.messages.push(msg(format!("notice: {n}"), false));
            }
            let elapsed = v.get("elapsed_s").and_then(Value::as_f64).map(|s| format!(" ({s:.1} s)")).unwrap_or_default();
            let rows = v.get("row_count").and_then(Value::as_u64).unwrap_or(0);
            if let Some(m) = v.get("metrics").filter(|m| m.is_object()) {
                // Delta answers UPDATE / DELETE / MERGE with a one-row count frame: the
                // Messages line says it; the frame is not a result set
                if m.get("source").and_then(Value::as_str) == Some("result") {
                    if let Some(i) = sql.sets.remove(&statement) {
                        if i + 1 == run.result_sets.len() {
                            run.result_sets.pop();
                        }
                    }
                }
                let n = m.get("affected_rows").and_then(Value::as_u64);
                let parts: Vec<String> = ["inserted", "updated", "deleted"].iter().filter_map(|k| m.get(*k).and_then(Value::as_u64).map(|x| format!("{} {}", fmt_count(x), k))).collect();
                let detail = if parts.len() > 1 { format!(" — {}", parts.join(", ")) } else { String::new() };
                match (n, m.get("error").and_then(Value::as_str)) {
                    (Some(n), _) => run.messages.push(msg(format!("{}({} row{} affected{detail}){elapsed}", where_(statement), fmt_count(n), if n == 1 { "" } else { "s" }), false)),
                    (None, Some(e)) => run.messages.push(msg(format!("{}statement completed{elapsed}; affected rows unknown: {e}", where_(statement)), false)),
                    (None, None) => run.messages.push(msg(format!("{}statement completed{elapsed}", where_(statement)), false)),
                }
            } else if sql.mode != SparkSqlMode::Run {
                let text = sql.texts.remove(&statement).unwrap_or_default();
                match explain_error(&text) {
                    Some(e) => {
                        sql.failed = true;
                        run.messages.push(msg(format!("{}{e}", where_(statement)), true));
                    }
                    None if sql.mode == SparkSqlMode::Parse => run.messages.push(msg(format!("{}parsed and analyzed{elapsed}", where_(statement)), false)),
                    None => {
                        run.text_plan = Some(plan_sections(&text));
                        run.messages.push(msg(format!("Plan ready{elapsed}"), false));
                    }
                }
            } else if sql.sets.contains_key(&statement) {
                if let Some(limit) = sql.limit {
                    if rows >= limit {
                        run.capped = true;
                        run.messages.push(msg(format!("{}the first {} rows (Settings → Notebooks → rows a Spark DataFrame brings back); there may be more", where_(statement), fmt_count(limit)), false));
                    }
                }
            } else {
                run.messages.push(msg(format!("{}statement completed{elapsed}", where_(statement)), false));
            }
        }
        Err(e) => {
            if !e.contains("KeyboardInterrupt: interrupted") {
                run.messages.push(msg(format!("{}{}", where_(statement), kernel::compact_spark_error(e)), true));
            }
        }
    }
}

/// Cancel: interrupt the session when this tab's statement is the one running; a run that is
/// still waiting for the session (pending) is simply dropped.
pub fn cancel(state: &mut AppState, cx: &Ctx, idx: usize) {
    let t = &mut state.tabs[idx];
    let tab = t.id;
    t.pending_run = None;
    let running_here = state.kernel.busy.as_ref().map(|(b, _)| *b == tab).unwrap_or(false);
    let Some(r) = state.tabs[idx].run.as_mut() else { return };
    if !r.is_live() {
        return;
    }
    if running_here {
        r.state = RunViewState::Cancelling;
        match kernel::interrupt(&mut state.kernel) {
            kernel::InterruptAction::Sent => cx.toast(ToastKind::Info, "Interrupting — Spark jobs are being cancelled. Cancel again to end the session."),
            kernel::InterruptAction::Killed => cx.toast(ToastKind::Info, "Stopping the Spark session; the next run starts a new one."),
            kernel::InterruptAction::Nothing => {}
        }
    } else {
        // queued behind another tab's statement, or the session is still starting
        state.kernel.waiting.retain(|w| w.tab != tab);
        finish_run(state, idx, RunViewState::Cancelled, None);
        if state.kernel.state.is_starting() && state.kernel.busy.is_none() && state.kernel.waiting.is_empty() {
            // nothing else wants the session: let it keep starting (early start semantics)
        }
    }
}

/// Streamed output of the running statement (stdout/stderr), as the notebook pump shows it.
pub fn on_output(state: &mut AppState, tab: TabId, stream: &str, text: &str) {
    let Some(t) = state.tab_mut(tab) else { return };
    let Some(run) = t.run.as_mut() else { return };
    if !run.is_live() {
        return;
    }
    for line in text.lines() {
        let t = line.trim_end();
        if t.contains(kernel::ARROW_MARK) {
            continue;
        }
        if stream == "stderr" && (t.is_empty() || t.contains(" WARN ") || t.contains(" INFO ") || t.starts_with('[') && t.contains("Stage ")) {
            continue;
        }
        run.messages.push(msg(t.to_string(), false));
    }
}

/// The worker's reply for this tab's statement: result sets, messages, the run's state, the
/// history record, and the Run to File sink when one is attached.
pub fn on_done(state: &mut AppState, idx: usize, result: &Result<Value, String>, blobs: &[Vec<u8>]) -> Vec<Followup> {
    if let Ok(v) = result {
        if v.get("sql").and_then(Value::as_bool) == Some(true) {
            return on_sql_done(state, idx, v);
        }
    }
    let o = kernel::outcome(result, blobs);
    let interrupted = o.interrupted || matches!(result, Err(e) if e.starts_with("interrupted"));
    let t = &mut state.tabs[idx];
    let sink = t.spark.as_mut().and_then(|s| s.sink.take());
    if let Some(sp) = t.spark.as_mut() {
        sp.sql = None;
    }
    let Some(run) = t.run.as_mut() else { return Vec::new() };
    run.elapsed = run.started.elapsed();
    run.messages.clear();
    let mut sets = Vec::new();
    for rs in o.result_sets {
        sets.push(rs.clone());
        run.result_sets.push(ResultSetView { rs, grid: GridState::default(), is_plan: false, profile: None });
    }
    run.total_rows = run.result_sets.iter().map(|s| s.rs.row_count() as u64).sum();
    run.messages.extend(o.messages);
    if o.failed {
        kernel::compact_sql_error(&mut run.messages);
    }
    run.state = if interrupted {
        RunViewState::Cancelled
    } else if o.failed {
        RunViewState::Failed
    } else {
        RunViewState::Done
    };
    if !interrupted {
        run.messages.push(msg(format!("Total execution time: {}", fmt_duration(run.elapsed)), false));
    }
    let error = run.messages.iter().rev().find(|m| m.is_error).map(|m| m.text.clone());
    let followups = vec![Followup::FinishHistory { history_id: run.history_id, elapsed: run.elapsed, rows: run.total_rows, cancelled: interrupted, failed: o.failed, error }];
    if let Some(sink) = sink {
        feed_sink(sink, sets, if interrupted { Some("cancelled".to_string()) } else if o.failed { Some(error_text(&state.tabs[idx])) } else { None });
    }
    followups
}

/// The end of a streamed run: state, elapsed, history, the export sink's end.
fn on_sql_done(state: &mut AppState, idx: usize, v: &Value) -> Vec<Followup> {
    use crate::session::SinkMsg;
    let t = &mut state.tabs[idx];
    let sink = t.spark.as_mut().and_then(|s| s.sink.take());
    let sql = t.spark.as_mut().and_then(|s| s.sql.take());
    let Some(run) = t.run.as_mut() else { return Vec::new() };
    let interrupted = v.get("interrupted").and_then(Value::as_bool) == Some(true);
    let failed = !interrupted && (v.get("ok").and_then(Value::as_bool) != Some(true) || sql.as_ref().map(|s| s.failed).unwrap_or(false));
    if sql.as_ref().map(|s| s.mode == SparkSqlMode::Plan && s.failed).unwrap_or(false) {
        t.results_tab = ResultsTab::Messages;
    }
    run.elapsed = run.started.elapsed();
    for s in run.result_sets.iter() {
        if matches!(s.rs.state(), cobalt_results::RunState::Streaming) {
            s.rs.set_state(if interrupted { cobalt_results::RunState::Cancelled } else { cobalt_results::RunState::Complete });
        }
    }
    run.total_rows = run.result_sets.iter().map(|s| s.rs.row_count() as u64).sum();
    run.state = if interrupted {
        RunViewState::Cancelled
    } else if failed {
        RunViewState::Failed
    } else {
        RunViewState::Done
    };
    if interrupted {
        run.messages.push(msg("Interrupted (Spark jobs cancelled)".into(), false));
    } else {
        run.messages.push(msg(format!("Total execution time: {}", fmt_duration(run.elapsed)), false));
    }
    let error = run.messages.iter().rev().find(|m| m.is_error).map(|m| m.text.clone());
    if let Some(sink) = sink {
        let open = sql.as_ref().and_then(|s| s.sink_open).is_some();
        let _ = sink.tx.send(if interrupted {
            SinkMsg::Failed("cancelled".into())
        } else if failed {
            SinkMsg::Failed(error.clone().unwrap_or_else(|| "the statement failed".into()))
        } else if open {
            SinkMsg::Failed("the statement ended before its rows did".into())
        } else {
            SinkMsg::RunEnd
        });
    }
    vec![Followup::FinishHistory { history_id: run.history_id, elapsed: run.elapsed, rows: run.total_rows, cancelled: interrupted, failed, error }]
}

fn error_text(t: &EditorTab) -> String {
    t.run.as_ref().and_then(|r| r.messages.iter().rev().find(|m| m.is_error).map(|m| m.text.clone())).unwrap_or_else(|| "the statement failed".into())
}

/// Run to File: the collected result sets go to the export thread the way the session actor
/// streams them (on a thread of their own; the channel is small and the writer may be slow).
fn feed_sink(sink: crate::session::RunSink, sets: Vec<std::sync::Arc<cobalt_results::ResultSet>>, failed: Option<String>) {
    use crate::session::SinkMsg;
    std::thread::Builder::new()
        .name("cobalt-spark-export".into())
        .spawn(move || {
            if let Some(e) = failed {
                let _ = sink.tx.send(SinkMsg::Failed(e));
                return;
            }
            for (i, rs) in sets.iter().enumerate() {
                if sink.tx.send(SinkMsg::SetStart { index: i, columns: rs.columns.clone(), schema: rs.schema.clone() }).is_err() {
                    return;
                }
                for b in rs.raw_batches() {
                    if sink.tx.send(SinkMsg::Batch(b)).is_err() {
                        return;
                    }
                }
                if sink.tx.send(SinkMsg::SetEnd).is_err() {
                    return;
                }
            }
            let _ = sink.tx.send(SinkMsg::RunEnd);
        })
        .ok();
}

/// The session ended under a live run: fail it (the notebook pump does the same for cells).
pub fn fail_live(state: &mut AppState, err: &str) {
    for i in 0..state.tabs.len() {
        if state.tabs[i].spark.is_none() {
            continue;
        }
        state.tabs[i].pending_run = None;
        let live = state.tabs[i].run.as_ref().map(|r| r.is_live()).unwrap_or(false);
        if live {
            finish_run(state, i, RunViewState::Failed, Some(err.to_string()));
        }
    }
}

fn finish_run(state: &mut AppState, idx: usize, st: RunViewState, error: Option<String>) {
    let t = &mut state.tabs[idx];
    let sink = t.spark.as_mut().and_then(|s| s.sink.take());
    if let Some(s) = t.spark.as_mut() {
        s.sql = None;
    }
    if let Some(r) = t.run.as_mut() {
        r.state = st;
        r.elapsed = r.started.elapsed();
        if let Some(e) = &error {
            r.messages.push(msg(e.clone(), true));
        }
    }
    if let Some(sink) = sink {
        let _ = sink.tx.send(crate::session::SinkMsg::Failed(error.unwrap_or_else(|| "cancelled".into())));
    }
}

/// Tabs whose run waited for the session (or the workspace's lakehouse list): try again.
pub fn pump_pending(state: &mut AppState, cx: &Ctx) {
    let pending: Vec<(usize, RunMode)> = state.tabs.iter().enumerate().filter_map(|(i, t)| t.spark.as_ref().and(t.pending_run).map(|m| (i, m))).collect();
    for (i, mode) in pending {
        if state.tabs.get(i).map(|t| t.spark.is_some()).unwrap_or(false) {
            state.tabs[i].pending_run = None;
            run(state, cx, i, mode);
        }
    }
}

/// Each frame: a Spark tab bound to a lakehouse gets that lakehouse's completion catalog
/// (loaded once per lakehouse, shared by its tabs).
pub fn tick(state: &mut AppState, cx: &Ctx) {
    let mut want: Vec<(usize, String, String, String)> = Vec::new();
    for (i, t) in state.tabs.iter().enumerate() {
        let Some(s) = &t.spark else { continue };
        let Some(b) = &s.binding else { continue };
        let Some(lh) = &b.lakehouse_id else { continue };
        if t.catalog.is_some() {
            continue;
        }
        want.push((i, b.workspace_id.clone(), lh.clone(), s.lakehouse_name.clone().unwrap_or_default()));
    }
    for (i, ws, lh, name) in want {
        if let Some(cat) = state.spark_catalogs.get(&lh).cloned() {
            state.tabs[i].catalog = Some(cat.clone());
            state.tabs[i].catalog_database = Some(cat.database.clone());
        } else if !state.spark_catalog_loading.contains(&lh) {
            crate::fabric::load_spark_catalog(state, cx, &ws, &lh, &name);
        }
    }
}

/// The pane's Refresh on a Spark tab: the lakehouse's catalog is read again.
pub fn refresh_catalog(state: &mut AppState, cx: &Ctx, workspace_id: &str, lakehouse_id: &str, lakehouse_name: &str) {
    state.spark_catalogs.remove(lakehouse_id);
    for t in state.tabs.iter_mut() {
        if t.spark.as_ref().and_then(|s| s.binding.as_ref()).and_then(|b| b.lakehouse_id.as_deref()) == Some(lakehouse_id) {
            t.catalog = None;
        }
    }
    crate::fabric::load_spark_catalog(state, cx, workspace_id, lakehouse_id, lakehouse_name);
}

/// The columns of a Delta table from one `_delta_log/N.json` commit file: the last
/// `metaData.schemaString` in it, as `ColumnInfo`s with Spark types mapped to SQL types.
pub fn delta_log_columns(text: &str) -> Option<Vec<cobalt_core::ColumnInfo>> {
    let mut found: Option<Vec<cobalt_core::ColumnInfo>> = None;
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let Some(schema) = v.get("metaData").and_then(|m| m.get("schemaString")).and_then(Value::as_str) else { continue };
        let Ok(sch) = serde_json::from_str::<Value>(schema) else { continue };
        let fields = sch.get("fields").and_then(Value::as_array).cloned().unwrap_or_default();
        let cols = fields
            .iter()
            .enumerate()
            .filter_map(|(i, f)| {
                let name = f.get("name").and_then(Value::as_str)?.to_string();
                let ty = spark_type_to_sql(f.get("type").unwrap_or(&Value::Null));
                let nullable = f.get("nullable").and_then(Value::as_bool).unwrap_or(true);
                Some(cobalt_core::ColumnInfo::new(name, ty, nullable, i))
            })
            .collect();
        found = Some(cols);
    }
    found
}

/// A Delta/Spark type (`"string"`, `"decimal(18,2)"`, `{"type": "array", …}`) as the SQL type
/// the grid and the completer show.
pub fn spark_type_to_sql(t: &Value) -> cobalt_core::SqlType {
    use cobalt_core::SqlType as S;
    let Some(name) = t.as_str() else {
        let kind = t.get("type").and_then(Value::as_str).unwrap_or("struct");
        return S::Other(kind.to_string());
    };
    let lower = name.to_ascii_lowercase();
    match lower.as_str() {
        "string" => S::NVarChar { len: None },
        "long" | "bigint" => S::BigInt,
        "integer" | "int" => S::Int,
        "short" | "smallint" => S::SmallInt,
        "byte" | "tinyint" => S::TinyInt,
        "double" => S::Float,
        "float" => S::Real,
        "boolean" => S::Bit,
        "date" => S::Date,
        "timestamp" | "timestamp_ntz" => S::DateTime2 { scale: 6 },
        "binary" => S::VarBinary { len: None },
        _ if lower.starts_with("decimal") => {
            let args: Vec<u8> = lower.trim_start_matches("decimal").trim_matches(|c| c == '(' || c == ')').split(',').filter_map(|a| a.trim().parse().ok()).collect();
            S::Decimal { precision: args.first().copied().unwrap_or(10), scale: args.get(1).copied().unwrap_or(0) }
        }
        _ => S::Other(name.to_string()),
    }
}

/// A notebook SQL cell as a Spark SQL tab with the notebook's binding.
pub fn open_from_cell(state: &mut AppState, cx: &Ctx, idx: usize, cell: usize) {
    let Some(nb) = state.tabs[idx].notebook.as_deref() else { return };
    let Some(c) = nb.nb.cells.get(cell) else { return };
    let body = c.split_magic().1.trim().to_string();
    let binding = nb.fabric.clone();
    let names = binding.as_ref().map(|b| {
        let (ws, lh) = names(state, b);
        (ws.unwrap_or_default(), lh)
    });
    new_tab(state, cx, binding, names, Some(body));
}

/// Release the tab's worker context ("Disconnect"); the session itself follows the lifecycle setting.
pub fn disconnect(state: &mut AppState, idx: usize) {
    let tab = state.tabs[idx].id;
    kernel::drop_context(&mut state.kernel, tab);
}

/// Insert text at the caret (the Lakehouse pane's "insert" on a Spark tab).
pub fn insert_at_cursor(t: &mut EditorTab, code: &str) {
    let cursor = t.editor.cursor.min(t.text.len());
    let cursor = (0..=cursor).rev().find(|&i| t.text.is_char_boundary(i)).unwrap_or(0);
    let mut text = String::with_capacity(t.text.len() + code.len() + 2);
    text.push_str(&t.text[..cursor]);
    let needs_nl = !text.is_empty() && !text.ends_with('\n');
    if needs_nl {
        text.push('\n');
    }
    text.push_str(code);
    let after = text.chars().count();
    let rest = &t.text[cursor..];
    if !rest.is_empty() && !rest.starts_with(['\n', '\r']) {
        text.push('\n');
    }
    text.push_str(rest);
    t.editor.pending_edit = Some(PendingEdit::SetText { text, cursor: after });
    t.editor.request_focus = true;
}

// ---------------------------------------------------------------------------------------------
// Snapshots (hot exit) and the Servers tree root
// ---------------------------------------------------------------------------------------------

pub fn snapshot_binding(t: &EditorTab) -> Option<String> {
    let s = t.spark.as_ref()?;
    let snap = Snapshot {
        workspace_id: s.binding.as_ref().map(|b| b.workspace_id.clone()),
        lakehouse_id: s.binding.as_ref().and_then(|b| b.lakehouse_id.clone()),
        write_mode: s.binding.as_ref().map(|b| b.write_mode.clone()),
        workspace_name: s.workspace_name.clone(),
        lakehouse_name: s.lakehouse_name.clone(),
    };
    Some(format!("spark:{}", serde_json::to_string(&snap).unwrap_or_default()))
}

/// A snapshot's `database` column back into a Spark tab state (None for ordinary tabs).
pub fn from_snapshot(database: &str) -> Option<SparkTab> {
    let json = database.strip_prefix("spark:")?;
    let snap: Snapshot = serde_json::from_str(json).unwrap_or_default();
    let binding = snap.workspace_id.map(|workspace_id| NotebookFabric { workspace_id, lakehouse_id: snap.lakehouse_id, write_mode: snap.write_mode.unwrap_or_else(|| "sandbox".into()), preload: false });
    Some(SparkTab { binding, workspace_name: snap.workspace_name, lakehouse_name: snap.lakehouse_name, sink: None, sql: None, parse_next: false, uncapped_next: false })
}

fn remember_recent(cx: &Ctx, t: &EditorTab) {
    let Some(s) = &t.spark else { return };
    let Some(b) = &s.binding else { return };
    let Some(lh_id) = &b.lakehouse_id else { return };
    let entry = SparkEntry { workspace_id: b.workspace_id.clone(), workspace_name: s.workspace_name.clone(), lakehouse_id: lh_id.clone(), lakehouse_name: s.lakehouse_name.clone().unwrap_or_default(), open: 0 };
    let mut list: Vec<SparkEntry> = cx.store.get_kv(RECENT_KEY).ok().flatten().unwrap_or_default();
    list.retain(|e| e.lakehouse_id != entry.lakehouse_id);
    list.insert(0, entry);
    list.truncate(RECENT_MAX);
    let _ = cx.store.set_kv(RECENT_KEY, &list);
}

/// Lakehouses for the Local Spark root: bound to open Spark tabs and notebooks first, then the
/// Fabric panel's pinned lakehouses, then the ones used before.
pub fn root(state: &AppState, cx: &Ctx) -> SparkRoot {
    let mut entries: Vec<SparkEntry> = Vec::new();
    let mut push = |e: SparkEntry| {
        if let Some(have) = entries.iter_mut().find(|x| x.lakehouse_id == e.lakehouse_id) {
            have.open += e.open;
            if have.lakehouse_name.is_empty() {
                have.lakehouse_name = e.lakehouse_name;
            }
            if have.workspace_name.is_empty() {
                have.workspace_name = e.workspace_name;
            }
        } else {
            entries.push(e);
        }
    };
    for t in &state.tabs {
        if let Some(s) = &t.spark {
            if let Some(b) = &s.binding {
                if let Some(lh) = &b.lakehouse_id {
                    push(SparkEntry { workspace_id: b.workspace_id.clone(), workspace_name: s.workspace_name.clone(), lakehouse_id: lh.clone(), lakehouse_name: s.lakehouse_name.clone().unwrap_or_default(), open: 1 });
                }
            }
        } else if let Some(b) = t.notebook.as_deref().and_then(|nb| nb.fabric.as_ref()) {
            if let Some(lh) = &b.lakehouse_id {
                let (ws, name) = names(state, b);
                push(SparkEntry { workspace_id: b.workspace_id.clone(), workspace_name: ws.unwrap_or_default(), lakehouse_id: lh.clone(), lakehouse_name: name.unwrap_or_default(), open: 0 });
            }
        }
    }
    for p in state.fabric.pins.iter().filter(|p| p.item_kind.eq_ignore_ascii_case("lakehouse")) {
        push(SparkEntry { workspace_id: p.workspace_id.clone(), workspace_name: p.workspace_name.clone(), lakehouse_id: p.item_id.clone(), lakehouse_name: p.display_name.clone(), open: 0 });
    }
    let recent: Vec<SparkEntry> = cx.store.get_kv(RECENT_KEY).ok().flatten().unwrap_or_default();
    for e in recent {
        push(e);
    }
    // names from the Fabric lists when they are fresher than what was stored
    for e in entries.iter_mut() {
        if let Some(w) = state.fabric.workspace(&e.workspace_id) {
            e.workspace_name = w.display_name.clone();
        }
        if let Some(n) = state.fabric.lakehouses(&e.workspace_id).and_then(|v| v.into_iter().find(|(_, i)| *i == e.lakehouse_id).map(|(n, _)| n)) {
            e.lakehouse_name = n;
        }
    }
    let k = &state.kernel;
    SparkRoot {
        session: k.state.label(),
        ready: k.state.is_ready(),
        starting: k.state.is_starting(),
        signed_in: state.fabric.slot.is_some(),
        entries,
    }
}

/// A tree entry → the binding a new tab gets (sandbox, no preload: the lakehouse's own policy applies).
pub fn binding_for(entry: &SparkEntry) -> NotebookFabric {
    NotebookFabric { workspace_id: entry.workspace_id.clone(), lakehouse_id: Some(entry.lakehouse_id.clone()), write_mode: "sandbox".into(), preload: false }
}

/// The binding a new Spark tab inherits from the active tab (a Spark tab or a Spark notebook).
pub fn inherited_binding(state: &AppState) -> (Option<NotebookFabric>, Option<(String, Option<String>)>) {
    let Some(t) = state.active() else { return (None, None) };
    if let Some(s) = &t.spark {
        return (s.binding.clone(), Some((s.workspace_name.clone(), s.lakehouse_name.clone())));
    }
    if let Some(b) = t.notebook.as_deref().and_then(|nb| nb.fabric.clone()) {
        let (ws, lh) = names(state, &b);
        return (Some(b), Some((ws.unwrap_or_default(), lh)));
    }
    (None, None)
}

/// The status bar / toolbar label of the session for a Spark tab.
pub fn session_label(state: &AppState) -> (String, bool) {
    let k = &state.kernel;
    let profile = &k.profile;
    let profile = if profile.is_empty() { "local" } else { profile.as_str() };
    match &k.state {
        KernelState::Ready { .. } if k.busy.is_some() => (format!("Local Spark ({profile}) · running"), true),
        KernelState::Ready { .. } => (format!("Local Spark ({profile}) · ready"), true),
        KernelState::Starting { since } => (format!("Local Spark ({profile}) · starting {}s", since.elapsed().as_secs()), false),
        KernelState::Failed(_) => (format!("Local Spark ({profile}) · failed"), false),
        KernelState::Stopped => (format!("Local Spark ({profile}) · stopped"), false),
    }
}

/// `EXPLAIN` of a statement that does not analyze answers the exception as its text
/// (`org.apache.spark.sql.AnalysisException: [CODE] …`): that message, compacted.
pub fn explain_error(text: &str) -> Option<String> {
    // EXPLAIN EXTENDED puts the exception under "== Analyzed Logical Plan =="; plain EXPLAIN
    // answers "Error occurred during query planning" without the detail (Spark 4)
    let first = text.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with("== "))?;
    if first.starts_with("Error occurred during query planning") {
        let detail = first.trim_start_matches("Error occurred during query planning").trim_start_matches(':').trim();
        return Some(if detail.is_empty() { "Spark could not plan this statement; use Parse or run it for the message.".to_string() } else { kernel::compact_spark_error(detail) });
    }
    let line = text.lines().find(|l| l.contains("Exception: ") || l.trim_start().starts_with("org.apache.spark"))?;
    let from = text.find(line).unwrap_or(0);
    let tail = &text[from..];
    let msg = match tail.split_once("Exception: ") {
        Some((_, rest)) => rest,
        None => tail,
    };
    Some(kernel::compact_spark_error(msg))
}

/// Split Spark's `EXPLAIN EXTENDED` text into its `== Title ==` sections (title, body).
pub fn plan_sections(text: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with("== ") && t.ends_with(" ==") && t.len() > 6 {
            out.push((t[3..t.len() - 3].to_string(), String::new()));
        } else if let Some((_, body)) = out.last_mut() {
            body.push_str(line);
            body.push('\n');
        } else if !t.is_empty() {
            out.push(("Plan".into(), format!("{line}\n")));
        }
    }
    for (_, b) in out.iter_mut() {
        let trimmed = b.trim_end().to_string();
        *b = trimmed;
    }
    out
}

fn msg(text: String, is_error: bool) -> MessageLine {
    MessageLine { text, is_error, is_batch_header: false, line: None, path: None }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_round_trips() {
        let mut t = EditorTab::new(3);
        t.spark = Some(SparkTab { binding: Some(NotebookFabric { workspace_id: "ws".into(), lakehouse_id: Some("lh".into()), write_mode: "readonly".into(), preload: false }), workspace_name: "Fabric test".into(), lakehouse_name: Some("test".into()), sink: None, sql: None, parse_next: false, uncapped_next: false });
        let s = snapshot_binding(&t).unwrap();
        assert!(s.starts_with("spark:{"));
        let back = from_snapshot(&s).unwrap();
        assert_eq!(back.binding, t.spark.as_ref().unwrap().binding);
        assert_eq!(back.lakehouse_name.as_deref(), Some("test"));
        assert!(from_snapshot("cobalt_test").is_none());
        let plain = from_snapshot("spark:{}").unwrap();
        assert!(plain.binding.is_none());
    }

    #[test]
    fn titles_follow_the_lakehouse() {
        let mut t = EditorTab::new(7);
        t.spark = Some(SparkTab { binding: None, workspace_name: String::new(), lakehouse_name: None, sink: None, sql: None, parse_next: false, uncapped_next: false });
        retitle(&mut t);
        assert_eq!(t.title, "SparkSQL_7");
        t.spark.as_mut().unwrap().lakehouse_name = Some("test".into());
        retitle(&mut t);
        assert_eq!(t.title, "SparkSQL_7 · test");
        t.custom_title = true;
        t.spark.as_mut().unwrap().lakehouse_name = None;
        retitle(&mut t);
        assert_eq!(t.title, "SparkSQL_7 · test");
    }

    #[test]
    fn statements_split_outside_quotes_and_comments() {
        let v = split_statements("SELECT ';' AS a; -- a; comment\nSELECT `x;y` FROM t;\n\nUSE db");
        assert_eq!(v, vec!["SELECT ';' AS a", "-- a; comment\nSELECT `x;y` FROM t", "USE db"]);
        assert!(split_statements(" ; ;").is_empty());
    }

    #[test]
    fn explain_text_splits_into_sections() {
        let v = plan_sections("== Parsed Logical Plan ==\n'Project [*]\n+- 'UnresolvedRelation [t]\n\n== Physical Plan ==\n*(1) Scan parquet\n");
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].0, "Parsed Logical Plan");
        assert_eq!(v[0].1, "'Project [*]\n+- 'UnresolvedRelation [t]");
        assert_eq!(v[1], ("Physical Plan".to_string(), "*(1) Scan parquet".to_string()));
        assert_eq!(plan_sections("just text")[0].0, "Plan");
        assert_eq!(explain_error("== Physical Plan ==
org.apache.spark.sql.AnalysisException: [TABLE_OR_VIEW_NOT_FOUND] The table `nope` cannot be found.; line 1 pos 14;
'Project [*]
"), Some("[TABLE_OR_VIEW_NOT_FOUND] The table `nope` cannot be found.; line 1 pos 14;".to_string()));
        assert!(explain_error("== Parsed Logical Plan ==
'Project [*]").is_none());
        // EXPLAIN EXTENDED carries the analysis error in the second section
        let extended = "== Parsed Logical Plan ==\n'Project [*]\n\n== Analyzed Logical Plan ==\norg.apache.spark.sql.AnalysisException: [TABLE_OR_VIEW_NOT_FOUND] The table `dbo`.`nope` cannot be found.; line 1 pos 31;\n'Project [*]\n";
        assert_eq!(explain_error(extended).as_deref(), Some("[TABLE_OR_VIEW_NOT_FOUND] The table `dbo`.`nope` cannot be found.; line 1 pos 31;"));
        assert!(explain_error("Error occurred during query planning: ").unwrap().starts_with("Spark could not plan"));
    }

    #[test]
    fn delta_log_schema_becomes_columns() {
        let log = r#"{"commitInfo":{"operation":"CREATE TABLE"}}
{"metaData":{"id":"x","schemaString":"{\"type\":\"struct\",\"fields\":[{\"name\":\"id\",\"type\":\"long\",\"nullable\":false,\"metadata\":{}},{\"name\":\"amount\",\"type\":\"decimal(18,2)\",\"nullable\":true,\"metadata\":{}},{\"name\":\"tags\",\"type\":{\"type\":\"array\",\"elementType\":\"string\",\"containsNull\":true},\"nullable\":true,\"metadata\":{}}]}","partitionColumns":[]}}
{"add":{"path":"part-0.parquet"}}"#;
        let cols = delta_log_columns(log).unwrap();
        assert_eq!(cols.len(), 3);
        assert_eq!(cols[0].name, "id");
        assert_eq!(cols[0].sql_type, cobalt_core::SqlType::BigInt);
        assert!(!cols[0].nullable);
        assert_eq!(cols[1].sql_type, cobalt_core::SqlType::Decimal { precision: 18, scale: 2 });
        assert_eq!(cols[2].sql_type, cobalt_core::SqlType::Other("array".into()));
        assert!(delta_log_columns("{\"add\":{}}").is_none());
    }

    #[test]
    fn insert_goes_on_its_own_line() {
        let mut t = EditorTab::new(1);
        t.text = "SELECT 1".into();
        t.editor.cursor = 8;
        insert_at_cursor(&mut t, "SELECT * FROM t LIMIT 100");
        match t.editor.pending_edit.take() {
            Some(PendingEdit::SetText { text, cursor }) => {
                assert_eq!(text, "SELECT 1\nSELECT * FROM t LIMIT 100");
                assert_eq!(cursor, text.chars().count());
            }
            other => panic!("{other:?}"),
        }
        // at the start of existing text the insert gets its own line
        let mut t = EditorTab::new(2);
        t.text = "SHOW TABLES".into();
        t.editor.cursor = 0;
        insert_at_cursor(&mut t, "SELECT 1");
        match t.editor.pending_edit.take() {
            Some(PendingEdit::SetText { text, cursor }) => {
                assert_eq!(text, "SELECT 1\nSHOW TABLES");
                assert_eq!(cursor, 8);
            }
            other => panic!("{other:?}"),
        }
    }
}
