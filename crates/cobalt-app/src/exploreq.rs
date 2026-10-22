//! Explore tabs: the pivot builder on a semantic model. An Explore tab is a DAX tab
//! (`tabularq.rs`, `EditorTab.tabular`) whose text is generated from an `ExploreSpec`
//! (`cobalt_core::explore`) by `cobalt_tabular::explore::to_dax`; runs, results, history and
//! snapshots are the DAX tab's. The matrix (Columns well) is a local pivot of the flat result
//! (`cobalt_results::pivot`). Design: `docs/design/semantic_models.md` §3.1 (5), §3.2.

use crate::fabric::FabricEvent;
use crate::ops::Ctx;
use crate::state::{hash_text, AppState, EditorTab, ExploreState, FilterEditor, Loadable, RunMode, ToastKind};
use crate::tabularq;
use cobalt_core::chart::Agg;
use cobalt_core::explore::{ExploreSource, ExploreSpec, FieldRef, FilterOp, FilterSpec, SortBy, SortSpec, ValueRef, EXPLORE_EXT};
use cobalt_core::tabular::{dax_column_ref, ModelCatalog, TabularDataType, TabularTarget};
use cobalt_results::CellFormatter;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const SNAPSHOT_PREFIX: &str = "explore:";
const DISTINCT_LIMIT: usize = 500;

/// Which well a field goes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Well {
    Rows,
    Columns,
    Values,
    Filters,
}

/// A new Explore tab on `target`, from a spec (a file, a pin) or empty.
pub fn new_tab(state: &mut AppState, cx: &Ctx, target: TabularTarget, spec: Option<ExploreSpec>, path: Option<PathBuf>) -> usize {
    let idx = tabularq::new_tab(state, cx, target.clone(), None);
    let t = &mut state.tabs[idx];
    let mut spec = spec.unwrap_or_else(|| ExploreSpec::new(ExploreSource::SemanticModel { target: target.clone() }));
    if spec.source.is_none() {
        spec.source = Some(ExploreSource::SemanticModel { target });
    }
    let run_when_loaded = !spec.is_empty();
    let mut ex = ExploreState::new(spec);
    ex.run_when_loaded = run_when_loaded;
    t.explore = Some(Box::new(ex));
    t.file_path = path;
    t.custom_title = false;
    retitle(t);
    t.mark_saved();
    regenerate(state, idx);
    idx
}

/// A new Explore tab over a local table (a result set, a file), from a spec or empty.
pub fn new_local_tab(state: &mut AppState, cx: &Ctx, table: &str, spec: Option<ExploreSpec>) -> Result<usize, String> {
    let cat = crate::localq::local_catalog(cx.local, table).ok_or_else(|| format!("no local table named {table}"))?;
    let idx = crate::localq::new_tab(state, cx, None);
    let t = &mut state.tabs[idx];
    t.model_catalog = Some(Arc::new(cat));
    let mut spec = spec.unwrap_or_else(|| ExploreSpec::new(ExploreSource::Local { table: table.to_string() }));
    if spec.source.is_none() {
        spec.source = Some(ExploreSource::Local { table: table.to_string() });
    }
    let run_when_loaded = !spec.is_empty();
    let mut ex = ExploreState::new(spec);
    ex.run_when_loaded = run_when_loaded;
    t.explore = Some(Box::new(ex));
    t.custom_title = false;
    retitle(t);
    t.mark_saved();
    regenerate(state, idx);
    state.active_tab = Some(idx);
    if run_when_loaded {
        run(state, cx, idx);
    }
    Ok(idx)
}

/// Explore a tab's result set (its local table).
pub fn explore_result(state: &mut AppState, cx: &Ctx, idx: usize, set: usize) {
    let Some(tab_id) = state.tabs.get(idx).map(|t| t.id) else { return };
    let Some(name) = crate::localq::table_for_result(cx, tab_id, set) else {
        cx.toast(ToastKind::Info, "This result is not registered as a local table yet (it may still be arriving).");
        return;
    };
    if let Err(e) = new_local_tab(state, cx, &name, None) {
        cx.toast(ToastKind::Warning, e);
    }
}

/// `Explore_n · model`, or the file's name.
pub fn retitle(t: &mut EditorTab) {
    if t.custom_title {
        return;
    }
    if let Some(p) = &t.file_path {
        t.title = p.file_name().map(|n| n.to_string_lossy().trim_end_matches(EXPLORE_EXT).to_string()).unwrap_or_else(|| "Explore".into());
        return;
    }
    let title = t.explore.as_ref().map(|e| e.spec.title.trim().to_string()).filter(|s| !s.is_empty());
    let source = t.explore.as_ref().and_then(|e| e.spec.source.as_ref()).and_then(|s| s.local_table()).map(str::to_string);
    if let Some(tt) = &t.tabular {
        t.title = match title {
            Some(s) => format!("{s} · {}", tt.target.model()),
            None => format!("Explore_{} · {}", t.untitled_index, tt.target.model()),
        };
    } else if let Some(table) = source {
        t.title = match title {
            Some(s) => format!("{s} · {table}"),
            None => format!("Explore_{} · {table}", t.untitled_index),
        };
    }
}

/// Open a `.cobalt-explore.json` file.
pub fn open_path(state: &mut AppState, cx: &Ctx, path: &Path) -> Option<usize> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            cx.toast(ToastKind::Error, format!("Could not read {}: {e}", path.display()));
            return None;
        }
    };
    let spec = match ExploreSpec::parse(&text) {
        Ok(s) => s,
        Err(e) => {
            cx.toast(ToastKind::Error, format!("Not an Explore file: {e}"));
            return None;
        }
    };
    let Some(ExploreSource::SemanticModel { target }) = spec.source.clone() else {
        cx.toast(ToastKind::Error, "The Explore file names no semantic model.");
        return None;
    };
    Some(new_tab(state, cx, target, Some(spec), Some(path.to_path_buf())))
}

/// Save (Save As when `save_as` or the tab has no file yet).
pub fn save(state: &mut AppState, cx: &Ctx, idx: usize, save_as: bool) -> bool {
    let Some(t) = state.tabs.get_mut(idx) else { return false };
    let Some(ex) = t.explore.as_deref_mut() else { return false };
    let path = match (&t.file_path, save_as) {
        (Some(p), false) => p.clone(),
        _ => {
            let name = if ex.spec.title.trim().is_empty() { "Explore".to_string() } else { ex.spec.title.trim().to_string() };
            let dlg = rfd::FileDialog::new().add_filter("Cobalt explore", &["json"]).set_file_name(format!("{name}{EXPLORE_EXT}"));
            match dlg.save_file() {
                Some(p) => p,
                None => return false,
            }
        }
    };
    match std::fs::write(&path, ex.spec.to_json()) {
        Ok(()) => {
            ex.saved_hash = ex.spec_hash();
            t.file_path = Some(path);
            t.custom_title = false;
            retitle(t);
            cx.toast(ToastKind::Success, "Explore saved.");
            true
        }
        Err(e) => {
            cx.toast(ToastKind::Error, format!("Could not save: {e}"));
            false
        }
    }
}

pub fn is_dirty(t: &EditorTab) -> bool {
    t.explore.as_ref().map(|e| e.spec_hash() != e.saved_hash && !(t.file_path.is_none() && e.spec.is_empty())).unwrap_or(false)
}

// ---------------------------------------------------------------------------------------------
// Generating and running
// ---------------------------------------------------------------------------------------------

fn catalog_of(state: &AppState, idx: usize) -> Option<Arc<ModelCatalog>> {
    state.tabs.get(idx)?.model_catalog.clone()
}

/// Rebuild the DAX from the spec (needs the catalog); the tab's text becomes the query.
pub fn regenerate(state: &mut AppState, idx: usize) {
    let cat = catalog_of(state, idx);
    let Some(t) = state.tabs.get_mut(idx) else { return };
    let Some(ex) = t.explore.as_deref_mut() else { return };
    if ex.spec.is_empty() {
        ex.dax = None;
        ex.dax_error = None;
        t.text.clear();
        return;
    }
    let Some(cat) = cat else {
        ex.dax = None;
        ex.dax_error = None; // not an error: the catalog is on its way
        return;
    };
    let generated = match ex.spec.source.as_ref().and_then(|s| s.local_table()) {
        Some(_) => cobalt_local::explore::to_sql(&ex.spec, &cat).map(|q| cobalt_tabular::explore::DaxQuery { text: q.text, group_columns: q.group_columns, value_columns: q.value_columns, subtotal_columns: q.subtotal_columns }),
        None => cobalt_tabular::explore::to_dax(&ex.spec, &cat),
    };
    match generated {
        Ok(q) => {
            t.text = q.text.clone();
            ex.dax = Some(q);
            ex.dax_error = None;
        }
        Err(e) => {
            ex.dax = None;
            ex.dax_error = Some(e);
        }
    }
}

/// Run the current spec.
pub fn run(state: &mut AppState, cx: &Ctx, idx: usize) {
    regenerate(state, idx);
    let Some(t) = state.tabs.get_mut(idx) else { return };
    let running = t.is_running();
    let Some(ex) = t.explore.as_deref_mut() else { return };
    if ex.spec.is_empty() {
        return;
    }
    if ex.dax.is_none() {
        if ex.dax_error.is_none() {
            ex.run_when_loaded = true; // waits for the catalog
        }
        return;
    }
    if running {
        // the change runs when the current run ends (`tick`)
        ex.dirty = true;
        return;
    }
    ex.dirty = false;
    ex.pivot = None;
    ex.pivot_built_for = None;
    let local = ex.spec.source.as_ref().and_then(|s| s.local_table()).is_some();
    if local {
        crate::localq::run(state, cx, idx, RunMode::All);
    } else {
        tabularq::run(state, cx, idx, RunMode::All);
    }
}

/// The spec changed: regenerate, and run when auto-run is on.
pub fn changed(state: &mut AppState, cx: &Ctx, idx: usize) {
    if let Some(ex) = state.tabs.get_mut(idx).and_then(|t| t.explore.as_deref_mut()) {
        ex.dirty = true;
        ex.matrix = !ex.spec.columns.is_empty();
    }
    retitle(&mut state.tabs[idx]);
    regenerate(state, idx);
    let auto = state.tabs.get(idx).and_then(|t| t.explore.as_ref()).map(|e| e.auto_run).unwrap_or(false);
    if auto {
        run(state, cx, idx);
    }
}

/// Each frame: runs that waited for the catalog, and the matrix for finished runs.
pub fn tick(state: &mut AppState, cx: &Ctx, fmt: &CellFormatter) {
    let mut to_run = Vec::new();
    for (i, t) in state.tabs.iter().enumerate() {
        if let Some(ex) = t.explore.as_deref() {
            let waiting = ex.run_when_loaded && t.model_catalog.is_some();
            // a change made while a run was in flight runs once that run ends
            let pending = ex.dirty && ex.auto_run && ex.dax.is_some() && !ex.spec.is_empty();
            if (waiting || pending) && !t.is_running() {
                to_run.push(i);
            }
        }
    }
    for i in to_run {
        if let Some(ex) = state.tabs[i].explore.as_deref_mut() {
            ex.run_when_loaded = false;
        }
        run(state, cx, i);
    }
    for t in state.tabs.iter_mut() {
        ensure_pivot(t, fmt);
    }
}

/// Build (or rebuild) the matrix from the first result set when the Columns well is used.
pub fn ensure_pivot(t: &mut EditorTab, fmt: &CellFormatter) {
    let Some(ex) = t.explore.as_deref_mut() else { return };
    if !ex.matrix {
        return;
    }
    let Some(q) = &ex.dax else { return };
    let Some(run) = &t.run else { return };
    let Some(set) = run.result_sets.first() else { return };
    if run.is_live() && set.rs.row_count() == 0 {
        return;
    }
    let gen = set.rs.generation();
    if ex.pivot_built_for == Some((gen, set.rs.row_count())) {
        return;
    }
    let rows = ex.spec.rows.len();
    let names: Vec<&str> = q.group_columns.iter().map(String::as_str).collect();
    let row_names = &names[..rows.min(names.len())];
    let col_names = &names[rows.min(names.len())..];
    let values: Vec<&str> = q.value_columns.iter().map(String::as_str).collect();
    let flags: Vec<Option<&str>> = (0..names.len()).map(|i| q.subtotal_columns.get(i).map(String::as_str)).collect();
    let row_flags = &flags[..rows.min(flags.len())];
    let col_flags = &flags[rows.min(flags.len())..];
    if let Some(spec) = cobalt_results::pivot::pivot_spec_by_name(&set.rs, row_names, col_names, &values, row_flags, col_flags) {
        let table = cobalt_results::pivot::pivot(&set.rs, &spec, fmt);
        ex.pivot = Some(Arc::new(table));
    }
    ex.pivot_built_for = Some((gen, set.rs.row_count()));
}

pub fn open_in_query_tab(state: &mut AppState, cx: &Ctx, idx: usize) {
    let Some(t) = state.tabs.get(idx) else { return };
    let Some(ex) = t.explore.as_deref() else { return };
    let text = ex.dax.as_ref().map(|q| q.text.clone()).unwrap_or_else(|| t.text.clone());
    if ex.spec.source.as_ref().and_then(|s| s.local_table()).is_some() {
        let i = crate::localq::new_tab(state, cx, Some(text));
        state.active_tab = Some(i);
        return;
    }
    let Some(tt) = &t.tabular else { return };
    let target = tt.target.clone();
    tabularq::new_tab(state, cx, target, Some(text));
}

// ---------------------------------------------------------------------------------------------
// Editing the spec
// ---------------------------------------------------------------------------------------------

/// The default aggregation for a column in the Values well.
pub fn default_agg(cat: Option<&ModelCatalog>, table: &str, column: &str) -> Agg {
    let numeric = cat.and_then(|c| c.table(table)).and_then(|t| t.columns.iter().find(|c| c.name.eq_ignore_ascii_case(column))).map(|c| c.data_type.is_numeric() && c.summarize_by.as_deref().map(|s| !s.eq_ignore_ascii_case("none")).unwrap_or(true)).unwrap_or(false);
    if numeric { Agg::Sum } else { Agg::Count }
}

pub fn add_field(state: &mut AppState, cx: &Ctx, idx: usize, field: FieldRef, well: Well) {
    let cat = catalog_of(state, idx);
    {
        let Some(ex) = state.tabs.get_mut(idx).and_then(|t| t.explore.as_deref_mut()) else { return };
        match well {
            Well::Rows | Well::Columns => {
                if field.is_measure() {
                    return;
                }
                ex.spec.rows.retain(|f| f != &field);
                ex.spec.columns.retain(|f| f != &field);
                if well == Well::Rows { ex.spec.rows.push(field) } else { ex.spec.columns.push(field) }
            }
            Well::Values => {
                if ex.spec.values.iter().any(|v| v.field == field) {
                    return;
                }
                let agg = match &field {
                    FieldRef::Column { table, column } => default_agg(cat.as_deref(), table, column),
                    FieldRef::Measure { .. } => Agg::None,
                };
                ex.spec.values.push(ValueRef { field, agg, label: None });
            }
            Well::Filters => {
                if let FieldRef::Column { table, column } = field {
                    ex.filter_editor = Some(FilterEditor { table, column, index: None, op: FilterOp::In { values: Vec::new() }, search: String::new(), text_a: String::new(), text_b: String::new(), last_n: 30 });
                }
                return;
            }
        }
    }
    changed(state, cx, idx);
}

/// What a drag carries: a field from the list (`from` None) or a chip out of a well.
#[derive(Clone, Debug)]
pub struct DragField {
    pub field: FieldRef,
    pub from: Option<Well>,
}

/// Whether `well` takes `field` (measures only go in Values; filters take columns).
pub fn accepts(well: Well, field: &FieldRef) -> bool {
    match well {
        Well::Rows | Well::Columns | Well::Filters => !field.is_measure(),
        Well::Values => true,
    }
}

/// A drop on a well: the field lands before `before` (a chip of that well) or at the end,
/// leaves the well it came from, and keeps its aggregation when it is already a value.
pub fn drop_field(state: &mut AppState, cx: &Ctx, idx: usize, field: FieldRef, from: Option<Well>, to: Well, before: Option<FieldRef>) {
    if !accepts(to, &field) {
        cx.toast(ToastKind::Info, if to == Well::Filters { "Filters take columns; a measure filters through its own expression." } else { "Measures go in Values." });
        return;
    }
    let cat = catalog_of(state, idx);
    {
        let Some(ex) = state.tabs.get_mut(idx).and_then(|t| t.explore.as_deref_mut()) else { return };
        // the field the sort points at, so a reorder keeps it
        let sorted_value = match &ex.spec.sort {
            Some(SortSpec { by: SortBy::Value { index }, .. }) => ex.spec.values.get(*index).map(|v| v.field.clone()),
            _ => None,
        };
        match to {
            Well::Rows | Well::Columns => {
                ex.spec.rows.retain(|f| f != &field);
                ex.spec.columns.retain(|f| f != &field);
                if from == Some(Well::Values) {
                    ex.spec.values.retain(|v| v.field != field);
                }
                let list = if to == Well::Rows { &mut ex.spec.rows } else { &mut ex.spec.columns };
                let at = before.and_then(|b| list.iter().position(|f| *f == b)).unwrap_or(list.len());
                list.insert(at, field);
            }
            Well::Values => {
                let existing = ex.spec.values.iter().position(|v| v.field == field).map(|i| ex.spec.values.remove(i));
                if matches!(from, Some(Well::Rows | Well::Columns)) {
                    ex.spec.rows.retain(|f| f != &field);
                    ex.spec.columns.retain(|f| f != &field);
                }
                let v = existing.unwrap_or_else(|| {
                    let agg = match &field {
                        FieldRef::Column { table, column } => default_agg(cat.as_deref(), table, column),
                        FieldRef::Measure { .. } => Agg::None,
                    };
                    ValueRef { field: field.clone(), agg, label: None }
                });
                let at = before.and_then(|b| ex.spec.values.iter().position(|v| v.field == b)).unwrap_or(ex.spec.values.len());
                ex.spec.values.insert(at, v);
            }
            Well::Filters => {
                if let FieldRef::Column { table, column } = field {
                    ex.filter_editor = Some(FilterEditor { table, column, index: None, op: FilterOp::In { values: Vec::new() }, search: String::new(), text_a: String::new(), text_b: String::new(), last_n: 30 });
                }
                return;
            }
        }
        // the sort follows its value, or goes when that value left
        if let Some(sf) = sorted_value {
            match ex.spec.values.iter().position(|v| v.field == sf) {
                Some(i) => {
                    if let Some(SortSpec { by: SortBy::Value { index }, .. }) = &mut ex.spec.sort {
                        *index = i;
                    }
                }
                None => ex.spec.sort = None,
            }
        }
        if let Some(SortSpec { by: SortBy::Field { field: sf }, .. }) = &ex.spec.sort {
            if !ex.spec.rows.contains(sf) && !ex.spec.columns.contains(sf) {
                ex.spec.sort = None;
            }
        }
    }
    changed(state, cx, idx);
}

pub fn remove_field(state: &mut AppState, cx: &Ctx, idx: usize, field: &FieldRef) {
    if let Some(ex) = state.tabs.get_mut(idx).and_then(|t| t.explore.as_deref_mut()) {
        ex.spec.remove(field);
    }
    changed(state, cx, idx);
}

/// Move a field within or between the Rows / Columns wells.
pub fn move_field(state: &mut AppState, cx: &Ctx, idx: usize, field: &FieldRef, well: Well, delta: i32) {
    if let Some(ex) = state.tabs.get_mut(idx).and_then(|t| t.explore.as_deref_mut()) {
        let list = match well {
            Well::Rows => &mut ex.spec.rows,
            Well::Columns => &mut ex.spec.columns,
            _ => return,
        };
        if let Some(i) = list.iter().position(|f| f == field) {
            let j = (i as i32 + delta).clamp(0, list.len() as i32 - 1) as usize;
            let f = list.remove(i);
            list.insert(j, f);
        }
    }
    changed(state, cx, idx);
}

pub fn set_agg(state: &mut AppState, cx: &Ctx, idx: usize, value_index: usize, agg: Agg) {
    if let Some(ex) = state.tabs.get_mut(idx).and_then(|t| t.explore.as_deref_mut()) {
        if let Some(v) = ex.spec.values.get_mut(value_index) {
            v.agg = agg;
        }
    }
    changed(state, cx, idx);
}

pub fn set_sort(state: &mut AppState, cx: &Ctx, idx: usize, sort: Option<SortSpec>) {
    if let Some(ex) = state.tabs.get_mut(idx).and_then(|t| t.explore.as_deref_mut()) {
        ex.spec.sort = sort;
    }
    changed(state, cx, idx);
}

pub fn set_top_n(state: &mut AppState, cx: &Ctx, idx: usize, n: Option<u64>) {
    if let Some(ex) = state.tabs.get_mut(idx).and_then(|t| t.explore.as_deref_mut()) {
        ex.spec.top_n = n.filter(|n| *n > 0);
    }
    changed(state, cx, idx);
}

pub fn set_subtotals(state: &mut AppState, cx: &Ctx, idx: usize, on: bool) {
    if let Some(ex) = state.tabs.get_mut(idx).and_then(|t| t.explore.as_deref_mut()) {
        ex.spec.subtotals = on;
    }
    changed(state, cx, idx);
}

/// Commit the filter popup's draft.
pub fn apply_filter(state: &mut AppState, cx: &Ctx, idx: usize) {
    if let Some(ex) = state.tabs.get_mut(idx).and_then(|t| t.explore.as_deref_mut()) {
        let Some(ed) = ex.filter_editor.take() else { return };
        let spec = FilterSpec { table: ed.table, column: ed.column, op: ed.op };
        match ed.index {
            Some(i) if i < ex.spec.filters.len() => ex.spec.filters[i] = spec,
            _ => ex.spec.filters.push(spec),
        }
    }
    changed(state, cx, idx);
}

pub fn edit_filter(state: &mut AppState, idx: usize, i: usize) {
    if let Some(ex) = state.tabs.get_mut(idx).and_then(|t| t.explore.as_deref_mut()) {
        if let Some(f) = ex.spec.filters.get(i) {
            let (a, b) = match &f.op {
                FilterOp::Between { lo, hi } => (lo.clone().unwrap_or_default(), hi.clone().unwrap_or_default()),
                FilterOp::Contains { text } | FilterOp::StartsWith { text } => (text.clone(), String::new()),
                _ => (String::new(), String::new()),
            };
            let last_n = match &f.op {
                FilterOp::RelativeDate { last, .. } => *last,
                _ => 30,
            };
            ex.filter_editor = Some(FilterEditor { table: f.table.clone(), column: f.column.clone(), index: Some(i), op: f.op.clone(), search: String::new(), text_a: a, text_b: b, last_n });
        }
    }
}

pub fn remove_filter(state: &mut AppState, cx: &Ctx, idx: usize, i: usize) {
    if let Some(ex) = state.tabs.get_mut(idx).and_then(|t| t.explore.as_deref_mut()) {
        if i < ex.spec.filters.len() {
            ex.spec.filters.remove(i);
        }
    }
    changed(state, cx, idx);
}

/// The distinct values of a column for the "is one of" picker (cached per model key).
pub fn load_distinct(state: &mut AppState, cx: &Ctx, idx: usize, table: &str, column: &str) {
    let Some(t) = state.tabs.get_mut(idx) else { return };
    if let Some(local) = t.explore.as_ref().and_then(|e| e.spec.source.as_ref()).and_then(|s| s.local_table()).map(str::to_string) {
        let Some(ex) = t.explore.as_deref_mut() else { return };
        let k = (table.to_string(), column.to_string());
        if ex.distinct.get(&k).map(|l| !l.needs_load()).unwrap_or(false) {
            return;
        }
        ex.distinct.insert(k.clone(), Loadable::Loading);
        let tx = cx.fabric_tx.clone();
        let egui = cx.egui.clone();
        let engine = Arc::clone(cx.local);
        let sql = format!("SELECT DISTINCT CAST({c} AS VARCHAR) AS v FROM {t} WHERE {c} IS NOT NULL ORDER BY 1 LIMIT {}", DISTINCT_LIMIT, c = crate::localq::quote(column), t = crate::localq::quote(&local));
        let key = format!("local:{local}");
        let (table, column) = k;
        cx.session.spawn(async move {
            let result = match engine.sql(&sql).await {
                Ok(df) => match df.collect().await {
                    Ok(batches) => Ok(batches.iter().flat_map(|b| {
                        let col = b.column(0);
                        (0..b.num_rows()).filter_map(|i| col.as_any().downcast_ref::<arrow::array::StringArray>().map(|a| a.value(i).to_string()).or_else(|| col.as_any().downcast_ref::<arrow::array::LargeStringArray>().map(|a| a.value(i).to_string())).or_else(|| col.as_any().downcast_ref::<arrow::array::StringViewArray>().map(|a| a.value(i).to_string()))).collect::<Vec<_>>()
                    }).collect::<Vec<String>>()),
                    Err(e) => Err(e.to_string()),
                },
                Err(e) => Err(e),
            };
            let _ = tx.send(FabricEvent::ExploreDistinct { key, table, column, result });
            egui.request_repaint();
        });
        return;
    }
    let Some(tt) = &t.tabular else { return };
    let target = tt.target.clone();
    let key = tt.model_key();
    let Some(ex) = t.explore.as_deref_mut() else { return };
    let k = (table.to_string(), column.to_string());
    if ex.distinct.get(&k).map(|l| !l.needs_load()).unwrap_or(false) {
        return;
    }
    let force = tabularq::force_xmla_for(state, cx, &target);
    let auth = match tabularq::auth(state, cx, &target, force) {
        Ok(a) => a,
        Err(e) => {
            if let Some(ex) = state.tabs[idx].explore.as_deref_mut() {
                ex.distinct.insert(k.clone(), Loadable::Failed(e));
            }
            return;
        }
    };
    if let Some(ex) = state.tabs[idx].explore.as_deref_mut() {
        ex.distinct.insert(k.clone(), Loadable::Loading);
    }
    let tx = cx.fabric_tx.clone();
    let egui = cx.egui.clone();
    let dax = format!("EVALUATE\n    TOPN({DISTINCT_LIMIT}, DISTINCT({}))\nORDER BY {}", dax_column_ref(table, column), dax_column_ref(table, column));
    let (table, column) = k;
    cx.session.spawn(async move {
        let result = match crate::session::tabular::connect(&auth, &target).await {
            Ok(client) => {
                let opts = cobalt_tabular::DaxOptions { row_limit: Some(DISTINCT_LIMIT as u64 + 1), ..Default::default() };
                match client.execute(&target, &dax, &opts).await {
                    Ok(stream) => match stream.collect().await {
                        Ok(results) => match results.iter().find_map(|r| r.table()) {
                            Some(t) => {
                                let name = t.columns.first().map(|c| c.name.clone()).unwrap_or_default();
                                Ok((0..t.num_rows()).filter_map(|r| t.text(&name, r)).collect::<Vec<String>>())
                            }
                            None => Err(results.iter().find_map(|r| r.error()).map(|e| e.to_string()).unwrap_or_else(|| "no rows".into())),
                        },
                        Err(e) => Err(e.to_string()),
                    },
                    Err(e) => Err(e.to_string()),
                }
            }
            Err(e) => Err(e),
        };
        let _ = tx.send(FabricEvent::ExploreDistinct { key, table, column, result });
        egui.request_repaint();
    });
}

/// The column's data type, for the filter editor's choice of operators.
pub fn column_type(cat: Option<&ModelCatalog>, table: &str, column: &str) -> TabularDataType {
    cat.and_then(|c| c.table(table)).and_then(|t| t.columns.iter().find(|c| c.name.eq_ignore_ascii_case(column))).map(|c| c.data_type).unwrap_or(TabularDataType::Unknown)
}

// ---------------------------------------------------------------------------------------------
// Snapshots (hot exit)
// ---------------------------------------------------------------------------------------------

#[derive(serde::Serialize, serde::Deserialize)]
struct Snapshot {
    target: TabularTarget,
    spec: ExploreSpec,
}

pub fn snapshot_binding(t: &EditorTab) -> Option<String> {
    let tt = t.tabular.as_ref()?;
    let ex = t.explore.as_deref()?;
    Some(format!("{SNAPSHOT_PREFIX}{}", serde_json::to_string(&Snapshot { target: tt.target.clone(), spec: ex.spec.clone() }).unwrap_or_default()))
}

/// A snapshot's `database` column back into (target, spec).
pub fn from_snapshot(database: &str) -> Option<(TabularTarget, ExploreSpec)> {
    let json = database.strip_prefix(SNAPSHOT_PREFIX)?;
    let s: Snapshot = serde_json::from_str(json).ok()?;
    Some((s.target, s.spec))
}

/// Restore an Explore tab's state onto a restored tab (the DAX tab part is set by the caller).
pub fn restore(t: &mut EditorTab, spec: ExploreSpec) {
    let run_when_loaded = !spec.is_empty();
    let mut ex = ExploreState::new(spec);
    ex.run_when_loaded = run_when_loaded;
    t.explore = Some(Box::new(ex));
    if t.file_path.is_some() {
        if let Some(ex) = t.explore.as_deref_mut() {
            ex.saved_hash = hash_text(&ex.spec.to_json());
        }
    }
}

/// One-line summary for titles and the agent.
pub fn describe(spec: &ExploreSpec) -> String {
    cobalt_tabular::explore::describe(spec)
}

/// The sort menu's current label for a value.
pub fn sort_label(spec: &ExploreSpec, by: &SortBy) -> &'static str {
    match &spec.sort {
        Some(s) if &s.by == by => {
            if s.desc { "sorted ↓" } else { "sorted ↑" }
        }
        _ => "",
    }
}
