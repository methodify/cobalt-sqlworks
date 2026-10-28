//! The dashboard tab: a toolbar (title, columns, refresh, add) and the tiles laid out in rows,
//! each a card with its own header (age, refresh, configure, edit, remove) and body (a number,
//! a chart, or a small grid).

use crate::dashboard as ops;
use crate::state::{Dialog, Tile, ToastKind};
use crate::ui::results::chart::{self, ChartAction};
use crate::ui::results::grid::{self, GridArgs};
use crate::ui::shell::Frame;
use crate::ui::theme::Theme;
use crate::ui::widgets::icon_button;
use cobalt_core::{interval_label, TileKind};
use egui::{RichText, Ui, Vec2};
use egui_phosphor::regular as icons;

enum DashAction {
    Refresh(usize),
    /// A tile pinned from Explore opens in the builder again.
    OpenExplore(usize),
    RefreshAll,
    Edit(Option<usize>),
    Remove(usize),
    Chart(usize, ChartAction),
    Save,
}

pub fn show(ui: &mut Ui, f: &mut Frame<'_>, idx: usize) {
    let theme = f.theme;
    let fmt = f.state.formatter.clone();
    let settings = f.cx.settings;
    let mut actions: Vec<DashAction> = Vec::new();
    let tab_id = f.state.tabs[idx].id;
    let Some(d) = f.state.tabs[idx].dashboard.as_deref_mut() else { return };
    // ---- toolbar ----
    egui::Frame::new().fill(theme.bg_sidebar).inner_margin(egui::Margin::symmetric(8, 4)).show(ui, |ui| {
        ui.horizontal(|ui| {
            let r = ui.add(egui::TextEdit::singleline(&mut d.file.title).desired_width(260.0).font(egui::TextStyle::Heading));
            r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, true, "dashboard title"));
            if r.changed() {
                d.dirty = true;
            }
            ui.label(RichText::new("Columns").size(12.0).color(theme.text_muted));
            for n in 2..=4u32 {
                let r = ui.add(egui::Button::selectable(d.file.columns == n, RichText::new(n.to_string()).size(12.0)).small());
                r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("dashboard columns {n}")));
                if r.clicked() && d.file.columns != n {
                    d.file.columns = n;
                    d.dirty = true;
                }
            }
            ui.separator();
            let r = ui.button(format!("{} Add tile…", icons::PLUS));
            r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "dashboard add tile"));
            if r.clicked() {
                actions.push(DashAction::Edit(None));
            }
            let r = ui.button(format!("{} Refresh all", icons::ARROWS_CLOCKWISE));
            r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "dashboard refresh all"));
            if r.clicked() {
                actions.push(DashAction::RefreshAll);
            }
            let r = ui.add_enabled(d.dirty, egui::Button::new(format!("{} Save", icons::FLOPPY_DISK)));
            r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "dashboard save"));
            if r.on_hover_text("Ctrl+S. A dashboard is a .cobalt-dash.json file you can share; connections are named by profile.").clicked() {
                actions.push(DashAction::Save);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let live = d.tiles.iter().filter(|t| t.is_live()).count();
                let text = format!("{} tile{}{}", d.tiles.len(), if d.tiles.len() == 1 { "" } else { "s" }, if live > 0 { format!(" · {live} refreshing") } else { String::new() });
                ui.label(RichText::new(text).size(11.0).color(theme.text_faint));
                if let Some(p) = d.home_for {
                    let name = f.state.library.profile(p).map(|x| x.display_name()).unwrap_or_default();
                    ui.label(RichText::new(format!("{} home page of {name}", icons::HOUSE)).size(11.0).color(theme.text_muted));
                }
            });
        });
    });
    // ---- tiles ----
    let columns = d.file.columns.max(1) as usize;
    let gap = 10.0;
    egui::ScrollArea::vertical().id_salt(("dashboard", tab_id)).auto_shrink([false, false]).show(ui, |ui| {
        ui.add_space(gap);
        let avail = ui.available_width() - gap * 2.0;
        let col_w = ((avail - gap * (columns as f32 - 1.0)) / columns as f32).max(160.0);
        if d.tiles.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(40.0);
                ui.label(RichText::new("No tiles yet.").size(14.0).color(theme.text_muted));
                ui.add_space(6.0);
                ui.label(RichText::new("Add tile… writes a query here; a result in any query tab pins itself with its context menu › Pin to dashboard….").size(12.0).color(theme.text_faint));
            });
            return;
        }
        // rows of tiles: a tile spans its size's columns; a row wraps when the next one does not fit
        let mut rows: Vec<Vec<usize>> = vec![Vec::new()];
        let mut used = 0usize;
        for (k, t) in d.tiles.iter().enumerate() {
            let span = (t.spec.size.span() as usize).min(columns);
            if used + span > columns && used > 0 {
                rows.push(Vec::new());
                used = 0;
            }
            rows.last_mut().unwrap().push(k);
            used += span;
        }
        for row in rows {
            let row_h = row.iter().map(|k| d.tiles[*k].spec.size.height()).fold(0.0_f32, f32::max);
            ui.horizontal_top(|ui| {
                ui.add_space(gap);
                for k in row {
                    let tile = &mut d.tiles[k];
                    let span = (tile.spec.size.span() as usize).min(columns) as f32;
                    let w = col_w * span + gap * (span - 1.0);
                    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, row_h), egui::Sense::hover());
                    let mut child = ui.new_child(egui::UiBuilder::new().id_salt(("tile", tab_id, tile.tab.id)).max_rect(rect).layout(egui::Layout::top_down(egui::Align::Min)));
                    child.set_clip_rect(rect.intersect(ui.clip_rect()));
                    tile_card(&mut child, tile, k, theme, &fmt, settings, &mut actions);
                    ui.add_space(gap);
                }
            });
            ui.add_space(gap);
        }
    });
    // ---- actions ----
    let cx = f.cx;
    for a in actions {
        match a {
            DashAction::Refresh(k) => ops::refresh_tile(f.state, cx, idx, k),
            DashAction::RefreshAll => ops::refresh_all(f.state, cx, idx),
            DashAction::OpenExplore(k) => {
                let spec = f.state.tabs.get(idx).and_then(|t| t.dashboard.as_deref()).and_then(|d| d.tiles.get(k)).map(|t| t.spec.clone());
                if let Some(spec) = spec {
                    if let (cobalt_core::TileSource::SemanticModel { target }, Some(ex)) = (spec.source, spec.explore) {
                        crate::exploreq::new_tab(f.state, f.cx, target, Some(*ex), None);
                    }
                }
            }
            DashAction::Edit(None) => {
                let profile = f.state.tabs.iter().find_map(|t| t.profile.as_ref().map(|p| p.id)).or_else(|| f.state.library.profiles.first().map(|p| p.id));
                let mut draft = cobalt_core::TileSpec::default();
                if let Some(p) = profile.and_then(|id| f.state.library.profile(id)) {
                    draft.profile = ops::profile_ref(p);
                    draft.database = p.database.clone();
                }
                f.state.dialog = Dialog::TileEditor { tab_index: idx, tile: None, draft, profile };
            }
            DashAction::Edit(Some(k)) => {
                if let Some(t) = f.state.tabs[idx].dashboard.as_deref().and_then(|d| d.tiles.get(k)) {
                    let draft = t.spec.clone();
                    let profile = ops::resolve_profile(f.state, &draft.profile).map(|p| p.id);
                    f.state.dialog = Dialog::TileEditor { tab_index: idx, tile: Some(k), draft, profile };
                }
            }
            DashAction::Remove(k) => ops::remove_tile(f.state, cx, idx, k),
            DashAction::Save => ops::save(f.state, cx, idx, false),
            DashAction::Chart(k, ca) => match ca {
                ChartAction::Capture { save } => {
                    // the tile's hidden tab is swapped in so the capture path finds the chart's rect
                    let purpose = if save { crate::state::CapturePurpose::SaveFile } else { crate::state::CapturePurpose::Clipboard };
                    let rect = f.state.tabs[idx].dashboard.as_deref().and_then(|d| d.tiles.get(k)).and_then(|t| t.tab.run.as_ref()).and_then(|r| r.result_sets.first()).and_then(|v| v.chart.as_ref()).and_then(|c| c.rect);
                    match rect {
                        Some(rect) => {
                            f.state.pending_capture = Some(crate::state::PendingCapture { tab: tab_id, set: 0, rect, purpose, requested: false });
                        }
                        None => cx.toast(ToastKind::Warning, "The chart has not been drawn yet."),
                    }
                }
                ChartAction::Keep => {}
                ChartAction::Close => {
                    if let Some(t) = f.state.tabs[idx].dashboard.as_deref_mut().and_then(|d| d.tiles.get_mut(k)) {
                        t.configure = false;
                    }
                }
            },
        }
    }
}

fn tile_card(ui: &mut Ui, tile: &mut Tile, k: usize, theme: &Theme, fmt: &cobalt_results::CellFormatter, settings: &cobalt_core::Settings, actions: &mut Vec<DashAction>) {
    let frame = egui::Frame::new().fill(theme.bg_panel).stroke(egui::Stroke::new(1.0, theme.border)).corner_radius(6.0).inner_margin(egui::Margin::symmetric(8, 6));
    frame.show(ui, |ui| {
        ui.set_min_size(ui.available_size());
        // header
        ui.horizontal(|ui| {
            ui.label(RichText::new(&tile.spec.title).strong().size(13.0));
            let status = if let Some(e) = &tile.error {
                (format!("{} {}", icons::WARNING, e.lines().next().unwrap_or("")), theme.error)
            } else if tile.is_live() {
                (format!("{} refreshing…", icons::CIRCLE_NOTCH), theme.accent)
            } else if tile.pending {
                (format!("{} connecting…", icons::CIRCLE_NOTCH), theme.text_muted)
            } else if let Some(r) = tile.tab.run.as_ref().filter(|r| r.state == crate::state::RunViewState::Failed) {
                (format!("{} {}", icons::WARNING, r.messages.iter().find(|m| m.is_error).map(|m| m.text.lines().next().unwrap_or("").to_string()).unwrap_or_else(|| "failed".into())), theme.error)
            } else {
                match tile.age_secs() {
                    Some(s) => (format!("{} ago{}", fmt_age(s), if tile.spec.interval_secs > 0 { format!(" · every {}", interval_label(tile.spec.interval_secs)) } else { String::new() }), theme.text_faint),
                    None => ("not run yet".to_string(), theme.text_faint),
                }
            };
            let l = ui.add(egui::Label::new(RichText::new(status.0).size(11.0).color(status.1)).truncate());
            l.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, format!("tile {} status", k + 1)));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let r = icon_button(ui, icons::TRASH, "Remove tile", true);
                r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("tile {} remove", k + 1)));
                if r.clicked() {
                    actions.push(DashAction::Remove(k));
                }
                let r = icon_button(ui, icons::PENCIL_SIMPLE, "Edit tile (title, query, connection, interval, size)", true);
                r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("tile {} edit", k + 1)));
                if r.clicked() {
                    actions.push(DashAction::Edit(Some(k)));
                }
                if tile.spec.explore.is_some() {
                    let r = icon_button(ui, icons::TABLE, "Open in Explore (the pivot builder this tile came from)", true);
                    r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("tile {} explore", k + 1)));
                    if r.clicked() {
                        actions.push(DashAction::OpenExplore(k));
                    }
                }
                if tile.spec.kind == TileKind::Chart {
                    let r = icon_button(ui, icons::SLIDERS_HORIZONTAL, if tile.configure { "Hide the chart settings" } else { "Chart settings (kind, X, Y, aggregation…)" }, true);
                    r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("tile {} configure", k + 1)));
                    if r.clicked() {
                        tile.configure = !tile.configure;
                    }
                }
                let r = icon_button(ui, icons::ARROWS_CLOCKWISE, "Refresh now", !tile.is_live());
                r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("tile {} refresh", k + 1)));
                if r.clicked() {
                    actions.push(DashAction::Refresh(k));
                }
            });
        });
        ui.add_space(2.0);
        // body
        let Some(run) = tile.tab.run.as_mut() else {
            ui.label(RichText::new(if tile.error.is_some() { "" } else { "Waiting for the first refresh…" }).size(12.0).color(theme.text_muted));
            return;
        };
        let Some(view) = run.result_sets.iter_mut().find(|s| !s.is_plan) else {
            if run.state == crate::state::RunViewState::Failed {
                for m in run.messages.iter().filter(|m| m.is_error).take(3) {
                    ui.add(egui::Label::new(RichText::new(&m.text).size(12.0).color(theme.error)).wrap());
                }
            } else if !run.is_live() {
                ui.label(RichText::new("The query returned no rows.").size(12.0).color(theme.text_muted));
            }
            return;
        };
        match tile.spec.kind {
            TileKind::Number => {
                let text = if view.rs.visible_count() > 0 { view.rs.cell_text(0, 0, fmt).to_string() } else { "–".into() };
                let name = view.rs.columns.first().map(|c| c.name.clone()).unwrap_or_default();
                let avail = ui.available_size();
                ui.allocate_ui(avail, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.add_space((avail.y * 0.5 - 30.0).max(0.0));
                        let size = (avail.y * 0.35).clamp(22.0, 64.0);
                        let l = ui.label(RichText::new(&text).size(size).strong().color(theme.accent));
                        l.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, format!("tile {} value", k + 1)));
                        if !name.is_empty() && name != text {
                            ui.label(RichText::new(name).size(12.0).color(theme.text_muted));
                        }
                    });
                });
            }
            TileKind::Chart => {
                if view.chart.is_none() {
                    ui.label(RichText::new("Preparing the chart…").size(12.0).color(theme.text_muted));
                    return;
                }
                for a in chart::show_with(ui, view, theme, fmt, 0, false, tile.configure) {
                    actions.push(DashAction::Chart(k, a));
                }
            }
            TileKind::Grid => {
                let h = ui.available_height();
                let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), h), egui::Sense::hover());
                let mut child = ui.new_child(egui::UiBuilder::new().id_salt(("tile-grid", tile.tab.id)).max_rect(rect).layout(egui::Layout::top_down(egui::Align::Min)));
                child.set_clip_rect(rect.intersect(ui.clip_rect()));
                let _ = grid::show(
                    &mut child,
                    GridArgs {
                        rs: &view.rs,
                        grid: &mut view.grid,
                        theme,
                        fmt,
                        font_size: settings.appearance.grid_font_size,
                        id_salt: egui::Id::new(("tile-grid", tile.tab.id)),
                        show_row_numbers: false,
                        max_col_width: settings.results.max_column_width,
                        sample_rows: settings.results.auto_size_sample_rows,
                        row_tint: None,
                        edits: None,
                    },
                );
            }
        }
    });
}

fn fmt_age(s: u64) -> String {
    if s < 60 {
        format!("{s} s")
    } else if s < 3600 {
        format!("{} min", s / 60)
    } else {
        format!("{} h {} min", s / 3600, (s % 3600) / 60)
    }
}
