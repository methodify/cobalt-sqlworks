//! The window chrome: menu bar, sidebar strip + sidebar, editor tabs, editor toolbar, the
//! editor/results split, status bar — and the command dispatcher.

use crate::commands::{Command, Keymap};
use crate::copy::CopyKind;
use crate::ops::{self, Ctx};
use crate::state::*;
use crate::ui::results::viewer::ViewerState;
use crate::ui::theme::Theme;
use crate::ui::widgets::{icon_button, tool_button};
use crate::ui::{editor, history, palette, plan, results, servers};
use cobalt_core::*;
use egui::{Color32, RichText, Sense, Stroke, Ui, Vec2};
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
    // overlays
    if let Some(item) = palette::show(ui, f.state, f.theme, f.keymap) {
        match item {
            palette::PaletteItem::Command(c) => dispatch(f, c),
            palette::PaletteItem::Connect(id) => {
                ops::new_query_tab(f.state, f.cx, Some(id), None, None, false);
            }
            palette::PaletteItem::Open(id, db) => {
                ops::new_query_tab(f.state, f.cx, Some(id), Some(db), None, false);
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
                item(ui, &mut cmds, Command::ToggleResults);
                item(ui, &mut cmds, Command::MaximizeResultSet);
                item(ui, &mut cmds, Command::ClearFilters);
                item(ui, &mut cmds, Command::OpenCellViewer);
            });
            ui.menu_button("View", |ui| {
                item(ui, &mut cmds, Command::CommandPalette);
                item(ui, &mut cmds, Command::ToggleSidebar);
                item(ui, &mut cmds, Command::ShowServers);
                item(ui, &mut cmds, Command::ShowHistory);
                item(ui, &mut cmds, Command::ShowFabric);
                ui.separator();
                item(ui, &mut cmds, Command::ToggleTheme);
                item(ui, &mut cmds, Command::ZoomIn);
                item(ui, &mut cmds, Command::ZoomOut);
                item(ui, &mut cmds, Command::ZoomReset);
                ui.separator();
                item(ui, &mut cmds, Command::ShowSettings);
            });
            ui.menu_button("Help", |ui| {
                item(ui, &mut cmds, Command::KeyboardShortcuts);
                item(ui, &mut cmds, Command::CheckForUpdates);
                item(ui, &mut cmds, Command::About);
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let r = ui.add(egui::Button::new(format!("{}  Search commands  {}", icons::MAGNIFYING_GLASS, km.shortcut_text(ui.ctx(), Command::CommandPalette))).frame(false));
                if r.clicked() {
                    cmds.push(Command::CommandPalette);
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
        let items = [(SidebarView::Servers, icons::HARD_DRIVES, "Servers (Ctrl+Shift+E)"), (SidebarView::Fabric, icons::CUBE, "Fabric (Ctrl+Shift+B)"), (SidebarView::History, icons::CLOCK_COUNTER_CLOCKWISE, "History (Ctrl+Shift+Y)")];
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
            let actions = servers::show(ui, &mut f.state.library, theme, active_profile);
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
        SidebarView::Fabric => {
            crate::fabric::on_panel_shown(f.state, f.cx);
            let actions = crate::ui::fabric::show(ui, f.state, theme);
            for a in actions {
                crate::fabric::action(f.state, f.cx, a);
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
                    let title = t.display_title();
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
                if resp.on_hover_text("New query (Ctrl+N)").clicked() {
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
        ui.label(RichText::new(format!("{} Command palette   ·   F5 Run   ·   Ctrl+Enter Run statement   ·   Ctrl+L Estimated plan   ·   Ctrl+M Actual plan", f.keymap.shortcut_text(ui.ctx(), Command::CommandPalette))).size(11.0).color(theme.text_faint));
    });
}

fn editor_area(ui: &mut Ui, f: &mut Frame<'_>) {
    let theme = f.theme;
    let idx = f.state.active_tab.unwrap();
    editor_toolbar(ui, f, idx);
    let avail = ui.available_rect_before_wrap();
    let tab = &mut f.state.tabs[idx];
    let results_visible = tab.results_visible && tab.run.is_some();
    let frac = tab.results_fraction.clamp(0.1, 0.92);
    let sep_h = 6.0;
    let editor_h = if results_visible { (avail.height() * (1.0 - frac) - sep_h / 2.0).max(60.0) } else { avail.height() };
    let editor_rect = egui::Rect::from_min_size(avail.min, Vec2::new(avail.width(), editor_h));
    let mut ed_ui = ui.new_child(egui::UiBuilder::new().max_rect(editor_rect).layout(egui::Layout::top_down(egui::Align::Min)));
    ed_ui.set_clip_rect(editor_rect);
    let out = editor::show(&mut ed_ui, tab, theme, f.cx.settings);
    if out.focused {
        f.state.focus = Focus::Editor;
        for rs in tab.run.iter_mut().flat_map(|r| r.result_sets.iter_mut()) {
            rs.grid.focused = false;
        }
    }
    if results_visible {
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
        let res_rect = egui::Rect::from_min_max(egui::pos2(avail.left(), sep_rect.bottom()), avail.max);
        let mut res_ui = ui.new_child(egui::UiBuilder::new().max_rect(res_rect).layout(egui::Layout::top_down(egui::Align::Min)));
        res_ui.set_clip_rect(res_rect);
        if tab.results_tab == ResultsTab::Plan {
            // plan body drawn separately (needs the tab strip from results::show first)
        }
        let actions = results::show(&mut res_ui, results::ResultsArgs { tab, theme, settings: f.cx.settings, fmt: &f.state.formatter, selection_summary: None });
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
            let mut vs: ViewerState = ui.ctx().memory(|m| m.data.get_temp::<std::sync::Arc<parking_lot::Mutex<Option<ViewerState>>>>(id)).and_then(|m| m.lock().take()).filter(|v| Arc::ptr_eq(&v.rs, &rs) && v.row == row && v.col == col).unwrap_or_else(|| ViewerState::new(rs.clone(), row, col));
            let mut open = true;
            let mut close = false;
            egui::Window::new("Cell value").id(id).open(&mut open).default_size([560.0, 420.0]).resizable(true).show(ui.ctx(), |ui| {
                close = crate::ui::results::viewer::show(ui, theme, &mut vs);
            });
            if !open || close {
                tab.run.as_mut().unwrap().result_sets[set].grid.viewer = None;
            } else {
                ui.ctx().memory_mut(|m| m.data.insert_temp(id, std::sync::Arc::new(parking_lot::Mutex::new(Some(vs)))));
            }
        }
    }
}

use std::sync::Arc;

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
            ui.separator();
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
                if !dbs.iter().any(|d| *d == current) && !current.is_empty() {
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
                if let Some(t) = f.state.active() {
                    ui.label(RichText::new("MSSQL").color(theme.text_muted));
                    ui.separator();
                    ui.label(format!("Ln {}, Col {}", t.editor.line, t.editor.col));
                    ui.separator();
                    if let Some(p) = &t.file_path {
                        ui.label(RichText::new(p.display().to_string()).color(theme.text_faint));
                    }
                }
                if let Some(p) = &f.state.export_progress {
                    let (done, total) = *p.lock();
                    ui.separator();
                    ui.label(RichText::new(format!("{} Exporting {} / {}", icons::EXPORT, fmt_count(done as u64), fmt_count(total as u64))).color(theme.accent));
                }
            });
        });
    });
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
        Command::Quit => cx.egui.send_viewport_cmd(egui::ViewportCommand::Close),
        Command::NewConnection => ops::open_connection_dialog(state, cx, None, None, None),
        Command::ConnectTab => {
            if let Some(i) = idx {
                let t = &state.tabs[i];
                match t.profile.clone() {
                    Some(p) => {
                        let tab = t.id;
                        let db = t.conn.database().map(str::to_string);
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
            if let Some(t) = state.active_mut() {
                let opts = cobalt_sql::format::FormatOptions::default();
                let formatted = cobalt_sql::format::format(&t.text, &opts);
                editor::set_text_keep_line(t, formatted);
            }
        }
        Command::ExecutionOptions => {
            if let Some(i) = idx {
                state.dialog = Dialog::ExecOptions { tab_index: i, opts: state.tabs[i].exec.clone() };
            }
        }
        Command::Find => {
            if let Some(t) = state.active_mut() {
                t.editor.find_open = true;
                if let Some((a, b)) = t.editor.selection {
                    let ab = editor::char_to_byte(&t.text, a);
                    let bb = editor::char_to_byte(&t.text, b);
                    if bb > ab && !t.text[ab..bb].contains('\n') {
                        t.editor.find_text = t.text[ab..bb].to_string();
                    }
                }
                cx.egui.memory_mut(|m| m.request_focus(egui::Id::new(("find", t.id))));
            }
        }
        Command::Replace => {
            if let Some(t) = state.active_mut() {
                t.editor.find_open = true;
            }
        }
        Command::GoToLine => {
            if let Some(t) = state.active_mut() {
                t.editor.goto_line_open = true;
                t.editor.goto_line_text.clear();
            }
        }
        Command::ToggleLineComment => {
            if let Some(t) = state.active_mut() {
                editor::toggle_line_comment(t);
            }
        }
        Command::ToggleBlockComment => {
            if let Some(t) = state.active_mut() {
                editor::toggle_block_comment(t);
            }
        }
        Command::TriggerCompletion => {
            if let Some(t) = state.active_mut() {
                let cursor = editor::char_to_byte(&t.text, t.editor.cursor);
                let anchor = cx.egui.input(|i| i.pointer.hover_pos()).unwrap_or(egui::pos2(300.0, 200.0));
                editor::open_completion(t, cursor, anchor, true);
            }
        }
        Command::UppercaseKeywords => {
            if let Some(t) = state.active_mut() {
                let up = cobalt_sql::casing::uppercase_keywords(&t.text);
                editor::set_text_keep_line(t, up);
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
                        g.find = Some(GridFind { text: String::new(), matches: Vec::new(), current: 0, generation: 0 });
                    }
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
        Command::CommandPalette => {
            state.palette_open = !state.palette_open;
            state.palette_query.clear();
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
        Command::ShowHistory => {
            state.sidebar_visible = true;
            state.sidebar_view = SidebarView::History;
            state.history.loaded = false;
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

pub fn unused_color(_c: Color32) {}
