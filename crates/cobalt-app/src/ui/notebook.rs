//! The notebook tab body: a toolbar and a scrolling list of cells. Markdown cells render in
//! place (double-click to edit); code cells use the Cobalt editor and show their result grids,
//! messages and cached outputs underneath.

use crate::commands::Command;
use crate::notebook as nbops;
use crate::ops;
use crate::state::*;
use crate::kernel::KernelState;
use crate::ui::editor::{self, EditorHost, EditorLayout};
use crate::ui::results::grid::{self, GridAction, GridArgs, HEADER_H, ROW_H};
use crate::ui::results::viewer::ViewerState;
use crate::ui::results::ResultsAction;
use crate::ui::shell::{dispatch, Frame};
use crate::ui::theme::Theme;
use crate::ui::widgets::icon_button;
use cobalt_notebook::{CellKind, CellLanguage, Output};
use egui::{Key, Modifiers, RichText, Sense, Ui, Vec2};
use egui_phosphor::regular as icons;

enum NbAction {
    Run(Vec<usize>),
    RunSelection(usize),
    Dequeue(usize),
    RunAndAdvance(usize),
    RunAndInsert(usize),
    Insert { at: usize, kind: CellKind },
    Delete(usize),
    UndoDelete,
    Move(usize, isize),
    SetKind(usize, CellKind),
    SetLanguage(usize, Option<CellLanguage>),
    SetKernel(NotebookKernel),
    SetFabric(Option<NotebookFabric>),
    SetLakehousePolicy(String, nbops::LakehousePolicy),
    LoadWorkspace(String),
    Cancel,
    ClearOutputs(Option<usize>),
    Results(usize, ResultsAction),
    Command(Command),
    /// A SQL cell's text as a Spark SQL query tab with the notebook's binding.
    OpenSparkTab(usize),
}

pub fn show(ui: &mut Ui, f: &mut Frame<'_>, idx: usize) {
    let theme = f.theme;
    let settings = f.cx.settings;
    let fmt = f.state.formatter.clone();
    let mut actions: Vec<NbAction> = Vec::new();
    let tab_id = f.state.tabs[idx].id;
    // a notebook still on its way from Fabric
    if let Some(l) = f.state.tabs[idx].notebook.as_deref().and_then(|nb| nb.loading.clone()) {
        ui.add_space(ui.available_height() * 0.3);
        ui.vertical_centered(|ui| {
            if l.failed {
                ui.label(RichText::new(icons::WARNING).size(28.0).color(theme.error));
                ui.add_space(6.0);
                ui.add(egui::Label::new(RichText::new(&l.message).color(theme.error)).wrap());
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.add_space((ui.available_width() - 200.0).max(0.0) / 2.0);
                    if ui.button(format!("{} Retry", icons::ARROWS_CLOCKWISE)).clicked() {
                        let (item_id, copy) = (l.item_id.clone(), l.copy);
                        if let Some(nb) = f.state.tabs[idx].notebook.as_deref_mut() {
                            if let Some(ld) = nb.loading.as_mut() {
                                ld.failed = false;
                                ld.message = "Fetching from Fabric…".into();
                            }
                        }
                        crate::fabric::open_notebook_again(f.state, f.cx, &item_id, copy);
                    }
                    if ui.button("Close tab").clicked() {
                        ops::close_tab(f.state, f.cx, idx, true);
                    }
                });
            } else {
                ui.spinner();
                ui.add_space(6.0);
                ui.label(RichText::new(&l.message).color(theme.text_muted));
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
            }
        });
        return;
    }
    let connected = f.state.tabs[idx].conn.is_connected();
    let conn_label = {
        let t = &f.state.tabs[idx];
        match &t.conn {
            ConnState::Connected { database, .. } => format!("{} · {database}", t.profile.as_ref().map(|p| p.display_name()).unwrap_or_default()),
            ConnState::Connecting => "Connecting…".into(),
            _ => t.profile.as_ref().map(|p| format!("{} (not connected)", p.display_name())).unwrap_or_else(|| "No connection".into()),
        }
    };

    // ---- toolbar ----
    let (selected, running, n_cells, has_undo, warnings, nb_kernel) = {
        let nb = f.state.tabs[idx].notebook.as_deref().unwrap();
        (nb.selected, nb.is_running(), nb.cells.len(), nb.undo_delete.is_some(), nb.warnings.clone(), nb.kernel)
    };
    let kernel_state = f.state.kernel.state.clone();
    let kernel_busy = f.state.kernel.busy.is_some();
    let spark_profile = settings.spark.profile.clone();
    let nb_fabric = f.state.tabs[idx].notebook.as_deref().and_then(|nb| nb.fabric.clone());
    let fabric_item = f.state.tabs[idx].fabric_item.as_ref().map(|fi| (fi.item.display_name.clone(), fi.saving, f.state.fabric.workspace(&fi.item.workspace_id).map(|w| w.display_name.clone()).unwrap_or_default()));
    let session_fabric = f.state.kernel.fabric.clone();
    let can_attach = f.state.kernel.has("register_lakehouse");
    let attached = f.state.kernel.contexts.len();
    let lh_policy = nb_fabric.as_ref().and_then(|b| b.lakehouse_id.clone()).map(|id| (id.clone(), nbops::lakehouse_policy(f.cx, &id)));
    // names for the chip
    let (ws_name, lh_name) = match &nb_fabric {
        Some(b) => (
            f.state.fabric.workspace(&b.workspace_id).map(|w| w.display_name.clone()).unwrap_or_else(|| b.workspace_id.chars().take(8).collect()),
            b.lakehouse_id.as_ref().map(|id| f.state.fabric.lakehouses(&b.workspace_id).and_then(|v| v.into_iter().find(|(_, i)| i == id).map(|(n, _)| n)).unwrap_or_else(|| id.chars().take(8).collect())),
        ),
        None => (String::new(), None),
    };
    let workspaces: Vec<(String, String)> = f.state.fabric.workspaces.get().map(|v| v.iter().map(|w| (w.id.clone(), w.display_name.clone())).collect()).unwrap_or_default();
    let ws_lakehouses: Option<Vec<(String, String)>> = nb_fabric.as_ref().and_then(|b| f.state.fabric.lakehouses(&b.workspace_id));
    let fabric_signed_in = f.state.fabric.slot.is_some();
    // a bound notebook resolves its workspace and lakehouse names on its own
    if let Some(b) = &nb_fabric {
        let ws_known = f.state.fabric.workspace(&b.workspace_id).is_some();
        if f.state.fabric.status.is_none() {
            crate::fabric::on_panel_shown(f.state, f.cx);
        } else if fabric_signed_in && (!ws_known || ws_lakehouses.is_none()) {
            actions.push(NbAction::LoadWorkspace(b.workspace_id.clone()));
        }
    }
    egui::Frame::new().fill(theme.bg_sidebar).inner_margin(egui::Margin::symmetric(6, 3)).show(ui, |ui| {
        ui.horizontal(|ui| {
            let r = icon_button(ui, icons::PLAY, "Run cell (Ctrl+Enter) · Shift+Enter runs and moves on", !running);
            r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "run cell"));
            if r.clicked() {
                actions.push(NbAction::Run(vec![selected]));
            }
            let r = icon_button(ui, icons::FAST_FORWARD, "Run all cells (F5)", !running);
            r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "run all cells"));
            if r.clicked() {
                actions.push(NbAction::Run((0..n_cells).collect()));
            }
            if icon_button(ui, icons::ARROW_LINE_UP, "Run cells above", !running && selected > 0).clicked() {
                actions.push(NbAction::Run((0..selected).collect()));
            }
            let r = icon_button(ui, icons::STOP, "Stop", running);
            r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "stop notebook"));
            if r.clicked() {
                actions.push(NbAction::Cancel);
            }
            ui.separator();
            let r = ui.add(egui::Button::new(format!("{} Code", icons::PLUS)).small());
            r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "add code cell"));
            if r.on_hover_text("Add a code cell below the current one (B)").clicked() {
                actions.push(NbAction::Insert { at: selected + 1, kind: CellKind::Code });
            }
            let r = ui.add(egui::Button::new(format!("{} Markdown", icons::PLUS)).small());
            r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "add markdown cell"));
            if r.on_hover_text("Add a Markdown cell below the current one").clicked() {
                actions.push(NbAction::Insert { at: selected + 1, kind: CellKind::Markdown });
            }
            ui.separator();
            if icon_button(ui, icons::ERASER, "Clear all outputs", !running).clicked() {
                actions.push(NbAction::ClearOutputs(None));
            }
            if has_undo && ui.small_button(format!("{} Undo delete", icons::ARROW_COUNTER_CLOCKWISE)).on_hover_text("Z").clicked() {
                actions.push(NbAction::UndoDelete);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if icon_button(ui, icons::FLOPPY_DISK, "Save notebook (Ctrl+S)", true).clicked() {
                    actions.push(NbAction::Command(Command::SaveFile));
                }
                ui.menu_button(format!("{} Export", icons::EXPORT), |ui| {
                    if ui.button("HTML page…").clicked() {
                        actions.push(NbAction::Command(Command::ExportNotebookHtml));
                        ui.close();
                    }
                    if ui.button("Markdown…").clicked() {
                        actions.push(NbAction::Command(Command::ExportNotebookMarkdown));
                        ui.close();
                    }
                });
                // kernel picker: the tab's connection, or the local Spark session
                let (kicon, klabel, kcolor) = match nb_kernel {
                    NotebookKernel::Connection => (if connected { icons::PLUG } else { icons::PLUGS }, conn_label.clone(), if connected { theme.text } else { theme.text_muted }),
                    NotebookKernel::Spark => {
                        use crate::kernel::KernelState as K;
                        let (l, c) = match &kernel_state {
                            K::Ready { .. } if kernel_busy => (format!("Local Spark ({spark_profile}) · running"), theme.accent),
                            K::Ready { .. } => (format!("Local Spark ({spark_profile}) · ready"), theme.success),
                            K::Starting { since } => (format!("Local Spark ({spark_profile}) · starting {}s", since.elapsed().as_secs()), theme.warning),
                            K::Failed(_) => (format!("Local Spark ({spark_profile}) · failed"), theme.error),
                            K::Stopped => (format!("Local Spark ({spark_profile})"), theme.text_muted),
                        };
                        (icons::FIRE, l, c)
                    }
                };
                let r = ui.add(egui::Button::new(RichText::new(format!("{kicon} {klabel}")).size(12.0).color(kcolor)).small());
                r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "notebook kernel"));
                let r = r.on_hover_text("Where code cells run. Click to choose: the tab's connection (SQL cells), or the local Spark session (PySpark and Spark SQL cells).");
                let mut pick: Option<NotebookKernel> = None;
                let mut change_conn = false;
                let mut show_log = false;
                let mut session_cmd: Option<Command> = None;
                egui::Popup::menu(&r).id(r.id.with("kernel-menu")).show(|ui| {
                    ui.set_min_width(260.0);
                    ui.label(RichText::new("Kernel").strong());
                    if ui.selectable_label(nb_kernel == NotebookKernel::Connection, format!("{} SQL · {conn_label}", icons::PLUG)).clicked() {
                        pick = Some(NotebookKernel::Connection);
                        ui.close();
                    }
                    if ui.selectable_label(nb_kernel == NotebookKernel::Spark, format!("{} Local Spark ({spark_profile}) · PySpark + Spark SQL", icons::FIRE)).on_hover_text("Runs on the runtime from Settings › Spark runtime. The first cell starts the session (20–60 s).").clicked() {
                        pick = Some(NotebookKernel::Spark);
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Change connection…").clicked() {
                        change_conn = true;
                        ui.close();
                    }
                    // the local Spark session itself
                    ui.separator();
                    ui.label(RichText::new(format!("Session · {}{}", kernel_state.label(), if attached > 0 { format!(" · {attached} notebook{}", if attached == 1 { "" } else { "s" }) } else { String::new() })).strong());
                    let running = kernel_state.is_ready() || kernel_state.is_starting();
                    if ui.add_enabled(!kernel_state.is_starting(), egui::Button::new(if running { format!("{} Restart session", icons::ARROWS_CLOCKWISE) } else { format!("{} Start session", icons::PLAY) })).clicked() {
                        session_cmd = Some(Command::KernelRestart);
                        ui.close();
                    }
                    if ui.add_enabled(running, egui::Button::new(format!("{} Stop session", icons::STOP))).clicked() {
                        session_cmd = Some(Command::KernelStop);
                        ui.close();
                    }
                    if kernel_busy && ui.button(format!("{} Interrupt the running cell", icons::HAND_PALM)).clicked() {
                        session_cmd = Some(Command::CancelQuery);
                        ui.close();
                    }
                    if ui.button("Session log").clicked() {
                        show_log = true;
                        ui.close();
                    }
                    if ui.button("Lakehouse shadows…").clicked() {
                        session_cmd = Some(Command::Shadows);
                        ui.close();
                    }
                });
                if let Some(k) = pick {
                    actions.push(NbAction::SetKernel(k));
                }
                // lakehouse binding (Spark kernel only)
                if nb_kernel == NotebookKernel::Spark {
                    let (label, color) = match &nb_fabric {
                        Some(b) => (format!("{} {}{} · {}", icons::DROP, lh_name.clone().unwrap_or_else(|| "no default lakehouse".into()), if ws_name.is_empty() { String::new() } else { format!(" ({ws_name})") }, b.write_mode), if b.write_mode == "writethrough" { theme.warning } else { theme.text }),
                        None => (format!("{} no lakehouse", icons::DROP), theme.text_muted),
                    };
                    let r = ui.add(egui::Button::new(RichText::new(label).size(12.0).color(color)).small());
                    r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "notebook lakehouse"));
                    let r = r.on_hover_text("Which workspace's lakehouses the Spark session sees, the default lakehouse for unqualified names, and the write mode. Changing it takes effect at the next session start.");
                    let mut set: Option<Option<NotebookFabric>> = None;
                    let mut load_ws: Option<String> = None;
                    let mut open_shadows = false;
                    egui::Popup::menu(&r).id(r.id.with("lakehouse-menu")).show(|ui| {
                        ui.set_min_width(320.0);
                        if !fabric_signed_in {
                            ui.label(RichText::new("Sign in on the Fabric panel to bind a lakehouse.").color(theme.text_muted));
                        }
                        ui.label(RichText::new("Workspace").strong());
                        let cur_ws = nb_fabric.as_ref().map(|b| b.workspace_id.clone());
                        if ui.selectable_label(cur_ws.is_none(), "None — plain local Spark").clicked() {
                            set = Some(None);
                            ui.close();
                        }
                        for (id, name) in &workspaces {
                            if ui.selectable_label(cur_ws.as_deref() == Some(id.as_str()), name).clicked() {
                                set = Some(Some(NotebookFabric { workspace_id: id.clone(), lakehouse_id: None, write_mode: nb_fabric.as_ref().map(|b| b.write_mode.clone()).unwrap_or_else(|| "sandbox".into()), preload: nb_fabric.as_ref().map(|b| b.preload).unwrap_or(false) }));
                                load_ws = Some(id.clone());
                                ui.close();
                            }
                        }
                        if workspaces.is_empty() && fabric_signed_in {
                            ui.label(RichText::new("Loading workspaces…").size(11.0).color(theme.text_faint));
                        }
                        if let Some(b) = &nb_fabric {
                            ui.separator();
                            ui.label(RichText::new("Default lakehouse").strong());
                            match &ws_lakehouses {
                                Some(lhs) => {
                                    if ui.selectable_label(b.lakehouse_id.is_none(), "None").clicked() {
                                        set = Some(Some(NotebookFabric { lakehouse_id: None, ..b.clone() }));
                                        ui.close();
                                    }
                                    for (name, id) in lhs {
                                        if ui.selectable_label(b.lakehouse_id.as_deref() == Some(id.as_str()), name).clicked() {
                                            set = Some(Some(NotebookFabric { lakehouse_id: Some(id.clone()), ..b.clone() }));
                                            ui.close();
                                        }
                                    }
                                    if lhs.is_empty() {
                                        ui.label(RichText::new("No lakehouses in this workspace").size(11.0).color(theme.text_faint));
                                    }
                                }
                                None => {
                                    ui.label(RichText::new("Loading lakehouses…").size(11.0).color(theme.text_faint));
                                    load_ws = Some(b.workspace_id.clone());
                                }
                            }
                            ui.separator();
                            ui.label(RichText::new("Write mode").strong());
                            for (mode, label, hint) in [("sandbox", "Sandbox — writes go to local shallow clones", "Default. OneLake is never written."), ("readonly", "Read only — writes fail", "Reads come from OneLake; any write errors."), ("writethrough", "Write through — writes reach OneLake", "Tables are external OneLake tables: INSERT/CREATE change the lakehouse.")] {
                                if ui.selectable_label(b.write_mode == mode, label).on_hover_text(hint).clicked() {
                                    set = Some(Some(NotebookFabric { write_mode: mode.into(), ..b.clone() }));
                                    ui.close();
                                }
                            }
                            if let Some((lh_id, policy)) = &lh_policy {
                                ui.separator();
                                ui.label(RichText::new("This lakehouse, whenever a session attaches it").strong());
                                let mut p = policy.clone();
                                let mut changed = false;
                                ui.horizontal(|ui| {
                                    ui.label("Preload");
                                    for (v, label, hint) in [("", "nothing", "Tables are cloned on first touch (1.5–2 s each)."), ("last", "tables used before", "After the session starts, the tables cloned in earlier sessions (and kept on disk) are cloned again in the background."), ("all", "all tables", "Every table is cloned in the background right after the session starts (32 at a time); progress in Lakehouse shadows.")] {
                                        let sel = if v.is_empty() { p.preload.is_empty() || p.preload == "none" } else { p.preload == v };
                                        if ui.selectable_label(sel, label).on_hover_text(hint).clicked() {
                                            p.preload = v.into();
                                            changed = true;
                                        }
                                    }
                                });
                                if ui.checkbox(&mut p.keep_clones, "Keep clones between sessions").on_hover_text("The shallow clones stay on disk when the session ends and are reused next time (frozen at the version they were cloned at — Discard in Lakehouse shadows re-clones). Applies from the next session start.").changed() {
                                    changed = true;
                                }
                                if changed {
                                    actions.push(NbAction::SetLakehousePolicy(lh_id.clone(), p));
                                }
                            }
                            ui.separator();
                            if ui.button(format!("{} Lakehouse pane", icons::SIDEBAR_SIMPLE)).on_hover_text("Tables and Files of this lakehouse in the sidebar").clicked() {
                                actions.push(NbAction::Command(Command::ShowLakehouse));
                                ui.close();
                            }
                            if ui.button("Lakehouse shadows…").clicked() {
                                open_shadows = true;
                                ui.close();
                            }
                        }
                    });
                    if let Some(s) = set {
                        actions.push(NbAction::SetFabric(s));
                    }
                    if let Some(w) = load_ws {
                        actions.push(NbAction::LoadWorkspace(w));
                    }
                    if open_shadows {
                        actions.push(NbAction::Command(Command::Shadows));
                    }
                    // the running session's binding, when it differs
                    if let (Some(sf), Some(b), KernelState::Ready { .. }) = (&session_fabric, &nb_fabric, &kernel_state) {
                        // the default lakehouse is the notebook's own context and a new workspace is
                        // attached in place; only the write mode (or an unattachable workspace) needs a restart
                        let mismatch = b.write_mode != sf.write_mode || (!sf.knows_workspace(&b.workspace_id) && !can_attach);
                        if mismatch {
                            let r = ui.add(egui::Button::new(RichText::new(format!("{} session: {}", icons::WARNING, sf.label())).size(11.0).color(theme.warning)).small());
                            if r.on_hover_text(if b.write_mode != sf.write_mode { "The running session is in a different write mode. Click to restart it with this notebook's." } else { "The running session does not know this workspace. Click to restart it with this notebook's." }).clicked() {
                                actions.push(NbAction::Command(Command::KernelRestart));
                            }
                        }
                    } else if session_fabric.is_none() && nb_fabric.is_some() && kernel_state.is_ready() {
                        let r = ui.add(egui::Button::new(RichText::new(format!("{} session has no lakehouse", icons::WARNING)).size(11.0).color(theme.warning)).small());
                        if r.on_hover_text("Restart the session to bind this notebook's lakehouse").clicked() {
                            actions.push(NbAction::Command(Command::KernelRestart));
                        }
                    }
                }
                if let Some((name, saving, ws)) = &fabric_item {
                    let r = ui.label(RichText::new(format!("{} Fabric: {name}{}", icons::CLOUD, if *saving { " · saving…" } else { "" })).size(12.0).color(theme.accent));
                    r.on_hover_text(format!("Opened from workspace {ws}. Save (Ctrl+S) writes back to Fabric after confirmation; Save As… makes a local copy."));
                }
                if change_conn {
                    actions.push(NbAction::Command(Command::ChangeConnection));
                }
                if show_log {
                    actions.push(NbAction::Command(Command::KernelLog));
                }
                if let Some(c) = session_cmd {
                    actions.push(NbAction::Command(c));
                }
                if running {
                    ui.label(RichText::new(format!("{} running", icons::CIRCLE_NOTCH)).size(12.0).color(theme.accent));
                    ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
                }
            });
        });
    });
    if !warnings.is_empty() {
        egui::Frame::new().fill(theme.tint(theme.warning, 0.12)).inner_margin(egui::Margin::symmetric(8, 4)).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{} {} note{} while reading the Fabric source file", icons::WARNING, warnings.len(), if warnings.len() == 1 { "" } else { "s" })).size(12.0));
                ui.small_button("Details").on_hover_text(warnings.join("\n")).clicked();
                if ui.small_button("Dismiss").clicked() {
                    if let Some(nb) = f.state.tabs[idx].notebook.as_deref_mut() {
                        nb.warnings.clear();
                    }
                }
            });
        });
    }

    // Shift+Enter / Alt+Enter belong to the notebook, not the cell editor: take them before the
    // cells draw (the editor would insert a newline first otherwise)
    let (shift_enter, alt_enter) = if f.state.dialog.is_open() || f.state.palette_open { (false, false) } else { ui.input_mut(|i| (i.consume_key(Modifiers::SHIFT, Key::Enter), i.consume_key(Modifiers::ALT, Key::Enter))) };

    // ---- cells ----
    let any_text_focus = ui.memory(|m| m.focused().is_some());
    let mut focused_cell: Option<usize> = None;
    let mut hovered_cell: Option<usize> = None;
    let grid_rows = settings.notebooks.grid_rows.max(3) as usize;
    let avail_w = ui.available_width();
    egui::ScrollArea::vertical().id_salt(("notebook-scroll", tab_id)).auto_shrink([false, false]).show(ui, |ui| {
        ui.add_space(6.0);
        let tab = &mut f.state.tabs[idx];
        let catalog = tab.catalog.as_deref();
        let databases = &tab.databases;
        let nb = tab.notebook.as_deref_mut().unwrap();
        let n = nb.cells.len();
        let default_lang = nb.nb.default_language();
        let group = nb.nb.language_group();
        for i in 0..n {
            let is_selected = nb.selected == i;
            let cell_kind = nb.nb.cells[i].kind;
            let lang = nb.nb.cell_language(&nb.nb.cells[i]);
            let cell_id = nb.cells[i].id.clone();
            let exec_count = nb.nb.cells[i].execution_count;
            let is_live = nb.cells[i].run.as_ref().map(|r| r.is_live()).unwrap_or(false);
            let queued = nb.is_queued(&cell_id);
            let has_selection = nb.cells[i].editor.cursors.has_selection();
            let frame_id = egui::Id::new(("nbcell-frame", tab_id, cell_id.as_str()));
            let hovered_before: bool = ui.memory(|m| m.data.get_temp(frame_id)).unwrap_or(false);
            let stroke = if is_selected { egui::Stroke::new(1.0, theme.accent) } else if hovered_before { egui::Stroke::new(1.0, theme.border_strong) } else { egui::Stroke::new(1.0, theme.border) };
            let resp = egui::Frame::new().fill(theme.bg_panel).stroke(stroke).corner_radius(4.0).inner_margin(egui::Margin { left: 2, right: 6, top: 4, bottom: 4 }).outer_margin(egui::Margin { left: 6, right: 10, top: 0, bottom: 6 }).show(ui, |ui| {
                ui.set_width(avail_w - 20.0);
                ui.horizontal_top(|ui| {
                    // gutter: execution count + run button
                    ui.allocate_ui_with_layout(Vec2::new(44.0, 10.0), egui::Layout::top_down(egui::Align::Center), |ui| {
                        ui.add_space(2.0);
                        if cell_kind == CellKind::Code {
                            let label = if is_live { format!("[{}]", icons::CIRCLE_NOTCH) } else if queued { "[…]".to_string() } else { exec_count.map(|c| format!("[{c}]")).unwrap_or_else(|| "[ ]".into()) };
                            let color = if is_live { theme.accent } else if queued { theme.warning } else { theme.text_faint };
                            ui.label(RichText::new(label).monospace().size(11.0).color(color));
                            // the run button: play, or "queued — click to cancel", or stop while running
                            let (icon, tip, a11y, icon_color) = if is_live {
                                (icons::STOP, "Stop", format!("stop cell {}", i + 1), theme.error)
                            } else if queued {
                                (icons::HOURGLASS, "Queued — click to cancel", format!("dequeue cell {}", i + 1), theme.warning)
                            } else {
                                (icons::PLAY, "Run this cell (Ctrl+Enter) · Ctrl+Shift+Enter runs the selected code", format!("run cell {}", i + 1), theme.text)
                            };
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 0.0;
                                let btn = ui.add(egui::Button::new(RichText::new(icon).size(13.0).color(icon_color)).frame(false));
                                btn.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, a11y.clone()));
                                if btn.on_hover_text(tip).clicked() {
                                    if is_live {
                                        actions.push(NbAction::Cancel);
                                    } else if queued {
                                        actions.push(NbAction::Dequeue(i));
                                    } else {
                                        actions.push(NbAction::Run(vec![i]));
                                    }
                                }
                                // run options for this cell
                                let more = ui.add(egui::Button::new(RichText::new(icons::CARET_DOWN).size(10.0).color(theme.text_faint)).frame(false).min_size(Vec2::new(12.0, 18.0)));
                                more.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("run options cell {}", i + 1)));
                                egui::Popup::menu(&more).id(more.id.with("run-menu")).show(|ui| {
                                    ui.set_min_width(220.0);
                                    if ui.add_enabled(!is_live, egui::Button::new("Run cell")).clicked() {
                                        actions.push(NbAction::Run(vec![i]));
                                        ui.close();
                                    }
                                    if ui.add_enabled(has_selection && !is_live, egui::Button::new("Run selected code")).on_hover_text("Ctrl+Shift+Enter").clicked() {
                                        actions.push(NbAction::RunSelection(i));
                                        ui.close();
                                    }
                                    ui.separator();
                                    if ui.add_enabled(i > 0, egui::Button::new("Run all above this cell")).clicked() {
                                        actions.push(NbAction::Run((0..i).collect()));
                                        ui.close();
                                    }
                                    if ui.button("Run this cell and all below").clicked() {
                                        actions.push(NbAction::Run((i..n).collect()));
                                        ui.close();
                                    }
                                    if nb_kernel == NotebookKernel::Spark && lang == CellLanguage::Sql {
                                        ui.separator();
                                        let r = ui.button(format!("{} Open in a Spark SQL tab", icons::FIRE));
                                        r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("cell {} open in spark tab", i + 1)));
                                        if r.on_hover_text("The cell's SQL in a query tab on the same lakehouse: streamed rows, plans, exports.").clicked() {
                                            actions.push(NbAction::OpenSparkTab(i));
                                            ui.close();
                                        }
                                    }
                                    if queued {
                                        ui.separator();
                                        if ui.button("Cancel queued run").clicked() {
                                            actions.push(NbAction::Dequeue(i));
                                            ui.close();
                                        }
                                    }
                                });
                            });
                        } else {
                            ui.label(RichText::new(icons::TEXT_AA).size(13.0).color(theme.text_faint)).on_hover_text("Markdown cell — double-click to edit");
                        }
                    });
                    ui.vertical(|ui| {
                        // header row: cell type / language, position controls — always shown (a
                        // hover-dependent header made every cell jump as the mouse moved)
                        let show_header = true;
                        if show_header {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 4.0;
                                let kind_label = match cell_kind {
                                    CellKind::Markdown => "Markdown".to_string(),
                                    CellKind::Raw => "Raw".to_string(),
                                    CellKind::Code => lang.label().to_string(),
                                };
                                egui::ComboBox::from_id_salt(("cell-kind", tab_id, cell_id.as_str())).width(100.0).selected_text(RichText::new(kind_label).size(11.0)).show_ui(ui, |ui| {
                                    if ui.selectable_label(cell_kind == CellKind::Code && lang == CellLanguage::Sql, "SQL").clicked() {
                                        actions.push(NbAction::SetKind(i, CellKind::Code));
                                        actions.push(NbAction::SetLanguage(i, if default_lang == CellLanguage::Sql { None } else { Some(CellLanguage::Sql) }));
                                    }
                                    if ui.selectable_label(cell_kind == CellKind::Code && lang == CellLanguage::Python, "PySpark").on_hover_text("Runs on the local Spark session (kernel button on the toolbar)").clicked() {
                                        actions.push(NbAction::SetKind(i, CellKind::Code));
                                        actions.push(NbAction::SetLanguage(i, if default_lang == CellLanguage::Python { None } else { Some(CellLanguage::Python) }));
                                    }
                                    if ui.selectable_label(cell_kind == CellKind::Markdown, "Markdown").clicked() {
                                        actions.push(NbAction::SetKind(i, CellKind::Markdown));
                                    }
                                });
                                let _ = &group;
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if icon_button(ui, icons::TRASH, "Delete cell (D D)", !is_live).clicked() {
                                        actions.push(NbAction::Delete(i));
                                    }
                                    if icon_button(ui, icons::ARROW_DOWN, "Move down", i + 1 < n).clicked() {
                                        actions.push(NbAction::Move(i, 1));
                                    }
                                    if icon_button(ui, icons::ARROW_UP, "Move up", i > 0).clicked() {
                                        actions.push(NbAction::Move(i, -1));
                                    }
                                    if icon_button(ui, icons::ROWS_PLUS_BOTTOM, "Insert code cell below (B)", true).clicked() {
                                        actions.push(NbAction::Insert { at: i + 1, kind: CellKind::Code });
                                    }
                                    if icon_button(ui, icons::ROWS_PLUS_TOP, "Insert code cell above (A)", true).clicked() {
                                        actions.push(NbAction::Insert { at: i, kind: CellKind::Code });
                                    }
                                    if cell_kind == CellKind::Code && (nb.cells[i].run.is_some() || !nb.cells[i].extra_outputs.is_empty()) {
                                        let collapsed = nb.cells[i].outputs_collapsed;
                                        if icon_button(ui, if collapsed { icons::CARET_DOWN } else { icons::CARET_UP }, if collapsed { "Show outputs" } else { "Collapse outputs" }, true).clicked() {
                                            nb.cells[i].outputs_collapsed = !collapsed;
                                        }
                                        if icon_button(ui, icons::ERASER, "Clear this cell's outputs", !is_live).clicked() {
                                            actions.push(NbAction::ClearOutputs(Some(i)));
                                        }
                                    }
                                });
                            });
                        }
                        // body
                        let editing = cell_kind != CellKind::Markdown || nb.cells[i].md_editing;
                        if editing {
                            let host_id = egui::Id::new(("cobalt-cell", tab_id, cell_id.as_str()));
                            let cs = &mut nb.cells[i];
                            let syntax = match (cell_kind, &lang) {
                                (CellKind::Code, CellLanguage::Sql) => if nb_kernel == NotebookKernel::Spark { editor::Syntax::SparkSql } else { editor::Syntax::Sql },
                                (CellKind::Code, CellLanguage::Python) => editor::Syntax::Python,
                                _ => editor::Syntax::Plain,
                            };
                            let mut host = EditorHost { id: host_id, text: &mut nb.nb.cells[i].source, editor: &mut cs.editor, catalog: if cell_kind == CellKind::Code { catalog } else { None }, databases, syntax };
                            let out = egui::Frame::new()
                                .fill(theme.bg_editor)
                                .stroke(egui::Stroke::new(1.0, theme.border))
                                .corner_radius(3.0)
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    editor::show_host(ui, &mut host, Vec::new(), EditorLayout::Auto { min_rows: if cell_kind == CellKind::Markdown { 3 } else { 2 } }, theme, settings)
                                })
                                .inner;
                            if out.changed {
                                nb.dirty = true;
                            }
                            if out.focused {
                                focused_cell = Some(i);
                            }
                            if cell_kind == CellKind::Markdown {
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new("Shift+Enter or Esc renders the Markdown").size(11.0).color(theme.text_faint));
                                });
                            }
                        } else {
                            let source = nb.nb.cells[i].source.clone();
                            let r = ui.scope(|ui| {
                                ui.set_min_height(24.0);
                                if source.trim().is_empty() {
                                    ui.label(RichText::new("Empty Markdown cell — double-click to edit").italics().color(theme.text_faint));
                                } else {
                                    egui_commonmark::CommonMarkViewer::new().max_image_width(Some(900)).show(ui, &mut nb.md_cache, &source);
                                }
                            });
                            let rect = r.response.rect.expand2(Vec2::new(4.0, 2.0));
                            let click = ui.interact(rect, egui::Id::new(("md-click", tab_id, cell_id.as_str())), Sense::click());
                            click.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("markdown cell {}", i + 1)));
                            if click.double_clicked() {
                                nb.cells[i].md_editing = true;
                                nb.cells[i].editor.request_focus = true;
                                nb.selected = i;
                            } else if click.clicked() {
                                nb.selected = i;
                            }
                        }
                        // outputs: an indented block under the editor with a rule on the left, like a
                        // notebook's Out[] area; text comes as blocks, result sets as grids
                        let has_outputs = nb.cells[i].run.is_some() || !nb.cells[i].extra_outputs.is_empty();
                        if cell_kind == CellKind::Code && !nb.cells[i].outputs_collapsed && has_outputs {
                            let cached = nb.cells[i].cached;
                            let extra = nb.cells[i].extra_outputs.clone();
                            ui.add_space(4.0);
                            let block = ui.horizontal_top(|ui| {
                            ui.add_space(10.0);
                            ui.vertical(|ui| {
                            ui.set_width(ui.available_width());
                            ui.spacing_mut().item_spacing.y = 4.0;
                            if let Some(run) = nb.cells[i].run.as_mut() {
                                let state_text = match run.state {
                                    RunViewState::Running => Some((format!("{} Executing…", icons::CIRCLE_NOTCH), theme.text_muted)),
                                    RunViewState::Cancelling => Some(("Cancelling…".to_string(), theme.text_muted)),
                                    RunViewState::Paused => Some(("Paused at the row cap".to_string(), theme.warning)),
                                    _ => None,
                                };
                                if let Some((t, c)) = state_text {
                                    ui.label(RichText::new(t).size(12.0).color(c));
                                }
                                // messages: errors and PRINT output; the row counts and timing live on the grid header
                                let has_grid = run.result_sets.iter().any(|s| !s.is_plan);
                                let elapsed = run.messages.iter().find_map(|m| m.text.strip_prefix("Total execution time: ").map(str::to_string));
                                // consecutive lines of one kind form one block (stdout / errors)
                                let mut blocks: Vec<(bool, Vec<String>)> = Vec::new();
                                for m in run.messages.iter().filter(|m| !m.is_batch_header) {
                                    if m.text.starts_with("Total execution time: ") || (has_grid && m.text.starts_with('(') && m.text.ends_with("returned)")) {
                                        continue;
                                    }
                                    match blocks.last_mut() {
                                        Some((err, lines)) if *err == m.is_error => lines.push(m.text.clone()),
                                        _ => blocks.push((m.is_error, vec![m.text.clone()])),
                                    }
                                }
                                for (is_err, lines) in blocks {
                                    let text = lines.join("\n");
                                    let color = if is_err { theme.error } else { theme.text };
                                    ui.add(egui::Label::new(RichText::new(text).size(12.0).color(color).monospace()).wrap());
                                }
                                if !has_grid && !run.is_live() {
                                    if let Some(e) = &elapsed {
                                        ui.label(RichText::new(format!("{} {e}", icons::TIMER)).size(11.0).color(theme.text_faint));
                                    }
                                }
                                let sets: Vec<usize> = run.result_sets.iter().enumerate().filter(|(_, s)| !s.is_plan).map(|(k, _)| k).collect();
                                let run_id = run.id;
                                let n_sets = run.result_sets.len();
                                let paused_set = run.paused_set;
                                for set in sets {
                                    let view = &mut run.result_sets[set];
                                    let rows = view.rs.row_count();
                                    let visible = view.rs.visible_count();
                                    let rows_label = if visible != rows { format!("{} of {} rows", fmt_count(visible as u64), fmt_count(rows as u64)) } else { format!("{} rows", fmt_count(rows as u64)) };
                                    ui.horizontal(|ui| {
                                        ui.label(RichText::new(format!("{}{rows_label}{}", if n_sets > 1 { format!("Result {} · ", set + 1) } else { String::new() }, elapsed.as_ref().filter(|_| set + 1 == n_sets).map(|e| format!(" · {e}")).unwrap_or_default())).size(11.0).color(theme.text_muted));
                                        if cached {
                                            ui.label(RichText::new("saved with the notebook").size(11.0).color(theme.text_faint)).on_hover_text("These rows were loaded from the file. Run the cell for fresh results.");
                                        }
                                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                            if icon_button(ui, icons::ARROW_SQUARE_OUT, "Open result set in its own tab", true).clicked() {
                                                actions.push(NbAction::Results(i, ResultsAction::PopOut { set }));
                                            }
                                            if icon_button(ui, icons::TABLE, "Save as table…", true).clicked() {
                                                actions.push(NbAction::Results(i, ResultsAction::SaveAsTable { set, selection_only: false }));
                                            }
                                            if icon_button(ui, icons::MICROSOFT_EXCEL_LOGO, "Open in Excel", true).clicked() {
                                                actions.push(NbAction::Results(i, ResultsAction::OpenInExcel { set, selection_only: false }));
                                            }
                                            if icon_button(ui, icons::CHART_BAR, "Profile columns", true).clicked() {
                                                actions.push(NbAction::Results(i, ResultsAction::Profile { set }));
                                            }
                                            if icon_button(ui, icons::COPY, "Copy with headers", true).clicked() {
                                                actions.push(NbAction::Results(i, ResultsAction::Copy { set, kind: crate::copy::CopyKind::TsvWithHeaders }));
                                            }
                                            if icon_button(ui, icons::FLOPPY_DISK, "Save results as…", true).clicked() {
                                                actions.push(NbAction::Results(i, ResultsAction::Export { set, selection_only: false }));
                                            }
                                            if !view.rs.view_spec().is_identity() && icon_button(ui, icons::FUNNEL_X, "Clear filters and sorts", true).clicked() {
                                                actions.push(NbAction::Results(i, ResultsAction::ApplyView { set, spec: cobalt_results::ViewSpec::default() }));
                                            }
                                        });
                                    });
                                    let shown_rows = visible.clamp(1, grid_rows);
                                    let h = HEADER_H + shown_rows as f32 * ROW_H + 14.0;
                                    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), h), Sense::hover());
                                    let mut child = ui.new_child(egui::UiBuilder::new().id_salt(("nb-rs-child", run_id, set)).max_rect(rect).layout(egui::Layout::top_down(egui::Align::Min)));
                                    child.set_clip_rect(rect.intersect(ui.clip_rect()));
                                    let ga = grid::show(
                                        &mut child,
                                        GridArgs {
                                            rs: &view.rs,
                                            grid: &mut view.grid,
                                            theme,
                                            fmt: &fmt,
                                            font_size: settings.appearance.grid_font_size,
                                            id_salt: egui::Id::new(("grid", tab_id, run_id, set)),
                                            show_row_numbers: settings.results.show_row_numbers,
                                            max_col_width: settings.results.max_column_width,
                                            sample_rows: settings.results.auto_size_sample_rows,
                                        },
                                    );
                                    for a in ga {
                                        match a {
                                            GridAction::ApplyView(spec) => actions.push(NbAction::Results(i, ResultsAction::ApplyView { set, spec })),
                                            GridAction::OpenViewer(r, c) => actions.push(NbAction::Results(i, ResultsAction::OpenViewer { set, row: r, col: c })),
                                            GridAction::Copy => actions.push(NbAction::Results(i, ResultsAction::Copy { set, kind: crate::copy::CopyKind::Tsv })),
                                            GridAction::ContextMenu(pos) => {
                                                view.grid.focused = true;
                                                ui.memory_mut(|m| m.data.insert_temp(egui::Id::new(("nb-grid-ctx", tab_id, cell_id.as_str(), set)), pos));
                                            }
                                        }
                                    }
                                    if view.grid.focused {
                                        focused_cell = Some(i);
                                    }
                                    // context menu (a subset of the results pane's)
                                    let ctx_id = egui::Id::new(("nb-grid-ctx", tab_id, cell_id.as_str(), set));
                                    let ctx_pos: Option<egui::Pos2> = ui.memory(|m| m.data.get_temp(ctx_id));
                                    if let Some(pos) = ctx_pos {
                                        let mut close = false;
                                        egui::Area::new(ctx_id.with("area")).order(egui::Order::Foreground).fixed_pos(pos).show(ui.ctx(), |ui| {
                                            egui::Frame::popup(ui.style()).fill(theme.bg_panel).show(ui, |ui| {
                                                ui.set_min_width(200.0);
                                                let items: &[(&str, crate::copy::CopyKind)] = &[("Copy", crate::copy::CopyKind::Tsv), ("Copy with headers", crate::copy::CopyKind::TsvWithHeaders), ("Copy as CSV", crate::copy::CopyKind::Csv), ("Copy as Markdown table", crate::copy::CopyKind::Markdown), ("Copy as JSON", crate::copy::CopyKind::Json), ("Copy as INSERT statements", crate::copy::CopyKind::Insert)];
                                                for (label, kind) in items {
                                                    if ui.button(*label).clicked() {
                                                        actions.push(NbAction::Results(i, ResultsAction::Copy { set, kind: *kind }));
                                                        close = true;
                                                    }
                                                }
                                                ui.separator();
                                                if ui.button("Select all").clicked() {
                                                    view.grid.selection = Selection::All;
                                                    close = true;
                                                }
                                                if ui.button("Open cell in viewer").clicked() {
                                                    if let Some((r, c)) = view.grid.anchor {
                                                        actions.push(NbAction::Results(i, ResultsAction::OpenViewer { set, row: r, col: c }));
                                                    }
                                                    close = true;
                                                }
                                                if ui.button("View row as record").clicked() {
                                                    if let Some((r, c)) = view.grid.anchor {
                                                        actions.push(NbAction::Results(i, ResultsAction::OpenRecord { set, row: r, col: c }));
                                                    }
                                                    close = true;
                                                }
                                                ui.separator();
                                                if ui.button("Save results as…").clicked() {
                                                    actions.push(NbAction::Results(i, ResultsAction::Export { set, selection_only: false }));
                                                    close = true;
                                                }
                                                if ui.button("Save as table…").clicked() {
                                                    actions.push(NbAction::Results(i, ResultsAction::SaveAsTable { set, selection_only: false }));
                                                    close = true;
                                                }
                                                if ui.button("Open in Excel").clicked() {
                                                    actions.push(NbAction::Results(i, ResultsAction::OpenInExcel { set, selection_only: false }));
                                                    close = true;
                                                }
                                                if ui.button("Profile columns…").clicked() {
                                                    actions.push(NbAction::Results(i, ResultsAction::Profile { set }));
                                                    close = true;
                                                }
                                                if ui.button("Open in its own tab").clicked() {
                                                    actions.push(NbAction::Results(i, ResultsAction::PopOut { set }));
                                                    close = true;
                                                }
                                                ui.separator();
                                                if ui.button("Clear filters and sorts").clicked() {
                                                    actions.push(NbAction::Results(i, ResultsAction::ApplyView { set, spec: cobalt_results::ViewSpec::default() }));
                                                    close = true;
                                                }
                                            });
                                        });
                                        let clicked_elsewhere = ui.input(|i| i.pointer.any_click()) && !ui.ctx().rect_contains_pointer(egui::LayerId::new(egui::Order::Foreground, ctx_id.with("area")), egui::Rect::from_min_size(pos, Vec2::new(220.0, 420.0)));
                                        if close || clicked_elsewhere || ui.input(|i| i.key_pressed(Key::Escape)) {
                                            ui.memory_mut(|m| m.data.remove::<egui::Pos2>(ctx_id));
                                        }
                                    }
                                    if paused_set == Some(set) {
                                        ui.horizontal(|ui| {
                                            ui.label(RichText::new(format!("{} Showing {} rows — the query is still open on the server.", icons::PAUSE_CIRCLE, fmt_count(rows as u64))).size(12.0));
                                            let cap = settings.execution.row_cap.max(1000);
                                            if ui.small_button(format!("Fetch {} more", fmt_count(cap))).clicked() {
                                                actions.push(NbAction::Results(i, ResultsAction::FetchMore { rows: Some(cap) }));
                                            }
                                            if ui.small_button("Fetch all").clicked() {
                                                actions.push(NbAction::Results(i, ResultsAction::FetchMore { rows: None }));
                                            }
                                        });
                                    }
                                }
                                if run.result_sets.is_empty() && !run.is_live() && run.messages.is_empty() {
                                    ui.label(RichText::new(if run.rows_affected.is_empty() { "No result sets." } else { "Command(s) completed successfully." }).size(12.0).color(theme.text_muted));
                                }
                                // profile windows for this cell's sets
                                for (set, view) in run.result_sets.iter_mut().enumerate() {
                                    crate::ui::results::profile_window(ui.ctx(), tab_id, set + 1000 * (i + 1), view, theme);
                                }
                            }
                            for o in &extra {
                                render_extra_output(ui, theme, o);
                            }
                            });
                            });
                            // the rule on the left of the output block
                            let r = block.response.rect;
                            ui.painter().line_segment([egui::pos2(r.left() + 3.0, r.top() + 2.0), egui::pos2(r.left() + 3.0, r.bottom() - 2.0)], egui::Stroke::new(2.0, theme.border_strong));
                        } else if cell_kind == CellKind::Code && nb.cells[i].outputs_collapsed {
                            let k = nb.cells[i].run.as_ref().map(|r| r.result_sets.len()).unwrap_or(0) + nb.cells[i].extra_outputs.len();
                            if k > 0 {
                                ui.label(RichText::new(format!("{} {k} output{} hidden", icons::DOTS_THREE, if k == 1 { "" } else { "s" })).size(11.0).color(theme.text_faint));
                            }
                        }
                    });
                });
            });
            let frame_rect = resp.response.rect;
            let hovered = ui.rect_contains_pointer(frame_rect);
            ui.memory_mut(|m| m.data.insert_temp(frame_id, hovered));
            if hovered {
                hovered_cell = Some(i);
                if ui.input(|i| i.pointer.any_pressed()) && !any_text_focus_pressed_in_other_widget(ui) {
                    nb.selected = i;
                }
            }
        }
        // "+" at the end
        ui.horizontal(|ui| {
            ui.add_space(50.0);
            if ui.small_button(format!("{} Code", icons::PLUS)).clicked() {
                actions.push(NbAction::Insert { at: n, kind: CellKind::Code });
            }
            if ui.small_button(format!("{} Markdown", icons::PLUS)).clicked() {
                actions.push(NbAction::Insert { at: n, kind: CellKind::Markdown });
            }
        });
        ui.add_space(40.0);
        if let Some(fc) = focused_cell {
            nb.selected = fc;
        }
        let _ = hovered_cell;
    });

    // ---- keyboard ----
    let (sel, editing_sel, n_cells) = {
        let nb = f.state.tabs[idx].notebook.as_deref().unwrap();
        let s = nb.selected.min(nb.cells.len().saturating_sub(1));
        (s, focused_cell.is_some(), nb.cells.len())
    };
    let editor_focused = focused_cell.is_some();
    if shift_enter {
        actions.push(NbAction::RunAndAdvance(sel));
    } else if alt_enter {
        actions.push(NbAction::RunAndInsert(sel));
    }
    if editor_focused {
        // Escape leaves the cell editor (command mode); a rendered Markdown cell closes its editor
        if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            let nb = f.state.tabs[idx].notebook.as_deref_mut().unwrap();
            if nb.nb.cells[sel].kind == CellKind::Markdown {
                nb.cells[sel].md_editing = false;
            }
            ui.memory_mut(|m| m.stop_text_input());
            if let Some(fid) = ui.memory(|m| m.focused()) {
                ui.memory_mut(|m| m.surrender_focus(fid));
            }
            f.state.focus = Focus::Other;
        }
    } else if !any_text_focus && !f.state.dialog.is_open() && !f.state.palette_open {
        // command mode: Jupyter's keys
        let consume = |key: Key| ui.input_mut(|i| i.consume_key(Modifiers::NONE, key));
        if consume(Key::ArrowDown) || consume(Key::J) {
            if sel + 1 < n_cells {
                f.state.tabs[idx].notebook.as_deref_mut().unwrap().selected = sel + 1;
            }
        } else if consume(Key::ArrowUp) || consume(Key::K) {
            if sel > 0 {
                f.state.tabs[idx].notebook.as_deref_mut().unwrap().selected = sel - 1;
            }
        } else if consume(Key::Enter) {
            let nb = f.state.tabs[idx].notebook.as_deref_mut().unwrap();
            if nb.nb.cells[sel].kind == CellKind::Markdown {
                nb.cells[sel].md_editing = true;
            }
            nb.cells[sel].editor.request_focus = true;
        } else if consume(Key::A) {
            actions.push(NbAction::Insert { at: sel, kind: CellKind::Code });
        } else if consume(Key::B) {
            actions.push(NbAction::Insert { at: sel + 1, kind: CellKind::Code });
        } else if consume(Key::M) {
            actions.push(NbAction::SetKind(sel, CellKind::Markdown));
        } else if consume(Key::Y) {
            actions.push(NbAction::SetKind(sel, CellKind::Code));
        } else if consume(Key::Z) {
            actions.push(NbAction::UndoDelete);
        } else if consume(Key::D) {
            // D D deletes
            let armed_id = egui::Id::new(("nb-dd", tab_id));
            let armed: Option<f64> = ui.memory(|m| m.data.get_temp(armed_id));
            let now = ui.input(|i| i.time);
            match armed {
                Some(t) if now - t < 1.0 => {
                    actions.push(NbAction::Delete(sel));
                    ui.memory_mut(|m| m.data.remove::<f64>(armed_id));
                }
                _ => {
                    ui.memory_mut(|m| m.data.insert_temp(armed_id, now));
                }
            }
        }
    }
    let _ = editing_sel;

    // ---- apply ----
    let cx = f.cx;
    for a in actions {
        match a {
            NbAction::Run(cells) => nbops::run_cells(f.state, cx, idx, cells),
            NbAction::RunSelection(i) => nbops::run_selection(f.state, cx, idx, i),
            NbAction::Dequeue(i) => nbops::dequeue(f.state, idx, i),
            NbAction::RunAndAdvance(i) => {
                let nb = f.state.tabs[idx].notebook.as_deref_mut().unwrap();
                let is_md = nb.nb.cells[i].kind == CellKind::Markdown;
                if is_md {
                    nb.cells[i].md_editing = false;
                }
                let last = i + 1 >= nb.cells.len();
                if !last {
                    nb.selected = i + 1;
                    nb.cells[i + 1].editor.request_focus = true;
                    if nb.nb.cells[i + 1].kind == CellKind::Markdown && !nb.cells[i + 1].md_editing {
                        // rendered markdown: just select it
                        nb.cells[i + 1].editor.request_focus = false;
                    }
                }
                if !is_md {
                    nbops::run_cells(f.state, cx, idx, vec![i]);
                }
                if last {
                    nbops::insert_cell(f.state, idx, i + 1, CellKind::Code, true);
                }
            }
            NbAction::RunAndInsert(i) => {
                let is_md = f.state.tabs[idx].notebook.as_deref().map(|nb| nb.nb.cells[i].kind == CellKind::Markdown).unwrap_or(false);
                if is_md {
                    f.state.tabs[idx].notebook.as_deref_mut().unwrap().cells[i].md_editing = false;
                } else {
                    nbops::run_cells(f.state, cx, idx, vec![i]);
                }
                nbops::insert_cell(f.state, idx, i + 1, CellKind::Code, true);
            }
            NbAction::Insert { at, kind } => {
                nbops::insert_cell(f.state, idx, at, kind, true);
            }
            NbAction::Delete(i) => nbops::delete_cell(f.state, idx, i),
            NbAction::UndoDelete => nbops::undo_delete(f.state, idx),
            NbAction::Move(i, d) => nbops::move_cell(f.state, idx, i, d),
            NbAction::SetKind(i, k) => nbops::set_kind(f.state, idx, i, k),
            NbAction::SetLanguage(i, l) => nbops::set_language(f.state, idx, i, l),
            NbAction::SetKernel(k) => nbops::set_kernel(f.state, idx, k),
            NbAction::SetFabric(b) => nbops::set_fabric(f.state, idx, b),
            NbAction::SetLakehousePolicy(id, p) => nbops::set_lakehouse_policy(f.cx, &id, &p),
            NbAction::LoadWorkspace(ws) => {
                if matches!(f.state.fabric.workspaces, Loadable::NotLoaded | Loadable::Failed(_)) {
                    crate::fabric::load_workspaces(f.state, f.cx);
                }
                if !f.state.fabric.items.contains_key(&ws) {
                    crate::fabric::load_items(f.state, f.cx, &ws);
                }
            }
            NbAction::Cancel => nbops::cancel(f.state, cx, idx),
            NbAction::ClearOutputs(only) => nbops::clear_outputs(f.state, idx, only),
            NbAction::OpenSparkTab(i) => crate::sparkq::open_from_cell(f.state, f.cx, idx, i),
            NbAction::Results(cell, action) => nbops::results_action(f.state, cx, idx, cell, action),
            NbAction::Command(c) => dispatch(f, c),
        }
    }
    // the cell viewer window for a grid in this notebook
    cell_viewer(ui, f, idx);
}

fn any_text_focus_pressed_in_other_widget(_ui: &Ui) -> bool {
    false
}

fn render_extra_output(ui: &mut Ui, theme: &Theme, o: &Output) {
    match o {
        Output::Error { .. } => {
            ui.add(egui::Label::new(RichText::new(o.preview_text()).size(12.0).color(theme.error).monospace()).wrap());
        }
        Output::Stream { name, text } => {
            ui.add(egui::Label::new(RichText::new(text.trim_end()).size(12.0).color(if name == "stderr" { theme.error } else { theme.text_muted }).monospace()).wrap());
        }
        _ => {
            let t = o.preview_text();
            if !t.trim().is_empty() {
                ui.add(egui::Label::new(RichText::new(t.trim_end()).size(12.0).color(theme.text).monospace()).wrap());
            }
        }
    }
}

/// The "Cell value" / "Record" window for whichever grid in the notebook asked for it.
fn cell_viewer(ui: &mut Ui, f: &mut Frame<'_>, idx: usize) {
    use crate::ui::results::viewer::{self, ViewerOutcome};
    let theme = f.theme;
    let tab = &mut f.state.tabs[idx];
    let tab_id = tab.id;
    let Some(nb) = tab.notebook.as_deref_mut() else { return };
    let req = nb.cells.iter_mut().enumerate().find_map(|(ci, c)| c.run.as_mut().and_then(|r| r.result_sets.iter_mut().enumerate().find_map(|(set, s)| s.grid.viewer.map(|(row, col)| (ci, set, row, col)))));
    let Some((ci, set, row, col)) = req else { return };
    let run = nb.cells[ci].run.as_mut().unwrap();
    let rs = run.result_sets[set].rs.clone();
    let id = egui::Id::new(("nb-viewer", tab_id));
    let want_record = std::mem::take(&mut run.result_sets[set].grid.viewer_record);
    let mut vs: ViewerState = ui.ctx().memory(|m| m.data.get_temp::<std::sync::Arc<parking_lot::Mutex<Option<ViewerState>>>>(id)).and_then(|m| m.lock().take()).filter(|v| std::sync::Arc::ptr_eq(&v.rs, &rs) && v.row == row && v.col == col).unwrap_or_else(|| ViewerState::new(rs.clone(), row, col));
    if want_record {
        vs.record = true;
    }
    let mut open = true;
    let mut outcome = ViewerOutcome::Open;
    crate::ui::chrome::Window::new(if vs.record { "Record" } else { "Cell value" }).id(id).open(&mut open).default_size([560.0, 420.0]).resizable(true).show(ui.ctx(), theme, |ui| {
        outcome = viewer::show(ui, theme, &mut vs);
    });
    let grid = &mut run.result_sets[set].grid;
    match outcome {
        _ if !open => grid.viewer = None,
        ViewerOutcome::Close => grid.viewer = None,
        ViewerOutcome::Goto { row, col, record } => {
            let mut next = ViewerState::new(rs.clone(), row, col);
            next.record = record;
            grid.viewer = Some((row, col));
            ui.ctx().memory_mut(|m| m.data.insert_temp(id, std::sync::Arc::new(parking_lot::Mutex::new(Some(next)))));
        }
        ViewerOutcome::Open => {
            ui.ctx().memory_mut(|m| m.data.insert_temp(id, std::sync::Arc::new(parking_lot::Mutex::new(Some(vs)))));
        }
    }
}

/// Summary for the status bar: "5 cells · 2 run".
pub fn status_summary(t: &EditorTab) -> Option<String> {
    let nb = t.notebook.as_deref()?;
    let total = nb.cells.len();
    let code = nb.nb.cells.iter().filter(|c| c.kind == CellKind::Code).count();
    let ran = nb.cells.iter().filter(|c| c.run.is_some()).count();
    Some(format!("{total} cell{} · {code} code · {ran} with output", if total == 1 { "" } else { "s" }))
}

pub fn _unused(_: &ops::Ctx) {}
