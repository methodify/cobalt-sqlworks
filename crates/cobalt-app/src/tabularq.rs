//! DAX query tabs on semantic models: an ordinary query tab (editor, grid, Messages, exports,
//! history) whose runs go to the Arrow query API through `session/tabular.rs` instead of a
//! connection. The model's catalog (tables, columns, measures) is loaded once per model and
//! shared by the Fabric panel's model explorer and the tab's completion.
//! Design: `docs/design/semantic_models.md`.

use crate::fabric::FabricEvent;
use crate::ops::{self, Ctx};
use crate::session::{Command, TabularAuth};
use crate::state::{hash_text, AppState, EditorTab, Loadable, ResultsTab, RunMode, RunView, RunViewState, TabularTab, ToastKind};
use cobalt_core::tabular::{dax_column_ref, dax_measure_ref, dax_table_ref, ModelCatalog, TabularTarget};
use cobalt_store::NewHistoryEntry;
use std::sync::Arc;

/// The binding stored in a tab snapshot's `database` column (`dax:` + JSON of the target).
const SNAPSHOT_PREFIX: &str = "dax:";

/// How a run on `target` signs in (`tabular_root::auth_for`), or the reason it cannot.
pub fn auth(state: &AppState, cx: &Ctx, target: &TabularTarget, force_xmla: bool) -> Result<TabularAuth, String> {
    crate::tabular_root::auth_for(state, cx, target, force_xmla)
}

/// Whether a target runs over XMLA: always for Azure AS and HTTP, on request for Power BI (the
/// flag of an open tab on the same model, else no).
pub fn force_xmla_for(state: &AppState, target: &TabularTarget) -> bool {
    if !matches!(target, TabularTarget::PowerBi { .. }) {
        return true;
    }
    state.tabs.iter().filter_map(|t| t.tabular.as_ref()).any(|tt| tt.target == *target && tt.force_xmla)
}

/// A new DAX tab on `target`, with `text` when given; the model's catalog starts loading.
pub fn new_tab(state: &mut AppState, cx: &Ctx, target: TabularTarget, text: Option<String>) -> usize {
    let idx = state.new_tab();
    let t = &mut state.tabs[idx];
    t.tabular = Some(TabularTab::new(target.clone()));
    if let Some(text) = text {
        t.text = text;
        t.mark_saved();
    }
    t.editor.request_focus = true;
    retitle(t);
    ensure_catalog(state, cx, &target);
    idx
}

/// `DAX_n · model` unless the user renamed the tab.
pub fn retitle(t: &mut EditorTab) {
    if t.custom_title {
        return;
    }
    if let Some(tt) = &t.tabular {
        t.title = format!("DAX_{} · {}", t.untitled_index, tt.target.model());
    }
}

/// Open `text` on the target: in the active DAX tab when it is on the same model and empty,
/// else in a new tab; run it when asked.
pub fn open_query(state: &mut AppState, cx: &Ctx, target: TabularTarget, text: String, run: bool) -> usize {
    let reuse = state.active_tab.filter(|&i| state.tabs.get(i).map(|t| t.tabular.as_ref().map(|tt| tt.target == target).unwrap_or(false) && t.text.trim().is_empty() && !t.is_running()).unwrap_or(false));
    let idx = match reuse {
        Some(i) => {
            state.tabs[i].text = text;
            state.tabs[i].mark_saved();
            i
        }
        None => new_tab(state, cx, target, Some(text)),
    };
    state.active_tab = Some(idx);
    if run {
        self::run(state, cx, idx, RunMode::All);
    }
    idx
}

/// Insert DAX at the caret of the active DAX tab on `target`, or open a tab with it.
pub fn insert_or_open(state: &mut AppState, cx: &Ctx, target: TabularTarget, text: &str) {
    if let Some(i) = state.active_tab {
        if let Some(t) = state.tabs.get_mut(i) {
            if t.tabular.as_ref().map(|tt| tt.target == target).unwrap_or(false) {
                crate::sparkq::insert_at_cursor(t, text);
                return;
            }
        }
    }
    new_tab(state, cx, target, Some(text.to_string()));
}

// ---------------------------------------------------------------------------------------------
// Catalog
// ---------------------------------------------------------------------------------------------

/// Load the model's catalog (and its DAX function list) once; `force` reloads it.
pub fn load_catalog(state: &mut AppState, cx: &Ctx, target: &TabularTarget, force: bool) {
    let key = target.key();
    // a failed load stays failed until Retry / Refresh (`force`): every tab open or tree draw
    // must not fire another request at a model that just refused
    if !force && state.model_catalogs.get(&key).map(|l| !l.needs_load()).unwrap_or(false) {
        return;
    }
    let force_xmla = force_xmla_for(state, target);
    let auth = match auth(state, cx, target, force_xmla) {
        Ok(a) => a,
        // no sign-in yet: stay NotLoaded, the panel's sign-in loads it (`ensure_all`)
        Err(e) if e == crate::session::tabular::NEED_SIGN_IN => return,
        Err(e) => {
            state.model_catalogs.insert(key, Loadable::Failed(e));
            return;
        }
    };
    state.model_catalogs.insert(key.clone(), Loadable::Loading);
    let want_functions = !state.dax_functions.contains_key(&key);
    let tx = cx.fabric_tx.clone();
    let egui = cx.egui.clone();
    let target = target.clone();
    cx.session.spawn(async move {
        let result = match crate::session::tabular::connect(&auth, &target).await {
            Ok(client) => {
                let cat = client.load_catalog(&target).await.map(Arc::new).map_err(|e| match e.hint() {
                    Some(h) => format!("{e}\n{h}"),
                    None => e.to_string(),
                });
                if cat.is_ok() && want_functions {
                    let f = client.load_functions(&target).await.map_err(|e| e.to_string());
                    let _ = tx.send(FabricEvent::DaxFunctions { key: key.clone(), result: f });
                }
                cat
            }
            Err(e) => {
                if e == crate::session::tabular::NEED_SIGN_IN {
                    let _ = tx.send(FabricEvent::NeedSignIn);
                }
                Err(e)
            }
        };
        let _ = tx.send(FabricEvent::ModelCatalog { key, result });
        egui.request_repaint();
    });
}

/// Catalogs for every open DAX tab's model (after the Fabric sign-in arrives).
pub fn ensure_all(state: &mut AppState, cx: &Ctx) {
    let mut targets: Vec<TabularTarget> = state.tabs.iter().filter_map(|t| t.tabular.as_ref().map(|tt| tt.target.clone())).collect();
    targets.dedup_by_key(|t| t.key());
    for t in targets {
        // a load that failed before the sign-in (or with a stale token) gets another go
        let failed = matches!(state.model_catalogs.get(&t.key()), Some(Loadable::Failed(_)));
        load_catalog(state, cx, &t, failed);
        ensure_catalog(state, cx, &t);
    }
}

pub fn ensure_catalog(state: &mut AppState, cx: &Ctx, target: &TabularTarget) {
    load_catalog(state, cx, target, false);
    // tabs made after the catalog arrived get their completion copy now
    let key = target.key();
    let cat = state.model_catalogs.get(&key).and_then(|l| l.get()).cloned();
    let fns = state.dax_functions.get(&key).cloned();
    for t in state.tabs.iter_mut().filter(|t| t.tabular.as_ref().map(|tt| tt.model_key() == key).unwrap_or(false)) {
        if t.model_catalog.is_none() {
            t.model_catalog = cat.clone();
        }
        if t.dax_functions.is_none() {
            t.dax_functions = fns.clone();
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Runs
// ---------------------------------------------------------------------------------------------

/// The text a run mode executes: the selection, the whole text, or (Ctrl+Enter) the EVALUATE
/// under the caret with the DEFINE block.
fn script_for(t: &EditorTab, mode: RunMode) -> Option<String> {
    let script = match mode {
        RunMode::Current => match crate::ui::editor::selected_script(t) {
            Some((s, _)) => s,
            None => {
                let cursor = crate::ui::editor::char_to_byte(&t.text, t.editor.cursor);
                let parts = cobalt_sql::dax::dax_statement_at(&t.text, cursor);
                if parts.is_empty() {
                    t.text.clone()
                } else {
                    parts.iter().map(|r| &t.text[r.clone()]).collect::<Vec<_>>().join("\n")
                }
            }
        },
        _ => crate::ui::editor::selected_script(t).map(|(s, _)| s).unwrap_or_else(|| t.text.clone()),
    };
    if script.trim().is_empty() {
        return None;
    }
    Some(script)
}

/// F5 and friends on a DAX tab.
pub fn run(state: &mut AppState, cx: &Ctx, idx: usize, mode: RunMode) {
    let (target, force) = match state.tabs.get(idx).and_then(|t| t.tabular.as_ref()) {
        Some(tt) => (tt.target.clone(), tt.force_xmla),
        None => return,
    };
    let auth = match auth(state, cx, &target, force) {
        Ok(a) => a,
        Err(e) => {
            cx.toast(ToastKind::Warning, e);
            return;
        }
    };
    let t = &mut state.tabs[idx];
    if t.is_running() {
        return;
    }
    if mode == RunMode::EstimatedPlan {
        cx.toast(ToastKind::Info, "DAX queries have no execution plan in Cobalt yet.");
        return;
    }
    let Some(script) = script_for(t, mode) else { return };
    t.last_run_mode = Some(mode);
    t.pending_run = None;
    let Some(tt) = t.tabular.as_mut() else { return };
    let uncapped = std::mem::take(&mut tt.uncapped_next);
    tt.transport = Some(if tt.uses_xmla() { "XMLA" } else { "Arrow query API" });
    let cap = cx.settings.execution.row_cap;
    let job = t.pending_export.take();
    let limit = if uncapped || job.is_some() || cap == 0 { None } else { Some(cap) };
    tt.last_limit = limit;
    let timeout = cx.settings.execution.command_timeout_secs;
    let opts = cobalt_tabular::DaxOptions {
        timeout_secs: if timeout == 0 { None } else { Some(timeout) },
        row_limit: limit,
        schema_only: false,
        effective_username: tt.effective_username.clone(),
        roles: tt.roles.clone(),
        custom_data: None,
        culture: None,
    };
    let run_id = cx.session.new_run();
    let mut view = RunView::new(run_id);
    view.export_target = job.as_ref().map(|j| j.display_target());
    view.script_hash = hash_text(&t.text);
    if cx.settings.history.capture {
        let mut e = NewHistoryEntry::new(format!("Power BI · {}", target.host_label()), script.clone());
        e.database = Some(target.model().to_string());
        e.tab_id = Some(t.id);
        view.history_id = cx.store.add_history(&e).ok();
    }
    t.run = Some(view);
    t.results_visible = true;
    t.results_tab = ResultsTab::Results;
    let tab_id = t.id;
    let sink = job.map(|j| ops::spawn_run_export(state, cx, Some(tab_id), *j));
    cx.session.send(Command::RunTabular { tab: tab_id, run: run_id, target, auth, text: script, opts, sink });
    state.history.loaded = false;
}

pub fn cancel(state: &mut AppState, cx: &Ctx, idx: usize) {
    if let Some(t) = state.tabs.get_mut(idx) {
        if let Some(r) = &mut t.run {
            if r.is_live() {
                r.state = RunViewState::Cancelling;
                cx.session.send(Command::Cancel { tab: t.id });
            }
        }
    }
}

/// The capped result's "Run again without the cap".
pub fn rerun_uncapped(state: &mut AppState, cx: &Ctx, idx: usize) {
    let Some(mode) = state.tabs[idx].last_run_mode.filter(|m| *m != RunMode::EstimatedPlan) else { return };
    if let Some(tt) = state.tabs[idx].tabular.as_mut() {
        tt.uncapped_next = true;
    }
    run(state, cx, idx, mode);
}

/// After a run: a result set that filled the row limit is "capped" (the banner offers an
/// uncapped re-run).
pub fn note_capped(t: &mut EditorTab) {
    let Some(limit) = t.tabular.as_ref().and_then(|tt| tt.last_limit) else { return };
    if let Some(r) = t.run.as_mut() {
        if r.result_sets.iter().any(|s| s.rs.row_count() as u64 >= limit) {
            r.capped = true;
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Quick queries (the model explorer's context menus)
// ---------------------------------------------------------------------------------------------

pub fn q_top_rows(table: &str, n: usize) -> String {
    format!("EVALUATE\n    TOPN({n}, {})", dax_table_ref(table))
}

pub fn q_count_rows(table: &str) -> String {
    format!("EVALUATE\n    ROW(\"Rows\", COUNTROWS({}))", dax_table_ref(table))
}

pub fn q_distinct(table: &str, column: &str) -> String {
    let c = dax_column_ref(table, column);
    format!("EVALUATE\n    DISTINCT({c})\nORDER BY {c}")
}

pub fn q_column_summary(table: &str, column: &str) -> String {
    let t = dax_table_ref(table);
    let c = dax_column_ref(table, column);
    format!("EVALUATE\n    ROW(\n        \"Rows\", COUNTROWS({t}),\n        \"Distinct\", DISTINCTCOUNT({c}),\n        \"Blank\", COUNTBLANK({c}),\n        \"Min\", MIN({c}),\n        \"Max\", MAX({c})\n    )")
}

pub fn q_evaluate_measure(measure: &str) -> String {
    format!("EVALUATE\n    ROW(\"{}\", {})", measure.replace('"', "\"\""), dax_measure_ref(measure))
}

pub fn q_measure_by(measure: &str, table: &str, column: &str) -> String {
    format!("EVALUATE\n    SUMMARIZECOLUMNS(\n        {},\n        \"{}\", {}\n    )\nORDER BY {} DESC", dax_column_ref(table, column), measure.replace('"', "\"\""), dax_measure_ref(measure), dax_measure_ref(measure))
}

pub fn q_define_measure(table: &str, measure: &str, expression: Option<&str>) -> String {
    let expr = expression.map(|e| e.trim().to_string()).filter(|e| !e.is_empty()).unwrap_or_else(|| "BLANK()".into());
    format!("DEFINE\n    MEASURE {}[{}] = {expr}\nEVALUATE\n    ROW(\"{}\", {})", dax_table_ref(table), measure.replace(']', "]]"), measure.replace('"', "\"\""), dax_measure_ref(measure))
}

pub fn q_define_all(catalog: &ModelCatalog, table: Option<&str>) -> String {
    let mut out = String::from("DEFINE\n");
    let mut n = 0;
    for (t, m) in catalog.all_measures() {
        if table.map(|x| x.eq_ignore_ascii_case(&t.name)).unwrap_or(true) {
            let expr = m.expression.as_deref().map(str::trim).filter(|e| !e.is_empty()).unwrap_or("BLANK()");
            out.push_str(&format!("    MEASURE {}[{}] = {expr}\n", dax_table_ref(&t.name), m.name.replace(']', "]]")));
            n += 1;
        }
    }
    if n == 0 {
        out.push_str("    // no measures\n");
    }
    out.push_str("EVALUATE\n    ROW(\"Measures\", ");
    out.push_str(&n.to_string());
    out.push(')');
    out
}

pub fn q_info() -> String {
    "EVALUATE INFO.VIEW.TABLES()\nEVALUATE INFO.VIEW.COLUMNS()\nEVALUATE INFO.VIEW.MEASURES()\nEVALUATE INFO.VIEW.RELATIONSHIPS()".into()
}

// ---------------------------------------------------------------------------------------------
// Snapshots (hot exit)
// ---------------------------------------------------------------------------------------------

#[derive(serde::Serialize, serde::Deserialize)]
struct Snapshot {
    target: TabularTarget,
    #[serde(default)]
    profile: Option<uuid::Uuid>,
    #[serde(default)]
    force_xmla: bool,
}

pub fn snapshot_binding(t: &EditorTab) -> Option<String> {
    let tt = t.tabular.as_ref()?;
    Some(format!("{SNAPSHOT_PREFIX}{}", serde_json::to_string(&Snapshot { target: tt.target.clone(), profile: tt.profile, force_xmla: tt.force_xmla }).unwrap_or_default()))
}

/// A snapshot's `database` column back into a DAX tab state (None for other tabs).
pub fn from_snapshot(database: &str) -> Option<TabularTab> {
    let json = database.strip_prefix(SNAPSHOT_PREFIX)?;
    let snap: Snapshot = match serde_json::from_str::<Snapshot>(json) {
        Ok(s) => s,
        Err(_) => Snapshot { target: serde_json::from_str(json).ok()?, profile: None, force_xmla: false },
    };
    let mut tt = TabularTab::new(snap.target);
    tt.profile = snap.profile;
    tt.force_xmla = snap.force_xmla;
    Some(tt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quick_queries() {
        assert_eq!(q_top_rows("Sales", 100), "EVALUATE\n    TOPN(100, 'Sales')");
        assert!(q_distinct("Date", "Year").contains("DISTINCT('Date'[Year])"));
        assert!(q_evaluate_measure("Total Sales").ends_with("ROW(\"Total Sales\", [Total Sales])"));
        assert!(q_define_measure("Sales", "M", Some(" SUM('Sales'[Amount]) ")).starts_with("DEFINE\n    MEASURE 'Sales'[M] = SUM('Sales'[Amount])\n"));
    }
}
