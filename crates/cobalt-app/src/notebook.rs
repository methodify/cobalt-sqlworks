//! Notebook tabs: open / new / save / export, cell structure edits, the per-tab run queue (SQL
//! cells run through the tab's session actor one at a time), and the bridge between cached
//! outputs in the file and live result sets in the grid.

use crate::ops::{self, Ctx};
use crate::session::Command;
use crate::state::*;
use crate::ui::results::ResultsAction;
use arrow::array::{ArrayRef, RecordBatch, StringArray};
use arrow::datatypes::{Field, Schema};
use cobalt_core::*;
use cobalt_notebook::{Cell, CellKind, CellLanguage, Notebook, Output, ARROW_MIME, DATARESOURCE_MIME};
use cobalt_results::{CellFormatter, ResultSet};
use cobalt_store::NewHistoryEntry;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const SQL_TYPES_KEY: &str = "cobalt.sql_types";

pub fn is_notebook_path(p: &Path) -> bool {
    p.extension().map(|e| e.eq_ignore_ascii_case("ipynb")).unwrap_or(false)
}

/// Open a notebook file (`.ipynb`, or a Fabric Git `notebook-content.py`). Returns false when the
/// file is not a notebook, so the caller opens it as text.
pub fn open_path(state: &mut AppState, cx: &Ctx, path: PathBuf) -> bool {
    let Ok(text) = std::fs::read_to_string(&path) else { return false };
    let text = text.strip_prefix('\u{feff}').map(str::to_string).unwrap_or(text);
    let is_py = path.extension().map(|e| e.eq_ignore_ascii_case("py")).unwrap_or(false);
    if !is_notebook_path(&path) && !(is_py && Notebook::looks_like_fabric_py(&text)) {
        return false;
    }
    if let Some(i) = state.tabs.iter().position(|t| t.file_path.as_ref() == Some(&path)) {
        state.active_tab = Some(i);
        return true;
    }
    let (nb, warnings) = if is_py {
        Notebook::from_fabric_py(&text)
    } else {
        match Notebook::parse(&text) {
            Ok(nb) => (nb, Vec::new()),
            Err(e) => {
                cx.toast(ToastKind::Error, format!("Could not open {}: {e}", path.display()));
                return true;
            }
        }
    };
    let title = path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "notebook.ipynb".into());
    let idx = install(state, cx, nb, Some(path), title);
    if let Some(nbs) = state.tabs[idx].notebook.as_mut() {
        nbs.warnings = warnings;
    }
    true
}

/// A new, empty notebook tab with one code cell.
pub fn new_tab(state: &mut AppState, cx: &Ctx, language: CellLanguage) -> usize {
    let mut nb = Notebook::new(language);
    nb.cells.push(Cell::code(""));
    let n = state.tabs.iter().filter(|t| t.is_notebook()).count() + 1;
    let idx = install(state, cx, nb, None, format!("Notebook_{n}.ipynb"));
    if let Some(nbs) = state.tabs[idx].notebook.as_mut() {
        nbs.cells[0].editor.request_focus = true;
    }
    idx
}

/// Make a notebook tab from a parsed document. Inherits the active tab's connection profile.
fn install(state: &mut AppState, cx: &Ctx, nb: Notebook, path: Option<PathBuf>, title: String) -> usize {
    let inherit = state.active().and_then(|t| t.profile.clone());
    let idx = state.new_tab();
    let mut nbs = NotebookState::new(nb);
    for (i, cell) in nbs.nb.cells.iter().enumerate() {
        let (run, extra) = load_outputs(cell, i);
        nbs.cells[i].run = run;
        nbs.cells[i].cached = true;
        nbs.cells[i].extra_outputs = extra;
    }
    let t = &mut state.tabs[idx];
    t.title = title;
    t.custom_title = true;
    t.file_path = path;
    t.profile = inherit;
    t.text = String::new();
    t.notebook = Some(Box::new(nbs));
    t.mark_saved();
    maybe_early_start(state, cx, idx);
    idx
}

/// Restore a notebook tab from a hot-exit snapshot (the snapshot text is the .ipynb JSON).
pub fn restore_from_snapshot(t: &mut EditorTab) -> bool {
    if !Notebook::looks_like_ipynb(&t.text) {
        return false;
    }
    let Ok(mut nb) = Notebook::parse(&t.text) else { return false };
    // the Fabric item a snapshot came from (see ops::snapshot_document)
    if let Some(serde_json::Value::Object(c)) = nb.metadata.remove("cobalt") {
        if let Some(fi) = c.get("fabric_item") {
            let item = cobalt_fabric::FabricItem {
                id: fi.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                workspace_id: fi.get("workspace_id").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                display_name: fi.get("display_name").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                description: String::new(),
                item_type: fi.get("item_type").and_then(|v| v.as_str()).unwrap_or("Notebook").to_string(),
                folder_id: None,
            };
            if !item.id.is_empty() {
                let platform = fi.get("platform").cloned().and_then(|p| serde_json::from_value(p).ok());
                t.fabric_item = Some(FabricItemRef { item, platform, saving: false });
            }
        }
    }
    let mut nbs = NotebookState::new(nb);
    for (i, cell) in nbs.nb.cells.iter().enumerate() {
        let (run, extra) = load_outputs(cell, i);
        nbs.cells[i].run = run;
        nbs.cells[i].cached = true;
        nbs.cells[i].extra_outputs = extra;
    }
    t.text.clear();
    t.notebook = Some(Box::new(nbs));
    t.custom_title = true;
    true
}

/// The document with the cells' current outputs folded in (what gets saved or snapshotted).
pub fn document(t: &mut EditorTab, max_rows: u64, fmt: &CellFormatter) -> Option<Notebook> {
    let nb = t.notebook.as_deref_mut()?;
    for (i, cs) in nb.cells.iter().enumerate() {
        let cell = &mut nb.nb.cells[i];
        if cell.kind != CellKind::Code {
            cell.outputs.clear();
            cell.execution_count = None;
            continue;
        }
        if cs.cached {
            continue; // the file's outputs are still what we show
        }
        cell.outputs = match &cs.run {
            Some(run) if !run.is_live() => outputs_from_run(run, cell.execution_count, max_rows, fmt),
            Some(_) => Vec::new(),
            None => Vec::new(),
        };
    }
    Some(nb.nb.clone())
}

pub fn save(state: &mut AppState, cx: &Ctx, idx: usize, save_as: bool) {
    let Some(t) = state.tabs.get(idx) else { return };
    if t.notebook.is_none() {
        return ops::save_file(state, cx, idx, save_as);
    }
    if t.fabric_item.is_some() && !save_as {
        state.dialog = Dialog::ConfirmFabricSave { tab_index: idx };
        return;
    }
    let path = match (&t.file_path, save_as) {
        (Some(p), false) => p.clone(),
        _ => {
            let mut dlg = rfd::FileDialog::new().add_filter("Notebook", &["ipynb"]).add_filter("Fabric notebook source (Git)", &["py"]).set_file_name(format!("{}.ipynb", t.title.trim_end_matches(".ipynb").trim_end_matches(".py")));
            if let Some(dir) = cx.settings.export.last_dir.as_ref() {
                dlg = dlg.set_directory(dir);
            }
            match dlg.save_file() {
                Some(p) => p,
                None => return,
            }
        }
    };
    let fmt = state.formatter.clone();
    let t = &mut state.tabs[idx];
    let Some(doc) = document(t, cx.settings.notebooks.max_output_rows, &fmt) else { return };
    let text = if path.extension().map(|e| e.eq_ignore_ascii_case("py")).unwrap_or(false) { doc.to_fabric_py() } else { doc.to_ipynb() };
    match std::fs::write(&path, &text) {
        Ok(()) => {
            t.file_path = Some(path.clone());
            t.title = path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or(t.title.clone());
            t.custom_title = true;
            t.fabric_item = None; // a local copy from here on
            t.mark_saved();
            state.flash(format!("Saved {}", path.display()));
        }
        Err(e) => cx.toast(ToastKind::Error, format!("Save failed: {e}")),
    }
}

/// A tab for a notebook that is being fetched from Fabric: a spinner until the definition
/// arrives, an error with Retry when it does not.
pub fn open_placeholder(state: &mut AppState, cx: &Ctx, item: &cobalt_fabric::FabricItem, copy: bool) -> usize {
    let title = if copy { format!("{} (copy).ipynb", item.display_name) } else { item.display_name.clone() };
    let idx = install(state, cx, Notebook::new(CellLanguage::Python), None, title);
    let t = &mut state.tabs[idx];
    if let Some(nbs) = t.notebook.as_deref_mut() {
        nbs.cells.clear();
        nbs.nb.cells.clear();
        nbs.loading = Some(NotebookLoading { item_id: item.id.clone(), copy, message: format!("Fetching {} from Fabric…", item.display_name), failed: false });
        nbs.dirty = false;
    }
    idx
}

/// The placeholder tab for a fetch, if it is still open.
fn placeholder_index(state: &AppState, item_id: &str, copy: bool) -> Option<usize> {
    state.tabs.iter().position(|t| t.notebook.as_deref().and_then(|nb| nb.loading.as_ref()).map(|l| l.item_id == item_id && l.copy == copy).unwrap_or(false))
}

pub fn placeholder_failed(state: &mut AppState, item_id: &str, copy: bool, message: String) {
    if let Some(i) = placeholder_index(state, item_id, copy) {
        if let Some(l) = state.tabs[i].notebook.as_deref_mut().and_then(|nb| nb.loading.as_mut()) {
            l.failed = true;
            l.message = message;
        }
    }
}

/// A notebook fetched from Fabric: `copy` opens it as a detached local notebook.
pub fn install_from_fabric(state: &mut AppState, cx: &Ctx, item: cobalt_fabric::FabricItem, def: cobalt_fabric::ItemDefinition, copy: bool) {
    use base64::Engine;
    let decode = |p: &cobalt_fabric::DefinitionPart| base64::engine::general_purpose::STANDARD.decode(p.payload.trim()).ok().and_then(|b| String::from_utf8(b).ok());
    let (nb, warnings) = if let Some(p) = def.part_with_suffix(".ipynb") {
        match decode(p).ok_or_else(|| "bad base64".to_string()).and_then(|t| Notebook::parse(&t).map_err(|e| e.to_string())) {
            Ok(nb) => (nb, Vec::new()),
            Err(e) => {
                placeholder_failed(state, &item.id, copy, format!("Could not read the notebook: {e}"));
                return;
            }
        }
    } else if let Some(p) = def.part_with_suffix(".py") {
        match decode(p) {
            Some(t) => Notebook::from_fabric_py(&t),
            None => {
                placeholder_failed(state, &item.id, copy, "Could not read the notebook: bad base64".into());
                return;
            }
        }
    } else {
        placeholder_failed(state, &item.id, copy, format!("The item has no notebook content ({} parts)", def.parts.len()));
        return;
    };
    let platform = def.part(".platform").cloned();
    let title = if copy { format!("{} (copy).ipynb", item.display_name) } else { item.display_name.clone() };
    // fill the placeholder tab when there is one, else open a new tab
    let idx = match placeholder_index(state, &item.id, copy) {
        Some(i) => {
            let mut nbs = NotebookState::new(nb);
            for (ci, cell) in nbs.nb.cells.iter().enumerate() {
                let (run, extra) = load_outputs(cell, ci);
                nbs.cells[ci].run = run;
                nbs.cells[ci].cached = true;
                nbs.cells[ci].extra_outputs = extra;
            }
            let t = &mut state.tabs[i];
            t.notebook = Some(Box::new(nbs));
            t.title = title;
            t.mark_saved();
            state.active_tab = Some(i);
            i
        }
        None => install(state, cx, nb, None, title),
    };
    maybe_early_start(state, cx, idx);
    let t = &mut state.tabs[idx];
    if !copy {
        t.fabric_item = Some(FabricItemRef { item: item.clone(), platform, saving: false });
    }
    if let Some(nbs) = t.notebook.as_deref_mut() {
        nbs.warnings = warnings;
        // a Fabric notebook's lakehouse lives in its workspace unless the metadata says otherwise
        if nbs.fabric.is_none() {
            nbs.fabric = Some(NotebookFabric { workspace_id: item.workspace_id.clone(), lakehouse_id: None, write_mode: "sandbox".into(), preload: false });
        }
    }
    state.flash(format!("Opened {} from Fabric{}", item.display_name, if copy { " as a copy" } else { "" }));
}

/// Bind a notebook to a workspace / default lakehouse / write mode; the metadata follows so
/// Fabric sees the same default lakehouse.
pub fn set_fabric(state: &mut AppState, idx: usize, binding: Option<NotebookFabric>) {
    // binding a lakehouse while an early-started plain session sits unused: rebind it now, so the
    // first cell is instant on the right session
    if binding.is_some() && state.kernel.fabric.is_none() && state.active_tab == Some(idx) {
        let _ = rebind_if_unused(state);
    }
    let names: Option<(String, Option<String>)> = binding.as_ref().map(|b| (state.fabric.workspace(&b.workspace_id).map(|w| w.display_name.clone()).unwrap_or_default(), b.lakehouse_id.as_ref().and_then(|id| state.fabric.lakehouses(&b.workspace_id).and_then(|v| v.into_iter().find(|(_, i)| i == id).map(|(n, _)| n)))));
    let Some(nb) = state.tabs.get_mut(idx).and_then(|t| t.notebook.as_deref_mut()) else { return };
    nb.fabric = binding.clone();
    let deps = nb.nb.metadata.entry("dependencies").or_insert_with(|| serde_json::Value::Object(Default::default()));
    if let serde_json::Value::Object(d) = deps {
        match (&binding, names) {
            (Some(b), Some((_, lh_name))) if b.lakehouse_id.is_some() => {
                let id = b.lakehouse_id.clone().unwrap_or_default();
                let mut lh = serde_json::Map::new();
                lh.insert("default_lakehouse".into(), serde_json::Value::String(id.clone()));
                if let Some(n) = lh_name {
                    lh.insert("default_lakehouse_name".into(), serde_json::Value::String(n));
                }
                lh.insert("default_lakehouse_workspace_id".into(), serde_json::Value::String(b.workspace_id.clone()));
                lh.insert("known_lakehouses".into(), serde_json::json!([{"id": id}]));
                d.insert("lakehouse".into(), serde_json::Value::Object(lh));
            }
            _ => {
                d.remove("lakehouse");
            }
        }
    }
    nb.dirty = true;
}

/// Resolve a notebook's binding into the session config. `Err(true)` = still loading the
/// workspace's items (try again on the Items event); `Err(false)` = cannot bind (told the user).
/// What a lakehouse does when a session attaches it — remembered per lakehouse id in the store.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LakehousePolicy {
    /// `none` · `last` (the tables cloned in earlier sessions) · `all` · `list` (`tables`).
    #[serde(default)]
    pub preload: String,
    #[serde(default)]
    pub tables: Vec<String>,
    /// Keep the shallow clones on disk between sessions (`persist_shadow`).
    #[serde(default)]
    pub keep_clones: bool,
}

const LAKEHOUSE_POLICY_PREFIX: &str = "pref:lakehouse:";

pub fn lakehouse_policy(cx: &Ctx, lakehouse_id: &str) -> LakehousePolicy {
    match cx.store.fabric_cache_get(&format!("{LAKEHOUSE_POLICY_PREFIX}{lakehouse_id}")) {
        Ok(Some((text, _))) => serde_json::from_str(&text).unwrap_or_default(),
        _ => LakehousePolicy::default(),
    }
}

pub fn set_lakehouse_policy(cx: &Ctx, lakehouse_id: &str, policy: &LakehousePolicy) {
    if let Ok(text) = serde_json::to_string(policy) {
        let _ = cx.store.fabric_cache_put(&format!("{LAKEHOUSE_POLICY_PREFIX}{lakehouse_id}"), &text);
    }
}

/// Early start (Settings → Notebooks & Spark): a Spark notebook that was just opened or created
/// brings the session up so its first cell does not wait. Quiet: a lakehouse notebook on a
/// signed-out Fabric panel does nothing (the first cell will say what is needed).
pub fn maybe_early_start(state: &mut AppState, cx: &Ctx, idx: usize) {
    if cx.settings.spark.early_start != "notebook_open" {
        return;
    }
    let Some(t) = state.tabs.get(idx) else { return };
    let spark = t.spark.is_some() || t.notebook.as_deref().map(|nb| nb.kernel == NotebookKernel::Spark).unwrap_or(false);
    if !spark {
        return;
    }
    if binding_of(t).is_some() && state.fabric.slot.is_none() {
        return;
    }
    if !matches!(state.kernel.state, crate::kernel::KernelState::Stopped) {
        return;
    }
    let _ = ensure_session(state, cx, idx);
}

// ---------------------------------------------------------------------------------------------
// Lakehouse pane
// ---------------------------------------------------------------------------------------------

/// Each frame the pane is visible: follow the active notebook's binding and load what is missing
/// (tables and the Files root from OneLake; the mirror status and shadows from the session).
pub fn lakehouse_pane_shown(state: &mut AppState, cx: &Ctx) {
    let binding = state.active().and_then(binding_of);
    let Some(b) = binding else {
        state.lakehouse_pane.selected = None;
        return;
    };
    let Some(lakehouses) = state.fabric.lakehouses(&b.workspace_id) else {
        if state.fabric.slot.is_some() && !state.fabric.items.get(&b.workspace_id).map(|l| l.is_loading()).unwrap_or(false) {
            if state.fabric.workspaces.get().is_none() {
                crate::fabric::load_workspaces(state, cx);
            }
            crate::fabric::load_items(state, cx, &b.workspace_id);
        }
        return;
    };
    // keep a pick within the same workspace; otherwise the notebook's default (or the first)
    let keep = match &state.lakehouse_pane.selected {
        Some((ws, _, id)) if *ws == b.workspace_id && lakehouses.iter().any(|(_, i)| i == id) => Some(id.clone()),
        _ => None,
    };
    let Some(id) = keep.or_else(|| b.lakehouse_id.clone().filter(|id| lakehouses.iter().any(|(_, i)| i == id))).or_else(|| lakehouses.first().map(|(_, i)| i.clone())) else {
        state.lakehouse_pane.selected = None;
        return;
    };
    let name = lakehouses.iter().find(|(_, i)| *i == id).map(|(n, _)| n.clone()).unwrap_or_default();
    let want = (b.workspace_id.clone(), name, id);
    if state.lakehouse_pane.selected.as_ref() != Some(&want) {
        state.lakehouse_pane.reset_data();
        state.lakehouse_pane.expanded.clear();
        state.lakehouse_pane.selected = Some(want.clone());
    }
    let (ws, lh_name, lh_id) = want;
    if state.lakehouse_pane.tables.is_none() && !state.lakehouse_pane.tables_loading {
        state.lakehouse_pane.tables_loading = true;
        crate::fabric::load_pane_tables(state, cx, &ws, &lh_id);
    }
    if !state.lakehouse_pane.files.contains_key("") && !state.lakehouse_pane.files_loading.contains("") {
        state.lakehouse_pane.files_loading.insert(String::new());
        crate::fabric::load_pane_files(state, cx, &ws, &lh_id, "");
    }
    if state.lakehouse_pane.refresh_at.map(|t| std::time::Instant::now() >= t).unwrap_or(false) {
        state.lakehouse_pane.refresh_at = None;
        state.lakehouse_pane.mirror = None;
        state.lakehouse_pane.mirror_pending = false;
        state.shadows.status = None;
        state.lakehouse_pane.note = None;
    } else if state.lakehouse_pane.refresh_at.is_some() {
        cx.egui.request_repaint_after(std::time::Duration::from_millis(500));
    }
    if state.kernel.state.is_ready() {
        if state.lakehouse_pane.mirror.is_none() && !state.lakehouse_pane.mirror_pending {
            state.lakehouse_pane.mirror_pending = crate::kernel::call(&mut state.kernel, "pane-mirror", "mirror_status", serde_json::json!({}));
        }
        if state.shadows.status.is_none() && !state.shadows.loading {
            refresh_shadows(state);
        }
    }
    let _ = lh_name;
}

pub fn lakehouse_action(state: &mut AppState, cx: &Ctx, a: crate::ui::lakehouse::LakehouseAction) {
    use crate::ui::lakehouse::LakehouseAction as A;
    let Some((ws, lh_name, lh_id)) = state.lakehouse_pane.selected.clone() else {
        if let A::Refresh = a {
            lakehouse_pane_shown(state, cx);
        }
        return;
    };
    match a {
        A::Select(id) => {
            if let Some(name) = state.fabric.lakehouses(&ws).and_then(|v| v.into_iter().find(|(_, i)| *i == id).map(|(n, _)| n)) {
                state.lakehouse_pane.reset_data();
                state.lakehouse_pane.expanded.clear();
                state.lakehouse_pane.selected = Some((ws, name, id));
                lakehouse_pane_shown(state, cx);
            }
        }
        A::MakeDefault(id) => {
            if let Some(idx) = state.active_tab {
                let binding = binding_of(&state.tabs[idx]);
                if let Some(b) = binding {
                    if state.tabs[idx].spark.is_some() {
                        crate::sparkq::set_binding(state, cx, idx, Some(NotebookFabric { lakehouse_id: Some(id), ..b }));
                    } else {
                        set_fabric(state, idx, Some(NotebookFabric { lakehouse_id: Some(id), ..b }));
                    }
                }
            }
        }
        A::Refresh => {
            if let (Some(cat), Some((_, _, lh_id))) = (&state.kernel.sail_catalog, state.lakehouse_pane.selected.clone()) {
                cat.invalidate(Some(&lh_id));
            }
            state.lakehouse_pane.reset_data();
            state.shadows.status = None;
            if state.active().map(|t| t.spark.is_some()).unwrap_or(false) {
                crate::sparkq::refresh_catalog(state, cx, &ws, &lh_id, &lh_name);
            }
            lakehouse_pane_shown(state, cx);
            // re-list the folders that are open
            let open: Vec<String> = state.lakehouse_pane.expanded.iter().cloned().collect();
            for rel in open {
                state.lakehouse_pane.files_loading.insert(rel.clone());
                crate::fabric::load_pane_files(state, cx, &ws, &lh_id, &rel);
            }
        }
        A::ExpandFiles(key) => {
            if let Some(schema) = key.strip_prefix("schema:") {
                let k = format!("schema:{schema}");
                if !state.lakehouse_pane.collapsed.remove(&k) {
                    state.lakehouse_pane.collapsed.insert(k);
                }
            } else if state.lakehouse_pane.expanded.remove(&key) {
                // collapsed
            } else {
                state.lakehouse_pane.expanded.insert(key.clone());
                if !state.lakehouse_pane.files.contains_key(&key) && !state.lakehouse_pane.files_loading.contains(&key) {
                    state.lakehouse_pane.files_loading.insert(key.clone());
                    crate::fabric::load_pane_files(state, cx, &ws, &lh_id, &key);
                }
            }
        }
        A::Mount(lh, table) => {
            // mount_table takes the `schema/table` entry form since local-spark-mcp 0.6.3 and
            // answers when the clone exists
            if !crate::kernel::call(&mut state.kernel, "pane-action", "mount_table", serde_json::json!({"lakehouse": lh, "table": table})) {
                cx.toast(ToastKind::Warning, "Start the Spark session first (run a cell or use the kernel menu).");
            } else {
                state.lakehouse_pane.note = Some(format!("Cloning {table}…"));
            }
        }
        A::Discard(table) => shadows_action(state, "discard_shadow", serde_json::json!({"table": table})),
        A::Restore(table) => shadows_action(state, "restore_shadow", serde_json::json!({"table": table, "version": 0})),
        A::Pull(rel) => {
            if !crate::kernel::call(&mut state.kernel, "pane-action", "sync_files", serde_json::json!({"paths": [rel], "direction": "pull", "lakehouse": lh_name})) {
                cx.toast(ToastKind::Warning, "Start the Spark session first (run a cell or use the kernel menu).");
            } else {
                state.lakehouse_pane.note = Some(format!("Pulling Files/{rel}…"));
            }
        }
        A::RemoveLocal(rel) => {
            if !crate::kernel::call(&mut state.kernel, "pane-action", "clear_mirror", serde_json::json!({"lakehouse": lh_name, "paths": [rel]})) {
                cx.toast(ToastKind::Warning, "Start the Spark session first (run a cell or use the kernel menu).");
            } else {
                state.lakehouse_pane.note = Some(format!("Removing the local copy of Files/{rel}…"));
            }
        }
        A::InsertCell(code) => {
            let Some(idx) = state.active_tab else { return };
            if state.tabs[idx].spark.is_some() {
                crate::sparkq::insert_at_cursor(&mut state.tabs[idx], &code);
                return;
            }
            let at = state.tabs[idx].notebook.as_deref().map(|nb| (nb.selected + 1).min(nb.cells.len())).unwrap_or(0);
            if let Some(i) = insert_cell(state, idx, at, CellKind::Code, true) {
                if let Some(nb) = state.tabs[idx].notebook.as_deref_mut() {
                    if let Some(cell) = nb.nb.cells.get_mut(i) {
                        cell.source = code;
                    }
                    nb.cells[i].editor.cursors.clamp(nb.nb.cells[i].source.chars().count());
                    nb.dirty = true;
                }
                set_language(state, idx, i, Some(CellLanguage::Python));
            }
        }
    }
}

/// A running session nobody has used yet (no context, no cell) — an early start, typically — can
/// be replaced without losing anything. Stops it and lets the restart consumer bring it back
/// with the active notebook's binding; queued cells survive the restart.
pub fn rebind_if_unused(state: &mut AppState) -> bool {
    let k = &state.kernel;
    let unused = (k.state.is_ready() || k.state.is_starting()) && k.contexts.is_empty() && k.busy.is_none() && k.waiting.is_empty();
    if !unused {
        return false;
    }
    crate::kernel::stop(&mut state.kernel);
    state.kernel_restart_pending = true;
    state.kernel.log.push_back("cobalt: rebinding the unused session to the notebook's lakehouse".into());
    true
}

/// The lifecycle policy: stop a session nobody has used for `idle_minutes`.
pub fn lifecycle_tick(state: &mut AppState, cx: &Ctx) {
    if cx.settings.spark.lifecycle != "idle" || !state.kernel.state.is_ready() {
        return;
    }
    let preload_running = state.shadows.preload.as_ref().map(|p| p.get("state").and_then(|s| s.as_str()) == Some("running")).unwrap_or(false);
    if preload_running {
        state.kernel.last_activity = std::time::Instant::now();
        return;
    }
    let limit = std::time::Duration::from_secs(60 * cx.settings.spark.idle_minutes.max(1) as u64);
    if state.kernel.idle() >= limit {
        crate::kernel::stop(&mut state.kernel);
        cx.toast(ToastKind::Info, format!("The local Spark session stopped after {} minutes without a cell (Settings › Notebooks & Spark › Session lifecycle).", cx.settings.spark.idle_minutes.max(1)));
    }
}

/// Closing the last Spark notebook ends the session when the lifecycle policy says so.
pub fn on_spark_notebook_closed(state: &mut AppState, cx: &Ctx) {
    if cx.settings.spark.lifecycle != "last_notebook" {
        return;
    }
    let any = state.tabs.iter().any(|t| t.spark.is_some() || t.notebook.as_deref().map(|nb| nb.kernel == NotebookKernel::Spark).unwrap_or(false));
    if !any && !matches!(state.kernel.state, crate::kernel::KernelState::Stopped) {
        crate::kernel::stop(&mut state.kernel);
        cx.toast(ToastKind::Info, "The last Spark notebook or tab closed; the local Spark session stopped (Settings › Notebooks & Spark › Session lifecycle).");
    }
}

/// The lakehouse binding a tab runs Spark with: a notebook's, or a Spark SQL query tab's.
pub fn binding_of(t: &EditorTab) -> Option<NotebookFabric> {
    if let Some(nb) = t.notebook.as_deref() {
        return nb.fabric.clone();
    }
    t.spark.as_ref().and_then(|s| s.binding.clone())
}

/// Whatever this tab had waiting for the session is dropped (queued cells, or a query tab's
/// pending run).
fn abandon_pending(state: &mut AppState, idx: usize) {
    let t = &mut state.tabs[idx];
    if let Some(nb) = t.notebook.as_deref_mut() {
        nb.queue.clear();
    }
    if t.spark.is_some() {
        t.pending_run = None;
    }
}

/// How a Spark run may proceed once the session and the tab's binding are reconciled.
pub(crate) enum SparkPrep {
    /// Submit: the context's default lakehouse and the workspaces to attach first.
    Ready { context_lakehouse: Option<String>, register: Vec<serde_json::Value> },
    /// Something is on its way (session start, token, item lists): keep the request pending.
    Wait,
    /// Not possible (sign-in missing, binding refused); the request was dropped.
    Abort,
}

fn kernel_fabric(state: &mut AppState, cx: &Ctx, idx: usize) -> Result<Option<crate::kernel::KernelFabric>, bool> {
    let Some(b) = binding_of(&state.tabs[idx]) else { return Ok(None) };
    let Some(slot) = state.fabric.slot else {
        if state.fabric.status() == crate::fabric::FabricStatus::SignedOut {
            crate::fabric::on_panel_shown(state, cx);
        }
        if state.fabric.slot.is_none() {
            cx.toast(ToastKind::Warning, "This notebook uses a Fabric lakehouse: sign in on the Fabric panel first, then run the cell again.");
            state.sidebar_visible = true;
            state.sidebar_view = SidebarView::Fabric;
            return Err(false);
        }
        return Err(true);
    };
    let lakehouses = match state.fabric.lakehouses(&b.workspace_id) {
        Some(v) => v,
        None => {
            if !state.fabric.items.get(&b.workspace_id).map(|l| l.is_loading()).unwrap_or(false) {
                if state.fabric.workspaces.get().is_none() {
                    crate::fabric::load_workspaces(state, cx);
                }
                crate::fabric::load_items(state, cx, &b.workspace_id);
            }
            return Err(true);
        }
    };
    let default_lakehouse = b.lakehouse_id.as_ref().and_then(|id| lakehouses.iter().find(|(_, i)| i == id).map(|(n, _)| n.clone()));
    if b.lakehouse_id.is_some() && default_lakehouse.is_none() {
        cx.toast(ToastKind::Warning, "The notebook's default lakehouse is not in that workspace any more; pick one from the lakehouse button.");
    }
    // per-lakehouse policies (the notebook's old preload flag means "all" for its default lakehouse)
    let mut preload = serde_json::Map::new();
    let mut preload_last = Vec::new();
    let mut persist_shadow = false;
    for (name, id) in &lakehouses {
        let mut p = lakehouse_policy(cx, id);
        if b.preload && p.preload.is_empty() && Some(name) == default_lakehouse.as_ref() {
            p.preload = "all".into();
        }
        persist_shadow |= p.keep_clones;
        match p.preload.as_str() {
            "all" => {
                preload.insert(name.clone(), serde_json::Value::Null);
            }
            "list" if !p.tables.is_empty() => {
                preload.insert(name.clone(), serde_json::Value::Array(p.tables.iter().map(|t| serde_json::Value::String(t.clone())).collect()));
            }
            "last" => preload_last.push(name.clone()),
            _ => {}
        }
    }
    let preload = if preload.is_empty() { serde_json::Value::Null } else { serde_json::Value::Object(preload) };
    // LakeSail has no shallow clones: its sessions are read-only or write-through
    let write_mode = if crate::runtime::engine(cx.settings).is_sail() && b.write_mode == "sandbox" { "readonly".to_string() } else { b.write_mode.clone() };
    Ok(Some(crate::kernel::KernelFabric { workspace_id: b.workspace_id.clone(), workspace_name: state.fabric.workspace(&b.workspace_id).map(|w| w.display_name.clone()).unwrap_or_default(), lakehouses, default_lakehouse, write_mode, slot, tenant: cx.settings.connections.entra_default_tenant.clone().filter(|s| !s.trim().is_empty()), preload, preload_last, persist_shadow, extra_workspaces: Vec::new() }))
}

/// Pump every Spark notebook with queued cells (after the kernel came up or items loaded).
pub fn pump_all_spark(state: &mut AppState, cx: &Ctx) {
    let tabs: Vec<TabId> = state.tabs.iter().filter(|t| t.notebook.as_deref().map(|nb| nb.kernel == NotebookKernel::Spark && !nb.queue.is_empty()).unwrap_or(false)).map(|t| t.id).collect();
    for tab in tabs {
        pump(state, cx, tab, false);
    }
    crate::sparkq::pump_pending(state, cx);
}

/// Ask the running session for its shadow state (the Shadows window).
pub fn refresh_shadows(state: &mut AppState) {
    if crate::kernel::call(&mut state.kernel, "shadows", "shadow_status", serde_json::json!({})) {
        state.shadows.loading = true;
        state.shadows.error = None;
    } else {
        state.shadows.status = None;
        state.shadows.error = Some("the local Spark session is not running".into());
    }
}

pub fn shadows_action(state: &mut AppState, method: &str, params: serde_json::Value) {
    // progress questions go on the control socket so they answer while a cell runs
    let sent = if method == "preload_status" { crate::kernel::control_call(&mut state.kernel, "shadows-action", method, params) } else { crate::kernel::call(&mut state.kernel, "shadows-action", method, params) };
    if sent {
        state.shadows.loading = true;
        state.shadows.error = None;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportKind {
    Html,
    Markdown,
}

pub fn export(state: &mut AppState, cx: &Ctx, idx: usize, kind: ExportKind, path: Option<PathBuf>) {
    let fmt = state.formatter.clone();
    let Some(t) = state.tabs.get_mut(idx) else { return };
    let title = t.title.trim_end_matches(".ipynb").to_string();
    let Some(doc) = document(t, cx.settings.notebooks.max_output_rows, &fmt) else { return };
    let (ext, label) = match kind {
        ExportKind::Html => ("html", "HTML"),
        ExportKind::Markdown => ("md", "Markdown"),
    };
    let path = match path {
        Some(p) => p,
        None => {
            let mut dlg = rfd::FileDialog::new().add_filter(label, &[ext]).set_file_name(format!("{title}.{ext}"));
            if let Some(dir) = cx.settings.export.last_dir.as_ref() {
                dlg = dlg.set_directory(dir);
            }
            match dlg.save_file() {
                Some(p) => p,
                None => return,
            }
        }
    };
    let text = match kind {
        ExportKind::Html => doc.to_html(&title),
        ExportKind::Markdown => doc.to_markdown(),
    };
    match std::fs::write(&path, text) {
        Ok(()) => cx.toast(ToastKind::Success, format!("Exported {}", path.display())),
        Err(e) => cx.toast(ToastKind::Error, format!("Export failed: {e}")),
    }
}

// ---------------------------------------------------------------------------------------------
// Cell structure
// ---------------------------------------------------------------------------------------------

pub fn insert_cell(state: &mut AppState, idx: usize, at: usize, kind: CellKind, focus: bool) -> Option<usize> {
    let nb = state.tabs.get_mut(idx)?.notebook.as_deref_mut()?;
    let at = at.min(nb.cells.len());
    let cell = match kind {
        CellKind::Markdown => Cell::markdown(""),
        _ => Cell::code(""),
    };
    let mut cs = CellState::new(cell.id.clone());
    cs.md_editing = kind == CellKind::Markdown;
    cs.editor.request_focus = focus;
    nb.nb.cells.insert(at, cell);
    nb.cells.insert(at, cs);
    nb.selected = at;
    nb.dirty = true;
    Some(at)
}

pub fn delete_cell(state: &mut AppState, idx: usize, at: usize) {
    let Some(nb) = state.tabs.get_mut(idx).and_then(|t| t.notebook.as_deref_mut()) else { return };
    if at >= nb.cells.len() {
        return;
    }
    if nb.cells[at].run.as_ref().map(|r| r.is_live()).unwrap_or(false) {
        return;
    }
    let cell = nb.nb.cells.remove(at);
    nb.cells.remove(at);
    nb.undo_delete = Some((at, cell));
    nb.selected = at.min(nb.cells.len().saturating_sub(1));
    nb.dirty = true;
    if nb.cells.is_empty() {
        let c = Cell::code("");
        nb.cells.push(CellState::new(c.id.clone()));
        nb.nb.cells.push(c);
        nb.selected = 0;
    }
}

pub fn undo_delete(state: &mut AppState, idx: usize) {
    let Some(nb) = state.tabs.get_mut(idx).and_then(|t| t.notebook.as_deref_mut()) else { return };
    let Some((at, cell)) = nb.undo_delete.take() else { return };
    let at = at.min(nb.cells.len());
    let (run, extra) = load_outputs(&cell, at);
    let mut cs = CellState::new(cell.id.clone());
    cs.run = run;
    cs.cached = true;
    cs.extra_outputs = extra;
    nb.nb.cells.insert(at, cell);
    nb.cells.insert(at, cs);
    nb.selected = at;
    nb.dirty = true;
}

pub fn move_cell(state: &mut AppState, idx: usize, at: usize, delta: isize) {
    let Some(nb) = state.tabs.get_mut(idx).and_then(|t| t.notebook.as_deref_mut()) else { return };
    let n = nb.cells.len();
    let to = at as isize + delta;
    if at >= n || to < 0 || to as usize >= n {
        return;
    }
    let to = to as usize;
    nb.nb.cells.swap(at, to);
    nb.cells.swap(at, to);
    nb.selected = to;
    nb.dirty = true;
}

pub fn set_kind(state: &mut AppState, idx: usize, at: usize, kind: CellKind) {
    let Some(nb) = state.tabs.get_mut(idx).and_then(|t| t.notebook.as_deref_mut()) else { return };
    let Some(cell) = nb.nb.cells.get_mut(at) else { return };
    if cell.kind == kind {
        return;
    }
    cell.kind = kind;
    cell.outputs.clear();
    cell.execution_count = None;
    let cs = &mut nb.cells[at];
    cs.run = None;
    cs.extra_outputs.clear();
    cs.md_editing = kind == CellKind::Markdown;
    cs.editor = EditorState::default();
    cs.editor.request_focus = true;
    nb.dirty = true;
}

pub fn set_language(state: &mut AppState, idx: usize, at: usize, lang: Option<CellLanguage>) {
    let Some(nb) = state.tabs.get_mut(idx).and_then(|t| t.notebook.as_deref_mut()) else { return };
    let group = nb.nb.language_group();
    let Some(cell) = nb.nb.cells.get_mut(at) else { return };
    cell.set_language_override(lang.as_ref(), group.as_deref());
    nb.dirty = true;
}

pub fn clear_outputs(state: &mut AppState, idx: usize, only: Option<usize>) {
    let Some(nb) = state.tabs.get_mut(idx).and_then(|t| t.notebook.as_deref_mut()) else { return };
    for (i, cs) in nb.cells.iter_mut().enumerate() {
        if only.map(|o| o != i).unwrap_or(false) {
            continue;
        }
        if cs.run.as_ref().map(|r| r.is_live()).unwrap_or(false) {
            continue;
        }
        cs.run = None;
        cs.extra_outputs.clear();
        cs.cached = false;
        nb.nb.cells[i].outputs.clear();
        nb.nb.cells[i].execution_count = None;
    }
    nb.dirty = true;
}

// ---------------------------------------------------------------------------------------------
// Running
// ---------------------------------------------------------------------------------------------

/// Queue cells (by index, in order) and start the first one.
pub fn run_cells(state: &mut AppState, cx: &Ctx, idx: usize, cells: Vec<usize>) {
    let Some(t) = state.tabs.get_mut(idx) else { return };
    let Some(nb) = t.notebook.as_deref_mut() else { return };
    for i in cells {
        if let Some(c) = nb.nb.cells.get(i) {
            if c.kind != CellKind::Code {
                continue;
            }
            let id = c.id.clone();
            if !nb.is_queued(&id) {
                nb.queue.push_back(QueuedCell { id, text: None });
            }
        }
    }
    let tab = t.id;
    pump(state, cx, tab, false);
}

/// Mark a cell as the notebook's parameters cell (Fabric's `parameters` tag), or unmark it;
/// one cell carries the tag at a time.
pub fn set_parameters(state: &mut AppState, idx: usize, cell: usize, on: bool) {
    let Some(nb) = state.tabs.get_mut(idx).and_then(|t| t.notebook.as_deref_mut()) else { return };
    if cell >= nb.nb.cells.len() {
        return;
    }
    for (i, c) in nb.nb.cells.iter_mut().enumerate() {
        c.set_parameters(on && i == cell);
    }
    nb.dirty = true;
}

/// The parameters cell and its assignments, for the Run with parameters dialog.
pub fn parameters_of(state: &AppState, idx: usize) -> Option<(usize, Vec<(String, String)>)> {
    let nb = state.tabs.get(idx)?.notebook.as_deref()?;
    let i = nb.nb.parameters_cell()?;
    Some((i, nb.nb.cells[i].assignments()))
}

/// Run every code cell with the parameters cell's assignments overridden: the overrides are
/// appended to that cell's text for this run only (what Fabric's "Run with parameters" and
/// `notebookutils.notebook.run(path, args)` do), the source stays as written.
pub fn run_with_parameters(state: &mut AppState, cx: &Ctx, idx: usize, params: Vec<(String, String)>) {
    let Some(t) = state.tabs.get_mut(idx) else { return };
    let Some(nb) = t.notebook.as_deref_mut() else { return };
    let Some(pi) = nb.nb.parameters_cell() else {
        cx.toast(ToastKind::Warning, "No parameters cell: mark one from a cell's run menu (Parameters cell).");
        return;
    };
    if nb.kernel != NotebookKernel::Spark || nb.language_of(pi) != CellLanguage::Python {
        cx.toast(ToastKind::Warning, "Run with parameters needs a Python parameters cell on the Spark kernel.");
        return;
    }
    let current = nb.nb.cells[pi].assignments();
    let overrides: Vec<String> = params.iter().filter(|(n, v)| !v.trim().is_empty() && current.iter().find(|(cn, _)| cn == n).map(|(_, cv)| cv != v.trim()).unwrap_or(true)).map(|(n, v)| format!("{n} = {}", v.trim())).collect();
    let injected = if overrides.is_empty() { None } else { Some(format!("{}\n\n# Cobalt: run with parameters\n{}\n", nb.nb.cells[pi].source.trim_end(), overrides.join("\n"))) };
    for i in 0..nb.nb.cells.len() {
        let c = &nb.nb.cells[i];
        if c.kind != CellKind::Code {
            continue;
        }
        let id = c.id.clone();
        if !nb.is_queued(&id) {
            nb.queue.push_back(QueuedCell { id, text: if i == pi { injected.clone() } else { None } });
        }
    }
    let tab = t.id;
    if !overrides.is_empty() {
        cx.toast(ToastKind::Info, format!("Running with {} parameter{} overridden", overrides.len(), if overrides.len() == 1 { "" } else { "s" }));
    }
    pump(state, cx, tab, false);
}

/// A cell's reported `%pip install` packages go into the runtime's Python packages and the
/// Libraries install starts (every installed engine environment gets them).
pub fn add_pip_packages(state: &mut AppState, cx: &Ctx, idx: usize, cell: usize) {
    let Some(cs) = state.tabs.get(idx).and_then(|t| t.notebook.as_deref()).and_then(|nb| nb.cells.get(cell)) else { return };
    if cs.pip_packages.is_empty() {
        return;
    }
    let specs = cs.pip_packages.clone();
    cx.toast(ToastKind::Info, format!("Adding {} to the runtime's Python packages and installing them (Settings › Notebooks & Spark › Libraries shows progress)", specs.join(", ")));
    state.settings_patch.push(SettingsPatch::AddPythonPackages(specs));
    state.install_libraries_requested = true;
}

/// Run only the selected text of a cell (Ctrl+Shift+Enter); the whole cell when nothing is
/// selected. The cell's source is left alone; its outputs show the selection's result.
pub fn run_selection(state: &mut AppState, cx: &Ctx, idx: usize, cell: usize) {
    let Some(t) = state.tabs.get_mut(idx) else { return };
    let Some(nb) = t.notebook.as_deref_mut() else { return };
    let Some(cs) = nb.cells.get(cell) else { return };
    if nb.nb.cells[cell].kind != CellKind::Code {
        return;
    }
    let text = &nb.nb.cells[cell].source;
    let sels: Vec<_> = cs.editor.cursors.sels.iter().filter(|s| !s.is_empty()).collect();
    let selected: Option<String> = if sels.is_empty() {
        None
    } else {
        let parts: Vec<String> = sels.iter().map(|s| text.chars().skip(s.min()).take(s.max() - s.min()).collect()).collect();
        Some(parts.join("\n"))
    };
    let id = cs.id.clone();
    if !nb.is_queued(&id) {
        nb.queue.push_back(QueuedCell { id, text: selected });
    }
    let tab = t.id;
    pump(state, cx, tab, false);
}

/// Cancel a queued (not yet started) cell: it goes back to inert.
pub fn dequeue(state: &mut AppState, idx: usize, cell: usize) {
    if let Some(nb) = state.tabs.get_mut(idx).and_then(|t| t.notebook.as_deref_mut()) {
        if let Some(cs) = nb.cells.get(cell) {
            let id = cs.id.clone();
            nb.dequeue(&id);
        }
    }
}

/// Start the next queued cell when nothing is running. After a failure the queue is dropped.
pub fn pump(state: &mut AppState, cx: &Ctx, tab: TabId, failed: bool) {
    let Some(idx) = state.tab_index(tab) else { return };
    let t = &mut state.tabs[idx];
    let Some(nb) = t.notebook.as_deref_mut() else { return };
    if failed {
        nb.queue.clear();
        return;
    }
    if nb.running_cell().is_some() {
        return;
    }
    // skip ids that no longer exist or are not code cells
    let (cell_idx, override_text) = loop {
        let Some(q) = nb.queue.front().cloned() else { return };
        match nb.cell_index(&q.id) {
            Some(i) if nb.nb.cells[i].kind == CellKind::Code => break (i, q.text),
            _ => {
                nb.queue.pop_front();
            }
        }
    };
    if !t.conn.is_connected() && nb.kernel != NotebookKernel::Spark {
        t.pending_run = Some(RunMode::All);
        match t.profile.clone() {
            Some(p) => {
                let db = t.reconnect_database();
                ops::begin_connect(state, cx, p, ConnectPurpose::Tab { tab, database: db });
            }
            None => {
                if let Some(nb) = state.tabs[idx].notebook.as_deref_mut() {
                    nb.queue.clear();
                }
                state.dialog = Dialog::ChangeConnection { tab_index: idx };
            }
        }
        return;
    }
    let lang = nb.language_of(cell_idx);
    if nb.kernel == NotebookKernel::Spark {
        return pump_spark(state, cx, idx, cell_idx, override_text);
    }
    if lang != CellLanguage::Sql {
        nb.queue.pop_front();
        cx.toast(ToastKind::Warning, format!("{} cells need the Local Spark kernel: pick it from the kernel button on the notebook toolbar.", lang.label()));
        let tab = t.id;
        return pump(state, cx, tab, false);
    }
    let (_, body) = nb.nb.cells[cell_idx].split_magic();
    let script = override_text.clone().unwrap_or_else(|| body.to_string());
    nb.queue.pop_front();
    if script.trim().is_empty() {
        let tab = t.id;
        return pump(state, cx, tab, false);
    }
    if t.profile.as_ref().map(|p| p.read_only_guard).unwrap_or(false) {
        if let Some(stmt) = ops::read_only_violation(&script) {
            nb.queue.clear();
            cx.toast(ToastKind::Warning, format!("Read-only connection: cell {} would run `{stmt}`. Switch the connection to run it.", cell_idx + 1));
            return;
        }
    }
    let mut opts = t.exec.clone().with_defaults(&cx.settings.execution.session);
    opts.row_cap = cx.settings.execution.row_cap;
    if opts.timeout_secs == 0 {
        opts.timeout_secs = cx.settings.execution.command_timeout_secs;
    }
    opts.plan = PlanMode::None;
    let run_id = cx.session.new_run();
    let mut view = RunView::new(run_id);
    view.script_hash = hash_text(&script);
    if cx.settings.history.capture {
        let mut e = NewHistoryEntry::new(t.profile.as_ref().map(|p| p.display_name()).unwrap_or_default(), script.clone());
        e.profile_id = t.profile.as_ref().map(|p| p.id);
        e.database = t.conn.database().map(str::to_string);
        e.tab_id = Some(t.id);
        view.history_id = cx.store.add_history(&e).ok();
    }
    nb.counter += 1;
    nb.nb.cells[cell_idx].execution_count = Some(nb.counter);
    nb.nb.cells[cell_idx].outputs.clear();
    let cs = &mut nb.cells[cell_idx];
    cs.run = Some(view);
    cs.cached = false;
    cs.extra_outputs.clear();
    cs.outputs_collapsed = false;
    nb.dirty = true;
    t.pending_run = None;
    cx.session.send(Command::Run { tab, run: run_id, script, opts, start_line: 1, sink: None });
    state.history.loaded = false;
}

pub fn cancel(state: &mut AppState, cx: &Ctx, idx: usize) {
    let Some(t) = state.tabs.get_mut(idx) else { return };
    let Some(nb) = t.notebook.as_deref_mut() else { return };
    nb.queue.clear();
    let spark = nb.kernel == NotebookKernel::Spark;
    if let Some(i) = nb.running_cell() {
        if let Some(r) = nb.cells[i].run.as_mut() {
            r.state = RunViewState::Cancelling;
        }
        if spark {
            match crate::kernel::interrupt(&mut state.kernel) {
                crate::kernel::InterruptAction::Sent => cx.toast(ToastKind::Info, "Interrupting the cell — Spark jobs are being cancelled. Stop again to end the session."),
                crate::kernel::InterruptAction::Killed => cx.toast(ToastKind::Info, "Stopping the Spark session; the next cell starts a new one."),
                crate::kernel::InterruptAction::Nothing => {}
            }
        } else {
            cx.session.send(Command::Cancel { tab: t.id });
        }
    } else if spark && state.kernel.state.is_starting() {
        crate::kernel::interrupt(&mut state.kernel);
    }
}

/// Switch a notebook between the tab's connection and the local Spark kernel.
pub fn set_kernel(state: &mut AppState, idx: usize, kernel: NotebookKernel) {
    if let Some(nb) = state.tabs.get_mut(idx).and_then(|t| t.notebook.as_deref_mut()) {
        if nb.is_running() {
            return;
        }
        nb.kernel = kernel;
    }
}

/// A Python string literal for `code`.
pub(crate) fn py_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Run the next queued cell on the local Spark kernel (starting it when needed).
/// Bring the local Spark session up for notebook `idx` when it is not running: the notebook's
/// lakehouse binding is resolved, a Fabric-bound session fetches its OneLake token first (the
/// Fabric event then starts the kernel), a plain session starts at once. `Ok(true)` = the
/// session is up or starting now; `Ok(false)` = something is pending (token, binding prompt)
/// and the caller should wait; `Err(())` = refused (runtime not installed, binding unresolved)
/// and the notebook's queue was cleared.
pub fn ensure_session(state: &mut AppState, cx: &Ctx, idx: usize) -> Result<bool, ()> {
    use crate::kernel::{self, KernelState, StartError};
    match &state.kernel.state {
        KernelState::Stopped | KernelState::Failed(_) => {}
        _ => return Ok(true),
    }
    if state.kernel.pending_fabric_start.is_some() {
        return Ok(false); // the OneLake token is on its way; cells stay queued
    }
    let fabric = match kernel_fabric(state, cx, idx) {
        Ok(f) => f,
        Err(true) => return Ok(false),
        Err(false) => {
            abandon_pending(state, idx);
            return Err(());
        }
    };
    if let Some(f) = fabric {
        // a OneLake token first (may need the browser); the Fabric event starts the kernel
        let slot = f.slot;
        state.kernel.pending_fabric_start = Some(f);
        crate::fabric::prepare_onelake(state, cx, slot);
        return Ok(false);
    }
    if let Err(StartError::NotProvisioned) = kernel::start(&mut state.kernel, cx.settings, cx.paths, cx.egui, None) {
        abandon_pending(state, idx);
        cx.toast(ToastKind::Warning, crate::kernel::not_installed_text(cx.settings));
        state.settings_open = true;
        state.settings_scroll_to = Some("Spark runtime");
        return Err(());
    }
    Ok(true)
}

/// Switch the engine the next session runs on (Spark menu, kernel picker): remembered in the
/// settings; a running session is restarted on the new engine.
pub fn set_engine(state: &mut AppState, cx: &Ctx, engine: &str) {
    let e = cobalt_runtime::Engine::parse(engine);
    if crate::runtime::engine(cx.settings) == e && !state.settings_patch.iter().any(|p| matches!(p, crate::state::SettingsPatch::SparkEngine(_))) {
        return;
    }
    state.settings_patch.push(crate::state::SettingsPatch::SparkEngine(e.key().to_string()));
    let running = state.kernel.state.is_ready() || state.kernel.state.is_starting();
    if running {
        state.kernel_restart_pending = true;
        crate::kernel::stop(&mut state.kernel);
        cx.toast(ToastKind::Info, format!("Restarting the session on {}. Variables and temp views from before are gone.", e.label()));
    } else {
        cx.toast(ToastKind::Info, format!("The next Spark session runs on {}.", e.label()));
    }
}

/// Reconcile the tab's binding with the running (or starting) session before a Spark run:
/// start the session, attach a workspace, warn about a binding the session cannot take, and
/// name the context's default lakehouse. Shared by notebook cells and Spark SQL query tabs.
pub(crate) fn prepare_spark(state: &mut AppState, cx: &Ctx, idx: usize) -> SparkPrep {
    use crate::kernel::KernelState;
    let tab = state.tabs[idx].id;
    let mut pending_register: Vec<serde_json::Value> = Vec::new();
    let mut context_lakehouse: Option<String> = None;
    // the kernel must be up or starting
    match &state.kernel.state {
        KernelState::Stopped | KernelState::Failed(_) => {
            if !matches!(ensure_session(state, cx, idx), Ok(true)) {
                return SparkPrep::Wait;
            }
        }
        KernelState::Ready { .. } => {
            // the notebook's binding against the running session's: the default lakehouse is
            // the notebook's own context (0.5.0), a new workspace is attached (0.4.3); only a
            // different write mode, or a lakehouse on a session started without one, needs a
            // restart — said once per notebook
            match kernel_fabric(state, cx, idx) {
                Ok(want) => {
                    let have = state.kernel.fabric.clone();
                    match (want, have) {
                        (None, _) => {}
                        (Some(_), None) => {
                            // the rebind happens once per tab: a session that comes back unbound
                            // must not loop; after that the run goes on as it is, with a warning
                            let first = state.kernel.binding_warned.insert(tab);
                            if first && rebind_if_unused(state) {
                                cx.toast(ToastKind::Info, "Rebinding the Spark session to this tab's lakehouse.");
                                return SparkPrep::Wait; // the request stays pending and runs on the rebound session
                            }
                            cx.toast(ToastKind::Warning, "The running Spark session has no lakehouse; this run uses it as it is. Restart the session (session menu) to bind this tab's lakehouse.");
                        }
                        (Some(w), Some(h)) => {
                            if w.write_mode != h.write_mode {
                                if state.kernel.binding_warned.insert(tab) {
                                    cx.toast(ToastKind::Warning, format!("The running Spark session is in {} mode; this notebook asks for {}. Restart the session (kernel menu) to switch.", h.write_mode, w.write_mode));
                                }
                            } else if !h.knows_workspace(&w.workspace_id) {
                                if state.kernel.has("register_lakehouse") {
                                    let mut fresh: Vec<(String, String)> = Vec::new();
                                    let mut clashes: Vec<String> = Vec::new();
                                    for (name, id) in &w.lakehouses {
                                        match h.lakehouses.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)) {
                                            Some((_, have_id)) if have_id == id => {}
                                            Some(_) => clashes.push(name.clone()),
                                            None => fresh.push((name.clone(), id.clone())),
                                        }
                                    }
                                    if !clashes.is_empty() {
                                        cx.toast(ToastKind::Warning, format!("Lakehouse name{} already attached from another workspace: {} — reach {} by path or restart the session.", if clashes.len() == 1 { "" } else { "s" }, clashes.join(", "), if clashes.len() == 1 { "it" } else { "them" }));
                                    }
                                    pending_register = fresh.iter().map(|(name, id)| serde_json::json!({"name": name, "id": id, "workspace_id": w.workspace_id})).collect();
                                    if let Some(f) = state.kernel.fabric.as_mut() {
                                        f.lakehouses.extend(fresh);
                                        f.extra_workspaces.push((w.workspace_id.clone(), w.workspace_name.clone()));
                                    }
                                    cx.toast(ToastKind::Info, format!("Attaching workspace {} to the running Spark session.", if w.workspace_name.is_empty() { w.workspace_id.clone() } else { w.workspace_name.clone() }));
                                } else if state.kernel.binding_warned.insert(tab) {
                                    cx.toast(ToastKind::Warning, format!("The running Spark session is bound to {}. Restart the session (kernel menu) to use this notebook's workspace (local-spark-mcp 0.4.3 attaches it without a restart).", h.label()));
                                }
                            }
                            context_lakehouse = w.default_lakehouse.clone();
                        }
                    }
                }
                Err(true) => return SparkPrep::Wait, // the workspace's lakehouses are still loading; the request stays pending
                Err(false) => return SparkPrep::Abort,
            }
        }
        _ => {}
    }
    // while the session is still starting the Ready arm did not run: the context's default
    // lakehouse still comes from the notebook's binding (resolved already for the start)
    if context_lakehouse.is_none() && binding_of(&state.tabs[idx]).is_some() {
        if let Ok(Some(w)) = kernel_fabric(state, cx, idx) {
            context_lakehouse = w.default_lakehouse;
        }
    }
    SparkPrep::Ready { context_lakehouse, register: pending_register }
}

fn pump_spark(state: &mut AppState, cx: &Ctx, idx: usize, cell_idx: usize, override_text: Option<String>) {
    use crate::kernel::{self, RunReq};
    let tab = state.tabs[idx].id;
    let (context_lakehouse, pending_register) = match prepare_spark(state, cx, idx) {
        SparkPrep::Ready { context_lakehouse, register } => (context_lakehouse, register),
        SparkPrep::Wait => return,
        SparkPrep::Abort => {
            abandon_pending(state, idx);
            return;
        }
    };
    let use_context = state.kernel.has("contexts");
    let want_description = state.kernel.has("job_description");
    let t = &mut state.tabs[idx];
    let nb = t.notebook.as_deref_mut().unwrap();
    let lang = nb.language_of(cell_idx);
    let cell = &nb.nb.cells[cell_idx];
    let (magic, body) = cell.split_magic();
    let limit = cx.settings.notebooks.spark_row_limit.max(1);
    let code = match lang {
        CellLanguage::Sql => format!("__cobalt_sql({}, {limit})", py_literal(override_text.as_deref().unwrap_or(body))),
        CellLanguage::Python => match (&override_text, magic.as_deref()) {
            (Some(t), _) => t.clone(),
            (None, Some("pyspark")) | (None, Some("python")) => body.to_string(),
            (None, _) => cell.source.clone(),
        },
        other => {
            nb.queue.pop_front();
            cx.toast(ToastKind::Warning, format!("{} cells are not supported on the local Spark kernel (Python and SQL are).", other.label()));
            return pump(state, cx, tab, false);
        }
    };
    nb.queue.pop_front();
    // `%pip install` lines are reported, not run in the session: the packages belong to the
    // runtime environment (Add to runtime under the cell puts them there)
    let (code, pip_packages) = if lang == CellLanguage::Python { cobalt_notebook::strip_pip_lines(&code) } else { (code, Vec::new()) };
    let code = if code.trim().is_empty() && !pip_packages.is_empty() { "pass".to_string() } else { code };
    if code.trim().is_empty() {
        return pump(state, cx, tab, false);
    }
    let run_id = cx.session.new_run();
    let mut view = RunView::new(run_id);
    if !pip_packages.is_empty() {
        view.messages.push(msg(format!("pip: {} — not installed from the notebook here; Add to runtime (under this cell) puts the package{} in the runtime's Python packages and installs {}.", pip_packages.join(", "), if pip_packages.len() == 1 { "" } else { "s" }, if pip_packages.len() == 1 { "it" } else { "them" }), false));
    }
    view.script_hash = hash_text(&code);
    if cx.settings.history.capture {
        let mut e = NewHistoryEntry::new(crate::kernel::history_source(&cx.settings.spark), cell.source.clone());
        e.tab_id = Some(t.id);
        view.history_id = cx.store.add_history(&e).ok();
    }
    nb.counter += 1;
    nb.nb.cells[cell_idx].execution_count = Some(nb.counter);
    nb.nb.cells[cell_idx].outputs.clear();
    let cs = &mut nb.cells[cell_idx];
    cs.run = Some(view);
    cs.cached = false;
    cs.extra_outputs.clear();
    cs.outputs_collapsed = false;
    cs.pip_packages = pip_packages;
    let cell_id = cs.id.clone();
    nb.dirty = true;
    let job_description = if want_description { code.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with("%%")).map(|l| l.chars().take(80).collect::<String>()) } else { None };
    // a context per notebook when the worker has them; before the session is up the features
    // are unknown, so the request carries the context and the thread decides
    let context = if use_context || !state.kernel.state.is_ready() { Some(kernel::context_id(tab)) } else { None };
    let context_name = Some(state.tabs[idx].title.trim_end_matches(".ipynb").to_string());
    kernel::run(&mut state.kernel, RunReq { tab, cell_id, code, sql: None, context, context_lakehouse, context_name, job_description, register: pending_register });
    state.history.loaded = false;
}

/// Each frame: finished Spark cells become outputs; a kernel that just came up gets the queued
/// cells; a kernel that died fails the cells still marked running.
pub fn poll_kernel(state: &mut AppState, cx: &Ctx) {
    crate::sparkq::tick(state, cx);
    let out = crate::kernel::poll(&mut state.kernel);
    // a session that starts with a preload: ask for its progress once so the window has it
    if out.ready_now && state.kernel.fabric.as_ref().map(|f| !f.preload.is_null()).unwrap_or(false) {
        shadows_action(state, "preload_status", serde_json::json!({}));
    }
    // "the tables I used last time": the persisted clones not yet registered become a preload
    if out.ready_now && state.kernel.fabric.as_ref().map(|f| !f.preload_last.is_empty()).unwrap_or(false) {
        crate::kernel::call(&mut state.kernel, "preload-last", "shadow_status", serde_json::json!({}));
    }
    lifecycle_tick(state, cx);
    for (tag, result) in out.calls {
        match tag.as_str() {
            "preload-last" => {
                if let Ok(v) = result {
                    let wanted: Vec<String> = state.kernel.fabric.as_ref().map(|f| f.preload_last.clone()).unwrap_or_default();
                    let mut per: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
                    for t in v.get("tables").and_then(|t| t.as_array()).cloned().unwrap_or_default() {
                        if t.get("registered").and_then(|r| r.as_bool()) != Some(false) {
                            continue;
                        }
                        let (Some(lh), Some(table)) = (t.get("lakehouse").and_then(|s| s.as_str()), t.get("table").and_then(|s| s.as_str())) else { continue };
                        // `lh__schema` + `table` is the schema form `schema/table` of lakehouse `lh`
                        let (name, entry) = match lh.split_once("__") {
                            Some((base, schema)) => (base.to_string(), format!("{schema}/{table}")),
                            None => (lh.to_string(), table.to_string()),
                        };
                        if !wanted.iter().any(|w| w.eq_ignore_ascii_case(&name)) {
                            continue;
                        }
                        if let Some(a) = per.entry(name).or_insert_with(|| serde_json::Value::Array(Vec::new())).as_array_mut() { a.push(serde_json::Value::String(entry)) }
                    }
                    if !per.is_empty() {
                        let n: usize = per.values().filter_map(|a| a.as_array()).map(|a| a.len()).sum();
                        state.kernel.log.push_back(format!("cobalt: preloading {n} table{} used in earlier sessions", if n == 1 { "" } else { "s" }));
                        shadows_action(state, "preload", serde_json::json!({"lakehouses": serde_json::Value::Object(per)}));
                    }
                }
            }
            "shadows" => {
                state.shadows.loading = false;
                match result {
                    Ok(v) => state.shadows.status = Some(v),
                    Err(e) => state.shadows.error = Some(e),
                }
            }
            "pane-mirror" => {
                state.lakehouse_pane.mirror_pending = false;
                match result {
                    Ok(v) => {
                        let name = state.lakehouse_pane.selected.as_ref().map(|(_, n, _)| n.clone()).unwrap_or_default();
                        state.lakehouse_pane.mirror = Some(v.get("lakehouses").and_then(|l| l.get(&name)).cloned().unwrap_or(serde_json::json!({})));
                    }
                    Err(e) => state.lakehouse_pane.note = Some(e),
                }
            }
            "pane-action" => {
                state.lakehouse_pane.note = Some(match &result {
                    Ok(v) if v.get("transferred").is_some() => format!("Pulled {} file{} ({} skipped, {} bytes)", v.get("transferred").and_then(|x| x.as_u64()).unwrap_or(0), if v.get("transferred").and_then(|x| x.as_u64()) == Some(1) { "" } else { "s" }, v.get("skipped").and_then(|x| x.as_u64()).unwrap_or(0), v.get("bytes").and_then(|x| x.as_u64()).unwrap_or(0)),
                    Ok(v) if v.get("removed").is_some() => format!("Removed {} local path{}", v.get("removed").and_then(|x| x.as_array()).map(|a| a.len()).unwrap_or(0), if v.get("removed").and_then(|x| x.as_array()).map(|a| a.len()) == Some(1) { "" } else { "s" }),
                    Ok(v) if v.get("table").is_some() => format!("Cloned {}", v.get("table").and_then(|x| x.as_str()).unwrap_or("")),
                    Ok(v) if v.get("state").is_some() => "Cloning in the background (Lakehouse shadows shows progress)…".into(),
                    Ok(_) => "Done".into(),
                    Err(e) => e.clone(),
                });
                // the mirror and the shadows changed
                state.lakehouse_pane.mirror = None;
                state.lakehouse_pane.mirror_pending = false;
                state.shadows.status = None;
            }
            "agent-call" => {
                state.kernel.last_call = Some(match result {
                    Ok(v) => serde_json::json!({"ok": true, "result": v}),
                    Err(e) => serde_json::json!({"ok": false, "error": e}),
                });
            }
            "shadows-action" => {
                state.shadows.loading = false;
                match result {
                    Ok(v) if v.get("tables_total").is_some() || v.get("lakehouses").map(|l| l.is_object()).unwrap_or(false) => {
                        // a preload_status reply
                        let was_running = state.shadows.preload.as_ref().map(|p| p.get("state").and_then(|s| s.as_str()) == Some("running")).unwrap_or(false);
                        let now_done = v.get("state").and_then(|s| s.as_str()) != Some("running");
                        state.shadows.preload = Some(v);
                        if was_running && now_done {
                            refresh_shadows(state);
                        }
                    }
                    Ok(v) => {
                        state.shadows.note = Some(v.to_string());
                        refresh_shadows(state);
                    }
                    Err(e) => state.shadows.error = Some(e),
                }
            }
            _ => {}
        }
    }
    if out.broke.is_some() {
        state.shadows.status = None;
        state.shadows.preload = None;
        state.shadows.loading = false;
    }
    // query tabs: streamed batches and finished statements
    for (tab, _cell_id, statement, bytes) in out.sql_batches {
        if let Some(idx) = state.tab_index(tab) {
            crate::sparkq::on_sql_batch(state, idx, statement, &bytes);
        }
    }
    for (tab, _cell_id, statement, result) in out.sql_statements {
        if let Some(idx) = state.tab_index(tab) {
            crate::sparkq::on_sql_statement(state, idx, statement, &result);
        }
    }
    // streamed output lands on the running cell as it is produced; the final reply replaces it
    for (tab, cell_id, stream, text) in out.outputs {
        let Some(idx) = state.tab_index(tab) else { continue };
        if state.tabs[idx].spark.is_some() {
            crate::sparkq::on_output(state, tab, &stream, &text);
            continue;
        }
        let Some(nb) = state.tabs[idx].notebook.as_deref_mut() else { continue };
        let Some(ci) = nb.cell_index(&cell_id) else { continue };
        let Some(run) = nb.cells[ci].run.as_mut() else { continue };
        if !run.is_live() {
            continue;
        }
        for line in text.lines() {
            let t = line.trim_end();
            if t.contains(crate::kernel::ARROW_MARK) {
                continue;
            }
            if stream == "stderr" && (t.is_empty() || t.contains(" WARN ") || t.contains(" INFO ") || t.starts_with('[') && t.contains("Stage ")) {
                continue;
            }
            run.messages.push(msg(t.to_string(), false));
        }
    }
    let mut followups = Vec::new();
    let mut pumps: Vec<(TabId, bool)> = Vec::new();
    for (req, result, blobs) in out.done {
        let Some(idx) = state.tab_index(req.tab) else { continue };
        if state.tabs[idx].spark.is_some() {
            followups.extend(crate::sparkq::on_done(state, idx, &result, &blobs));
            continue;
        }
        let Some(nb) = state.tabs[idx].notebook.as_deref_mut() else { continue };
        let Some(ci) = nb.cell_index(&req.cell_id) else { continue };
        let o = crate::kernel::outcome(&result, &blobs);
        let is_sql = nb.language_of(ci) == CellLanguage::Sql;
        let cs = &mut nb.cells[ci];
        let Some(run) = cs.run.as_mut() else { continue };
        let interrupted = o.interrupted || matches!(&result, Err(e) if e.starts_with("interrupted"));
        run.elapsed = run.started.elapsed();
        // the reply carries the complete stdout: the streamed lines are replaced
        run.messages.clear();
        for rs in o.result_sets {
            run.result_sets.push(ResultSetView { rs, grid: GridState::default(), is_plan: false, profile: None });
        }
        run.total_rows = run.result_sets.iter().map(|s| s.rs.row_count() as u64).sum();
        run.messages.extend(o.messages);
        if o.failed && is_sql {
            crate::kernel::compact_sql_error(&mut run.messages);
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
        followups.push(Followup::FinishHistory { history_id: run.history_id, elapsed: run.elapsed, rows: run.total_rows, cancelled: interrupted, failed: o.failed, error: run.messages.iter().find(|m| m.is_error).map(|m| m.text.clone()) });
        pumps.push((req.tab, o.failed || interrupted));
    }
    if let Some(err) = &out.broke {
        crate::sparkq::fail_live(state, err);
        // an intentional restart keeps the queued cells: they run on the new session
        let keep_queues = state.kernel_restart_pending;
        for t in state.tabs.iter_mut() {
            if let Some(nb) = t.notebook.as_deref_mut() {
                if nb.kernel != NotebookKernel::Spark {
                    continue;
                }
                if !keep_queues {
                    nb.queue.clear();
                }
                for cs in nb.cells.iter_mut() {
                    if let Some(r) = cs.run.as_mut() {
                        if r.is_live() {
                            r.state = RunViewState::Failed;
                            r.elapsed = r.started.elapsed();
                            r.messages.push(msg(err.clone(), true));
                        }
                    }
                }
            }
        }
    }
    ops::handle_followups(state, cx, followups);
    for (tab, failed) in pumps {
        pump(state, cx, tab, failed);
    }
    if out.ready_now {
        let tabs: Vec<TabId> = state.tabs.iter().filter(|t| t.notebook.as_deref().map(|nb| nb.kernel == NotebookKernel::Spark && !nb.queue.is_empty()).unwrap_or(false)).map(|t| t.id).collect();
        for tab in tabs {
            pump(state, cx, tab, false);
        }
        crate::sparkq::pump_pending(state, cx);
    }
}

/// Run a results action against a cell's result sets by lending the cell's run to the tab's
/// `run` slot for the duration (every results op resolves `state.tabs[idx].run`).
pub fn results_action(state: &mut AppState, cx: &Ctx, idx: usize, cell: usize, action: ResultsAction) {
    let Some(t) = state.tabs.get_mut(idx) else { return };
    let Some(nb) = t.notebook.as_deref_mut() else { return };
    let Some(cs) = nb.cells.get_mut(cell) else { return };
    std::mem::swap(&mut cs.run, &mut t.run);
    ops::results_action(state, cx, idx, action);
    if let Some(t) = state.tabs.get_mut(idx) {
        if let Some(nb) = t.notebook.as_deref_mut() {
            if let Some(cs) = nb.cells.get_mut(cell) {
                std::mem::swap(&mut cs.run, &mut t.run);
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Outputs <-> result sets
// ---------------------------------------------------------------------------------------------

fn msg(text: String, is_error: bool) -> MessageLine {
    MessageLine { text, is_error, is_batch_header: false, line: None, path: None }
}

/// Rebuild a cell's cached outputs as a finished run (grids from Arrow / ADS payloads, messages
/// from streams and errors) plus whatever cannot be shown as a grid or a line of text.
pub fn load_outputs(cell: &Cell, _index: usize) -> (Option<RunView>, Vec<Output>) {
    if cell.kind != CellKind::Code || cell.outputs.is_empty() {
        return (None, Vec::new());
    }
    let mut run = RunView::new(cobalt_core::RunId(u64::MAX - _index as u64));
    run.state = RunViewState::Done;
    let mut extra = Vec::new();
    for o in &cell.outputs {
        match o {
            Output::Stream { name, text } => {
                for line in text.lines() {
                    run.messages.push(msg(line.to_string(), name == "stderr"));
                }
            }
            Output::Error { ename, evalue, traceback } => {
                let text = if !traceback.is_empty() {
                    cobalt_notebook::strip_ansi(&traceback.join("\n"))
                } else if ename.is_empty() || ename == "Error" {
                    evalue.clone()
                } else {
                    format!("{ename}: {evalue}")
                };
                run.messages.push(msg(text, true));
                run.state = RunViewState::Failed;
            }
            Output::ExecuteResult { data, .. } | Output::DisplayData { data, .. } => {
                let set_index = run.result_sets.len();
                if let Some(bytes) = data.arrow() {
                    match result_set_from_ipc(&bytes, set_index) {
                        Ok(rs) => {
                            run.result_sets.push(ResultSetView { rs, grid: GridState::default(), is_plan: false, profile: None });
                            continue;
                        }
                        Err(e) => run.messages.push(msg(format!("Could not read the saved result set: {e}"), true)),
                    }
                } else if let Some(dr) = data.json(DATARESOURCE_MIME) {
                    if let Some(rs) = result_set_from_dataresource(dr, set_index) {
                        run.result_sets.push(ResultSetView { rs, grid: GridState::default(), is_plan: false, profile: None });
                        continue;
                    }
                }
                if data.entries.keys().any(|k| k == ARROW_MIME || k == DATARESOURCE_MIME) {
                    continue;
                }
                extra.push(o.clone());
            }
        }
    }
    run.total_rows = run.result_sets.iter().map(|s| s.rs.row_count() as u64).sum();
    (Some(run), extra)
}

/// An Arrow IPC stream from the worker: the columns (SQL types from the stream's metadata when
/// the worker wrote them, else suggested from the Arrow types) and its batches.
pub(crate) fn ipc_columns_batches(bytes: &[u8]) -> Result<(Vec<ColumnInfo>, Vec<RecordBatch>), String> {
    let reader = arrow::ipc::reader::StreamReader::try_new(std::io::Cursor::new(bytes), None).map_err(|e| e.to_string())?;
    let schema = reader.schema();
    let batches: Vec<RecordBatch> = reader.collect::<Result<_, _>>().map_err(|e| e.to_string())?;
    let types: Vec<String> = schema.metadata().get(SQL_TYPES_KEY).and_then(|s| serde_json::from_str(s).ok()).unwrap_or_default();
    let columns: Vec<ColumnInfo> = schema
        .fields()
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let st = types.get(i).and_then(|t| cobalt_import::parse_sql_type(t)).unwrap_or_else(|| cobalt_import::suggest_sql_type(f.data_type(), 0));
            ColumnInfo::new(f.name().clone(), st, f.is_nullable(), i)
        })
        .collect();
    Ok((columns, batches))
}

/// Append a worker batch to a result set whose schema came from the SQL types: columns are cast
/// to the set's Arrow types when they differ. Returns the batch as appended (for an export sink).
/// A column as the result set's schema wants it. Arrow's `cast` covers the scalar changes
/// (Spark's timestamps and decimals); a nested column (struct, list, map) has no cast to text,
/// so each value is rendered the way Arrow prints it (`{costCenter: 100, division: Retail}`).
pub(crate) fn cast_for_grid(c: &ArrayRef, target: &arrow::datatypes::DataType) -> Result<ArrayRef, String> {
    use arrow::datatypes::DataType as D;
    if c.data_type() == target {
        return Ok(c.clone());
    }
    let nested = matches!(c.data_type(), D::Struct(_) | D::List(_) | D::LargeList(_) | D::FixedSizeList(..) | D::Map(..) | D::ListView(_) | D::LargeListView(_) | D::Union(..));
    if nested && matches!(target, D::Utf8 | D::LargeUtf8) {
        let fmt = arrow::util::display::ArrayFormatter::try_new(c.as_ref(), &arrow::util::display::FormatOptions::default().with_null("")).map_err(|e| e.to_string())?;
        let values: Vec<Option<String>> = (0..c.len()).map(|i| if c.is_null(i) { None } else { Some(fmt.value(i).to_string()) }).collect();
        let arr: ArrayRef = if matches!(target, D::Utf8) { Arc::new(arrow::array::StringArray::from(values)) } else { Arc::new(arrow::array::LargeStringArray::from(values)) };
        return Ok(arr);
    }
    arrow::compute::cast(c, target).map_err(|e| e.to_string())
}

pub(crate) fn append_cast(rs: &ResultSet, b: RecordBatch) -> Result<RecordBatch, String> {
    let cols: Vec<ArrayRef> = b.columns().iter().zip(rs.schema.fields()).map(|(c, f)| cast_for_grid(c, f.data_type())).collect::<Result<_, _>>()?;
    let b = RecordBatch::try_new(rs.schema.clone(), cols).map_err(|e| e.to_string())?;
    rs.append(b.clone()).map_err(|e| e.to_string())?;
    Ok(b)
}

/// Like `append_cast`, but the result set keeps only its first `preview` rows (Run to File:
/// the rows go to the file; the grid shows a sample). The cast batch comes back whole.
pub(crate) fn append_preview(rs: &ResultSet, b: RecordBatch, preview: usize) -> Result<RecordBatch, String> {
    let cols: Vec<ArrayRef> = b.columns().iter().zip(rs.schema.fields()).map(|(c, f)| cast_for_grid(c, f.data_type())).collect::<Result<_, _>>()?;
    let b = RecordBatch::try_new(rs.schema.clone(), cols).map_err(|e| e.to_string())?;
    let have = rs.row_count();
    if have < preview {
        let take = (preview - have).min(b.num_rows());
        rs.append(b.slice(0, take)).map_err(|e| e.to_string())?;
    }
    Ok(b)
}

pub(crate) fn result_set_from_ipc(bytes: &[u8], index: usize) -> Result<Arc<ResultSet>, String> {
    let (columns, batches) = ipc_columns_batches(bytes)?;
    // the result set derives its own schema from the SQL types; cast each batch to it
    let rs = ResultSet::new(index, columns, Arc::new(cobalt_results::MemoryBudget::unlimited()), std::env::temp_dir());
    for b in batches {
        append_cast(&rs, b)?;
    }
    rs.set_state(cobalt_results::RunState::Complete);
    Ok(rs)
}

/// Azure Data Studio's `application/vnd.dataresource+json`: every column as text.
fn result_set_from_dataresource(v: &serde_json::Value, index: usize) -> Option<Arc<ResultSet>> {
    let fields: Vec<String> = v.get("schema")?.get("fields")?.as_array()?.iter().filter_map(|f| f.get("name").and_then(|n| n.as_str()).map(str::to_string)).collect();
    let rows = v.get("data")?.as_array()?;
    let columns: Vec<ColumnInfo> = fields.iter().enumerate().map(|(i, n)| ColumnInfo::new(n.clone(), SqlType::NVarChar { len: None }, true, i)).collect();
    let arrays: Vec<ArrayRef> = fields
        .iter()
        .map(|name| {
            let vals: Vec<Option<String>> = rows
                .iter()
                .map(|r| match r.get(name) {
                    None | Some(serde_json::Value::Null) => None,
                    Some(serde_json::Value::String(s)) => Some(s.clone()),
                    Some(other) => Some(other.to_string()),
                })
                .collect();
            Arc::new(StringArray::from(vals)) as ArrayRef
        })
        .collect();
    let schema = Arc::new(Schema::new(fields.iter().map(|n| Field::new(n, arrow::datatypes::DataType::Utf8, true)).collect::<Vec<_>>()));
    let batch = RecordBatch::try_new(schema, arrays).ok()?;
    Some(ResultSet::from_batches(index, columns, vec![batch]))
}

/// A finished run as nbformat outputs: messages as streams/errors, result sets as Cobalt table
/// outputs (Arrow IPC capped at `max_rows`, plus HTML / Markdown / text previews).
pub fn outputs_from_run(run: &RunView, count: Option<u64>, max_rows: u64, fmt: &CellFormatter) -> Vec<Output> {
    let mut out = Vec::new();
    let mut stdout = String::new();
    let flush = |stdout: &mut String, out: &mut Vec<Output>| {
        if !stdout.is_empty() {
            out.push(Output::stdout(std::mem::take(stdout)));
        }
    };
    // consecutive error lines (a traceback) become one error output
    let mut err_lines: Vec<String> = Vec::new();
    let flush_err = |err_lines: &mut Vec<String>, out: &mut Vec<Output>| {
        if err_lines.is_empty() {
            return;
        }
        let last = err_lines.last().cloned().unwrap_or_default();
        let tb = if err_lines.len() > 1 { std::mem::take(err_lines) } else { Vec::new() };
        err_lines.clear();
        out.push(Output::error("Error", last, tb));
    };
    for m in &run.messages {
        if m.is_batch_header {
            continue;
        }
        if m.is_error {
            flush(&mut stdout, &mut out);
            err_lines.push(m.text.clone());
        } else {
            flush_err(&mut err_lines, &mut out);
            stdout.push_str(&m.text);
            stdout.push('\n');
        }
    }
    flush_err(&mut err_lines, &mut out);
    flush(&mut stdout, &mut out);
    for v in run.result_sets.iter().filter(|s| !s.is_plan) {
        match table_output(&v.rs, count, max_rows, fmt) {
            Ok(o) => out.push(o),
            Err(e) => out.push(Output::stderr(format!("(result set not saved: {e})"))),
        }
    }
    out
}

fn table_output(rs: &ResultSet, count: Option<u64>, max_rows: u64, fmt: &CellFormatter) -> Result<Output, String> {
    let total = rs.visible_count() as u64;
    let keep = total.min(max_rows) as usize;
    let batch = rs.view_to_single_batch().map_err(|e| e.to_string())?;
    let batch = batch.slice(0, keep.min(batch.num_rows()));
    let mut md = batch.schema().metadata().clone();
    md.insert(SQL_TYPES_KEY.into(), serde_json::to_string(&rs.columns.iter().map(|c| c.sql_type.to_string()).collect::<Vec<_>>()).unwrap_or_default());
    let schema = Arc::new(Schema::new_with_metadata(batch.schema().fields().clone(), md));
    let batch = RecordBatch::try_new(schema.clone(), batch.columns().to_vec()).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    {
        let mut w = arrow::ipc::writer::StreamWriter::try_new(&mut bytes, &schema).map_err(|e| e.to_string())?;
        w.write(&batch).map_err(|e| e.to_string())?;
        w.finish().map_err(|e| e.to_string())?;
    }
    // previews: the first rows as HTML / Markdown / text
    let preview_rows = keep.min(25);
    let names: Vec<&str> = rs.columns.iter().map(|c| c.name.as_str()).collect();
    let mut html = String::from("<table><thead><tr>");
    for n in &names {
        html.push_str(&format!("<th>{}</th>", html_escape(n)));
    }
    html.push_str("</tr></thead><tbody>");
    let mut mdt = format!("| {} |\n|{}|\n", names.join(" | "), names.iter().map(|_| "---").collect::<Vec<_>>().join("|"));
    let mut plain = names.join("\t");
    plain.push('\n');
    for r in 0..preview_rows {
        html.push_str("<tr>");
        let cells: Vec<String> = (0..rs.column_count()).map(|c| rs.cell_text(r, c, fmt).to_string()).collect();
        for c in &cells {
            html.push_str(&format!("<td>{}</td>", html_escape(c)));
        }
        html.push_str("</tr>");
        mdt.push_str(&format!("| {} |\n", cells.iter().map(|c| c.replace('|', "\\|").replace('\n', " ")).collect::<Vec<_>>().join(" | ")));
        plain.push_str(&cells.join("\t"));
        plain.push('\n');
    }
    html.push_str("</tbody></table>");
    if (preview_rows as u64) < total {
        html.push_str(&format!("<p><i>{} of {} rows</i></p>", preview_rows, total));
        plain.push_str(&format!("… {} of {} rows\n", preview_rows, total));
    }
    Ok(Output::table(count, &bytes, html, mdt, plain, total, (keep as u64) < total))
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::Int32Array;

    #[test]
    fn table_output_roundtrips_through_ipc() {
        let cols = vec![ColumnInfo::new("id", SqlType::Int, false, 0), ColumnInfo::new("name", SqlType::NVarChar { len: Some(50) }, true, 1)];
        let rs = ResultSet::from_batches(0, cols.clone(), vec![]);
        let schema = rs.schema.clone();
        let batch = RecordBatch::try_new(schema, vec![Arc::new(Int32Array::from(vec![1, 2, 3])) as ArrayRef, Arc::new(StringArray::from(vec![Some("a"), None, Some("c")])) as ArrayRef]).unwrap();
        let rs = ResultSet::from_batches(0, cols, vec![batch]);
        let fmt = CellFormatter::default();
        let o = table_output(&rs, Some(1), 2, &fmt).unwrap();
        assert_eq!(o.table_rows(), Some(3));
        let data = o.data().unwrap();
        assert!(data.html().unwrap().contains("<th>id</th>"));
        assert!(data.markdown().unwrap().starts_with("| id | name |"));
        let back = result_set_from_ipc(&data.arrow().unwrap(), 0).unwrap();
        assert_eq!(back.row_count(), 2);
        assert_eq!(back.columns[1].sql_type.to_string(), "nvarchar(50)");
        assert_eq!(back.cell_text(0, 1, &fmt).as_ref(), "a");
    }

    #[test]
    fn ads_dataresource_loads() {
        let v = serde_json::json!({"schema": {"fields": [{"name": "one"}, {"name": "two"}]}, "data": [{"one": "1", "two": "x"}, {"one": "2", "two": null}]});
        let rs = result_set_from_dataresource(&v, 0).unwrap();
        assert_eq!(rs.row_count(), 2);
        assert_eq!(rs.columns.len(), 2);
    }

    #[test]
    fn cached_outputs_become_a_run() {
        let mut c = Cell::code("SELECT 1");
        c.outputs.push(Output::stdout("hello\nworld\n"));
        c.outputs.push(Output::error("E", "bad", vec![]));
        let (run, extra) = load_outputs(&c, 0);
        let run = run.unwrap();
        assert_eq!(run.messages.len(), 3);
        assert_eq!(run.state, RunViewState::Failed);
        assert!(extra.is_empty());
    }
}
