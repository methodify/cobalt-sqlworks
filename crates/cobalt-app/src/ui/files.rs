//! The Files sidebar: a folder of query files, browsed as a tree. Click opens the file in a tab;
//! the folder is remembered between sessions.

use crate::state::{AppState, FilesState};
use crate::ui::theme::Theme;
use crate::ui::widgets::{icon_button, section_title, tree_row, TreeRow};
use egui::{RichText, Ui};
use egui_phosphor::regular as icons;
use std::path::{Path, PathBuf};

pub enum FilesAction {
    PickRoot,
    Refresh,
    Open(PathBuf),
    NewFile(PathBuf),
    NewNotebook(PathBuf),
    Reveal(PathBuf),
}

const SHOWN_EXTENSIONS: &[&str] = &["sql", "ipynb", "txt", "sqlplan", "csv", "tsv", "json", "md", "toml", "py"];

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
                        actions.push(FilesAction::NewFile(root));
                    }
                }
            });
        });
        match state.files.root.clone() {
            None => {
                ui.add_space(8.0);
                ui.label(RichText::new("Open a folder of .sql files to browse and open them here. The folder is remembered.").size(12.0).color(theme.text_muted));
                ui.add_space(4.0);
                if ui.button(format!("{} Open folder…", icons::FOLDER_OPEN)).clicked() {
                    actions.push(FilesAction::PickRoot);
                }
            }
            Some(root) => {
                let name = root.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| root.display().to_string());
                let r = ui.label(RichText::new(format!("{} {name}", icons::FOLDER)).size(12.0).color(theme.text_muted));
                r.on_hover_text(root.display().to_string());
                ui.add(egui::TextEdit::singleline(&mut state.files.filter).hint_text(format!("{} Filter files", icons::MAGNIFYING_GLASS)).desired_width(f32::INFINITY));
                let filter = state.files.filter.trim().to_lowercase();
                egui::ScrollArea::vertical().id_salt("files-tree").auto_shrink([false, false]).show(ui, |ui| {
                    dir_children(ui, &mut state.files, theme, &root, 0, &filter, &mut actions);
                });
            }
        }
    });
    actions
}

/// Sorted entries of a directory (folders first, hidden names skipped), cached until Refresh.
fn entries(fs: &mut FilesState, dir: &Path) -> Vec<(PathBuf, bool)> {
    if let Some(e) = fs.cache.get(dir) {
        return e.clone();
    }
    let mut out: Vec<(PathBuf, bool)> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
                .map(|e| {
                    let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                    (e.path(), is_dir)
                })
                .filter(|(p, is_dir)| *is_dir || p.extension().and_then(|x| x.to_str()).map(|x| SHOWN_EXTENSIONS.contains(&x.to_ascii_lowercase().as_str())).unwrap_or(false))
                .collect()
        })
        .unwrap_or_default();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.file_name().cmp(&b.0.file_name())));
    fs.cache.insert(dir.to_path_buf(), out.clone());
    out
}

fn dir_children(ui: &mut Ui, fs: &mut FilesState, theme: &Theme, dir: &Path, depth: usize, filter: &str, actions: &mut Vec<FilesAction>) {
    for (path, is_dir) in entries(fs, dir) {
        let name = path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        if is_dir {
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
                if ui.button("New .sql file here…").clicked() {
                    actions.push(FilesAction::NewFile(path.clone()));
                    ui.close();
                }
                if ui.button("New notebook here…").clicked() {
                    actions.push(FilesAction::NewNotebook(path.clone()));
                    ui.close();
                }
                if ui.button("Reveal in file manager").clicked() {
                    actions.push(FilesAction::Reveal(path.clone()));
                    ui.close();
                }
            });
            if expanded {
                dir_children(ui, fs, theme, &path, depth + 1, filter, actions);
            }
        } else {
            if !filter.is_empty() && !name.to_lowercase().contains(filter) {
                continue;
            }
            let icon = match path.extension().and_then(|x| x.to_str()).map(|x| x.to_ascii_lowercase()).as_deref() {
                Some("sql") => icons::FILE_SQL,
                Some("ipynb") => icons::NOTEBOOK,
                Some("py") => icons::FILE_PY,
                Some("sqlplan") => icons::TREE_STRUCTURE,
                _ => icons::FILE_TEXT,
            };
            let r = tree_row(ui, theme, TreeRow { depth, expandable: false, expanded: false, loading: false, icon, icon_color: None, label: &name, detail: None, selected: false, color_dot: None, kind: "file" });
            if r.response.clicked() || r.response.double_clicked() {
                actions.push(FilesAction::Open(path.clone()));
            }
            r.response.context_menu(|ui| {
                if ui.button("Open").clicked() {
                    actions.push(FilesAction::Open(path.clone()));
                    ui.close();
                }
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
}
