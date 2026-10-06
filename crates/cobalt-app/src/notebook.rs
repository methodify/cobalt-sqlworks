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
use std::time::Instant;

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
    let _ = cx;
    idx
}

/// Restore a notebook tab from a hot-exit snapshot (the snapshot text is the .ipynb JSON).
pub fn restore_from_snapshot(t: &mut EditorTab) -> bool {
    if !Notebook::looks_like_ipynb(&t.text) {
        return false;
    }
    let Ok(nb) = Notebook::parse(&t.text) else { return false };
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
            t.mark_saved();
            state.flash(format!("Saved {}", path.display()));
        }
        Err(e) => cx.toast(ToastKind::Error, format!("Save failed: {e}")),
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
            if !nb.queue.contains(&id) {
                nb.queue.push_back(id);
            }
        }
    }
    let tab = t.id;
    pump(state, cx, tab, false);
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
    let cell_idx = loop {
        let Some(id) = nb.queue.front().cloned() else { return };
        match nb.cell_index(&id) {
            Some(i) if nb.nb.cells[i].kind == CellKind::Code => break i,
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
        return pump_spark(state, cx, idx, cell_idx);
    }
    if lang != CellLanguage::Sql {
        nb.queue.pop_front();
        cx.toast(ToastKind::Warning, format!("{} cells need the Local Spark kernel: pick it from the kernel button on the notebook toolbar.", lang.label()));
        let tab = t.id;
        return pump(state, cx, tab, false);
    }
    let (_, body) = nb.nb.cells[cell_idx].split_magic();
    let script = body.to_string();
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
    let mut view = RunView::new(run_id, PlanMode::None);
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
            crate::kernel::interrupt(&mut state.kernel);
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
fn py_literal(s: &str) -> String {
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
fn pump_spark(state: &mut AppState, cx: &Ctx, idx: usize, cell_idx: usize) {
    use crate::kernel::{self, KernelState, RunReq, StartError};
    let tab = state.tabs[idx].id;
    // the kernel must be up or starting
    match &state.kernel.state {
        KernelState::Stopped | KernelState::Failed(_) => {
            if let Err(StartError::NotProvisioned) = kernel::start(&mut state.kernel, cx.settings, cx.paths, cx.egui) {
                if let Some(nb) = state.tabs[idx].notebook.as_deref_mut() {
                    nb.queue.clear();
                }
                cx.toast(ToastKind::Warning, "The local Spark runtime is not installed yet. Install it under Settings → Spark runtime, then run the cell again.");
                state.settings_open = true;
                state.settings_scroll_to = Some("Spark runtime");
                return;
            }
        }
        _ => {}
    }
    let t = &mut state.tabs[idx];
    let nb = t.notebook.as_deref_mut().unwrap();
    let lang = nb.language_of(cell_idx);
    let cell = &nb.nb.cells[cell_idx];
    let (magic, body) = cell.split_magic();
    let limit = cx.settings.notebooks.spark_row_limit.max(1);
    let code = match lang {
        CellLanguage::Sql => format!("__cobalt_sql({}, {limit})", py_literal(body)),
        CellLanguage::Python => match magic.as_deref() {
            Some("pyspark") | Some("python") => body.to_string(),
            _ => cell.source.clone(),
        },
        other => {
            nb.queue.pop_front();
            cx.toast(ToastKind::Warning, format!("{} cells are not supported on the local Spark kernel (Python and SQL are).", other.label()));
            return pump(state, cx, tab, false);
        }
    };
    nb.queue.pop_front();
    if code.trim().is_empty() {
        return pump(state, cx, tab, false);
    }
    let run_id = cx.session.new_run();
    let mut view = RunView::new(run_id, PlanMode::None);
    view.script_hash = hash_text(&code);
    if cx.settings.history.capture {
        let mut e = NewHistoryEntry::new(format!("Local Spark ({})", cx.settings.spark.profile), cell.source.clone());
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
    let cell_id = cs.id.clone();
    nb.dirty = true;
    kernel::run(&mut state.kernel, RunReq { tab, cell_id, code });
    state.history.loaded = false;
}

/// Each frame: finished Spark cells become outputs; a kernel that just came up gets the queued
/// cells; a kernel that died fails the cells still marked running.
pub fn poll_kernel(state: &mut AppState, cx: &Ctx) {
    let out = crate::kernel::poll(&mut state.kernel);
    let mut followups = Vec::new();
    let mut pumps: Vec<(TabId, bool)> = Vec::new();
    for (req, result) in out.done {
        let Some(idx) = state.tab_index(req.tab) else { continue };
        let Some(nb) = state.tabs[idx].notebook.as_deref_mut() else { continue };
        let Some(ci) = nb.cell_index(&req.cell_id) else { continue };
        let o = crate::kernel::outcome(&result);
        let cs = &mut nb.cells[ci];
        let Some(run) = cs.run.as_mut() else { continue };
        let interrupted = matches!(&result, Err(e) if e.starts_with("interrupted"));
        run.elapsed = run.started.elapsed();
        for rs in o.result_sets {
            run.result_sets.push(ResultSetView { rs, grid: GridState::default(), is_plan: false, profile: None });
        }
        run.total_rows = run.result_sets.iter().map(|s| s.rs.row_count() as u64).sum();
        run.messages.extend(o.messages);
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
        for t in state.tabs.iter_mut() {
            if let Some(nb) = t.notebook.as_deref_mut() {
                if nb.kernel != NotebookKernel::Spark {
                    continue;
                }
                nb.queue.clear();
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
    MessageLine { text, is_error, is_batch_header: false, line: None, at: Instant::now(), path: None }
}

/// Rebuild a cell's cached outputs as a finished run (grids from Arrow / ADS payloads, messages
/// from streams and errors) plus whatever cannot be shown as a grid or a line of text.
pub fn load_outputs(cell: &Cell, _index: usize) -> (Option<RunView>, Vec<Output>) {
    if cell.kind != CellKind::Code || cell.outputs.is_empty() {
        return (None, Vec::new());
    }
    let mut run = RunView::new(cobalt_core::RunId(u64::MAX - _index as u64), PlanMode::None);
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

pub(crate) fn result_set_from_ipc(bytes: &[u8], index: usize) -> Result<Arc<ResultSet>, String> {
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
    // the result set derives its own schema from the SQL types; cast each batch to it
    let rs = ResultSet::new(index, columns, Arc::new(cobalt_results::MemoryBudget::unlimited()), std::env::temp_dir());
    for b in batches {
        let cols: Vec<ArrayRef> = b.columns().iter().zip(rs.schema.fields()).map(|(c, f)| if c.data_type() == f.data_type() { Ok(c.clone()) } else { arrow::compute::cast(c, f.data_type()) }).collect::<Result<_, _>>().map_err(|e| e.to_string())?;
        let b = RecordBatch::try_new(rs.schema.clone(), cols).map_err(|e| e.to_string())?;
        rs.append(b).map_err(|e| e.to_string())?;
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
