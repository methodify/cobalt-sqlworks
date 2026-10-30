//! The Local sidebar: the in-process engine's tables — every open tab's result sets, files and
//! Delta folders, scratch tables (kept across sessions), lakehouse tables read from OneLake and
//! tables made by statements — with what each costs, and the things to do with one: query it,
//! explore it, diff it, keep it as a scratch table, forget it.

use crate::state::AppState;
use crate::ui::theme::Theme;
use crate::ui::widgets::{icon_button, section_title, tree_row, TreeRow};
use cobalt_local::{TableInfo, TableSource};
use egui::{RichText, Ui};
use egui_phosphor::regular as icons;

pub enum LocalAction {
    /// A Local query tab (over one table, or empty).
    Query(Option<String>),
    Explore(String),
    Diff(String),
    /// Reveal the tab a result table comes from.
    Reveal(String),
    Rename(String),
    /// Forget a table (a scratch table's file is deleted; asks first).
    Forget(String),
    /// Keep a table as `scratch.<name>`.
    SaveScratch(String),
    /// Pick a file (Parquet, CSV, JSON lines, Arrow) to register.
    RegisterFile,
    /// Pick a Delta folder to register.
    RegisterDelta,
    Refresh,
    Toggle(String),
    InsertName(String),
    CopyName(String),
}

/// Groups in display order.
fn group_of(t: &TableInfo) -> usize {
    match t.source {
        TableSource::Result { .. } => 0,
        TableSource::File { .. } => 1,
        TableSource::Scratch { .. } => 2,
        TableSource::OneLake { .. } => 3,
        TableSource::Sqlite { .. } | TableSource::DuckDb { .. } => 4,
        TableSource::Statement => 5,
    }
}

const GROUPS: [(&str, &str); 6] = [
    ("Results", "live with their tabs: a table goes when its tab closes or runs again"),
    ("Files and Delta", "read on every scan; nothing is copied"),
    ("Scratch", "kept across sessions as Parquet in the app data folder"),
    ("OneLake", "lakehouse tables read with your Fabric sign-in"),
    ("Databases", "tables of SQLite and DuckDB database files, read from the file on every scan"),
    ("Made by statements", "CREATE TABLE … AS in a Local tab; gone when the app closes"),
];

fn icon_of(t: &TableInfo) -> &'static str {
    match t.source {
        TableSource::Result { .. } => icons::TABLE,
        TableSource::File { ref path } => {
            if path.is_dir() {
                icons::FOLDER_OPEN
            } else {
                icons::FILE
            }
        }
        TableSource::Scratch { .. } => icons::ARCHIVE,
        TableSource::OneLake { .. } => icons::CLOUD,
        TableSource::Sqlite { .. } | TableSource::DuckDb { .. } => icons::FILE_SQL,
        TableSource::Statement => icons::CODE,
    }
}

fn source_text(t: &TableInfo) -> String {
    match &t.source {
        TableSource::Result { title, set, .. } => {
            if *set == 0 {
                format!("result of {title}")
            } else {
                format!("result set {} of {title}", set + 1)
            }
        }
        TableSource::File { path } => path.display().to_string(),
        TableSource::Scratch { path } => format!("scratch table, {}", path.display()),
        TableSource::OneLake { url } => url.clone(),
        TableSource::Sqlite { path, table } | TableSource::DuckDb { path, table } => format!("{table} in {}", path.display()),
        TableSource::Statement => "made by a statement in a Local tab".to_string(),
    }
}

fn detail_of(t: &TableInfo) -> String {
    let mut parts = Vec::new();
    if t.live {
        parts.push("streaming…".to_string());
    } else if t.rows_known {
        parts.push(format!("{} rows", group_digits(t.rows)));
    } else {
        parts.push("rows on scan".to_string());
    }
    parts.push(format!("{} cols", t.columns.len()));
    if let Some(b) = t.memory_bytes {
        parts.push(crate::ui::lakehouse::fmt_bytes(b as u64));
    } else if let Some(b) = t.file_bytes {
        parts.push(crate::ui::lakehouse::fmt_bytes(b));
    }
    parts.join(" · ")
}

fn group_digits(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub fn show(ui: &mut Ui, state: &mut AppState, tables: &[TableInfo], theme: &Theme) -> Vec<LocalAction> {
    let mut actions = Vec::new();
    egui::Frame::new().inner_margin(egui::Margin::symmetric(6, 4)).show(ui, |ui| {
        ui.horizontal(|ui| {
            section_title(ui, theme, "Local tables");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let r = icon_button(ui, icons::ARROWS_CLOCKWISE, "Refresh the list and every Local tab's completion", true);
                if r.clicked() {
                    actions.push(LocalAction::Refresh);
                }
                let r = icon_button(ui, icons::FILE_ARROW_UP, "Register a file or a Delta folder as a local table…", true);
                r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "local register"));
                egui::Popup::menu(&r).show(|ui| {
                    if ui.button(format!("{} A file (Parquet, CSV, JSON lines, Arrow)…", icons::FILE)).clicked() {
                        actions.push(LocalAction::RegisterFile);
                        ui.close();
                    }
                    if ui.button(format!("{} A Delta folder…", icons::FOLDER_OPEN)).clicked() {
                        actions.push(LocalAction::RegisterDelta);
                        ui.close();
                    }
                });
                let r = icon_button(ui, icons::LIGHTNING, "New local query (SQL over these tables)", true);
                r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "local new query"));
                if r.clicked() {
                    actions.push(LocalAction::Query(None));
                }
            });
        });
        let memory: u64 = tables.iter().filter_map(|t| t.memory_bytes).map(|b| b as u64).sum();
        let scratch = tables.iter().filter(|t| matches!(t.source, TableSource::Scratch { .. })).count();
        let mut summary = format!("{} tables · {} in memory", tables.len(), crate::ui::lakehouse::fmt_bytes(memory));
        if scratch > 0 {
            summary.push_str(&format!(" · {scratch} scratch"));
        }
        ui.label(RichText::new(summary).size(12.0).color(theme.text_muted)).on_hover_text("Result tables share their tab's memory (nothing is copied); files and scratch tables are read from disk when queried. DataFusion 53 in-process.");
        if tables.is_empty() {
            ui.add_space(8.0);
            ui.label(RichText::new("Nothing here yet. Every result set of every tab becomes a local table named after its tab the moment it starts arriving. Drop a Parquet, CSV, JSON lines or Arrow file, or a Delta folder, on the window to query it in place; Save as local table on a result keeps it across sessions.").size(12.0).color(theme.text_muted));
            ui.add_space(4.0);
            if ui.button(format!("{} New local query", icons::LIGHTNING)).clicked() {
                actions.push(LocalAction::Query(None));
            }
            return;
        }
        let r = ui.add(egui::TextEdit::singleline(&mut state.local_filter).hint_text(format!("{} Filter tables and columns", icons::MAGNIFYING_GLASS)).desired_width(f32::INFINITY));
        r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, true, "local filter"));
        let filter = state.local_filter.trim().to_lowercase();
        let active_local = state.active().map(|t| t.local.is_some()).unwrap_or(false);
        egui::ScrollArea::vertical().id_salt("local-tables").auto_shrink([false, false]).show(ui, |ui| {
            for (g, (title, hint)) in GROUPS.iter().enumerate() {
                let mut rows: Vec<&TableInfo> = tables.iter().filter(|t| group_of(t) == g).collect();
                if !filter.is_empty() {
                    rows.retain(|t| t.name.to_lowercase().contains(&filter) || t.columns.iter().any(|c| c.name.to_lowercase().contains(&filter)));
                }
                if rows.is_empty() {
                    continue;
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(title.to_uppercase()).size(11.0).strong().color(theme.text_muted)).on_hover_text(*hint);
                    ui.label(RichText::new(rows.len().to_string()).size(11.0).color(theme.text_muted));
                });
                for t in rows {
                    table_row(ui, state, theme, t, active_local, &filter, &mut actions);
                }
            }
            ui.add_space(12.0);
        });
    });
    actions
}

fn table_row(ui: &mut Ui, state: &mut AppState, theme: &Theme, t: &TableInfo, active_local: bool, filter: &str, actions: &mut Vec<LocalAction>) {
    let expanded = state.local_expanded.contains(&t.name);
    let detail = detail_of(t);
    let color = if t.live { Some(theme.accent) } else { None };
    let r = tree_row(ui, theme, TreeRow { depth: 0, expandable: true, expanded, loading: t.live, icon: icon_of(t), icon_color: color, label: &t.name, detail: Some(&detail), selected: false, color_dot: None, kind: "local table" });
    if r.toggle || r.response.clicked() {
        actions.push(LocalAction::Toggle(t.name.clone()));
    }
    if r.response.double_clicked() {
        actions.push(LocalAction::Query(Some(t.name.clone())));
    }
    let is_result = matches!(t.source, TableSource::Result { .. });
    let is_scratch = matches!(t.source, TableSource::Scratch { .. });
    let source = source_text(t);
    r.response.clone().on_hover_text(format!("{source}\nDouble-click: a Local query tab over it. Drag a column name into the editor from the Servers tree's Local root."));
    r.response.context_menu(|ui| {
        if ui.button(format!("{} Query (new Local tab)", icons::LIGHTNING)).clicked() {
            actions.push(LocalAction::Query(Some(t.name.clone())));
            ui.close();
        }
        if active_local && ui.button(format!("{} Insert name into editor", icons::PENCIL_SIMPLE)).clicked() {
            actions.push(LocalAction::InsertName(t.name.clone()));
            ui.close();
        }
        if ui.button(format!("{} Explore (pivot builder, no SQL)", icons::ROWS)).clicked() {
            actions.push(LocalAction::Explore(t.name.clone()));
            ui.close();
        }
        if ui.button(format!("{} Diff with…", icons::ARROWS_LEFT_RIGHT)).clicked() {
            actions.push(LocalAction::Diff(t.name.clone()));
            ui.close();
        }
        ui.separator();
        if !is_scratch && ui.button(format!("{} Save as scratch table… (kept across sessions)", icons::ARCHIVE)).clicked() {
            actions.push(LocalAction::SaveScratch(t.name.clone()));
            ui.close();
        }
        if !is_scratch && ui.button(format!("{} Rename…", icons::PENCIL_SIMPLE)).clicked() {
            actions.push(LocalAction::Rename(t.name.clone()));
            ui.close();
        }
        if is_result && ui.button(format!("{} Reveal the tab", icons::EYE)).clicked() {
            actions.push(LocalAction::Reveal(t.name.clone()));
            ui.close();
        }
        if ui.button("Copy name").clicked() {
            actions.push(LocalAction::CopyName(t.name.clone()));
            ui.close();
        }
        ui.separator();
        let label = match t.source {
            TableSource::Scratch { .. } => format!("{} Delete the scratch table…", icons::TRASH),
            TableSource::Result { .. } => format!("{} Forget (the tab keeps its results)", icons::TRASH),
            TableSource::File { .. } | TableSource::OneLake { .. } | TableSource::Sqlite { .. } | TableSource::DuckDb { .. } => format!("{} Forget (the data stays where it is)", icons::TRASH),
            TableSource::Statement => format!("{} Drop", icons::TRASH),
        };
        if ui.button(label).clicked() {
            actions.push(LocalAction::Forget(t.name.clone()));
            ui.close();
        }
    });
    if expanded {
        for c in &t.columns {
            if !filter.is_empty() && !t.name.to_lowercase().contains(filter) && !c.name.to_lowercase().contains(filter) {
                continue;
            }
            let ty = c.sql_type.to_string();
            let cr = tree_row(ui, theme, TreeRow { depth: 1, expandable: false, expanded: false, loading: false, icon: icons::COLUMNS, icon_color: None, label: &c.name, detail: Some(&ty), selected: false, color_dot: None, kind: "column" });
            cr.response.context_menu(|ui| {
                if ui.button("Copy name").clicked() {
                    actions.push(LocalAction::CopyName(c.name.clone()));
                    ui.close();
                }
                if active_local && ui.button("Insert name into editor").clicked() {
                    actions.push(LocalAction::InsertName(c.name.clone()));
                    ui.close();
                }
            });
        }
    }
}
