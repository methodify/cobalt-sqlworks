//! The results area under an editor: Results / Messages / Plan tabs, stacked result sets,
//! the fetch-more bar, the cell viewer, and the per-grid context menu.

pub mod grid;
pub mod viewer;

use crate::copy::CopyKind;
use crate::state::{EditorTab, ResultsTab, RunViewState, Selection};
use crate::ui::theme::Theme;
use crate::ui::widgets::icon_button;
use cobalt_core::Settings;
use cobalt_results::{CellFormatter, RunState};
use egui::{Color32, Pos2, RichText, Ui, Vec2};
use egui_phosphor::regular as icons;
use grid::GridAction;

/// Requests bubbled up to the app layer.
#[derive(Debug, Clone)]
pub enum ResultsAction {
    FetchMore { set: usize, rows: Option<u64> },
    Cancel,
    Copy { set: usize, kind: CopyKind },
    Export { set: usize, selection_only: bool },
    ApplyView { set: usize, spec: cobalt_results::ViewSpec },
    OpenViewer { set: usize, row: usize, col: usize },
    JumpToLine(u32),
    Summarize { set: usize },
    PopOut { set: usize },
}

pub struct ResultsArgs<'a> {
    pub tab: &'a mut EditorTab,
    pub theme: &'a Theme,
    pub settings: &'a Settings,
    pub fmt: &'a CellFormatter,
    pub selection_summary: Option<&'a str>,
}

pub fn show(ui: &mut Ui, args: ResultsArgs<'_>) -> Vec<ResultsAction> {
    let mut actions = Vec::new();
    let theme = args.theme;
    let tab = args.tab;
    let Some(run) = tab.run.as_mut() else {
        egui::Frame::new().fill(theme.bg_panel).show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new("Run a query (F5) to see results here").color(theme.text_faint));
            });
        });
        return actions;
    };
    let data_sets: Vec<usize> = run.result_sets.iter().enumerate().filter(|(_, s)| !s.is_plan).map(|(i, _)| i).collect();
    let has_plan = !run.plans.is_empty();

    // tab strip
    egui::Frame::new().fill(theme.bg_sidebar).inner_margin(egui::Margin::symmetric(6, 2)).show(ui, |ui| {
        ui.horizontal(|ui| {
            for (t, label) in [(ResultsTab::Results, "Results"), (ResultsTab::Messages, "Messages"), (ResultsTab::Plan, "Plan")] {
                if t == ResultsTab::Plan && !has_plan {
                    continue;
                }
                let selected = tab.results_tab == t;
                let mut text = RichText::new(label).size(13.0);
                if selected {
                    text = text.color(theme.accent).strong();
                }
                let count = match t {
                    ResultsTab::Results => data_sets.len(),
                    ResultsTab::Messages => run.messages.iter().filter(|m| m.is_error).count(),
                    ResultsTab::Plan => run.plans.len(),
                };
                let label_text = if t == ResultsTab::Messages && count > 0 { format!("{label} ({count} error{})", if count == 1 { "" } else { "s" }) } else if t == ResultsTab::Results && count > 1 { format!("{label} ({count})") } else { label.to_string() };
                let _ = text;
                let mut text = RichText::new(label_text).size(13.0);
                if selected {
                    text = text.color(theme.accent).strong();
                }
                let r = ui.add(egui::Button::new(text).frame(false));
                if selected {
                    let rect = r.rect;
                    ui.painter().line_segment([rect.left_bottom(), rect.right_bottom()], egui::Stroke::new(2.0, theme.accent));
                }
                if r.clicked() {
                    tab.results_tab = t;
                }
            }
            ui.separator();
            // status
            let status = match run.state {
                RunViewState::Running => format!("{} Executing…", icons::CIRCLE_NOTCH),
                RunViewState::Paused => "Paused at row cap".to_string(),
                RunViewState::Cancelling => "Cancelling…".to_string(),
                RunViewState::Done => "Completed".to_string(),
                RunViewState::Failed => "Completed with errors".to_string(),
                RunViewState::Cancelled => "Cancelled".to_string(),
            };
            let color = match run.state {
                RunViewState::Failed => theme.error,
                RunViewState::Done => theme.success,
                _ => theme.text_muted,
            };
            ui.label(RichText::new(status).size(12.0).color(color));
            if run.is_live() {
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if tab.results_tab == ResultsTab::Results {
                    if let Some(&set) = data_sets.first() {
                        let set = run.result_sets.iter().position(|s| !s.is_plan && s.grid.focused).unwrap_or(set);
                        if icon_button(ui, icons::ARROWS_OUT, "Maximize / restore this result set", true).clicked() {
                            run.maximized = if run.maximized == Some(set) { None } else { Some(set) };
                        }
                        if icon_button(ui, icons::FLOPPY_DISK, "Save results as…", true).clicked() {
                            actions.push(ResultsAction::Export { set, selection_only: false });
                        }
                        if icon_button(ui, icons::COPY, "Copy with headers (Ctrl+Shift+C)", true).clicked() {
                            actions.push(ResultsAction::Copy { set, kind: CopyKind::TsvWithHeaders });
                        }
                        if icon_button(ui, icons::ARROW_SQUARE_OUT, "Open result set in its own tab", true).clicked() {
                            actions.push(ResultsAction::PopOut { set });
                        }
                        let rs = &run.result_sets[set].rs;
                        let vs = rs.view_spec();
                        if !vs.is_identity() && icon_button(ui, icons::FUNNEL_X, "Clear filters and sorts", true).clicked() {
                            actions.push(ResultsAction::ApplyView { set, spec: cobalt_results::ViewSpec::default() });
                        }
                    }
                }
            });
        });
    });

    match tab.results_tab {
        ResultsTab::Results => {
            if data_sets.is_empty() {
                egui::Frame::new().fill(theme.bg_panel).show(ui, |ui| {
                    ui.set_min_size(ui.available_size());
                    ui.vertical_centered(|ui| {
                        ui.add_space(20.0);
                        let text = if run.is_live() { "Waiting for results…" } else if run.rows_affected.is_empty() { "The query returned no result sets." } else { "Command(s) completed successfully." };
                        ui.label(RichText::new(text).color(theme.text_muted));
                    });
                });
            } else {
                let avail = ui.available_height();
                let shown: Vec<usize> = match run.maximized {
                    Some(m) if data_sets.contains(&m) => vec![m],
                    _ => data_sets.clone(),
                };
                let n = shown.len();
                let each = if n == 1 { avail } else { (avail / n as f32).max(160.0) };
                egui::ScrollArea::vertical().id_salt(("results-stack", tab.id)).auto_shrink([false, false]).show(ui, |ui| {
                    for (k, &set) in shown.iter().enumerate() {
                        let paused = run.paused_set == Some(set);
                        let bar_h = if paused { 30.0 } else { 0.0 };
                        let header_h = if n > 1 { 20.0 } else { 0.0 };
                        let grid_h = (each - bar_h - header_h - if k + 1 < n { 6.0 } else { 0.0 }).max(80.0);
                        if n > 1 {
                            let rs = &run.result_sets[set].rs;
                            ui.horizontal(|ui| {
                                ui.add_space(6.0);
                                ui.label(RichText::new(format!("Result {}  ·  {} rows", set + 1, crate::state::fmt_count(rs.row_count() as u64))).size(11.0).color(theme.text_muted));
                            });
                        }
                        let view = &mut run.result_sets[set];
                        let find_h = if view.grid.find.is_some() { 28.0 } else { 0.0 };
                        if view.grid.find.is_some() {
                            find_bar(ui, view, theme, args.fmt);
                        }
                        let grid_h = (grid_h - find_h).max(60.0);
                        let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), grid_h), egui::Sense::hover());
                        let mut child = ui.new_child(egui::UiBuilder::new().id_salt(("rs-child", run.id, set)).max_rect(rect).layout(egui::Layout::top_down(egui::Align::Min)));
                        child.set_clip_rect(rect);
                        let grid_actions = grid::show(
                            &mut child,
                            grid::GridArgs {
                                rs: &view.rs,
                                grid: &mut view.grid,
                                theme,
                                fmt: args.fmt,
                                font_size: args.settings.appearance.grid_font_size,
                                id_salt: egui::Id::new(("grid", tab.id, run.id, set)),
                                show_row_numbers: args.settings.results.show_row_numbers,
                                max_col_width: args.settings.results.max_column_width,
                                sample_rows: args.settings.results.auto_size_sample_rows,
                            },
                        );
                        for ga in grid_actions {
                            match ga {
                                GridAction::ApplyView(spec) => actions.push(ResultsAction::ApplyView { set, spec }),
                                GridAction::OpenViewer(r, c) => actions.push(ResultsAction::OpenViewer { set, row: r, col: c }),
                                GridAction::ContextMenu(pos) => {
                                    view.grid.focused = true;
                                    ui.memory_mut(|m| m.data.insert_temp(egui::Id::new(("grid-ctx", tab.id, set)), pos));
                                }
                                GridAction::Copy => actions.push(ResultsAction::Copy { set, kind: CopyKind::Tsv }),
                                GridAction::CopyWithHeaders => actions.push(ResultsAction::Copy { set, kind: CopyKind::TsvWithHeaders }),
                            }
                        }
                        // context menu
                        let ctx_id = egui::Id::new(("grid-ctx", tab.id, set));
                        let ctx_pos: Option<Pos2> = ui.memory(|m| m.data.get_temp(ctx_id));
                        if let Some(pos) = ctx_pos {
                            let mut close = false;
                            egui::Area::new(ctx_id.with("area")).order(egui::Order::Foreground).fixed_pos(pos).show(ui.ctx(), |ui| {
                                egui::Frame::popup(ui.style()).fill(theme.bg_panel).show(ui, |ui| {
                                    ui.set_min_width(220.0);
                                    let items: &[(&str, CopyKind)] = &[
                                        ("Copy", CopyKind::Tsv),
                                        ("Copy with headers", CopyKind::TsvWithHeaders),
                                        ("Copy headers", CopyKind::HeadersOnly),
                                        ("Copy as CSV", CopyKind::Csv),
                                        ("Copy as Markdown table", CopyKind::Markdown),
                                        ("Copy as JSON", CopyKind::Json),
                                        ("Copy as INSERT statements", CopyKind::Insert),
                                        ("Copy as IN list", CopyKind::InList),
                                    ];
                                    for (label, kind) in items {
                                        if ui.button(*label).clicked() {
                                            actions.push(ResultsAction::Copy { set, kind: *kind });
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
                                            actions.push(ResultsAction::OpenViewer { set, row: r, col: c });
                                        }
                                        close = true;
                                    }
                                    ui.separator();
                                    if ui.button("Save results as…").clicked() {
                                        actions.push(ResultsAction::Export { set, selection_only: false });
                                        close = true;
                                    }
                                    if ui.button("Save selection as…").clicked() {
                                        actions.push(ResultsAction::Export { set, selection_only: true });
                                        close = true;
                                    }
                                    ui.separator();
                                    if ui.button("Clear filters and sorts").clicked() {
                                        actions.push(ResultsAction::ApplyView { set, spec: cobalt_results::ViewSpec::default() });
                                        close = true;
                                    }
                                    if ui.button(if run.maximized == Some(set) { "Restore" } else { "Maximize" }).clicked() {
                                        run.maximized = if run.maximized == Some(set) { None } else { Some(set) };
                                        close = true;
                                    }
                                });
                            });
                            let clicked_elsewhere = ui.input(|i| i.pointer.any_click()) && !ui.ctx().rect_contains_pointer(egui::LayerId::new(egui::Order::Foreground, ctx_id.with("area")), egui::Rect::from_min_size(pos, Vec2::new(240.0, 360.0)));
                            if close || clicked_elsewhere || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                                ui.memory_mut(|m| m.data.remove::<Pos2>(ctx_id));
                            }
                        }
                        if paused {
                            let rows = view.rs.row_count();
                            egui::Frame::new().fill(theme.tint(theme.warning, 0.15)).inner_margin(egui::Margin::symmetric(8, 4)).show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new(format!("{} Showing {} rows — the query is still open on the server.", icons::PAUSE_CIRCLE, crate::state::fmt_count(rows as u64))).size(12.0));
                                    let cap = args.settings.execution.row_cap.max(1000);
                                    if ui.small_button(format!("Fetch {} more", crate::state::fmt_count(cap))).clicked() {
                                        actions.push(ResultsAction::FetchMore { set, rows: Some(cap) });
                                    }
                                    if ui.small_button("Fetch all").clicked() {
                                        actions.push(ResultsAction::FetchMore { set, rows: None });
                                    }
                                    if ui.small_button("Stop").clicked() {
                                        actions.push(ResultsAction::Cancel);
                                    }
                                });
                            });
                        }
                        if k + 1 < n {
                            ui.add_space(6.0);
                        }
                    }
                });
            }
        }
        ResultsTab::Messages => {
            egui::Frame::new().fill(theme.bg_editor).inner_margin(8.0).show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                egui::ScrollArea::vertical().id_salt(("messages", tab.id)).auto_shrink([false, false]).stick_to_bottom(run.is_live()).show(ui, |ui| {
                    ui.style_mut().override_font_id = Some(egui::FontId::monospace(args.settings.appearance.grid_font_size));
                    for m in &run.messages {
                        let color = if m.is_error { theme.error } else if m.is_batch_header { theme.text_muted } else { theme.text };
                        let text = RichText::new(&m.text).color(color);
                        if let Some(line) = m.line {
                            let r = ui.add(egui::Label::new(text.underline()).sense(egui::Sense::click()));
                            if r.clicked() {
                                actions.push(ResultsAction::JumpToLine(line));
                            }
                            r.on_hover_text(format!("Go to line {line}"));
                        } else {
                            ui.label(text);
                        }
                    }
                    if run.messages.is_empty() {
                        ui.label(RichText::new("No messages.").color(theme.text_faint));
                    }
                });
            });
            let resp = ui.interact(ui.min_rect(), egui::Id::new(("messages-ctx", tab.id)), egui::Sense::click());
            resp.context_menu(|ui| {
                if ui.button("Copy all").clicked() {
                    let all: String = run.messages.iter().map(|m| m.text.clone()).collect::<Vec<_>>().join("\n");
                    ui.ctx().copy_text(all);
                    ui.close();
                }
            });
        }
        ResultsTab::Plan => {
            // drawn by the plan module via the shell (needs the parsed plan cache)
        }
    }
    actions
}

/// Find-in-results bar. Matches are recomputed when the text or the view changes (capped at 200k cells).
fn find_bar(ui: &mut Ui, view: &mut crate::state::ResultSetView, theme: &Theme, fmt: &CellFormatter) {
    let rs = view.rs.clone();
    let Some(find) = view.grid.find.as_mut() else { return };
    let mut close = false;
    let mut step: i32 = 0;
    egui::Frame::new().fill(theme.bg_sidebar).inner_margin(egui::Margin::symmetric(8, 3)).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(icons::MAGNIFYING_GLASS);
            let r = ui.add(egui::TextEdit::singleline(&mut find.text).hint_text("Find in results").desired_width(220.0).id(egui::Id::new(("grid-find", rs.index, std::sync::Arc::as_ptr(&rs) as usize))));
            if !r.has_focus() && find.matches.is_empty() && find.text.is_empty() {
                r.request_focus();
            }
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                step = 1;
                r.request_focus();
            }
            let gen = rs.generation();
            let changed = r.changed() || find.generation != gen;
            if changed {
                find.generation = gen;
                find.matches.clear();
                find.current = 0;
                let needle = find.text.to_lowercase();
                if !needle.is_empty() {
                    let rows = rs.visible_count();
                    let cols = rs.column_count();
                    let mut budget = 200_000usize;
                    'outer: for row in 0..rows {
                        for col in 0..cols {
                            if budget == 0 {
                                break 'outer;
                            }
                            budget -= 1;
                            if rs.cell_text(row, col, fmt).to_lowercase().contains(&needle) {
                                find.matches.push((row, col));
                            }
                        }
                    }
                }
                step = if find.matches.is_empty() { 0 } else { 0 };
                if !find.matches.is_empty() {
                    let (r0, c0) = find.matches[0];
                    view.grid.anchor = Some((r0, c0));
                    view.grid.selection = Selection::Cells { r0, c0, r1: r0, c1: c0 };
                    view.grid.scroll_to = Some((r0, c0));
                }
            }
            if ui.small_button(icons::CARET_UP).on_hover_text("Previous (Shift+Enter)").clicked() {
                step = -1;
            }
            if ui.small_button(icons::CARET_DOWN).on_hover_text("Next (Enter)").clicked() {
                step = 1;
            }
            let label = if find.text.is_empty() { String::new() } else if find.matches.is_empty() { "No matches".into() } else { format!("{} of {}", find.current + 1, find.matches.len()) };
            ui.label(RichText::new(label).size(12.0).color(theme.text_muted));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button(icons::X).clicked() {
                    close = true;
                }
            });
        });
    });
    if step != 0 && !find.matches.is_empty() {
        let n = find.matches.len();
        find.current = ((find.current as i32 + step).rem_euclid(n as i32)) as usize;
        let (r0, c0) = find.matches[find.current];
        view.grid.anchor = Some((r0, c0));
        view.grid.selection = Selection::Cells { r0, c0, r1: r0, c1: c0 };
        view.grid.scroll_to = Some((r0, c0));
    }
    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        close = true;
    }
    if close {
        view.grid.find = None;
    }
}

pub fn state_color(theme: &Theme, state: &RunState) -> Color32 {
    match state {
        RunState::Streaming | RunState::Paused => theme.info,
        RunState::Complete => theme.success,
        RunState::Cancelled => theme.warning,
        RunState::Error { .. } => theme.error,
    }
}
