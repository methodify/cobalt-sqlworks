//! The Files sidebar: a folder browsed as a tree, every row doing what a dropped file does
//! (`docs/design/files_tab.md`). A row shows the kind Cobalt sees the file as; click opens it
//! that way (a data file as a local table, a database file as a connection, a notebook in the
//! notebook editor, a query in a preview tab); the context menu offers the kind's verbs. The
//! folder is remembered between sessions.

use crate::drop::DropAction;
use crate::state::{AppState, FilesState};
use crate::ui::theme::Theme;
use crate::ui::widgets::{icon_button, section_title, tree_row, TreeRow};
use egui::{RichText, Ui};
use egui_phosphor::regular as icons;
use std::path::{Path, PathBuf};

/// A file dragged out of the sidebar (dropped on an editor group, it opens there).
#[derive(Clone, Debug)]
pub struct DragFile(pub PathBuf);

pub enum FilesAction {
    PickRoot,
    /// This folder becomes the root (a subfolder, or the parent through the Up button).
    SetRoot(PathBuf),
    Refresh,
    /// Open the file as what it is; `preview` = reuse / mark a preview tab (a single click).
    Open(PathBuf, bool),
    /// Open the file's text in a query tab whatever its kind.
    OpenText(PathBuf),
    NewFile(PathBuf),
    NewNotebook(PathBuf),
    Reveal(PathBuf),
    /// A data file or a Delta folder as a local table (a Local tab over it).
    Query(PathBuf),
    /// The import dialog on the active connection with this file.
    Import(PathBuf),
}

/// What a row is, from its name, extension and (database files) header: the kind the drop
/// path would act on, plus the text kinds the sidebar opens as plain text.
#[derive(Clone, Debug, PartialEq)]
pub enum FileKind {
    Folder,
    Drop(DropAction),
    /// `.md`, `.toml`, `.xml` and the like: opened as text.
    Text,
    /// Listed only with "every file" on; opens as text.
    Other,
}

const TEXT_EXTENSIONS: &[&str] = &["md", "toml", "xml", "yaml", "yml", "ini", "log", "cfg"];

impl FileKind {
    pub fn of(path: &Path, is_dir: bool) -> FileKind {
        if is_dir {
            return if path.join("_delta_log").is_dir() { FileKind::Drop(DropAction::DeltaFolder) } else { FileKind::Folder };
        }
        match crate::drop::classify_shallow(path) {
            DropAction::Unsupported => {
                let ext = path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).unwrap_or_default();
                if TEXT_EXTENSIONS.contains(&ext.as_str()) { FileKind::Text } else { FileKind::Other }
            }
            other => FileKind::Drop(other),
        }
    }

    /// The row's detail text.
    pub fn label(&self, path: &Path) -> &'static str {
        let ext = path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).unwrap_or_default();
        match self {
            FileKind::Folder => "",
            FileKind::Text => "text",
            FileKind::Other => "",
            FileKind::Drop(d) => match d {
                DropAction::Query { kind, .. } => match kind {
                    crate::drop::QueryKind::Spark => "Spark SQL",
                    crate::drop::QueryKind::Dax => "DAX",
                    crate::drop::QueryKind::Local => "local SQL",
                    _ => if ext == "txt" { "text" } else { "SQL" },
                },
                DropAction::Plan => "plan",
                DropAction::Notebook => "notebook",
                DropAction::PythonNotebook => "Python",
                DropAction::Dashboard => "dashboard",
                DropAction::Explore => "Explore",
                DropAction::Import => "Excel",
                DropAction::LocalTable => crate::drop::data_label(&ext),
                DropAction::SqliteDb => "SQLite database",
                DropAction::DuckDb => "DuckDB database",
                DropAction::Connections => "connections",
                DropAction::AdsSettings => "ADS settings",
                DropAction::DeltaFolder => "Delta table",
                DropAction::Folder => "",
                DropAction::Unsupported => "",
            },
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            FileKind::Folder => icons::FOLDER_SIMPLE,
            FileKind::Text | FileKind::Other => icons::FILE_TEXT,
            FileKind::Drop(d) => match d {
                DropAction::Query { .. } => icons::FILE_SQL,
                DropAction::Plan => icons::TREE_STRUCTURE,
                DropAction::Notebook => icons::NOTEBOOK,
                DropAction::PythonNotebook => icons::FILE_PY,
                DropAction::Dashboard => icons::SQUARES_FOUR,
                DropAction::Explore => icons::ROWS,
                DropAction::Import => icons::MICROSOFT_EXCEL_LOGO,
                DropAction::LocalTable => icons::TABLE,
                DropAction::SqliteDb | DropAction::DuckDb => icons::DATABASE,
                DropAction::Connections | DropAction::AdsSettings => icons::PLUGS,
                DropAction::DeltaFolder => icons::FOLDER_OPEN,
                DropAction::Folder => icons::FOLDER_SIMPLE,
                DropAction::Unsupported => icons::FILE,
            },
        }
    }

    fn is_data(&self) -> bool {
        matches!(self, FileKind::Drop(DropAction::LocalTable | DropAction::DeltaFolder))
    }
    fn is_text(&self) -> bool {
        matches!(self, FileKind::Text | FileKind::Other | FileKind::Drop(DropAction::Query { .. }))
    }
}

pub fn show(ui: &mut Ui, state: &mut AppState, theme: &Theme) -> Vec<FilesAction> {
    let mut actions = Vec::new();
    egui::Frame::new().inner_margin(egui::Margin::symmetric(6, 4)).show(ui, |ui| {
        ui.horizontal(|ui| {
            section_title(ui, theme, "Files");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if icon_button(ui, icons::FOLDER_OPEN, "Open folder…", true).clicked() {
                    actions.push(FilesAction::PickRoot);
                }
                if let Some(root) = state.files.root.clone() {
                    if icon_button(ui, icons::ARROWS_CLOCKWISE, "Refresh", true).clicked() {
                        actions.push(FilesAction::Refresh);
                    }
                    if icon_button(ui, icons::NOTEBOOK, "New notebook in this folder", true).clicked() {
                        actions.push(FilesAction::NewNotebook(root.clone()));
                    }
                    if icon_button(ui, icons::FILE_PLUS, "New .sql file in this folder", true).clicked() {
                        actions.push(FilesAction::NewFile(root.clone()));
                    }
                    let r = icon_button(ui, if state.files.show_all { icons::EYE } else { icons::EYE_SLASH }, if state.files.show_all { "Showing every file; click to list only the kinds Cobalt opens" } else { "Listing the kinds Cobalt opens; click to show every file" }, true);
                    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "files show all"));
                    if r.clicked() {
                        state.files.show_all = !state.files.show_all;
                        state.files.cache.clear();
                    }
                    if let Some(parent) = root.parent().filter(|p| !p.as_os_str().is_empty()).map(Path::to_path_buf) {
                        let r = icon_button(ui, icons::ARROW_ELBOW_LEFT_UP, "Up: the parent folder becomes the root", true);
                        r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "files up"));
                        if r.clicked() {
                            actions.push(FilesAction::SetRoot(parent));
                        }
                    }
                }
            });
        });
        match state.files.root.clone() {
            None => {
                ui.add_space(8.0);
                ui.label(RichText::new("Open a folder to browse it here: queries, notebooks, data files, database files and plans open as what they are. The folder is remembered.").size(12.0).color(theme.text_muted));
                ui.add_space(4.0);
                if ui.button(format!("{} Open folder…", icons::FOLDER_OPEN)).clicked() {
                    actions.push(FilesAction::PickRoot);
                }
            }
            Some(root) => {
                let name = root.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| root.display().to_string());
                let r = ui.label(RichText::new(format!("{} {name}", icons::FOLDER)).size(12.0).color(theme.text_muted));
                r.on_hover_text(root.display().to_string());
                let f = ui.add(egui::TextEdit::singleline(&mut state.files.filter).hint_text(format!("{} Filter files", icons::MAGNIFYING_GLASS)).desired_width(f32::INFINITY));
                f.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, "files filter"));
                let filter = state.files.filter.trim().to_lowercase();
                egui::ScrollArea::vertical().id_salt("files-tree").auto_shrink([false, false]).show(ui, |ui| {
                    dir_children(ui, &mut state.files, theme, &root, 0, &filter, &mut actions);
                });
            }
        }
    });
    actions
}

/// Sorted entries of a directory (folders first, hidden names skipped) with their kinds,
/// cached until Refresh.
fn entries(fs: &mut FilesState, dir: &Path) -> Vec<(PathBuf, bool, FileKind)> {
    if let Some(e) = fs.cache.get(dir) {
        return e.clone();
    }
    let show_all = fs.show_all;
    let mut out: Vec<(PathBuf, bool, FileKind)> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
                .map(|e| {
                    let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                    let path = e.path();
                    let kind = FileKind::of(&path, is_dir);
                    (path, is_dir, kind)
                })
                .filter(|(_, is_dir, kind)| *is_dir || show_all || *kind != FileKind::Other)
                .collect()
        })
        .unwrap_or_default();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.file_name().cmp(&b.0.file_name())));
    fs.cache.insert(dir.to_path_buf(), out.clone());
    out
}

fn dir_children(ui: &mut Ui, fs: &mut FilesState, theme: &Theme, dir: &Path, depth: usize, filter: &str, actions: &mut Vec<FilesAction>) {
    for (path, is_dir, kind) in entries(fs, dir) {
        let name = path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        if is_dir && kind == FileKind::Folder {
            // with a filter on, every folder is open so matches anywhere show up
            let expanded = !filter.is_empty() || fs.expanded.contains(&path);
            let r = tree_row(ui, theme, TreeRow { depth, expandable: true, expanded, loading: false, icon: icons::FOLDER_SIMPLE, icon_color: Some(theme.warning), label: &name, detail: None, selected: false, color_dot: None, kind: "folder" });
            if (r.toggle || r.response.clicked()) && filter.is_empty() {
                if expanded {
                    fs.expanded.remove(&path);
                } else {
                    fs.expanded.insert(path.clone());
                }
            }
            r.response.context_menu(|ui| {
                if ui.button(format!("{} Open as the root", icons::FOLDER_OPEN)).on_hover_text("Browse this folder at the top of the sidebar (Up goes back)").clicked() {
                    actions.push(FilesAction::SetRoot(path.clone()));
                    ui.close();
                }
                if ui.button("New .sql file here…").clicked() {
                    actions.push(FilesAction::NewFile(path.clone()));
                    ui.close();
                }
                if ui.button("New notebook here…").clicked() {
                    actions.push(FilesAction::NewNotebook(path.clone()));
                    ui.close();
                }
                ui.separator();
                if ui.button("Reveal in file manager").clicked() {
                    actions.push(FilesAction::Reveal(path.clone()));
                    ui.close();
                }
                if ui.button("Copy path").clicked() {
                    ui.ctx().copy_text(path.display().to_string());
                    ui.close();
                }
            });
            if expanded {
                dir_children(ui, fs, theme, &path, depth + 1, filter, actions);
            }
            continue;
        }
        if !filter.is_empty() && !name.to_lowercase().contains(filter) {
            continue;
        }
        let detail = kind.label(&path);
        let icon_color = if kind == FileKind::Other { Some(theme.text_faint) } else if kind.is_data() { Some(theme.accent) } else { None };
        let r = tree_row(ui, theme, TreeRow { depth, expandable: false, expanded: false, loading: false, icon: kind.icon(), icon_color, label: &name, detail: if detail.is_empty() { None } else { Some(detail) }, selected: false, color_dot: None, kind: if is_dir { "folder" } else { "file" } });
        if r.response.double_clicked() {
            actions.push(FilesAction::Open(path.clone(), false));
        } else if r.response.clicked() {
            actions.push(FilesAction::Open(path.clone(), true));
        }
        // a drag takes the file to an editor group (clicks still open it here)
        let drag = ui.interact(r.response.rect, r.response.id.with("drag"), egui::Sense::drag());
        if drag.drag_started() {
            egui::DragAndDrop::set_payload(ui.ctx(), DragFile(path.clone()));
        }
        let response = r.response.on_hover_text(open_hint(&kind));
        response.context_menu(|ui| {
            ui.label(RichText::new(if detail.is_empty() { "File".to_string() } else { detail.to_string() }).size(11.0).color(theme.text_muted));
            ui.separator();
            match &kind {
                FileKind::Drop(DropAction::LocalTable | DropAction::DeltaFolder) => {
                    if ui.button(format!("{} Query as a local table", icons::LIGHTNING)).on_hover_text("A Local tab over the file: SQL on it, joins with other tables, Explore, Diff; nothing is copied").clicked() {
                        actions.push(FilesAction::Query(path.clone()));
                        ui.close();
                    }
                    if !is_dir && ui.button(format!("{} Import into a table…", icons::DATABASE)).on_hover_text("The import dialog on the active connection (a new table or an existing one)").clicked() {
                        actions.push(FilesAction::Import(path.clone()));
                        ui.close();
                    }
                }
                FileKind::Drop(DropAction::Import) => {
                    if ui.button(format!("{} Import into a table…", icons::DATABASE)).on_hover_text("The import dialog on the active connection: pick the sheet, the columns and the table").clicked() {
                        actions.push(FilesAction::Import(path.clone()));
                        ui.close();
                    }
                }
                FileKind::Drop(DropAction::SqliteDb | DropAction::DuckDb) => {
                    if ui.button(format!("{} Open as a connection", icons::DATABASE)).on_hover_text("A connection in the Servers tree and a query tab on it").clicked() {
                        actions.push(FilesAction::Open(path.clone(), false));
                        ui.close();
                    }
                }
                FileKind::Drop(DropAction::Notebook) => {
                    if ui.button(format!("{} Open the notebook", icons::NOTEBOOK)).clicked() {
                        actions.push(FilesAction::Open(path.clone(), false));
                        ui.close();
                    }
                }
                FileKind::Drop(DropAction::PythonNotebook) => {
                    if ui.button(format!("{} Open as a PySpark notebook", icons::NOTEBOOK)).on_hover_text("One cell holding the file").clicked() {
                        actions.push(FilesAction::Open(path.clone(), false));
                        ui.close();
                    }
                }
                FileKind::Drop(DropAction::Plan) => {
                    if ui.button(format!("{} Open the plan", icons::TREE_STRUCTURE)).clicked() {
                        actions.push(FilesAction::Open(path.clone(), false));
                        ui.close();
                    }
                }
                FileKind::Drop(DropAction::Dashboard | DropAction::Explore | DropAction::Connections | DropAction::AdsSettings) => {
                    if ui.button("Open").clicked() {
                        actions.push(FilesAction::Open(path.clone(), false));
                        ui.close();
                    }
                }
                _ => {
                    if ui.button("Open").clicked() {
                        actions.push(FilesAction::Open(path.clone(), true));
                        ui.close();
                    }
                    if ui.button("Open in a new tab").on_hover_text("Not a preview: the tab stays").clicked() {
                        actions.push(FilesAction::Open(path.clone(), false));
                        ui.close();
                    }
                }
            }
            if !kind.is_text() && !is_dir && ui.button("Open as text").clicked() {
                actions.push(FilesAction::OpenText(path.clone()));
                ui.close();
            }
            ui.separator();
            if ui.button("Reveal in file manager").clicked() {
                actions.push(FilesAction::Reveal(path.clone()));
                ui.close();
            }
            if ui.button("Copy path").clicked() {
                ui.ctx().copy_text(path.display().to_string());
                ui.close();
            }
        });
    }
}

fn open_hint(kind: &FileKind) -> &'static str {
    match kind {
        FileKind::Drop(DropAction::LocalTable) => "Click: a Local tab over the file (a local table; nothing is copied). Drag onto an editor group to open it there.",
        FileKind::Drop(DropAction::DeltaFolder) => "Click: the Delta table as a local table in a Local tab.",
        FileKind::Drop(DropAction::SqliteDb) => "Click: a SQLite connection in the Servers tree and a query tab on it.",
        FileKind::Drop(DropAction::DuckDb) => "Click: a DuckDB connection in the Servers tree and a query tab on it.",
        FileKind::Drop(DropAction::Notebook) => "Click: the notebook editor.",
        FileKind::Drop(DropAction::PythonNotebook) => "Click: a PySpark notebook with the file as one cell.",
        FileKind::Drop(DropAction::Plan) => "Click: the plan viewer.",
        FileKind::Drop(DropAction::Dashboard) => "Click: the dashboard.",
        FileKind::Drop(DropAction::Explore) => "Click: the Explore builder.",
        FileKind::Drop(DropAction::Import) => "Click: the import dialog on the active connection.",
        FileKind::Drop(DropAction::Query { kind: crate::drop::QueryKind::Spark, .. }) => "Click: a Spark SQL tab (preview). Double-click keeps the tab.",
        FileKind::Drop(DropAction::Query { kind: crate::drop::QueryKind::Dax, .. }) => "Click: a DAX tab (preview). Double-click keeps the tab.",
        FileKind::Other => "Click: the text in a query tab (preview). Not a kind Cobalt recognises.",
        _ => "Click: a preview tab (replaced by the next click). Double-click keeps the tab. Drag onto an editor group to open it there.",
    }
}
