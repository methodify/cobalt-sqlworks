//! The Lakehouse sidebar: the Fabric notebook's left pane, for the active Spark notebook's
//! lakehouse. Tables (grouped by schema, with the session's clone state and per-table actions)
//! and Files (listed live from OneLake with sizes; pull a folder to the local mirror, drop the
//! local copy, insert a read cell). Nothing is synced unless asked.

use crate::state::{AppState, LakehousePane};
use crate::ui::theme::Theme;
use crate::ui::widgets::{icon_button, section_title, tree_row, TreeRow};
use egui::{RichText, Ui};
use egui_phosphor::regular as icons;

pub enum LakehouseAction {
    /// Show another lakehouse of the notebook's workspace in the pane.
    Select(String),
    /// Make the shown lakehouse the active notebook's default.
    MakeDefault(String),
    Refresh,
    ExpandFiles(String),
    /// `(lakehouse name, table entry `t` or `schema/t`)`
    Mount(String, String),
    /// Session-catalog spelling of the shadow (`lh.t` or `lh__schema.t`).
    Discard(String),
    Restore(String),
    /// `sync_files(paths=[rel])` on the shown lakehouse.
    Pull(String),
    /// `clear_mirror(paths=[rel])` on the shown lakehouse.
    RemoveLocal(String),
    /// A new code cell with this source in the active notebook.
    InsertCell(String),
}

pub fn fmt_bytes(n: u64) -> String {
    let f = n as f64;
    if n < 1024 {
        format!("{n} B")
    } else if f < 1024.0 * 1024.0 {
        format!("{:.0} KB", f / 1024.0)
    } else if f < 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1} MB", f / 1048576.0)
    } else {
        format!("{:.2} GB", f / 1073741824.0)
    }
}

pub fn show(ui: &mut Ui, state: &mut AppState, theme: &Theme) -> Vec<LakehouseAction> {
    let mut actions = Vec::new();
    egui::Frame::new().inner_margin(egui::Margin::symmetric(6, 4)).show(ui, |ui| {
        ui.horizontal(|ui| {
            section_title(ui, theme, "Lakehouse");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if icon_button(ui, icons::ARROWS_CLOCKWISE, "Refresh tables, files and the mirror status", true).clicked() {
                    actions.push(LakehouseAction::Refresh);
                }
            });
        });
        let Some((ws_id, lh_name, lh_id)) = state.lakehouse_pane.selected.clone() else {
            ui.add_space(8.0);
            ui.label(RichText::new("Shows the tables and Files of a Spark notebook's lakehouse. Open a notebook on the Local Spark kernel and bind a lakehouse with the lakehouse button on its toolbar.").size(12.0).color(theme.text_muted));
            return;
        };
        // which lakehouse of the workspace, and whether it is the notebook's default
        let lakehouses = state.fabric.lakehouses(&ws_id).unwrap_or_default();
        let default_id = state.active().and_then(|t| t.notebook.as_deref()).and_then(|nb| nb.fabric.as_ref()).and_then(|b| b.lakehouse_id.clone());
        let is_default = default_id.as_deref() == Some(lh_id.as_str());
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("lakehouse-pane-pick").width(180.0).selected_text(RichText::new(format!("{} {lh_name}", icons::DROP)).strong()).show_ui(ui, |ui| {
                for (n, id) in &lakehouses {
                    let label = if default_id.as_deref() == Some(id.as_str()) { format!("{n} (default)") } else { n.clone() };
                    if ui.selectable_label(*id == lh_id, label).clicked() {
                        actions.push(LakehouseAction::Select(id.clone()));
                    }
                }
            });
            if is_default {
                ui.label(RichText::new("default").size(11.0).color(theme.text_faint)).on_hover_text("Unqualified table names and Files/ in the active notebook mean this lakehouse.");
            } else if ui.small_button("Make default").on_hover_text("Unqualified table names and Files/ in the active notebook will mean this lakehouse (applies from the notebook's next context).").clicked() {
                actions.push(LakehouseAction::MakeDefault(lh_id.clone()));
            }
        });
        ui.add(egui::TextEdit::singleline(&mut state.lakehouse_pane.filter).hint_text(format!("{} Filter tables and files", icons::MAGNIFYING_GLASS)).desired_width(f32::INFINITY));
        let filter = state.lakehouse_pane.filter.trim().to_lowercase();
        let pane = &state.lakehouse_pane;
        let session_ready = state.kernel.state.is_ready();
        // shadow state per `db.table` from the session's shadow_status
        let shadows: std::collections::HashMap<String, (String, Option<String>)> = state
            .shadows
            .status
            .as_ref()
            .and_then(|s| s.get("tables"))
            .and_then(|t| t.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|t| Some((format!("{}.{}", t.get("lakehouse")?.as_str()?, t.get("table")?.as_str()?), (t.get("state").and_then(|s| s.as_str()).unwrap_or("").to_string(), t.get("cloned_at").and_then(|s| s.as_str()).map(|s| s.to_string())))))
                    .collect()
            })
            .unwrap_or_default();
        let mirror = pane.mirror.clone();
        let pulled: Vec<String> = mirror.as_ref().and_then(|m| m.get("pulled")).and_then(|p| p.as_array()).map(|a| a.iter().filter_map(|x| x.as_str()).map(|s| s.trim_matches('/').to_string()).collect()).unwrap_or_default();
        let fetched: Vec<String> = mirror.as_ref().and_then(|m| m.get("fetched")).and_then(|p| p.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().or_else(|| x.get("path").and_then(|p| p.as_str()))).map(|s| s.trim_matches('/').to_string()).collect()).unwrap_or_default();
        egui::ScrollArea::vertical().id_salt("lakehouse-pane").auto_shrink([false, false]).show(ui, |ui| {
            // ---------------- Tables ----------------
            ui.add_space(4.0);
            ui.label(RichText::new("Tables").strong().size(12.0));
            match &pane.tables {
                None if pane.tables_loading => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(RichText::new("Listing Tables/ on OneLake…").size(11.0).color(theme.text_muted));
                    });
                }
                None => {
                    ui.label(RichText::new("Not listed yet.").size(11.0).color(theme.text_faint));
                }
                Some(Err(e)) => {
                    ui.label(RichText::new(e).size(11.0).color(theme.error));
                }
                Some(Ok(tables)) => {
                    if tables.is_empty() {
                        ui.label(RichText::new("No tables.").size(11.0).color(theme.text_faint));
                    }
                    // top-level tables first, then schema groups
                    let mut schemas: Vec<String> = tables.iter().filter_map(|t| t.schema.clone()).collect();
                    schemas.sort();
                    schemas.dedup();
                    for t in tables.iter().filter(|t| t.schema.is_none()) {
                        table_row(ui, theme, &lh_name, None, &t.name, 0, &filter, &shadows, session_ready, &mut actions);
                    }
                    for schema in schemas {
                        let key = format!("schema:{schema}");
                        let expanded = !filter.is_empty() || !pane.collapsed.contains(&key);
                        let n = tables.iter().filter(|t| t.schema.as_deref() == Some(schema.as_str())).count();
                        let detail = format!("{n} table{}", if n == 1 { "" } else { "s" });
                        let r = tree_row(ui, theme, TreeRow { depth: 0, expandable: true, expanded, loading: false, icon: icons::FOLDER_SIMPLE, icon_color: Some(theme.text_muted), label: &schema, detail: Some(&detail), selected: false, color_dot: None, kind: "schema" });
                        if (r.toggle || r.response.clicked()) && filter.is_empty() {
                            actions.push(LakehouseAction::ExpandFiles(key.clone())); // reuses the collapse set
                        }
                        if expanded {
                            for t in tables.iter().filter(|t| t.schema.as_deref() == Some(schema.as_str())) {
                                table_row(ui, theme, &lh_name, Some(&schema), &t.name, 1, &filter, &shadows, session_ready, &mut actions);
                            }
                        }
                    }
                }
            }
            // ---------------- Files ----------------
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Files").strong().size(12.0));
                if let Some(m) = &mirror {
                    let local = m.get("local_bytes").and_then(|v| v.as_u64()).unwrap_or(0) + m.get("fetched_bytes").and_then(|v| v.as_u64()).unwrap_or(0);
                    let text = if local == 0 { "nothing local".to_string() } else { format!("{} local", fmt_bytes(local)) };
                    ui.label(RichText::new(text).size(11.0).color(theme.text_faint)).on_hover_text("Files fetched on first open or pulled into the local mirror for this lakehouse. Spark reads Files/ from OneLake directly; only Python's /lakehouse/default/Files and explicit pulls use the mirror.");
                } else if !session_ready {
                    ui.label(RichText::new("session stopped — listing from OneLake only").size(11.0).color(theme.text_faint));
                }
            });
            files_children(ui, theme, pane, "", 0, &filter, &pulled, &fetched, &lh_name, session_ready, &mut actions);
        });
        if let Some(n) = &pane.note {
            ui.label(RichText::new(n).size(11.0).color(theme.text_muted));
        }
    });
    actions
}

#[allow(clippy::too_many_arguments)]
fn table_row(ui: &mut Ui, theme: &Theme, lh: &str, schema: Option<&str>, name: &str, depth: usize, filter: &str, shadows: &std::collections::HashMap<String, (String, Option<String>)>, session_ready: bool, actions: &mut Vec<LakehouseAction>) {
    if !filter.is_empty() && !name.to_lowercase().contains(filter) {
        return;
    }
    let db = match schema {
        Some(s) => format!("{lh}__{s}"),
        None => lh.to_string(),
    };
    let entry = match schema {
        Some(s) => format!("{s}/{name}"),
        None => name.to_string(),
    };
    let spelling = match schema {
        Some(s) => format!("{lh}.{s}.{name}"),
        None => format!("{lh}.{name}"),
    };
    let shadow_key = format!("{db}.{name}");
    let (state_label, dot, detail) = match shadows.get(&shadow_key) {
        Some((st, at)) if st == "written" => ("written", Some(theme.warning), at.clone().map(|a| format!("written · cloned {}", &a[..a.len().min(16)]))),
        Some((_, at)) => ("cloned", Some(theme.success), at.clone().map(|a| format!("cloned {}", &a[..a.len().min(16)]))),
        None => ("", None, None),
    };
    let r = tree_row(ui, theme, TreeRow { depth, expandable: false, expanded: false, loading: false, icon: icons::TABLE, icon_color: None, label: name, detail: detail.as_deref(), selected: false, color_dot: dot, kind: "lakehouse table" });
    r.response.dnd_set_drag_payload(spelling.clone());
    let r = r.response.on_hover_text(format!("{spelling}{}\nDrag into a cell; double-click for a cell that reads it.", if state_label.is_empty() { " — not touched in this session" } else { "" }));
    if r.double_clicked() {
        actions.push(LakehouseAction::InsertCell(format!("display(spark.table(\"{spelling}\"))")));
    }
    r.context_menu(|ui| {
        if ui.button("Insert a cell that reads it").clicked() {
            actions.push(LakehouseAction::InsertCell(format!("display(spark.table(\"{spelling}\"))")));
            ui.close();
        }
        if ui.button("Copy name").clicked() {
            ui.ctx().copy_text(spelling.clone());
            ui.close();
        }
        ui.separator();
        if ui.add_enabled(session_ready && state_label.is_empty(), egui::Button::new("Clone now")).on_hover_text("Materialize the table in the session now (a shallow clone in sandbox mode) so the first query does not wait.").clicked() {
            actions.push(LakehouseAction::Mount(lh.to_string(), entry.clone()));
            ui.close();
        }
        if ui.add_enabled(session_ready && !state_label.is_empty(), egui::Button::new("Discard clone")).on_hover_text("Drop the local clone; the next touch clones from OneLake again (local writes are lost).").clicked() {
            actions.push(LakehouseAction::Discard(shadow_key.clone()));
            ui.close();
        }
        if ui.add_enabled(session_ready && state_label == "written", egui::Button::new("Rewind to the clone")).on_hover_text("Keep the clone but drop the local writes made since it was cloned.").clicked() {
            actions.push(LakehouseAction::Restore(shadow_key.clone()));
            ui.close();
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn files_children(ui: &mut Ui, theme: &Theme, pane: &LakehousePane, dir: &str, depth: usize, filter: &str, pulled: &[String], fetched: &[String], lh: &str, session_ready: bool, actions: &mut Vec<LakehouseAction>) {
    match pane.files.get(dir) {
        None => {
            if pane.files_loading.contains(dir) {
                ui.horizontal(|ui| {
                    ui.add_space(depth as f32 * 14.0);
                    ui.spinner();
                    ui.label(RichText::new("Listing…").size(11.0).color(theme.text_muted));
                });
            } else if dir.is_empty() {
                ui.label(RichText::new("Not listed yet.").size(11.0).color(theme.text_faint));
            }
        }
        Some(Err(e)) => {
            ui.label(RichText::new(e).size(11.0).color(theme.error));
        }
        Some(Ok(entries)) => {
            if entries.is_empty() && dir.is_empty() {
                ui.label(RichText::new("Files/ is empty.").size(11.0).color(theme.text_faint));
            }
            for e in entries {
                let rel = if dir.is_empty() { e.name.clone() } else { format!("{dir}/{}", e.name) };
                let is_pulled = pulled.iter().any(|p| rel == *p || rel.starts_with(&format!("{p}/")));
                if e.is_dir {
                    let expanded = pane.expanded.contains(&rel);
                    let detail = if is_pulled { Some("local copy") } else { None };
                    let r = tree_row(ui, theme, TreeRow { depth, expandable: true, expanded, loading: pane.files_loading.contains(&rel), icon: icons::FOLDER_SIMPLE, icon_color: Some(if is_pulled { theme.accent } else { theme.warning }), label: &e.name, detail, selected: false, color_dot: None, kind: "folder" });
                    if r.toggle || r.response.clicked() {
                        actions.push(LakehouseAction::ExpandFiles(rel.clone()));
                    }
                    r.response.context_menu(|ui| {
                        if ui.add_enabled(session_ready && !is_pulled, egui::Button::new(format!("{} Pull to local", icons::DOWNLOAD_SIMPLE))).on_hover_text("sync_files: copies this folder into the local mirror so native readers (DuckDB, Arrow files, ML loaders) can open it. Python's open() does not need this.").clicked() {
                            actions.push(LakehouseAction::Pull(rel.clone()));
                            ui.close();
                        }
                        if ui.add_enabled(session_ready && is_pulled, egui::Button::new(format!("{} Refresh local copy", icons::ARROWS_CLOCKWISE))).clicked() {
                            actions.push(LakehouseAction::Pull(rel.clone()));
                            ui.close();
                        }
                        if ui.add_enabled(session_ready && is_pulled, egui::Button::new(format!("{} Remove local copy", icons::TRASH))).on_hover_text("Deletes the mirrored folder (local writes not yet pushed go with it).").clicked() {
                            actions.push(LakehouseAction::RemoveLocal(rel.clone()));
                            ui.close();
                        }
                        ui.separator();
                        if ui.button("Copy Files/ path").clicked() {
                            ui.ctx().copy_text(format!("Files/{rel}"));
                            ui.close();
                        }
                        if ui.button("Copy /lakehouse/default path").clicked() {
                            ui.ctx().copy_text(format!("/lakehouse/default/Files/{rel}"));
                            ui.close();
                        }
                    });
                    if expanded {
                        files_children(ui, theme, pane, &rel, depth + 1, filter, pulled, fetched, lh, session_ready, actions);
                    }
                } else {
                    if !filter.is_empty() && !e.name.to_lowercase().contains(filter) {
                        continue;
                    }
                    let is_local = is_pulled || fetched.iter().any(|f| *f == rel);
                    let size = fmt_bytes(e.size);
                    let detail = if is_local { format!("{size} · local") } else { size };
                    let ext = e.name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
                    let icon = match ext.as_str() {
                        "csv" | "tsv" => icons::FILE_CSV,
                        "parquet" | "arrow" | "orc" | "avro" => icons::FILE_ARCHIVE,
                        "json" | "jsonl" => icons::FILE_CODE,
                        "txt" | "md" => icons::FILE_TEXT,
                        "png" | "jpg" | "jpeg" | "gif" => icons::FILE_IMAGE,
                        _ => icons::FILE,
                    };
                    let r = tree_row(ui, theme, TreeRow { depth, expandable: false, expanded: false, loading: false, icon, icon_color: if is_local { Some(theme.accent) } else { None }, label: &e.name, detail: Some(&detail), selected: false, color_dot: None, kind: "file" });
                    let read_code = read_cell_code(&rel, &ext);
                    r.response.dnd_set_drag_payload(format!("Files/{rel}"));
                    let resp = r.response.on_hover_text(format!("Files/{rel}\nDrag into a cell for the path; double-click for a cell that reads it."));
                    if resp.double_clicked() {
                        actions.push(LakehouseAction::InsertCell(read_code.clone()));
                    }
                    resp.context_menu(|ui| {
                        if ui.button("Insert a cell that reads it").clicked() {
                            actions.push(LakehouseAction::InsertCell(read_code.clone()));
                            ui.close();
                        }
                        if ui.add_enabled(session_ready && !is_local, egui::Button::new(format!("{} Pull to local", icons::DOWNLOAD_SIMPLE))).on_hover_text("Copies this file into the local mirror for native readers; Python's open() fetches it by itself.").clicked() {
                            actions.push(LakehouseAction::Pull(rel.clone()));
                            ui.close();
                        }
                        if ui.add_enabled(session_ready && is_local, egui::Button::new(format!("{} Remove local copy", icons::TRASH))).clicked() {
                            actions.push(LakehouseAction::RemoveLocal(rel.clone()));
                            ui.close();
                        }
                        ui.separator();
                        if ui.button("Copy Files/ path").clicked() {
                            ui.ctx().copy_text(format!("Files/{rel}"));
                            ui.close();
                        }
                        if ui.button("Copy /lakehouse/default path").clicked() {
                            ui.ctx().copy_text(format!("/lakehouse/default/Files/{rel}"));
                            ui.close();
                        }
                    });
                }
            }
        }
    }
}

/// A PySpark cell that reads the file by its extension.
fn read_cell_code(rel: &str, ext: &str) -> String {
    match ext {
        "csv" | "tsv" => format!("df = spark.read.option(\"header\", True).option(\"inferSchema\", True){}.csv(\"Files/{rel}\")\ndisplay(df)", if ext == "tsv" { ".option(\"sep\", \"\\t\")" } else { "" }),
        "parquet" => format!("df = spark.read.parquet(\"Files/{rel}\")\ndisplay(df)"),
        "json" | "jsonl" => format!("df = spark.read.json(\"Files/{rel}\")\ndisplay(df)"),
        "orc" => format!("df = spark.read.orc(\"Files/{rel}\")\ndisplay(df)"),
        "avro" => format!("df = spark.read.format(\"avro\").load(\"Files/{rel}\")\ndisplay(df)"),
        "txt" | "md" | "log" => format!("with open(\"/lakehouse/default/Files/{rel}\") as f:\n    print(f.read()[:2000])"),
        _ => format!("df = spark.read.format(\"binaryFile\").load(\"Files/{rel}\")\ndisplay(df.drop(\"content\"))"),
    }
}
