//! The window chrome: menu bar, sidebar strip + sidebar, editor tabs, editor toolbar, the
//! editor/results split, status bar — and the command dispatcher.

use crate::commands::{Command, Keymap};
use crate::copy::CopyKind;
use crate::ops::{self, Ctx};
use crate::state::*;
use crate::ui::results::viewer::ViewerState;
use crate::ui::theme::Theme;
use crate::ui::widgets::{icon_button, tool_button};
use crate::ui::{editor, files, history, palette, plan, results, servers};
use cobalt_core::*;
use egui::{RichText, Sense, Stroke, Ui, Vec2};
use egui_phosphor::regular as icons;

pub struct Frame<'a> {
    pub state: &'a mut AppState,
    pub cx: &'a Ctx<'a>,
    pub theme: &'a Theme,
    pub keymap: &'a Keymap,
}

pub fn show(ui: &mut Ui, f: &mut Frame<'_>) {
    let ctx = ui.ctx().clone();
    let ctx = &ctx;
    menu_bar(ui, f);
    status_bar(ui, f);
    sidebar_strip(ui, f);
    if f.state.sidebar_visible {
        sidebar(ui, f);
    }
    egui::CentralPanel::default().frame(egui::Frame::new().fill(f.theme.bg)).show(ui, |ui| {
        tab_strip(ui, f);
        if f.state.active_tab.is_some() {
            editor_area(ui, f);
        } else {
            welcome(ui, f);
        }
    });
    // commands queued by widgets that could not call dispatch themselves (status bar)
    for c in std::mem::take(&mut f.state.pending_commands) {
        dispatch(f, c);
    }
    // a restart finishes once the old session is gone; it rebinds to the active notebook
    if f.state.kernel_restart_pending && matches!(f.state.kernel.state, crate::kernel::KernelState::Stopped | crate::kernel::KernelState::Failed(_)) && f.state.kernel.busy.is_none() {
        f.state.kernel_restart_pending = false;
        // the active Spark notebook or Spark SQL tab decides the binding of the new session
        let active_nb = f.state.active_tab.filter(|i| f.state.tabs[*i].spark.is_some() || f.state.tabs[*i].notebook.as_deref().map(|nb| nb.kernel == NotebookKernel::Spark).unwrap_or(false));
        if let Some(i) = active_nb {
            // the normal path: binding resolution, OneLake token, start (no cell needed)
            let _ = crate::notebook::ensure_session(f.state, f.cx, i);
        } else if let Err(crate::kernel::StartError::NotProvisioned) = crate::kernel::start(&mut f.state.kernel, f.cx.settings, f.cx.paths, f.cx.egui, None) {
            f.cx.toast(ToastKind::Warning, "The local Spark runtime is not installed (Settings → Spark runtime).");
        }
    }
    kernel_log_window(ctx, f);
    shadows_window(ctx, f);
    changelog_window(ctx, f);
    // overlays
    if let Some(item) = palette::show(ui, f.state, f.theme, f.keymap) {
        match item {
            palette::PaletteItem::Command(c) => dispatch(f, c),
            palette::PaletteItem::Connect(id) => {
                ops::new_query_tab(f.state, f.cx, Some(id), None, None, false);
            }
            palette::PaletteItem::Object(profile, obj) => {
                use cobalt_core::ObjectKind;
                let action = match obj.kind {
                    ObjectKind::Table | ObjectKind::View | ObjectKind::Synonym | ObjectKind::TableFunction => crate::ui::servers::TreeAction::SelectTop { profile, obj },
                    ObjectKind::Procedure => crate::ui::servers::TreeAction::Script { profile, obj, kind: cobalt_driver::ScriptKind::Execute },
                    _ => crate::ui::servers::TreeAction::Script { profile, obj, kind: cobalt_driver::ScriptKind::Create },
                };
                ops::tree_action(f.state, f.cx, action);
            }
        }
    }
    crate::ui::dialogs::show(ctx, f);
}

fn menu_bar(ui: &mut Ui, f: &mut Frame<'_>) {
    let theme = f.theme;
    egui::Panel::top("menu").resizable(false).show_separator_line(false).frame(egui::Frame::new().fill(theme.bg_sidebar).inner_margin(egui::Margin::symmetric(6, 2))).show(ui, |ui| {
        egui::MenuBar::new().ui(ui, |ui| {
            let km = f.keymap;
            let mut cmds: Vec<Command> = Vec::new();
            let item = |ui: &mut Ui, cmds: &mut Vec<Command>, c: Command| {
                let info = crate::commands::info(c);
                let sc = km.shortcut_text(ui.ctx(), c);
                let r = ui.add(egui::Button::new(info.label).shortcut_text(sc));
                if r.clicked() {
                    cmds.push(c);
                    ui.close();
                }
            };
            ui.menu_button("File", |ui| {
                item(ui, &mut cmds, Command::NewQuery);
                item(ui, &mut cmds, Command::NewNotebook);
                item(ui, &mut cmds, Command::NewSparkQuery);
                item(ui, &mut cmds, Command::OpenFile);
                item(ui, &mut cmds, Command::OpenPlanFile);
                ui.separator();
                item(ui, &mut cmds, Command::SaveFile);
                item(ui, &mut cmds, Command::SaveFileAs);
                ui.separator();
                item(ui, &mut cmds, Command::CloseTab);
                item(ui, &mut cmds, Command::ReopenClosedTab);
                ui.separator();
                item(ui, &mut cmds, Command::ImportAdsSettings);
                item(ui, &mut cmds, Command::ImportConnections);
                item(ui, &mut cmds, Command::ExportConnections);
                ui.separator();
                item(ui, &mut cmds, Command::ImportFile);
                ui.separator();
                item(ui, &mut cmds, Command::ExportNotebookHtml);
                item(ui, &mut cmds, Command::ExportNotebookMarkdown);
                ui.separator();
                item(ui, &mut cmds, Command::Quit);
            });
            ui.menu_button("Edit", |ui| {
                item(ui, &mut cmds, Command::Find);
                item(ui, &mut cmds, Command::Replace);
                item(ui, &mut cmds, Command::GoToLine);
                ui.separator();
                item(ui, &mut cmds, Command::ToggleLineComment);
                item(ui, &mut cmds, Command::ToggleBlockComment);
                item(ui, &mut cmds, Command::UppercaseKeywords);
                item(ui, &mut cmds, Command::FormatDocument);
                ui.separator();
                item(ui, &mut cmds, Command::TriggerCompletion);
            });
            ui.menu_button("Query", |ui| {
                item(ui, &mut cmds, Command::RunQuery);
                item(ui, &mut cmds, Command::RunCurrentStatement);
                item(ui, &mut cmds, Command::RunToFile);
                item(ui, &mut cmds, Command::CancelQuery);
                ui.separator();
                item(ui, &mut cmds, Command::EstimatedPlan);
                item(ui, &mut cmds, Command::ToggleActualPlan);
                item(ui, &mut cmds, Command::ParseQuery);
                ui.separator();
                item(ui, &mut cmds, Command::ConnectTab);
                item(ui, &mut cmds, Command::ChangeConnection);
                item(ui, &mut cmds, Command::DisconnectTab);
                ui.separator();
                item(ui, &mut cmds, Command::ExecutionOptions);
                ui.separator();
                item(ui, &mut cmds, Command::RunAllCells);
                item(ui, &mut cmds, Command::RunCellsAbove);
                item(ui, &mut cmds, Command::RunCellsBelow);
                item(ui, &mut cmds, Command::RunCellSelection);
                item(ui, &mut cmds, Command::KernelRestart);
                item(ui, &mut cmds, Command::KernelStop);
                item(ui, &mut cmds, Command::KernelLog);
                item(ui, &mut cmds, Command::Shadows);
                item(ui, &mut cmds, Command::RefreshIntelliSense);
            });
            ui.menu_button("Results", |ui| {
                item(ui, &mut cmds, Command::CopyWithHeaders);
                item(ui, &mut cmds, Command::CopyHeaders);
                item(ui, &mut cmds, Command::CopyAsMarkdown);
                item(ui, &mut cmds, Command::CopyAsJson);
                item(ui, &mut cmds, Command::CopyAsCsv);
                item(ui, &mut cmds, Command::CopyAsInsert);
                item(ui, &mut cmds, Command::CopyAsInList);
                ui.separator();
                for c in [Command::SaveResultsCsv, Command::SaveResultsExcel, Command::SaveResultsJson, Command::SaveResultsXml, Command::SaveResultsMarkdown, Command::SaveResultsParquet, Command::SaveResultsArrow, Command::SaveResultsDelta] {
                    item(ui, &mut cmds, c);
                }
                ui.separator();
                item(ui, &mut cmds, Command::SaveAsTable);
                item(ui, &mut cmds, Command::OpenInExcel);
                item(ui, &mut cmds, Command::ProfileColumns);
                ui.separator();
                item(ui, &mut cmds, Command::ToggleResults);
                item(ui, &mut cmds, Command::MaximizeResultSet);
                item(ui, &mut cmds, Command::ClearFilters);
                item(ui, &mut cmds, Command::OpenCellViewer);
            });
            ui.menu_button("View", |ui| {
                item(ui, &mut cmds, Command::Palette);
                item(ui, &mut cmds, Command::ToggleSidebar);
                item(ui, &mut cmds, Command::ShowServers);
                item(ui, &mut cmds, Command::ShowHistory);
                item(ui, &mut cmds, Command::ShowFiles);
                item(ui, &mut cmds, Command::ShowFabric);
                item(ui, &mut cmds, Command::ShowLakehouse);
                ui.separator();
                item(ui, &mut cmds, Command::ToggleTheme);
                item(ui, &mut cmds, Command::ZoomIn);
                item(ui, &mut cmds, Command::ZoomOut);
                item(ui, &mut cmds, Command::ZoomReset);
                ui.separator();
                item(ui, &mut cmds, Command::ShowSettings);
            });
            ui.menu_button("Help", |ui| {
                item(ui, &mut cmds, Command::Welcome);
                item(ui, &mut cmds, Command::Changelog);
                item(ui, &mut cmds, Command::KeyboardShortcuts);
                item(ui, &mut cmds, Command::CheckForUpdates);
                item(ui, &mut cmds, Command::About);
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let r = ui.add(egui::Button::new(format!("{}  Search commands  {}", icons::MAGNIFYING_GLASS, km.shortcut_text(ui.ctx(), Command::Palette))).frame(false));
                if r.clicked() {
                    cmds.push(Command::Palette);
                }
            });
            for c in cmds {
                dispatch(f, c);
            }
        });
    });
}

fn sidebar_strip(ui: &mut Ui, f: &mut Frame<'_>) {
    let theme = f.theme;
    egui::Panel::left("strip").exact_size(44.0).resizable(false).show_separator_line(false).frame(egui::Frame::new().fill(theme.bg_sidebar)).show(ui, |ui| {
        ui.add_space(6.0);
        let items = [(SidebarView::Servers, icons::HARD_DRIVES, "Servers (Ctrl+Shift+E)"), (SidebarView::Fabric, icons::CUBE, "Fabric (Ctrl+Shift+B)"), (SidebarView::Lakehouse, icons::DROP, "Lakehouse — tables and Files of the active notebook's lakehouse"), (SidebarView::Files, icons::FOLDER_OPEN, "Files"), (SidebarView::History, icons::CLOCK_COUNTER_CLOCKWISE, "History (Ctrl+Shift+Y)")];
        for (view, icon, tip) in items {
            let active = f.state.sidebar_visible && f.state.sidebar_view == view;
            let color = if active { theme.accent } else { theme.text_muted };
            let (rect, resp) = ui.allocate_exact_size(Vec2::new(44.0, 40.0), Sense::click());
            if active {
                ui.painter().rect_filled(egui::Rect::from_min_size(rect.min, Vec2::new(3.0, 40.0)), 0.0, theme.accent);
            }
            if resp.hovered() {
                ui.painter().rect_filled(rect.shrink(4.0), 6.0, theme.bg_hover);
            }
            ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, icon, egui::FontId::proportional(22.0), color);
            resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("sidebar {:?}", view)));
            let resp = resp.on_hover_text(tip);
            if resp.clicked() {
                if active {
                    f.state.sidebar_visible = false;
                } else {
                    f.state.sidebar_visible = true;
                    f.state.sidebar_view = view;
                }
            }
        }
        ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
            ui.add_space(6.0);
            let (rect, resp) = ui.allocate_exact_size(Vec2::new(44.0, 40.0), Sense::click());
            if resp.hovered() {
                ui.painter().rect_filled(rect.shrink(4.0), 6.0, theme.bg_hover);
            }
            ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, icons::GEAR, egui::FontId::proportional(22.0), theme.text_muted);
            if resp.on_hover_text("Settings (Ctrl+,)").clicked() {
                f.state.settings_open = true;
            }
        });
    });
}

fn sidebar(ui: &mut Ui, f: &mut Frame<'_>) {
    let theme = f.theme;
    let panel = egui::Panel::left("sidebar").resizable(true).default_size(280.0).size_range(200.0..=640.0).frame(egui::Frame::new().fill(theme.bg_sidebar));
    let resp = panel.show(ui, |ui| match f.state.sidebar_view {
        SidebarView::Servers => {
            let active_profile = f.state.active().and_then(|t| t.profile.as_ref()).map(|p| p.id);
            let spark_root = crate::sparkq::root(f.state, f.cx);
            let spark_active = f.state.active().map(|t| t.spark.is_some()).unwrap_or(false);
            let mut spark_expanded = f.state.spark_root_expanded;
            let actions = servers::show(ui, &mut f.state.library, theme, active_profile, &spark_root, &mut spark_expanded, spark_active);
            f.state.spark_root_expanded = spark_expanded;
            for a in actions {
                ops::tree_action(f.state, f.cx, a);
            }
        }
        SidebarView::History => {
            let actions = history::show(ui, f.state, theme);
            for a in actions {
                history_action(f, a);
            }
        }
        SidebarView::Files => {
            let actions = files::show(ui, f.state, theme);
            for a in actions {
                ops::files_action(f.state, f.cx, a);
            }
        }
        SidebarView::Fabric => {
            crate::fabric::on_panel_shown(f.state, f.cx);
            let actions = crate::ui::fabric::show(ui, f.state, theme);
            for a in actions {
                crate::fabric::action(f.state, f.cx, a);
            }
        }
        SidebarView::Lakehouse => {
            crate::notebook::lakehouse_pane_shown(f.state, f.cx);
            let actions = crate::ui::lakehouse::show(ui, f.state, theme);
            for a in actions {
                crate::notebook::lakehouse_action(f.state, f.cx, a);
            }
        }
    });
    f.state.sidebar_width = resp.response.rect.width();
}

fn history_action(f: &mut Frame<'_>, a: history::HistoryAction) {
    use history::HistoryAction as H;
    match a {
        H::Refresh => ops::refresh_history(f.state, f.cx),
        H::OpenInTab(id) | H::Run(id) => {
            let run = matches!(a, H::Run(_));
            if let Some(e) = f.state.history.entries.iter().find(|e| e.id == id).cloned() {
                let title = format!("History {}", e.started.with_timezone(&chrono::Local).format("%m-%d %H:%M"));
                let profile = e.profile_id.filter(|p| f.state.library.profile(*p).is_some());
                ops::new_query_tab(f.state, f.cx, profile, e.database.clone(), Some((title, e.sql.clone())), run && profile.is_some());
            }
        }
        H::Star(id, on) => {
            let _ = f.cx.store.set_starred(id, on);
            f.state.history.loaded = false;
        }
        H::Delete(id) => {
            let _ = f.cx.store.delete_history(id);
            f.state.history.loaded = false;
        }
        H::ClearAll => {
            let _ = f.cx.store.clear_history(true);
            f.state.history.loaded = false;
        }
        H::Copy(s) => {
            f.cx.egui.copy_text(s);
            f.state.flash("Copied");
        }
    }
}

fn tab_strip(ui: &mut Ui, f: &mut Frame<'_>) {
    let theme = f.theme;
    let mut activate: Option<usize> = None;
    let mut close: Option<usize> = None;
    let mut new_tab = false;
    let mut new_notebook = false;
    let mut rename: Option<usize> = None;
    let mut pin: Option<usize> = None;
    let mut close_others: Option<usize> = None;
    egui::Frame::new().fill(theme.bg_sidebar).show(ui, |ui| {
        ui.set_width(ui.available_width());
        egui::ScrollArea::horizontal().id_salt("tabs-scroll").auto_shrink([false, true]).scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 1.0;
                for (i, t) in f.state.tabs.iter().enumerate() {
                    let active = f.state.active_tab == Some(i);
                    let color = t.profile.as_ref().and_then(|p| f.state.library.color_for(p)).map(Theme::color32);
                    let mut title = t.display_title();
                    if f.cx.settings.appearance.spid_in_tab_title {
                        if let ConnState::Connected { spid: Some(s), .. } = &t.conn {
                            title.push_str(&format!("  ({s})"));
                        }
                    }
                    let running = t.is_running();
                    let font = egui::FontId::proportional(13.0);
                    let galley = ui.painter().layout_no_wrap(title.clone(), font.clone(), theme.text);
                    let w = galley.size().x + 44.0 + if t.pinned { 14.0 } else { 0.0 };
                    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w.max(90.0), 32.0), Sense::click());
                    let a11y_title = title.clone();
                    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, active, format!("tab {a11y_title}")));
                    let bg = if active { theme.bg_panel } else if resp.hovered() { theme.bg_hover } else { theme.bg_sidebar };
                    ui.painter().rect_filled(rect, egui::CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 }, bg);
                    if let Some(c) = color {
                        ui.painter().rect_filled(egui::Rect::from_min_size(rect.min, Vec2::new(rect.width(), 3.0)), egui::CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 }, c);
                    } else if active {
                        ui.painter().rect_filled(egui::Rect::from_min_size(rect.min, Vec2::new(rect.width(), 3.0)), egui::CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 }, theme.accent);
                    }
                    let mut x = rect.left() + 10.0;
                    if t.pinned {
                        ui.painter().text(egui::pos2(x + 4.0, rect.center().y), egui::Align2::CENTER_CENTER, icons::PUSH_PIN, egui::FontId::proportional(11.0), theme.text_faint);
                        x += 14.0;
                    }
                    if running {
                        ui.painter().text(egui::pos2(x + 5.0, rect.center().y), egui::Align2::CENTER_CENTER, icons::CIRCLE_NOTCH, egui::FontId::proportional(12.0), theme.accent);
                        x += 16.0;
                        ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
                    }
                    let text_color = if active { theme.text } else { theme.text_muted };
                    ui.painter().galley(egui::pos2(x, rect.center().y - galley.size().y / 2.0), galley, text_color);
                    // close button
                    let cr = egui::Rect::from_center_size(egui::pos2(rect.right() - 14.0, rect.center().y), Vec2::new(18.0, 18.0));
                    let cresp = ui.interact(cr, resp.id.with("close"), Sense::click());
                    if cresp.hovered() {
                        ui.painter().rect_filled(cr, 4.0, theme.bg_selection);
                    }
                    if active || resp.hovered() || cresp.hovered() {
                        ui.painter().text(cr.center(), egui::Align2::CENTER_CENTER, icons::X, egui::FontId::proportional(12.0), theme.text_muted);
                    }
                    if cresp.clicked() {
                        close = Some(i);
                    } else if resp.clicked() {
                        activate = Some(i);
                    }
                    if resp.middle_clicked() {
                        close = Some(i);
                    }
                    if resp.double_clicked() {
                        rename = Some(i);
                    }
                    resp.context_menu(|ui| {
                        if ui.button("Rename…").clicked() {
                            rename = Some(i);
                            ui.close();
                        }
                        if ui.button(if t.pinned { "Unpin" } else { "Pin" }).clicked() {
                            pin = Some(i);
                            ui.close();
                        }
                        ui.separator();
                        if ui.button("Close").clicked() {
                            close = Some(i);
                            ui.close();
                        }
                        if ui.button("Close others").clicked() {
                            close_others = Some(i);
                            ui.close();
                        }
                    });
                }
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(32.0, 32.0), Sense::click());
                if resp.hovered() {
                    ui.painter().rect_filled(rect.shrink(4.0), 4.0, theme.bg_hover);
                }
                ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, icons::PLUS, egui::FontId::proportional(16.0), theme.text_muted);
                resp.context_menu(|ui| {
                    if ui.button("New query").clicked() {
                        new_tab = true;
                        ui.close();
                    }
                    if ui.button("New notebook").clicked() {
                        new_notebook = true;
                        ui.close();
                    }
                });
                if resp.on_hover_text("New query (Ctrl+N) · right-click for a notebook").clicked() {
                    new_tab = true;
                }
            });
        });
    });
    if let Some(i) = activate {
        f.state.active_tab = Some(i);
        f.state.focus = Focus::Editor;
        f.state.tabs[i].editor.request_focus = true;
    }
    if let Some(i) = pin {
        f.state.tabs[i].pinned = !f.state.tabs[i].pinned;
    }
    if let Some(i) = rename {
        f.state.dialog = Dialog::Rename { tab_index: i, title: f.state.tabs[i].title.clone() };
    }
    if let Some(i) = close {
        if !f.state.tabs[i].pinned {
            ops::close_tab(f.state, f.cx, i, false);
        }
    }
    if let Some(keep) = close_others {
        let ids: Vec<TabId> = f.state.tabs.iter().enumerate().filter(|(j, t)| *j != keep && !t.pinned && !t.is_dirty()).map(|(_, t)| t.id).collect();
        for id in ids {
            if let Some(idx) = f.state.tab_index(id) {
                ops::close_tab(f.state, f.cx, idx, true);
            }
        }
    }
    if new_tab {
        dispatch(f, Command::NewQuery);
    }
    if new_notebook {
        dispatch(f, Command::NewNotebook);
    }
}

/// Sandbox clones of OneLake tables in the running session: inspect, discard, rewind.
fn shadows_window(ctx: &egui::Context, f: &mut Frame<'_>) {
    if !f.state.shadows.open {
        return;
    }
    let theme = f.theme;
    let mut open = true;
    let mut action: Option<(&'static str, serde_json::Value)> = None;
    let mut refresh = false;
    {
        let sh = &f.state.shadows;
        let k = &f.state.kernel;
        crate::ui::chrome::Window::new("Lakehouse shadows").id(egui::Id::new("shadows-window")).open(&mut open).default_size([640.0, 360.0]).resizable(true).show(ctx, theme, |ui| {
            match &k.fabric {
                Some(fb) => {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("{} · workspace {}", fb.label(), fb.workspace_name)).strong());
                        if let Some(n) = k.token_requests() {
                            ui.label(RichText::new(format!("· {n} OneLake token request{}", if n == 1 { "" } else { "s" })).size(11.0).color(theme.text_faint));
                        }
                    });
                    if let Some(e) = k.token_error() {
                        ui.label(RichText::new(format!("OneLake token endpoint: {e}")).size(11.0).color(theme.error));
                    }
                }
                None => {
                    ui.label(RichText::new("The running session has no lakehouse binding (or is not running). Bind a notebook with the lakehouse button and restart the session.").color(theme.text_muted));
                }
            }
            ui.label(RichText::new("In sandbox mode the first touch of a lakehouse table makes a Delta shallow clone here; reads and writes hit the clone, OneLake stays untouched. Discard to re-clone from OneLake next time; rewind to the clone's first version to drop local writes.").size(11.0).color(theme.text_muted));
            ui.horizontal(|ui| {
                if ui.add_enabled(!sh.loading, egui::Button::new("Refresh")).clicked() {
                    refresh = true;
                }
                let has = sh.status.as_ref().and_then(|s| s.get("tables")).and_then(|t| t.as_array()).map(|a| !a.is_empty()).unwrap_or(false);
                if ui.add_enabled(!sh.loading && has, egui::Button::new("Discard all")).on_hover_text("Drop every clone; OneLake is not affected").clicked() {
                    action = Some(("discard_shadow", serde_json::json!({})));
                }
                if ui.add_enabled(!sh.loading && has, egui::Button::new("Discard written")).on_hover_text("Drop only the clones that local writes changed").clicked() {
                    action = Some(("discard_shadow", serde_json::json!({"only": "written"})));
                }
                if sh.loading {
                    ui.spinner();
                }
            });
            if let Some(e) = &sh.error {
                ui.label(RichText::new(e).color(theme.error));
            }
            if let Some(n) = &sh.note {
                ui.label(RichText::new(n).size(11.0).color(theme.text_faint));
            }
            if let Some(p) = &sh.preload {
                let state_s = p.get("state").and_then(|v| v.as_str()).unwrap_or("idle");
                if state_s != "idle" {
                    let (total, done, failed) = (p.get("tables_total").and_then(|v| v.as_u64()).unwrap_or(0), p.get("tables_done").and_then(|v| v.as_u64()).unwrap_or(0), p.get("tables_failed").and_then(|v| v.as_u64()).unwrap_or(0));
                    ui.horizontal(|ui| {
                        if state_s == "running" {
                            ui.spinner();
                        }
                        ui.label(RichText::new(format!("Preload {state_s}: {done} of {total} tables cloned{}", if failed > 0 { format!(", {failed} failed") } else { String::new() })).size(12.0).color(if state_s == "failed" { theme.error } else { theme.text }));
                    });
                    if total > 0 {
                        ui.add(egui::ProgressBar::new(done as f32 / total as f32).desired_width(320.0));
                    }
                    for e in p.get("errors").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_str()).take(3).collect::<Vec<_>>()).unwrap_or_default() {
                        ui.label(RichText::new(e).size(11.0).color(theme.error));
                    }
                    if let Some(n) = p.get("note").and_then(|v| v.as_str()) {
                        ui.label(RichText::new(n).size(11.0).color(theme.text_faint));
                    }
                }
            }
            if let Some(st) = &sh.status {
                ui.label(RichText::new(format!("write mode {} · shadow root {}", st.get("write_mode").and_then(|v| v.as_str()).unwrap_or("?"), st.get("shadow_root").and_then(|v| v.as_str()).unwrap_or("?"))).size(11.0).color(theme.text_muted));
                let tables = st.get("tables").and_then(|t| t.as_array()).cloned().unwrap_or_default();
                if tables.is_empty() {
                    ui.label(RichText::new("No clones yet — nothing has touched a lakehouse table this session.").color(theme.text_faint));
                } else {
                    egui::Grid::new("shadows-grid").striped(true).num_columns(5).spacing([12.0, 4.0]).show(ui, |ui| {
                        ui.label(RichText::new("Lakehouse").strong());
                        ui.label(RichText::new("Table").strong());
                        ui.label(RichText::new("State").strong());
                        ui.label(RichText::new("Version").strong());
                        ui.label("");
                        ui.end_row();
                        for t in &tables {
                            let lh = t.get("lakehouse").and_then(|v| v.as_str()).unwrap_or("");
                            let tb = t.get("table").and_then(|v| v.as_str()).unwrap_or("");
                            let state_s = t.get("state").and_then(|v| v.as_str()).unwrap_or("");
                            ui.label(lh);
                            ui.label(tb);
                            ui.label(RichText::new(state_s).color(if state_s == "written" { theme.warning } else { theme.text_muted }));
                            ui.label(t.get("version").map(|v| v.to_string()).unwrap_or_default());
                            ui.horizontal(|ui| {
                                if ui.small_button("Discard").clicked() {
                                    action = Some(("discard_shadow_one", serde_json::json!({"lakehouse": lh, "table": tb})));
                                }
                                if state_s == "written" && ui.small_button("Rewind").on_hover_text("Back to the clone's first version (drops local writes)").clicked() {
                                    action = Some(("restore_shadow", serde_json::json!({"table": format!("{lh}.{tb}"), "version": 0})));
                                }
                            });
                            ui.end_row();
                        }
                    });
                }
                let dv = st.get("deletion_vector_tables").and_then(|t| t.as_array()).cloned().unwrap_or_default();
                if !dv.is_empty() {
                    ui.label(RichText::new(format!("{} table{} with deletion vectors are live read-only views", dv.len(), if dv.len() == 1 { "" } else { "s" })).size(11.0).color(theme.text_faint));
                }
            }
        });
    }
    if !open {
        f.state.shadows.open = false;
    }
    if refresh {
        crate::notebook::refresh_shadows(f.state);
    }
    if let Some((method, params)) = action {
        if method == "discard_shadow_one" {
            let table = format!("{}.{}", params.get("lakehouse").and_then(|v| v.as_str()).unwrap_or(""), params.get("table").and_then(|v| v.as_str()).unwrap_or(""));
            crate::notebook::shadows_action(f.state, "discard_shadow", serde_json::json!({"table": table}));
        } else {
            crate::notebook::shadows_action(f.state, method, params);
        }
    }
    // preload progress: poll the worker (control socket) while a preload runs and the window is open
    if f.state.shadows.open && f.state.kernel.state.is_ready() {
        let running = f.state.shadows.preload.as_ref().map(|p| p.get("state").and_then(|s| s.as_str()) == Some("running")).unwrap_or(true);
        let key = egui::Id::new("preload-poll");
        let now = ctx.input(|i| i.time);
        let last: f64 = ctx.memory(|m| m.data.get_temp(key)).unwrap_or(0.0);
        if running && now - last > 3.0 {
            ctx.memory_mut(|m| m.data.insert_temp(key, now));
            crate::notebook::shadows_action(f.state, "preload_status", serde_json::json!({}));
            ctx.request_repaint_after(std::time::Duration::from_secs(3));
        }
    }
}

/// The local Spark session's console (worker stderr: Spark, py4j, Ivy, tracebacks).
/// Help → What's new: the changelog, bundled copy first, GitHub's when it arrives.
fn changelog_window(ctx: &egui::Context, f: &mut Frame<'_>) {
    if !f.state.changelog.open {
        return;
    }
    // a finished fetch lands in `fetched`
    let done = f.state.changelog.pending.as_ref().and_then(|slot| slot.lock().take());
    if let Some(r) = done {
        f.state.changelog.pending = None;
        f.state.changelog.fetched = Some(r);
    }
    let theme = f.theme;
    let mut open = true;
    let mut refetch = false;
    crate::ui::chrome::Window::new("What's new").id(egui::Id::new("changelog-window")).open(&mut open).default_size([760.0, 560.0]).min_size([420.0, 300.0]).resizable(true).show(ctx, theme, |ui| {
        let cl = &mut f.state.changelog;
        let (text, note, is_error) = match (&cl.fetched, cl.pending.is_some()) {
            (Some(Ok(t)), _) => (t.as_str(), "latest from GitHub".to_string(), false),
            (Some(Err(e)), _) => (crate::update::BUNDLED_CHANGELOG, format!("the copy bundled with this build — GitHub could not be reached ({e})"), true),
            (None, true) => (crate::update::BUNDLED_CHANGELOG, "the copy bundled with this build — fetching the latest from GitHub…".to_string(), false),
            (None, false) => (crate::update::BUNDLED_CHANGELOG, "the copy bundled with this build".to_string(), false),
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("You are on {}", crate::update::CURRENT_VERSION)).strong());
            ui.label(RichText::new(format!("· {note}")).size(11.0).color(if is_error { theme.warning } else { theme.text_muted }));
            if cl.pending.is_some() {
                ui.spinner();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("Open on GitHub").clicked() {
                    let _ = open::that(crate::update::CHANGELOG_PAGE);
                }
                if ui.add_enabled(cl.pending.is_none(), egui::Button::new("Refresh").small()).clicked() {
                    refetch = true;
                }
            });
        });
        ui.separator();
        egui::ScrollArea::vertical().id_salt("changelog-scroll").auto_shrink([false, false]).show(ui, |ui| {
            ui.set_width(ui.available_width() - 8.0);
            // the UI font has no U+2192 (the arrow in "Settings → Editor"); the Markdown viewer cannot fall back for it
            let text = text.replace('→', "\u{203a}");
            egui_commonmark::CommonMarkViewer::new().max_image_width(Some(700)).show(ui, &mut cl.cache, &text);
        });
    });
    if refetch {
        f.state.changelog.fetched = None;
        crate::update::fetch_changelog(f.state, f.cx);
    }
    if !open {
        f.state.changelog.open = false;
    }
}

fn kernel_log_window(ctx: &egui::Context, f: &mut Frame<'_>) {
    if !f.state.kernel.log_open {
        return;
    }
    let theme = f.theme;
    let mut open = true;
    let k = &mut f.state.kernel;
    crate::ui::chrome::Window::new("Local Spark session").id(egui::Id::new("kernel-log")).open(&mut open).default_size([760.0, 380.0]).resizable(true).show(ctx, theme, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(k.state.label()).strong());
            if let crate::kernel::KernelState::Ready { info, .. } = &k.state {
                ui.label(RichText::new(format!("{} · {}", info.get("profile").and_then(|v| v.as_str()).unwrap_or(""), info.get("master").and_then(|v| v.as_str()).unwrap_or(""))).size(11.0).color(theme.text_muted));
            }
            if let crate::kernel::KernelState::Failed(e) = &k.state {
                ui.label(RichText::new(e).size(11.0).color(theme.error));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("Copy").clicked() {
                    ui.ctx().copy_text(k.log.iter().cloned().collect::<Vec<_>>().join("\n"));
                }
                if ui.small_button("Clear").clicked() {
                    k.log.clear();
                }
            });
        });
        egui::Frame::new().fill(theme.bg_editor).inner_margin(6.0).show(ui, |ui| {
            egui::ScrollArea::vertical().id_salt("kernel-log-scroll").stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
                ui.style_mut().override_font_id = Some(egui::FontId::monospace(11.0));
                for l in k.log.iter().rev().take(600).collect::<Vec<_>>().into_iter().rev() {
                    let color = if l.contains(" ERROR ") || l.contains("Traceback") || l.contains("Error:") { theme.error } else if l.contains(" WARN ") { theme.warning } else { theme.text_muted };
                    ui.add(egui::Label::new(RichText::new(l).color(color)).wrap());
                }
                if k.log.is_empty() {
                    ui.label(RichText::new("Nothing yet.").color(theme.text_faint));
                }
            });
        });
    });
    if !open {
        f.state.kernel.log_open = false;
    }
}

fn welcome(ui: &mut Ui, f: &mut Frame<'_>) {
    let theme = f.theme;
    ui.add_space(60.0);
    ui.vertical_centered(|ui| {
        ui.label(RichText::new(format!("{}  Cobalt SQL Works", icons::DATABASE)).size(28.0).color(theme.accent));
        ui.add_space(4.0);
        ui.label(RichText::new("Fast, focused SQL for SQL Server, Azure SQL and Fabric.").color(theme.text_muted));
        ui.add_space(24.0);
        ui.horizontal(|ui| {
            ui.add_space(ui.available_width() / 2.0 - 220.0);
            if ui.add(egui::Button::new(format!("{}  New query", icons::FILE_PLUS)).min_size(Vec2::new(140.0, 32.0))).clicked() {
                dispatch(f, Command::NewQuery);
            }
            if ui.add(egui::Button::new(format!("{}  New connection", icons::PLUG)).min_size(Vec2::new(140.0, 32.0))).clicked() {
                dispatch(f, Command::NewConnection);
            }
            if ui.add(egui::Button::new(format!("{}  Open file", icons::FOLDER_OPEN)).min_size(Vec2::new(140.0, 32.0))).clicked() {
                dispatch(f, Command::OpenFile);
            }
        });
        ui.add_space(24.0);
        let recent = f.cx.store.recent_profiles(6).unwrap_or_default();
        if !recent.is_empty() {
            ui.label(RichText::new("Recent connections").size(12.0).color(theme.text_muted));
            for p in recent {
                if ui.add(egui::Button::new(format!("{}  {}   {}", icons::HARD_DRIVES, p.display_name(), p.database.clone().unwrap_or_default())).frame(false)).clicked() {
                    ops::new_query_tab(f.state, f.cx, Some(p.id), None, None, false);
                }
            }
        }
        ui.add_space(24.0);
        ui.label(RichText::new(format!("{} Command palette   ·   F5 Run   ·   Ctrl+Enter Run statement   ·   Ctrl+L Estimated plan   ·   Ctrl+M Actual plan", f.keymap.shortcut_text(ui.ctx(), Command::Palette))).size(11.0).color(theme.text_faint));
    });
}

/// The getting-started pane shown beside a fresh, empty query tab (Settings → Appearance).
fn welcome_pane(ui: &mut Ui, f: &mut Frame<'_>, idx: usize) {
    let theme = f.theme;
    let tab_id = f.state.tabs[idx].id;
    egui::Frame::new().fill(theme.bg_sidebar).inner_margin(egui::Margin::symmetric(18, 14)).show(ui, |ui| {
        ui.set_min_size(ui.available_size());
        egui::ScrollArea::vertical().id_salt("welcome-pane").auto_shrink([false, false]).show(ui, |ui| {
            ui.label(RichText::new("Getting started").size(18.0).color(theme.accent));
            ui.add_space(8.0);
            let b = |ui: &mut Ui, icon: &str, label: &str| ui.add(egui::Button::new(format!("{icon}  {label}")).min_size(Vec2::new(230.0, 30.0))).clicked();
            if b(ui, icons::PLUG, "Connect this tab…") {
                dispatch(f, Command::ConnectTab);
            }
            if b(ui, icons::FOLDER_OPEN, "Open a .sql file or notebook…") {
                dispatch(f, Command::OpenFile);
            }
            if b(ui, icons::NOTEBOOK, "New notebook (SQL cells + Markdown)") {
                dispatch(f, Command::NewNotebook);
            }
            if b(ui, icons::UPLOAD_SIMPLE, "Import data from a file…") {
                dispatch(f, Command::ImportFile);
            }
            if b(ui, icons::CUBE, "Browse Fabric workspaces") {
                dispatch(f, Command::ShowFabric);
            }
            if b(ui, icons::KEYBOARD, "Keyboard shortcuts") {
                dispatch(f, Command::KeyboardShortcuts);
            }
            let recent = f.cx.store.recent_profiles(6).unwrap_or_default();
            if !recent.is_empty() {
                ui.add_space(12.0);
                ui.label(RichText::new("Recent connections").size(12.0).color(theme.text_muted));
                for p in recent {
                    let label = format!("{}  {}   {}", icons::HARD_DRIVES, p.display_name(), p.database.clone().unwrap_or_default());
                    if ui.add(egui::Button::new(label).frame(false)).clicked() {
                        ops::begin_connect(f.state, f.cx, p, ConnectPurpose::Tab { tab: tab_id, database: None });
                    }
                }
            }
            ui.add_space(12.0);
            ui.label(RichText::new("Tips").size(12.0).color(theme.text_muted));
            for tip in [
                "F5 runs the script, Ctrl+Enter the statement under the caret; a selection runs only the selection.",
                "Ctrl+Shift+O jumps to any table, view or procedure the app knows about.",
                "Alt+Click adds a cursor; Ctrl+D selects the next occurrence; Ctrl+Alt+Down adds a cursor below.",
                "Right-click a result grid: Save as table (any connected tab), Open in Excel, Profile columns, Totals row.",
                "Run to File streams a query straight into Parquet, Delta, CSV… or a OneLake lakehouse.",
                "Hover a table in Servers for its columns, row count and size; drag its name into the editor.",
            ] {
                ui.label(RichText::new(format!("•  {tip}")).size(12.0));
            }
            ui.add_space(14.0);
            if ui.add(egui::Button::new(RichText::new("Hide this pane").size(11.0)).frame(false)).on_hover_text("Settings → Appearance brings it back; Help → Welcome opens a tab with it").clicked() {
                f.state.settings_patch.push(SettingsPatch::ShowWelcome(false));
            }
        });
    });
}

fn editor_area(ui: &mut Ui, f: &mut Frame<'_>) {
    let theme = f.theme;
    let idx = f.state.active_tab.unwrap();
    if f.state.tabs[idx].is_notebook() {
        crate::ui::notebook::show(ui, f, idx);
        return;
    }
    // a fresh, empty tab gets the getting-started pane on its right (until something is typed)
    let pristine = {
        let t = &f.state.tabs[idx];
        t.text.is_empty() && t.profile.is_none() && t.run.is_none() && !t.custom_title && t.file_path.is_none() && !t.conn.is_connected()
    };
    if pristine && f.cx.settings.appearance.show_welcome {
        let avail = ui.available_rect_before_wrap();
        let pane_w = (avail.width() * 0.42).clamp(260.0, 420.0);
        let pane_rect = egui::Rect::from_min_max(egui::pos2(avail.right() - pane_w, avail.top()), avail.max);
        let editor_rect = egui::Rect::from_min_max(avail.min, egui::pos2(avail.right() - pane_w, avail.bottom()));
        let mut pane_ui = ui.new_child(egui::UiBuilder::new().max_rect(pane_rect).layout(egui::Layout::top_down(egui::Align::Min)));
        pane_ui.set_clip_rect(pane_rect);
        welcome_pane(&mut pane_ui, f, idx);
        if f.state.active_tab != Some(idx) || f.state.tabs.len() <= idx {
            return;
        }
        let mut ed_ui = ui.new_child(egui::UiBuilder::new().max_rect(editor_rect).layout(egui::Layout::top_down(egui::Align::Min)));
        ed_ui.set_clip_rect(editor_rect);
        let tab = &mut f.state.tabs[idx];
        let out = editor::show(&mut ed_ui, tab, theme, f.cx.settings);
        if out.focused {
            f.state.focus = Focus::Editor;
        }
        return;
    }
    editor_toolbar(ui, f, idx);
    let avail = ui.available_rect_before_wrap();
    let tab = &mut f.state.tabs[idx];
    let results_visible = tab.results_visible && tab.run.is_some();
    // "Maximize this result set" gives the results pane the whole tab: the editor is hidden until
    // the user restores (same button, context menu, or Escape from the grid).
    let maximized = results_visible && tab.run.as_ref().map(|r| r.maximized.is_some()).unwrap_or(false);
    let frac = tab.results_fraction.clamp(0.1, 0.92);
    let sep_h = 6.0;
    let editor_h = if maximized { 0.0 } else if results_visible { (avail.height() * (1.0 - frac) - sep_h / 2.0).max(60.0) } else { avail.height() };
    let editor_rect = egui::Rect::from_min_size(avail.min, Vec2::new(avail.width(), editor_h));
    if !maximized {
        let mut ed_ui = ui.new_child(egui::UiBuilder::new().max_rect(editor_rect).layout(egui::Layout::top_down(egui::Align::Min)));
        ed_ui.set_clip_rect(editor_rect);
        let out = editor::show(&mut ed_ui, tab, theme, f.cx.settings);
        if let Some(n) = out.notice {
            f.cx.toast(ToastKind::Info, n);
        }
        if out.focused {
            f.state.focus = Focus::Editor;
            for rs in tab.run.iter_mut().flat_map(|r| r.result_sets.iter_mut()) {
                rs.grid.focused = false;
            }
        }
    }
    if results_visible {
        let sep_rect = if maximized {
            egui::Rect::from_min_size(avail.min, Vec2::new(avail.width(), 0.0))
        } else {
            // separator
            let sep_rect = egui::Rect::from_min_size(egui::pos2(avail.left(), editor_rect.bottom()), Vec2::new(avail.width(), sep_h));
            let sep = ui.interact(sep_rect, egui::Id::new(("split", tab.id)), Sense::drag());
            ui.painter().rect_filled(sep_rect, 0.0, if sep.hovered() || sep.dragged() { theme.accent } else { theme.border });
            if sep.hovered() || sep.dragged() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
            }
            if sep.dragged() {
                let dy = sep.drag_delta().y;
                let new_editor_h = (editor_h + dy).clamp(60.0, avail.height() - 80.0);
                tab.results_fraction = 1.0 - (new_editor_h + sep_h / 2.0) / avail.height();
            }
            sep_rect
        };
        let res_rect = egui::Rect::from_min_max(egui::pos2(avail.left(), sep_rect.bottom()), avail.max);
        let mut res_ui = ui.new_child(egui::UiBuilder::new().max_rect(res_rect).layout(egui::Layout::top_down(egui::Align::Min)));
        res_ui.set_clip_rect(res_rect);
        if tab.results_tab == ResultsTab::Plan {
            // plan body drawn separately (needs the tab strip from results::show first)
        }
        let actions = results::show(&mut res_ui, results::ResultsArgs { tab, theme, settings: f.cx.settings, fmt: &f.state.formatter });
        if f.state.tabs[idx].results_tab == ResultsTab::Plan {
            let tab = &mut f.state.tabs[idx];
            let body = egui::Rect::from_min_max(egui::pos2(res_rect.left(), res_rect.top() + 30.0), res_rect.max);
            let mut plan_ui = ui.new_child(egui::UiBuilder::new().max_rect(body).layout(egui::Layout::top_down(egui::Align::Min)));
            plan_ui.set_clip_rect(body);
            plan::show(&mut plan_ui, tab, theme, f.cx.settings);
        }
        for a in actions {
            ops::results_action(f.state, f.cx, idx, a);
        }
        // grid focus bookkeeping
        let tab = &mut f.state.tabs[idx];
        if let Some(r) = &tab.run {
            if r.result_sets.iter().any(|s| s.grid.focused) {
                f.state.focus = Focus::Results;
            }
        }
        // cell viewer window
        let viewer_req = tab.run.as_mut().and_then(|r| r.result_sets.iter_mut().enumerate().find_map(|(i, s)| s.grid.viewer.map(|(row, col)| (i, row, col))));
        if let Some((set, row, col)) = viewer_req {
            let rs = tab.run.as_ref().unwrap().result_sets[set].rs.clone();
            let id = egui::Id::new(("viewer", tab.id));
            use crate::ui::results::viewer::ViewerOutcome;
            let want_record = std::mem::take(&mut tab.run.as_mut().unwrap().result_sets[set].grid.viewer_record);
            let mut vs: ViewerState = ui.ctx().memory(|m| m.data.get_temp::<std::sync::Arc<parking_lot::Mutex<Option<ViewerState>>>>(id)).and_then(|m| m.lock().take()).filter(|v| Arc::ptr_eq(&v.rs, &rs) && v.row == row && v.col == col).unwrap_or_else(|| ViewerState::new(rs.clone(), row, col));
            if want_record {
                vs.record = true;
            }
            let mut open = true;
            let mut outcome = ViewerOutcome::Open;
            crate::ui::chrome::Window::new(if vs.record { "Record" } else { "Cell value" }).id(id).open(&mut open).default_size([560.0, 420.0]).resizable(true).show(ui.ctx(), theme, |ui| {
                outcome = crate::ui::results::viewer::show(ui, theme, &mut vs);
            });
            let grid = &mut tab.run.as_mut().unwrap().result_sets[set].grid;
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
    }
}

use std::sync::Arc;

/// A Spark SQL tab's toolbar segment: the session chip (state, start/stop/restart, log) and the
/// lakehouse chip (workspace, default lakehouse, write mode), the way a Spark notebook has them.
fn spark_toolbar(ui: &mut Ui, f: &mut Frame<'_>, idx: usize, cmds: &mut Vec<Command>) {
    let theme = f.theme;
    let tab_id = f.state.tabs[idx].id;
    let (label, ready) = crate::sparkq::session_label(f.state);
    let busy_here = f.state.kernel.busy.as_ref().map(|(t, _)| *t == tab_id).unwrap_or(false);
    let color = if busy_here { theme.accent } else if ready { theme.success } else if f.state.kernel.state.is_starting() { theme.warning } else { theme.text_muted };
    let r = ui.add(egui::Button::new(RichText::new(format!("{} {label}", icons::FIRE)).size(12.0).color(color)).small());
    r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "spark session"));
    let r = r.on_hover_text("The local Spark session this tab runs on (shared with Spark notebooks; one context per tab).");
    let mut session_cmd: Option<Command> = None;
    let mut show_log = false;
    let mut start = false;
    egui::Popup::menu(&r).id(egui::Id::new(("spark-session-menu", tab_id))).show(|ui| {
        ui.set_min_width(240.0);
        let k = &f.state.kernel.state;
        ui.label(RichText::new(format!("Session · {}", k.label())).strong());
        let running = k.is_ready() || k.is_starting();
        if !running && ui.button(format!("{} Start session", icons::PLAY)).clicked() {
            start = true;
            ui.close();
        }
        if running && ui.add_enabled(!k.is_starting(), egui::Button::new(format!("{} Restart session", icons::ARROWS_CLOCKWISE))).clicked() {
            session_cmd = Some(Command::KernelRestart);
            ui.close();
        }
        if ui.add_enabled(running, egui::Button::new(format!("{} Stop session", icons::STOP))).clicked() {
            session_cmd = Some(Command::KernelStop);
            ui.close();
        }
        if f.state.kernel.busy.is_some() && ui.button(format!("{} Interrupt the running statement", icons::HAND_PALM)).clicked() {
            session_cmd = Some(Command::CancelQuery);
            ui.close();
        }
        ui.separator();
        if ui.button("Session log").clicked() {
            show_log = true;
            ui.close();
        }
        if ui.button("Lakehouse shadows…").clicked() {
            session_cmd = Some(Command::Shadows);
            ui.close();
        }
    });
    if let Some(c) = session_cmd {
        cmds.push(c);
    }
    if show_log {
        f.state.kernel.log_open = true;
    }
    if start {
        // through the tab's binding (lakehouse, OneLake token), not a plain session
        let _ = crate::notebook::ensure_session(f.state, f.cx, idx);
    }
    // lakehouse chip
    let (binding, ws_name, lh_name) = {
        let s = f.state.tabs[idx].spark.as_ref().unwrap();
        (s.binding.clone(), s.workspace_name.clone(), s.lakehouse_name.clone())
    };
    let signed_in = f.state.fabric.slot.is_some();
    let (label, color) = match &binding {
        Some(_) => (format!("{} {}{}", icons::DROP, lh_name.clone().unwrap_or_else(|| "no default lakehouse".into()), if ws_name.is_empty() { String::new() } else { format!(" ({ws_name})") }), theme.text),
        None => (format!("{} no lakehouse", icons::DROP), theme.text_muted),
    };
    let r = ui.add(egui::Button::new(RichText::new(label).size(12.0).color(color)).small());
    r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "spark lakehouse"));
    let r = r.on_hover_text("Which workspace's lakehouses the session sees, the default lakehouse for unqualified names, and the write mode. Takes effect at the next run.");
    let popup_id = egui::Id::new(("spark-lakehouse-menu", tab_id));
    if f.state.open_spark_chip == Some(tab_id) {
        f.state.open_spark_chip = None;
        egui::Popup::open_id(ui.ctx(), popup_id);
    }
    let workspaces: Vec<(String, String)> = f.state.fabric.workspaces.get().map(|v| v.iter().map(|w| (w.id.clone(), w.display_name.clone())).collect()).unwrap_or_default();
    let ws_lakehouses: Option<Vec<(String, String)>> = binding.as_ref().and_then(|b| f.state.fabric.lakehouses(&b.workspace_id));
    let mut set: Option<Option<crate::state::NotebookFabric>> = None;
    let mut load_ws: Option<String> = None;
    let mut need_workspaces = false;
    let mut show_pane = false;
    egui::Popup::menu(&r).id(popup_id).show(|ui| {
        ui.set_min_width(320.0);
        if !signed_in {
            ui.label(RichText::new("Sign in on the Fabric panel to bind a lakehouse.").color(theme.text_muted));
            if ui.button("Open the Fabric panel").clicked() {
                cmds.push(Command::ShowFabric);
                ui.close();
            }
        }
        ui.label(RichText::new("Workspace").strong());
        let cur_ws = binding.as_ref().map(|b| b.workspace_id.clone());
        if ui.selectable_label(cur_ws.is_none(), "None — plain local Spark").clicked() {
            set = Some(None);
            ui.close();
        }
        for (id, name) in &workspaces {
            if ui.selectable_label(cur_ws.as_deref() == Some(id.as_str()), name).clicked() {
                set = Some(Some(crate::state::NotebookFabric { workspace_id: id.clone(), lakehouse_id: None, write_mode: binding.as_ref().map(|b| b.write_mode.clone()).unwrap_or_else(|| "sandbox".into()), preload: false }));
                load_ws = Some(id.clone());
                ui.close();
            }
        }
        if workspaces.is_empty() && signed_in {
            ui.label(RichText::new("Loading workspaces…").size(11.0).color(theme.text_faint));
            need_workspaces = true;
        }
        if let Some(b) = &binding {
            ui.separator();
            ui.label(RichText::new("Default lakehouse").strong());
            match &ws_lakehouses {
                Some(lhs) => {
                    if ui.selectable_label(b.lakehouse_id.is_none(), "None").clicked() {
                        set = Some(Some(crate::state::NotebookFabric { lakehouse_id: None, ..b.clone() }));
                        ui.close();
                    }
                    for (name, id) in lhs {
                        if ui.selectable_label(b.lakehouse_id.as_deref() == Some(id.as_str()), name).clicked() {
                            set = Some(Some(crate::state::NotebookFabric { lakehouse_id: Some(id.clone()), ..b.clone() }));
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
            for (mode, label, hint) in [("sandbox", "Sandbox — writes go to local shallow clones", "Default. OneLake is never written."), ("readonly", "Read only — writes fail", "Reads from OneLake; any write raises."), ("writethrough", "Write through — writes go to OneLake", "Writes land in the lakehouse. Needs a session started in this mode.")] {
                if ui.selectable_label(b.write_mode == mode, label).on_hover_text(hint).clicked() {
                    set = Some(Some(crate::state::NotebookFabric { write_mode: mode.into(), ..b.clone() }));
                    ui.close();
                }
            }
            ui.separator();
            if ui.button(format!("{} Lakehouse pane", icons::SIDEBAR_SIMPLE)).on_hover_text("Tables and Files of this lakehouse in the sidebar").clicked() {
                show_pane = true;
                ui.close();
            }
        }
    });
    if need_workspaces && f.state.fabric.workspaces.get().is_none() && !f.state.fabric.workspaces.is_loading() {
        crate::fabric::load_workspaces(f.state, f.cx);
    }
    if let Some(ws) = load_ws {
        if f.state.fabric.lakehouses(&ws).is_none() && !f.state.fabric.items.get(&ws).map(|l| l.is_loading()).unwrap_or(false) {
            crate::fabric::load_items(f.state, f.cx, &ws);
        }
    }
    if let Some(b) = set {
        crate::sparkq::set_binding(f.state, f.cx, idx, b);
    } else {
        crate::sparkq::refresh_names(f.state, idx);
    }
    if show_pane {
        cmds.push(Command::ShowLakehouse);
    }
    ui.separator();
}

fn editor_toolbar(ui: &mut Ui, f: &mut Frame<'_>, idx: usize) {
    let theme = f.theme;
    let mut cmds: Vec<Command> = Vec::new();
    let mut change_db: Option<String> = None;
    egui::Frame::new().fill(theme.bg_panel).inner_margin(egui::Margin::symmetric(6, 3)).show(ui, |ui| {
        ui.horizontal(|ui| {
            let tab = &f.state.tabs[idx];
            let running = tab.is_running();
            let connected = tab.conn.is_connected();
            if tool_button(ui, icons::PLAY, "Run", &format!("Run (F5) · current statement: {}", f.keymap.shortcut_text(ui.ctx(), Command::RunCurrentStatement)), !running).clicked() {
                cmds.push(Command::RunQuery);
            }
            if tool_button(ui, icons::STOP, "Cancel", "Cancel (Alt+Break)", running).clicked() {
                cmds.push(Command::CancelQuery);
            }
            if tool_button(ui, icons::EXPORT, "Run to file", &format!("Run the query straight into a file or lakehouse, without filling the grid ({})", f.keymap.shortcut_text(ui.ctx(), Command::RunToFile)), !running).clicked() {
                cmds.push(Command::RunToFile);
            }
            ui.separator();
            if tab.spark.is_some() {
                spark_toolbar(ui, f, idx, &mut cmds);
            }
            let tab = &f.state.tabs[idx];
            if tab.spark.is_some() {
                // no connection, database list or plans on a Spark tab
            } else {
            match &tab.conn {
                ConnState::Connected { engine, spid, .. } => {
                    let name = tab.profile.as_ref().map(|p| p.display_name()).unwrap_or_default();
                    let color = tab.profile.as_ref().and_then(|p| f.state.library.color_for(p)).map(Theme::color32).unwrap_or(theme.success);
                    ui.label(RichText::new(icons::PLUGS_CONNECTED).color(color));
                    let r = ui.add(egui::Button::new(RichText::new(&name).size(13.0)).frame(false)).on_hover_text(format!("{}\n{}\nSPID {}", engine.short_label(), engine.version, spid.map(|s| s.to_string()).unwrap_or_default()));
                    if r.clicked() {
                        cmds.push(Command::ChangeConnection);
                    }
                    if tool_button(ui, icons::PLUGS, "Disconnect", "Disconnect", !running).clicked() {
                        cmds.push(Command::DisconnectTab);
                    }
                }
                ConnState::Connecting => {
                    ui.add(egui::Spinner::new().size(14.0));
                    ui.label(RichText::new("Connecting…").color(theme.text_muted));
                }
                ConnState::Failed { error, hint } => {
                    let r = ui.add(egui::Button::new(RichText::new(format!("{} Connection failed", icons::WARNING)).color(theme.error)).frame(false));
                    r.on_hover_text(format!("{error}{}", hint.as_ref().map(|h| format!("\n\n{h}")).unwrap_or_default()));
                    if tool_button(ui, icons::PLUG, "Connect", "Connect", true).clicked() {
                        cmds.push(Command::ConnectTab);
                    }
                }
                ConnState::Disconnected => {
                    let label = tab.profile.as_ref().map(|p| format!("Connect to {}", p.display_name())).unwrap_or_else(|| "Connect…".into());
                    if tool_button(ui, icons::PLUG, &label, "Connect (or choose a connection)", true).clicked() {
                        cmds.push(if tab.profile.is_some() { Command::ConnectTab } else { Command::ChangeConnection });
                    }
                }
            }
            // database dropdown
            if connected {
                let current = tab.conn.database().unwrap_or("").to_string();
                let caps = match &tab.conn {
                    ConnState::Connected { engine, .. } => engine.capabilities,
                    _ => EngineKind::SqlServer.capabilities(),
                };
                ui.separator();
                ui.label(RichText::new(icons::DATABASE).color(theme.text_muted));
                let mut dbs: Vec<String> = tab.databases.get().map(|d| d.iter().filter(|x| !x.is_system || x.name == current).map(|x| x.name.clone()).collect()).unwrap_or_default();
                if !dbs.contains(&current) && !current.is_empty() {
                    dbs.insert(0, current.clone());
                }
                let load_error = match &tab.databases {
                    Loadable::Failed(e) => Some(e.clone()),
                    _ => None,
                };
                let loading = tab.databases.is_loading();
                // Always a switcher when connected: engines without USE reconnect on selection, and a
                // failed list still shows the current database plus a way to retry.
                let r = egui::ComboBox::from_id_salt(("dbcombo", tab.id)).selected_text(&current).width(180.0).show_ui(ui, |ui| {
                    for d in &dbs {
                        if ui.selectable_label(*d == current, d).clicked() && *d != current {
                            change_db = Some(d.clone());
                        }
                    }
                    if !caps.multiple_databases && dbs.len() > 1 {
                        ui.label(RichText::new("Switching reconnects on this engine").small().weak());
                    }
                    ui.separator();
                    if loading {
                        ui.label(RichText::new("Loading databases…").small().weak());
                    } else if ui.selectable_label(false, format!("{} Refresh list", icons::ARROWS_CLOCKWISE)).clicked() {
                        cmds.push(Command::RefreshDatabases);
                        ui.close();
                    }
                });
                if let Some(e) = load_error {
                    r.response.on_hover_text(format!("Could not list databases: {e}\nUse Refresh list to try again."));
                }
            }
            ui.separator();
            if tool_button(ui, icons::TREE_STRUCTURE, "Est. plan", "Display estimated execution plan (Ctrl+L)", !running).clicked() {
                cmds.push(Command::EstimatedPlan);
            }
            let actual_supported = tab.conn.capabilities().map(|c| c.actual_plans).unwrap_or(true);
            let actual = tab.actual_plan && actual_supported;
            let r = ui.add_enabled(!running && actual_supported, egui::Button::new(RichText::new(format!("{} Actual plan", if actual { icons::CHECK_SQUARE } else { icons::SQUARE })).color(if actual { theme.accent } else { theme.text })).frame(false).min_size(Vec2::new(0.0, 24.0)));
            if r.on_hover_text(if actual_supported { "Include actual execution plan (Ctrl+M)" } else { "This engine does not support actual execution plans" }).clicked() {
                cmds.push(Command::ToggleActualPlan);
            }
            if tool_button(ui, icons::CHECK, "Parse", "Parse (Shift+Alt+P)", !running && connected).clicked() {
                cmds.push(Command::ParseQuery);
            }
            }
            if tool_button(ui, icons::TEXT_INDENT, "Format", "Format document (Shift+Alt+F)", true).clicked() {
                cmds.push(Command::FormatDocument);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if icon_button(ui, icons::SLIDERS_HORIZONTAL, "Execution options", true).clicked() {
                    cmds.push(Command::ExecutionOptions);
                }
                let has_results = tab.run.is_some();
                if icon_button(ui, if tab.results_visible { icons::ARROWS_IN_LINE_VERTICAL } else { icons::ARROWS_OUT_LINE_VERTICAL }, "Toggle results (Ctrl+Shift+R)", has_results).clicked() {
                    cmds.push(Command::ToggleResults);
                }
                if let Some(p) = &tab.profile {
                    if p.read_only_guard {
                        ui.label(RichText::new(format!("{} read-only guard", icons::SHIELD_CHECK)).size(11.0).color(theme.warning)).on_hover_text("Non-read statements ask for confirmation on this connection.");
                    }
                }
            });
        });
    });
    ui.painter().line_segment([ui.min_rect().left_bottom(), ui.min_rect().right_bottom()], Stroke::new(1.0, theme.border));
    if let Some(db) = change_db {
        ops::change_database(f.state, f.cx, idx, db);
    }
    for c in cmds {
        dispatch(f, c);
    }
}

fn status_bar(ui: &mut Ui, f: &mut Frame<'_>) {
    let theme = f.theme;
    egui::Panel::bottom("status").resizable(false).show_separator_line(false).frame(egui::Frame::new().fill(theme.bg_sidebar).inner_margin(egui::Margin::symmetric(10, 3))).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.style_mut().override_font_id = Some(egui::FontId::proportional(12.0));
            let tab = f.state.active();
            if let Some(t) = tab {
                if let Some(sp) = &t.spark {
                    let (label, ready) = crate::sparkq::session_label(f.state);
                    ui.label(RichText::new(icons::CIRCLE).color(if ready { theme.success } else { theme.text_muted }).size(9.0));
                    match (&sp.lakehouse_name, sp.workspace_name.is_empty()) {
                        (Some(lh), false) => ui.label(format!("{lh} ({})", sp.workspace_name)),
                        (Some(lh), true) => ui.label(lh.clone()),
                        (None, _) => ui.label("no lakehouse"),
                    };
                    ui.label(RichText::new(label).color(theme.text_muted));
                } else {
                match &t.conn {
                    ConnState::Connected { engine, spid, database } => {
                        let color = t.profile.as_ref().and_then(|p| f.state.library.color_for(p)).map(Theme::color32).unwrap_or(theme.success);
                        ui.label(RichText::new(icons::CIRCLE).color(color).size(9.0));
                        let name = t.profile.as_ref().map(|p| p.display_name()).unwrap_or_default();
                        ui.label(format!("{name} · {database}"));
                        ui.label(RichText::new(format!("{}{}", engine.short_label(), spid.map(|s| format!(" · SPID {s}")).unwrap_or_default())).color(theme.text_muted));
                    }
                    ConnState::Connecting => {
                        ui.label(RichText::new("Connecting…").color(theme.text_muted));
                    }
                    _ => {
                        ui.label(RichText::new("Not connected").color(theme.text_muted));
                    }
                }
                }
                if let Some(s) = crate::ui::notebook::status_summary(t) {
                    ui.separator();
                    ui.label(RichText::new(s).color(theme.text_muted));
                }
                if let Some(r) = &t.run {
                    ui.separator();
                    let elapsed = if r.is_live() && r.state != RunViewState::Paused { r.started.elapsed() } else { r.elapsed };
                    let label = if r.is_live() { format!("{} {}", icons::TIMER, fmt_duration(elapsed)) } else { fmt_duration(elapsed) };
                    ui.label(RichText::new(label).color(if r.is_live() { theme.accent } else { theme.text }));
                    let rows: u64 = r.result_sets.iter().filter(|s| !s.is_plan).map(|s| s.rs.row_count() as u64).sum();
                    let affected: u64 = r.rows_affected.iter().sum();
                    if rows > 0 || affected == 0 {
                        ui.label(format!("{} rows", fmt_count(rows)));
                    } else {
                        ui.label(format!("{} rows affected", fmt_count(affected)));
                    }
                    // selection summary
                    if let Some(s) = r.result_sets.iter().find(|s| s.grid.focused && s.grid.selection.cell_count(s.rs.visible_count(), s.rs.column_count()) > 1) {
                        let (rows_r, cols_r) = s.grid.selection.resolve(s.rs.visible_count(), s.rs.column_count()).unwrap();
                        let count = (rows_r.end() - rows_r.start() + 1) * (cols_r.end() - cols_r.start() + 1);
                        if count <= 200_000 {
                            let cells = rows_r.flat_map(|r| cols_r.clone().map(move |c| (r, c)));
                            let sum = cobalt_results::summarize(&s.rs, cells, 200_000);
                            ui.separator();
                            if sum.is_numeric() {
                                ui.label(RichText::new(format!("Sum {}  Avg {}  Min {}  Max {}  Count {}", fmt_num(sum.sum.unwrap_or(0.0)), fmt_num(sum.avg.unwrap_or(0.0)), fmt_num(sum.min.unwrap_or(0.0)), fmt_num(sum.max.unwrap_or(0.0)), sum.count)).color(theme.text_muted));
                            } else {
                                ui.label(RichText::new(format!("Count {}  Distinct {}  Nulls {}", sum.count, sum.distinct, sum.nulls)).color(theme.text_muted));
                            }
                        }
                    }
                }
            }
            if let Some((msg, at)) = &f.state.status_flash {
                if at.elapsed().as_secs_f32() < 4.0 {
                    ui.separator();
                    ui.label(RichText::new(msg).color(theme.accent));
                    ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // the local Spark kernel, once anything has used it
                let k = &f.state.kernel;
                let show_kernel = !matches!(k.state, crate::kernel::KernelState::Stopped) || f.state.tabs.iter().any(|t| t.notebook.as_deref().map(|nb| nb.kernel == NotebookKernel::Spark).unwrap_or(false));
                if show_kernel {
                    let (color, icon) = match &k.state {
                        crate::kernel::KernelState::Ready { .. } if k.busy.is_some() => (theme.accent, icons::CIRCLE_NOTCH),
                        crate::kernel::KernelState::Ready { .. } => (theme.success, icons::CIRCLE),
                        crate::kernel::KernelState::Starting { .. } => (theme.warning, icons::CIRCLE_NOTCH),
                        crate::kernel::KernelState::Failed(_) => (theme.error, icons::WARNING),
                        crate::kernel::KernelState::Stopped => (theme.text_faint, icons::CIRCLE),
                    };
                    let mut label = if k.busy.is_some() { format!("{} · running a cell", k.state.label()) } else { k.state.label() };
                    if !k.contexts.is_empty() {
                        label.push_str(&format!(" · {} tab{}", k.contexts.len(), if k.contexts.len() == 1 { "" } else { "s" }));
                    }
                    let r = ui.add(egui::Button::new(RichText::new(format!("{icon} {label}")).color(color)).frame(false));
                    r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "spark kernel"));
                    let hover = match &k.state {
                        crate::kernel::KernelState::Ready { info, .. } => format!("Local Spark session ({})\nprofile {} · app {}\nClick for restart / stop / log", k.profile, info.get("profile").and_then(|v| v.as_str()).unwrap_or(""), info.get("app_id").and_then(|v| v.as_str()).unwrap_or("")),
                        crate::kernel::KernelState::Failed(e) => format!("Local Spark session failed:\n{e}"),
                        _ => "Local Spark session · click for restart / stop / log".to_string(),
                    };
                    let r = r.on_hover_text(hover);
                    let mut cmd: Option<Command> = None;
                    r.context_menu(|ui| {
                        if ui.button("Restart session").clicked() {
                            cmd = Some(Command::KernelRestart);
                            ui.close();
                        }
                        if ui.button("Stop session").clicked() {
                            cmd = Some(Command::KernelStop);
                            ui.close();
                        }
                        if ui.button("Show log").clicked() {
                            cmd = Some(Command::KernelLog);
                            ui.close();
                        }
                        if ui.button("Lakehouse shadows…").clicked() {
                            cmd = Some(Command::Shadows);
                            ui.close();
                        }
                    });
                    if r.clicked() {
                        cmd = Some(Command::KernelLog);
                    }
                    if let Some(c) = cmd {
                        f.state.pending_commands.push(c);
                    }
                    ui.separator();
                }
                if let Some(t) = f.state.active() {
                    ui.label(RichText::new(if t.spark.is_some() { "Spark SQL" } else { "MSSQL" }).color(theme.text_muted));
                    if crate::gpu::is_software() {
                        ui.separator();
                        ui.label(RichText::new(format!("{} software rendering", icons::CPU)).color(theme.warning)).on_hover_text(format!("No GPU is available, so frames are drawn on the CPU.\nRenderer: {}", crate::gpu::adapter_label().unwrap_or("?")));
                    }
                    ui.separator();
                    ui.label(editor::widget::status_text(&t.text, &t.editor.cursors));
                    ui.separator();
                    if let Some(p) = &t.file_path {
                        ui.label(RichText::new(p.display().to_string()).color(theme.text_faint));
                    }
                }
                if let Some(text) = f.state.active_mut().and_then(selection_summary) {
                    ui.separator();
                    ui.label(RichText::new(text).color(theme.text)).on_hover_text("Aggregates over the selected cells (first 200,000 cells)");
                }
                if let Some(p) = &f.state.export_progress {
                    let (done, total) = *p.lock();
                    ui.separator();
                    let text = if total == 0 { format!("{} Exporting {} rows", icons::EXPORT, fmt_count(done as u64)) } else { format!("{} Exporting {} / {}", icons::EXPORT, fmt_count(done as u64), fmt_count(total as u64)) };
                    ui.label(RichText::new(text).color(theme.accent));
                }
            });
        });
    });
}

/// Excel-style aggregates for the focused result set's selection, cached on the grid.
pub(crate) fn selection_summary(t: &mut EditorTab) -> Option<String> {
    let run = t.run.as_mut()?;
    let pos = run.result_sets.iter().position(|s| !s.is_plan && s.grid.focused).or_else(|| run.result_sets.iter().position(|s| !s.is_plan))?;
    let v = &mut run.result_sets[pos];
    let sel = v.grid.selection.clone();
    if matches!(sel, Selection::None) {
        return None;
    }
    let rows = v.rs.visible_count();
    let (r, c) = sel.resolve(rows, v.rs.column_count())?;
    let cells = (r.end() - r.start() + 1) * (c.end() - c.start() + 1);
    if cells < 2 {
        return None;
    }
    let generation = v.grid.view_generation;
    let fresh = match &v.grid.summary {
        Some((s, g, n, _)) => *s == sel && *g == generation && *n == rows,
        None => false,
    };
    if !fresh {
        let (r2, c2) = (r.clone(), c.clone());
        let summary = cobalt_results::summary::summarize(&v.rs, r2.flat_map(move |row| c2.clone().map(move |col| (row, col))), 200_000);
        v.grid.summary = Some((sel.clone(), generation, rows, summary));
    }
    let s = &v.grid.summary.as_ref()?.3;
    Some(summary_text(s))
}

/// The summary already computed for the focused set (read-only; for the agent's `state`).
#[cfg(feature = "agent")]
pub(crate) fn cached_summary(t: &EditorTab) -> Option<String> {
    let run = t.run.as_ref()?;
    let v = run.result_sets.iter().find(|s| !s.is_plan && s.grid.focused).or_else(|| run.result_sets.iter().find(|s| !s.is_plan))?;
    let (sel, _, _, s) = v.grid.summary.as_ref()?;
    if *sel != v.grid.selection {
        return None;
    }
    Some(summary_text(s))
}

fn summary_text(s: &cobalt_results::summary::Summary) -> String {
    let mut parts = vec![format!("Count {}", fmt_count(s.count as u64))];
    if let (Some(sum), Some(avg), Some(min), Some(max)) = (s.sum, s.avg, s.min, s.max) {
        parts.push(format!("Sum {}", fmt_num(sum)));
        parts.push(format!("Avg {}", fmt_num(avg)));
        parts.push(format!("Min {}", fmt_num(min)));
        parts.push(format!("Max {}", fmt_num(max)));
    }
    parts.push(format!("Distinct {}", fmt_count(s.distinct as u64)));
    if s.nulls > 0 {
        parts.push(format!("Null {}", fmt_count(s.nulls as u64)));
    }
    parts.join("  ·  ")
}

fn fmt_num(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        fmt_count(v.abs() as u64).pipe(|s| if v < 0.0 { format!("-{s}") } else { s })
    } else {
        format!("{v:.4}").trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

trait Pipe: Sized {
    fn pipe<R>(self, f: impl FnOnce(Self) -> R) -> R {
        f(self)
    }
}
impl<T> Pipe for T {}

/// Execute a command against the current state.
pub fn dispatch(f: &mut Frame<'_>, cmd: Command) {
    let state = &mut *f.state;
    let cx = f.cx;
    let idx = state.active_tab;
    match cmd {
        Command::NewQuery => {
            // inherit the active tab's connection
            let (profile, db) = state.active().map(|t| (t.profile.as_ref().map(|p| p.id), t.conn.database().map(str::to_string))).unwrap_or((None, None));
            ops::new_query_tab(state, cx, profile, db, None, false);
        }
        Command::NewSparkQuery => {
            let (binding, names) = crate::sparkq::inherited_binding(state);
            crate::sparkq::new_tab(state, cx, binding, names, None);
        }
        Command::NewNotebook => {
            let lang = if cx.settings.notebooks.default_language.eq_ignore_ascii_case("pyspark") { cobalt_notebook::CellLanguage::Python } else { cobalt_notebook::CellLanguage::Sql };
            crate::notebook::new_tab(state, cx, lang);
        }
        Command::ExportNotebookHtml => {
            if let Some(i) = idx {
                if state.tabs[i].is_notebook() {
                    crate::notebook::export(state, cx, i, crate::notebook::ExportKind::Html, None);
                } else {
                    cx.toast(ToastKind::Info, "Open a notebook tab to export it.");
                }
            }
        }
        Command::ExportNotebookMarkdown => {
            if let Some(i) = idx {
                if state.tabs[i].is_notebook() {
                    crate::notebook::export(state, cx, i, crate::notebook::ExportKind::Markdown, None);
                } else {
                    cx.toast(ToastKind::Info, "Open a notebook tab to export it.");
                }
            }
        }
        Command::RunAllCells => {
            if let Some(i) = idx {
                if state.tabs[i].is_notebook() {
                    ops::run(state, cx, i, RunMode::All);
                }
            }
        }
        Command::RunCellsAbove => {
            if let Some(i) = idx {
                if let Some(sel) = state.tabs[i].notebook.as_deref().map(|nb| nb.selected) {
                    crate::notebook::run_cells(state, cx, i, (0..sel).collect());
                }
            }
        }
        Command::RunCellsBelow => {
            if let Some(i) = idx {
                if let Some((sel, n)) = state.tabs[i].notebook.as_deref().map(|nb| (nb.selected, nb.cells.len())) {
                    crate::notebook::run_cells(state, cx, i, (sel..n).collect());
                }
            }
        }
        Command::RunCellSelection => {
            if let Some(i) = idx {
                match state.tabs[i].notebook.as_deref().map(|nb| nb.selected) {
                    Some(sel) => crate::notebook::run_selection(state, cx, i, sel),
                    // in a query tab: the selection, as F5 does
                    None => ops::run(state, cx, i, RunMode::Selection),
                }
            }
        }
        Command::AddCodeCell | Command::AddMarkdownCell => {
            if let Some(i) = idx {
                if let Some(sel) = state.tabs[i].notebook.as_deref().map(|nb| nb.selected) {
                    let kind = if cmd == Command::AddCodeCell { cobalt_notebook::CellKind::Code } else { cobalt_notebook::CellKind::Markdown };
                    crate::notebook::insert_cell(state, i, sel + 1, kind, true);
                }
            }
        }
        Command::SparkRuntime => {
            state.settings_open = true;
            state.settings_scroll_to = Some("Spark runtime");
        }
        Command::KernelRestart => {
            crate::kernel::stop(&mut state.kernel);
            state.kernel_restart_pending = true;
        }
        Command::Shadows => {
            state.shadows.open = !state.shadows.open;
            if state.shadows.open {
                crate::notebook::refresh_shadows(state);
            }
        }
        Command::KernelStop => crate::kernel::stop(&mut state.kernel),
        Command::KernelLog => state.kernel.log_open = !state.kernel.log_open,
        Command::OpenFile => ops::open_file(state, cx, None),
        Command::OpenPlanFile => {
            if let Some(p) = rfd::FileDialog::new().add_filter("Execution plan", &["sqlplan", "xml"]).pick_file() {
                ops::open_plan_file(state, cx, p);
            }
        }
        Command::SaveFile => {
            if let Some(i) = idx {
                ops::save_file(state, cx, i, false);
            }
        }
        Command::SaveFileAs => {
            if let Some(i) = idx {
                ops::save_file(state, cx, i, true);
            }
        }
        Command::CloseTab => {
            if let Some(i) = idx {
                ops::close_tab(state, cx, i, false);
            }
        }
        Command::ReopenClosedTab => ops::reopen_closed_tab(state, cx),
        Command::NextTab | Command::PrevTab => {
            if let Some(i) = idx {
                let n = state.tabs.len();
                state.active_tab = Some(if cmd == Command::NextTab { (i + 1) % n } else { (i + n - 1) % n });
            }
        }
        Command::ImportAdsSettings => {
            let path = cobalt_store::default_ads_settings_path().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
            state.dialog = Dialog::AdsImport { path, summary: None, error: None };
        }
        Command::ExportConnections => {
            if let Some(p) = rfd::FileDialog::new().add_filter("JSON", &["json"]).set_file_name("cobalt-connections.json").save_file() {
                ops::export_connections(state, cx, &p);
            }
        }
        Command::ImportConnections => {
            if let Some(p) = rfd::FileDialog::new().add_filter("JSON", &["json"]).pick_file() {
                ops::import_connections(state, cx, &p);
            }
        }
        Command::ImportFile => {
            if let Some(i) = idx {
                ops::open_import_dialog(state, cx, i, None);
            }
        }
        Command::Quit => cx.egui.send_viewport_cmd(egui::ViewportCommand::Close),
        Command::NewConnection => ops::open_connection_dialog(state, cx, None, None, None),
        Command::ConnectTab => {
            if let Some(i) = idx {
                let t = &state.tabs[i];
                if t.spark.is_some() {
                    state.open_spark_chip = Some(t.id);
                    return;
                }
                match t.profile.clone() {
                    Some(p) => {
                        let tab = t.id;
                        let db = t.reconnect_database();
                        ops::begin_connect(state, cx, p, ConnectPurpose::Tab { tab, database: db });
                    }
                    None => state.dialog = Dialog::ChangeConnection { tab_index: i },
                }
            }
        }
        Command::DisconnectTab => {
            if let Some(i) = idx {
                ops::disconnect_tab(state, cx, i);
            }
        }
        Command::ChangeConnection => {
            if let Some(i) = idx {
                if state.tabs[i].spark.is_some() {
                    state.open_spark_chip = Some(state.tabs[i].id);
                    return;
                }
                state.dialog = Dialog::ChangeConnection { tab_index: i };
            } else {
                ops::open_connection_dialog(state, cx, None, None, None);
            }
        }
        Command::RefreshTree => {
            let ids: Vec<ProfileId> = state.library.servers.iter().filter(|(_, s)| s.creds.is_some()).map(|(id, _)| *id).collect();
            for id in ids {
                ops::tree_action(state, cx, servers::TreeAction::RefreshServer(id));
            }
        }
        Command::RefreshIntelliSense => {
            if let Some(t) = state.active() {
                if let (Some(p), Some(db)) = (t.profile.clone(), t.conn.database().map(str::to_string)) {
                    let tab = t.id;
                    let _ = cx.store.invalidate_catalog(p.id, Some(&db));
                    ops::request_meta(state, cx, p.id, crate::session::MetadataRequest::LoadCatalog { database: db.clone() }, MetaPurpose::Catalog { tab, database: db });
                    state.flash("Refreshing IntelliSense…");
                }
            }
        }
        Command::RunQuery => {
            if let Some(i) = idx {
                ops::run(state, cx, i, RunMode::All);
            }
        }
        Command::RunCurrentStatement => {
            if let Some(i) = idx {
                ops::run(state, cx, i, RunMode::Current);
            }
        }
        Command::RunToFile => {
            if let Some(i) = idx {
                ops::open_run_to_file(state, cx, i, RunMode::All);
            }
        }
        Command::CancelQuery => {
            if let Some(i) = idx {
                ops::cancel(state, cx, i);
            }
        }
        Command::EstimatedPlan => {
            if let Some(i) = idx {
                ops::run(state, cx, i, RunMode::EstimatedPlan);
            }
        }
        Command::ToggleActualPlan => {
            if let Some(t) = state.active_mut() {
                t.actual_plan = !t.actual_plan;
            }
        }
        Command::ParseQuery => {
            if let Some(i) = idx {
                let t = &state.tabs[i];
                if t.spark.is_some() {
                    cx.toast(ToastKind::Info, "Parse is not available on Spark SQL tabs; run EXPLAIN as a statement.");
                    return;
                }
                let script = format!("SET PARSEONLY ON;\n{}\nSET PARSEONLY OFF;", t.text);
                if t.conn.is_connected() {
                    let opts = t.exec.clone();
                    ops::execute(state, cx, i, script, opts, 1);
                    state.tabs[i].results_tab = ResultsTab::Messages;
                } else {
                    cx.toast(ToastKind::Warning, "Connect first to parse against the server.");
                }
            }
        }
        Command::FormatDocument => {
            let opts = cobalt_sql::format::FormatOptions { uppercase_keywords: cx.settings.editor.format_uppercase_keywords, indent: cx.settings.editor.format_indent as usize, lines_between_queries: cx.settings.editor.format_blank_lines as usize };
            if let Some(t) = state.active_mut() {
                let mut h = t.active_host();
                let formatted = cobalt_sql::format::format(h.text, &opts);
                editor::set_text_keep_line(&mut h, formatted);
            }
        }
        Command::ExecutionOptions => {
            if let Some(i) = idx {
                state.dialog = Dialog::ExecOptions { tab_index: i, opts: state.tabs[i].exec.clone() };
            }
        }
        Command::Find => {
            // Ctrl+F follows the keyboard: a focused result grid gets "find in results", the editor
            // gets its own find bar
            let grid_focused = state.active().and_then(|t| t.run.as_ref()).map(|r| r.result_sets.iter().any(|s| !s.is_plan && s.grid.focused)).unwrap_or(false);
            if grid_focused {
                dispatch(f, Command::FindInResults);
                return;
            }
            if let Some(t) = state.active_mut() {
                let h = t.active_host();
                h.editor.find_open = true;
                if let Some((a, b)) = h.editor.selection {
                    let ab = editor::char_to_byte(h.text, a);
                    let bb = editor::char_to_byte(h.text, b);
                    if bb > ab && !h.text[ab..bb].contains('\n') {
                        h.editor.find_text = h.text[ab..bb].to_string();
                    }
                }
                let fid = h.id.with("find");
                cx.egui.memory_mut(|m| m.request_focus(fid));
            }
        }
        Command::Replace => {
            if let Some(t) = state.active_mut() {
                t.active_host().editor.find_open = true;
            }
        }
        Command::GoToLine => {
            if let Some(t) = state.active_mut() {
                let h = t.active_host();
                h.editor.goto_line_open = true;
                h.editor.goto_line_text.clear();
            }
        }
        Command::ToggleLineComment => {
            if let Some(t) = state.active_mut() {
                editor::toggle_line_comment(&mut t.active_host());
            }
        }
        Command::ToggleBlockComment => {
            if let Some(t) = state.active_mut() {
                editor::toggle_block_comment(&mut t.active_host());
            }
        }
        Command::TriggerCompletion => {
            if let Some(t) = state.active_mut() {
                let anchor = cx.egui.input(|i| i.pointer.hover_pos()).unwrap_or(egui::pos2(300.0, 200.0));
                let mut h = t.active_host();
                let cursor = editor::char_to_byte(h.text, h.editor.cursor);
                editor::open_completion(&mut h, cursor, anchor, true);
            }
        }
        Command::UppercaseKeywords => {
            if let Some(t) = state.active_mut() {
                let mut h = t.active_host();
                let up = cobalt_sql::casing::uppercase_keywords(h.text);
                editor::set_text_keep_line(&mut h, up);
            }
        }
        Command::ToggleResults => {
            if let Some(t) = state.active_mut() {
                t.results_visible = !t.results_visible;
            }
        }
        Command::FocusEditorOrResults => {
            if let Some(t) = state.active_mut() {
                let grid_focused = t.run.as_ref().map(|r| r.result_sets.iter().any(|s| s.grid.focused)).unwrap_or(false);
                if grid_focused {
                    for s in t.run.iter_mut().flat_map(|r| r.result_sets.iter_mut()) {
                        s.grid.focused = false;
                    }
                    t.editor.request_focus = true;
                } else if let Some(s) = t.run.as_mut().and_then(|r| r.result_sets.iter_mut().find(|s| !s.is_plan)) {
                    s.grid.focused = true;
                    if s.grid.anchor.is_none() {
                        s.grid.anchor = Some((0, 0));
                        s.grid.selection = Selection::Cells { r0: 0, c0: 0, r1: 0, c1: 0 };
                    }
                    cx.egui.memory_mut(|m| m.stop_text_input());
                }
            }
        }
        Command::CopySelection | Command::CopyWithHeaders | Command::CopyHeaders | Command::CopyAsMarkdown | Command::CopyAsJson | Command::CopyAsCsv | Command::CopyAsInsert | Command::CopyAsInList => {
            if let Some(i) = idx {
                if let Some(set) = focused_set(state, i) {
                    let kind = match cmd {
                        Command::CopySelection => CopyKind::Tsv,
                        Command::CopyWithHeaders => CopyKind::TsvWithHeaders,
                        Command::CopyHeaders => CopyKind::HeadersOnly,
                        Command::CopyAsMarkdown => CopyKind::Markdown,
                        Command::CopyAsJson => CopyKind::Json,
                        Command::CopyAsCsv => CopyKind::Csv,
                        Command::CopyAsInsert => CopyKind::Insert,
                        _ => CopyKind::InList,
                    };
                    ops::copy_cells(state, cx, i, set, kind);
                }
            }
        }
        Command::SelectAllCells => {
            if let Some(i) = idx {
                if let Some(set) = focused_set(state, i) {
                    state.tabs[i].run.as_mut().unwrap().result_sets[set].grid.selection = Selection::All;
                }
            }
        }
        Command::FindInResults => {
            if let Some(i) = idx {
                if let Some(set) = focused_set(state, i) {
                    let g = &mut state.tabs[i].run.as_mut().unwrap().result_sets[set].grid;
                    if g.find.is_none() {
                        g.find = Some(GridFind::default());
                    }
                }
            }
        }
        Command::ProfileColumns => {
            if let Some(i) = idx {
                if let Some(set) = focused_set(state, i) {
                    ops::results_action(state, cx, i, results::ResultsAction::Profile { set });
                }
            }
        }
        Command::OpenInExcel => {
            if let Some(i) = idx {
                if let Some(set) = focused_set(state, i) {
                    ops::open_in_excel(state, cx, i, set, false);
                }
            }
        }
        Command::SaveAsTable => {
            if let Some(i) = idx {
                if let Some(set) = focused_set(state, i) {
                    ops::open_results_to_table(state, cx, i, set, false);
                }
            }
        }
        Command::SaveResultsCsv | Command::SaveResultsExcel | Command::SaveResultsJson | Command::SaveResultsXml | Command::SaveResultsMarkdown | Command::SaveResultsParquet | Command::SaveResultsArrow | Command::SaveResultsDelta => {
            if let Some(i) = idx {
                if let Some(set) = focused_set(state, i) {
                    ops::open_export_dialog(state, cx, i, set, false);
                    let fi = match cmd {
                        Command::SaveResultsCsv => 0,
                        Command::SaveResultsExcel => 2,
                        Command::SaveResultsJson => 3,
                        Command::SaveResultsXml => 5,
                        Command::SaveResultsMarkdown => 6,
                        Command::SaveResultsParquet => 7,
                        Command::SaveResultsArrow => 8,
                        _ => 9,
                    };
                    if let Dialog::Export(d) = &mut state.dialog {
                        d.format_index = fi;
                        let ext = ops::FORMAT_LABELS[fi].1;
                        d.path = std::path::Path::new(&d.path).with_extension(if ext == "delta" { "" } else { ext }).to_string_lossy().to_string();
                    }
                }
            }
        }
        Command::MaximizeResultSet => {
            if let Some(i) = idx {
                if let Some(set) = focused_set(state, i) {
                    let r = state.tabs[i].run.as_mut().unwrap();
                    r.maximized = if r.maximized == Some(set) { None } else { Some(set) };
                }
            }
        }
        Command::OpenCellViewer => {
            if let Some(i) = idx {
                if let Some(set) = focused_set(state, i) {
                    let g = &mut state.tabs[i].run.as_mut().unwrap().result_sets[set].grid;
                    if let Some(a) = g.anchor {
                        g.viewer = Some(a);
                    }
                }
            }
        }
        Command::ClearFilters => {
            if let Some(i) = idx {
                if let Some(set) = focused_set(state, i) {
                    ops::apply_view(state, cx, i, set, cobalt_results::ViewSpec::default());
                }
            }
        }
        Command::SavePlanFile => {
            if let Some(t) = state.active() {
                if let Some(pv) = t.run.as_ref().and_then(|r| r.plans.first()) {
                    if let Some(p) = rfd::FileDialog::new().add_filter("Execution plan", &["sqlplan"]).set_file_name("plan.sqlplan").save_file() {
                        match std::fs::write(&p, &pv.xml) {
                            Ok(()) => state.flash(format!("Saved {}", p.display())),
                            Err(e) => cx.toast(ToastKind::Error, e.to_string()),
                        }
                    }
                } else {
                    cx.toast(ToastKind::Warning, "No execution plan in this tab.");
                }
            }
        }
        Command::ShowPlanXml => {
            if let Some(t) = state.active() {
                if let Some(pv) = t.run.as_ref().and_then(|r| r.plans.first()) {
                    let xml = pv.xml.clone();
                    let idx = state.new_tab();
                    state.tabs[idx].title = "Plan XML".into();
                    state.tabs[idx].custom_title = true;
                    state.tabs[idx].text = xml;
                    state.tabs[idx].mark_saved();
                }
            }
        }
        Command::PlanZoomIn | Command::PlanZoomOut | Command::PlanZoomFit => {
            if let Some(t) = state.active_mut() {
                if let Some(pv) = t.run.as_mut().and_then(|r| r.plans.first_mut()) {
                    match cmd {
                        Command::PlanZoomIn => pv.zoom = (pv.zoom * 1.2).min(4.0),
                        Command::PlanZoomOut => pv.zoom = (pv.zoom / 1.2).max(0.2),
                        _ => {
                            pv.zoom = 0.0;
                        }
                    }
                }
            }
        }
        Command::Palette => {
            state.palette_open = !state.palette_open;
            state.palette_query.clear();
            state.palette_selected = 0;
        }
        Command::FindObject => {
            state.palette_open = true;
            state.palette_query = "#".into();
            state.palette_selected = 0;
        }
        Command::ToggleSidebar => state.sidebar_visible = !state.sidebar_visible,
        Command::ShowServers => {
            state.sidebar_visible = true;
            state.sidebar_view = SidebarView::Servers;
        }
        Command::ShowFabric => {
            state.sidebar_visible = true;
            state.sidebar_view = SidebarView::Fabric;
        }
        Command::ShowLakehouse => {
            state.sidebar_visible = true;
            state.sidebar_view = SidebarView::Lakehouse;
        }
        Command::ShowHistory => {
            state.sidebar_visible = true;
            state.sidebar_view = SidebarView::History;
            state.history.loaded = false;
        }
        Command::ShowFiles => {
            state.sidebar_visible = true;
            state.sidebar_view = SidebarView::Files;
        }
        Command::Welcome => {
            state.settings_patch.push(SettingsPatch::ShowWelcome(true));
            ops::new_query_tab(state, cx, None, None, None, false);
        }
        Command::ShowSettings => state.settings_open = true,
        Command::ToggleTheme => {
            let next = if f.theme.is_dark() { ThemeChoice::Light } else { ThemeChoice::Dark };
            state.settings_patch.push(SettingsPatch::Theme(next));
        }
        Command::ZoomIn => state.settings_patch.push(SettingsPatch::UiScale((cx.settings.appearance.ui_scale + 0.1).min(2.5))),
        Command::ZoomOut => state.settings_patch.push(SettingsPatch::UiScale((cx.settings.appearance.ui_scale - 0.1).max(0.6))),
        Command::ZoomReset => state.settings_patch.push(SettingsPatch::UiScale(1.0)),
        Command::About => state.about_open = true,
        Command::Changelog => {
            state.changelog.open = true;
            crate::update::fetch_changelog(state, cx);
        }
        Command::RefreshDatabases => {
            if let Some(i) = idx {
                let tab = state.tabs[i].id;
                ops::handle_followups(state, cx, vec![Followup::LoadTabDatabases(tab)]);
            }
        }
        Command::CheckForUpdates => state.update_check_requested = true,
        Command::KeyboardShortcuts => state.shortcuts_open = true,
    }
}

fn focused_set(state: &AppState, idx: usize) -> Option<usize> {
    let r = state.tabs.get(idx)?.run.as_ref()?;
    r.result_sets.iter().position(|s| s.grid.focused && !s.is_plan).or_else(|| r.result_sets.iter().position(|s| !s.is_plan))
}

