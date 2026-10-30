//! The results area under an editor: Results / Messages / Plan tabs, stacked result sets,
//! the fetch-more bar, the cell viewer, and the per-grid context menu.

pub mod chart;
pub mod matrix;
pub mod grid;
pub mod profile;
pub mod viewer;

use crate::copy::CopyKind;
use crate::state::{EditorTab, ResultsTab, RunViewState, Selection};
use crate::ui::theme::Theme;
use crate::ui::widgets::icon_button;
use cobalt_core::Settings;
use cobalt_results::CellFormatter;
use egui::{Pos2, RichText, Ui, Vec2};
use egui_phosphor::regular as icons;
use grid::GridAction;

/// Requests bubbled up to the app layer.
#[derive(Debug, Clone)]
pub enum ResultsAction {
    FetchMore { rows: Option<u64> },
    /// A capped Spark result: run the same text again without the row cap.
    RerunUncapped,
    Cancel,
    Copy { set: usize, kind: CopyKind },
    Export { set: usize, selection_only: bool },
    ApplyView { set: usize, spec: cobalt_results::ViewSpec },
    OpenViewer { set: usize, row: usize, col: usize },
    /// The viewer in record mode for one row (every column, one per line).
    OpenRecord { set: usize, row: usize, col: usize },
    JumpToLine(u32),
    PopOut { set: usize },
    /// Totals row under the headers (None removes it).
    SetTotals { set: usize, kind: Option<crate::state::TotalKind> },
    /// Column profile window for the set.
    Profile { set: usize },
    /// Read the chart's pixels on the next frame: to the clipboard, a PNG file, or a cell's outputs.
    CaptureChart { set: usize, purpose: crate::state::CapturePurpose },
    /// Pin the result set (its query, connection and chart) as a dashboard tile.
    PinToDashboard { set: usize },
    /// Write the set (or selection) to a temporary .xlsx and open it with the system's app.
    OpenInExcel { set: usize, selection_only: bool },
    /// Load the set (or selection) into a table on a connected tab's database.
    SaveAsTable { set: usize, selection_only: bool },
    /// Open a Local query tab over this result set (`SELECT * FROM <its table> LIMIT 100`).
    QueryLocally { set: usize },
    /// The Explore builder over this result set's local table.
    ExploreLocally { set: usize },
    /// The Diff setup with this result set's table as A.
    DiffLocally { set: usize },
    /// Edit Data on the set (a single-table SELECT on a file connection).
    EditBegin { set: usize },
    EditSave { set: usize },
    /// Drop the staged changes; `off` also leaves Edit Data.
    EditDiscard { set: usize, off: bool },
    /// Persist this result set as `scratch.<name>` (back next session).
    SaveLocally { set: usize },
}

pub struct ResultsArgs<'a> {
    pub tab: &'a mut EditorTab,
    pub theme: &'a Theme,
    pub settings: &'a Settings,
    pub fmt: &'a CellFormatter,
}

pub fn show(ui: &mut Ui, args: ResultsArgs<'_>) -> Vec<ResultsAction> {
    let diff_tab = args.tab.local.as_ref().map(|l| l.diff.is_some()).unwrap_or(false);
    let file_conn = args.tab.profile.as_ref().map(|p| p.is_file() || p.engine == cobalt_core::EngineFamily::Postgres).unwrap_or(false);
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
    let has_plan = !run.plans.is_empty() || run.text_plan.is_some();

    // what the tab strip shows, read before the tools borrow the tab
    let err_count = run.messages.iter().filter(|m| m.is_error).count();
    let plan_count = run.plans.len().max(usize::from(run.text_plan.is_some()));
    let plan_flags_all: usize = run.plans.iter().filter_map(|p| p.parsed.as_ref()).map(|p| p.all_statements().iter().map(|s| s.warnings.len() + s.missing_indexes.len() + s.nodes.iter().map(|n| n.warnings.len()).sum::<usize>()).sum::<usize>()).sum();
    let run_state = run.state;
    let run_live = run.is_live();
    let tab_id = tab.id;
    let rows_key = egui::Id::unique(("results-toolbar-rows", tab_id));
    let two_rows: bool = ui.data(|d| d.get_temp(rows_key)).unwrap_or(false);
    // the result-set tools; on a narrow pane (a group beside another) they take their own row
    let icons = |ui: &mut Ui, tab: &mut EditorTab, actions: &mut Vec<ResultsAction>| {
        let run = tab.run.as_mut().unwrap();
                {
                    let side = tab.results_side.unwrap_or(args.settings.appearance.results_side);
                    let r = icon_button(ui, if side { icons::ROWS } else { icons::COLUMNS }, if side { "Results under the editor" } else { "Results beside the editor" }, true);
                    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "results side"));
                    if r.clicked() {
                        tab.results_side = Some(!side);
                    }
                }
                if tab.results_tab == ResultsTab::Results {
                    if let Some(&set) = data_sets.first() {
                        let set = run.result_sets.iter().position(|s| !s.is_plan && s.grid.focused).unwrap_or(set);
                        let is_max = run.maximized.is_some();
                        let charted = run.result_sets[set].chart.is_some();
                        let r = icon_button(ui, if charted { icons::TABLE } else { icons::CHART_LINE }, if charted { "Back to the grid" } else { "Chart this result set (bar, line, area, scatter, pie; follows the grid's sort and filter)" }, true);
                        r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "chart toggle"));
                        if r.clicked() {
                            let view = &mut run.result_sets[set];
                            if charted {
                                view.chart = None;
                            } else {
                                let selected: Vec<usize> = match view.grid.selection {
                                    Selection::Cols { c0, c1 } | Selection::Cells { c0, c1, .. } => (c0.min(c1)..=c0.max(c1)).collect(),
                                    _ => Vec::new(),
                                };
                                view.chart = Some(crate::state::ChartView::new(cobalt_results::chart::default_spec(&view.rs, &selected)));
                            }
                        }
                        let r = icon_button(ui, if is_max { icons::ARROWS_IN } else { icons::ARROWS_OUT }, if is_max { "Restore the editor" } else { "Maximize this result set (hides the editor)" }, true);
                        r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, if is_max { "restore results" } else { "maximize results" }));
                        if r.clicked() {
                            run.maximized = if is_max { None } else { Some(set) };
                        }
                        if icon_button(ui, icons::FLOPPY_DISK, "Save results as…", true).clicked() {
                            actions.push(ResultsAction::Export { set, selection_only: false });
                        }
                        if icon_button(ui, icons::COPY, "Copy with headers (Ctrl+Shift+C)", true).clicked() {
                            actions.push(ResultsAction::Copy { set, kind: CopyKind::TsvWithHeaders });
                        }
                        if icon_button(ui, icons::MAGNIFYING_GLASS, "Find in results (Ctrl+F while the grid has focus)", true).clicked() {
                            let g = &mut run.result_sets[set].grid;
                            g.find.get_or_insert_with(Default::default);
                            g.focused = true;
                        }
                        if icon_button(ui, icons::CHART_BAR, "Profile columns", true).clicked() {
                            actions.push(ResultsAction::Profile { set });
                        }
                        if icon_button(ui, icons::MICROSOFT_EXCEL_LOGO, "Open in Excel", true).clicked() {
                            actions.push(ResultsAction::OpenInExcel { set, selection_only: false });
                        }
                        if icon_button(ui, icons::TABLE, "Save as table…", true).clicked() {
                            actions.push(ResultsAction::SaveAsTable { set, selection_only: false });
                        }
                        if file_conn {
                            let on = run.result_sets[set].edits.is_some();
                            let r = icon_button(ui, icons::PENCIL_SIMPLE, if on { "Leave Edit Data (drops pending changes)" } else { "Edit Data: change cells, add and delete rows in this grid; Save writes them in one transaction (single-table SELECT with the key)" }, !run_live);
                            r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, format!("edit data {set}")));
                            if r.clicked() {
                                actions.push(if on { ResultsAction::EditDiscard { set, off: true } } else { ResultsAction::EditBegin { set } });
                            }
                        }
                        let r = icon_button(ui, icons::FUNNEL_SIMPLE, "Query this result (SQL over it in a Local tab; join it with other results)", true);
                        r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, format!("query locally {set}")));
                        if r.clicked() {
                            actions.push(ResultsAction::QueryLocally { set });
                        }
                        let r = icon_button(ui, icons::ROWS, "Explore this result (pivot: rows, columns, values, filters; no SQL)", true);
                        r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, format!("explore locally {set}")));
                        if r.clicked() {
                            actions.push(ResultsAction::ExploreLocally { set });
                        }
                        let r = icon_button(ui, icons::ARCHIVE, "Save as local table… (scratch.<name>, kept across sessions in the app data folder)", true);
                        r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, format!("save locally {set}")));
                        if r.clicked() {
                            actions.push(ResultsAction::SaveLocally { set });
                        }
                        let r = icon_button(ui, icons::ARROWS_LEFT_RIGHT, "Diff with another result… (added / removed / changed rows by key)", true);
                        r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, format!("diff locally {set}")));
                        if r.clicked() {
                            actions.push(ResultsAction::DiffLocally { set });
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
    };

    // tab strip
    let toolbar = egui::Frame::new().fill(theme.bg_sidebar).inner_margin(egui::Margin::symmetric(6, 2)).show(ui, |ui| {
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
                    ResultsTab::Messages => err_count,
                    ResultsTab::Plan => plan_count,
                };
                let plan_flags: usize = if t == ResultsTab::Plan { plan_flags_all } else { 0 };
                let mut label_text = if t == ResultsTab::Messages && count > 0 { format!("{label} ({count} error{})", if count == 1 { "" } else { "s" }) } else if t == ResultsTab::Results && count > 1 { format!("{label} ({count})") } else { label.to_string() };
                if plan_flags > 0 {
                    label_text.push_str(&format!("  {} {plan_flags}", icons::WARNING));
                }
                let _ = text;
                let mut text = RichText::new(label_text).size(13.0);
                if selected {
                    text = text.color(theme.accent).strong();
                }
                let r = ui.add(egui::Button::new(text).frame(false));
                r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, format!("results tab {label}")));
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
            let status = match run_state {
                RunViewState::Running => format!("{} Executing…", icons::CIRCLE_NOTCH),
                RunViewState::Paused => "Paused at row cap".to_string(),
                RunViewState::Cancelling => "Cancelling…".to_string(),
                RunViewState::Done => "Completed".to_string(),
                RunViewState::Failed => "Completed with errors".to_string(),
                RunViewState::Cancelled => "Cancelled".to_string(),
            };
            let color = match run_state {
                RunViewState::Failed => theme.error,
                RunViewState::Done => theme.success,
                _ => theme.text_muted,
            };
            ui.label(RichText::new(status).size(12.0).color(color));
            if run_live {
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
            }
            // one row while the tools fit beside the tabs; they move to a second row when they
            // collide, and come back once the pane is wide enough for both
            let tabs_end = ui.cursor().min.x;
            if !two_rows {
                let r = ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| icons(ui, tab, &mut actions));
                ui.data_mut(|d| d.insert_temp(rows_key.with("w"), r.response.rect.width()));
                if r.response.rect.left() < tabs_end + 6.0 {
                    ui.data_mut(|d| d.insert_temp(rows_key, true));
                }
            } else {
                let need: f32 = ui.data(|d| d.get_temp(rows_key.with("w"))).unwrap_or(400.0);
                if ui.max_rect().right() - tabs_end > need + 24.0 {
                    ui.data_mut(|d| d.insert_temp(rows_key, false));
                }
            }
        });
        if two_rows {
            ui.horizontal(|ui| {
                let r = ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| icons(ui, tab, &mut actions));
                ui.data_mut(|d| d.insert_temp(rows_key.with("w"), r.response.rect.width()));
            });
        }
    });
    ui.data_mut(|d| d.insert_temp(egui::Id::unique(("results-toolbar-h", tab_id)), toolbar.response.rect.height()));
    let run = tab.run.as_mut().unwrap();

    match tab.results_tab {
        ResultsTab::Results => {
            if run.capped && (tab.spark.is_some() || tab.tabular.is_some()) {
                let cap = if tab.tabular.is_some() { args.settings.execution.row_cap } else { args.settings.notebooks.spark_row_limit };
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{} Capped at {} rows per statement", icons::INFO, crate::state::fmt_count(cap))).size(12.0).color(theme.text_muted));
                    if ui.add_enabled(!run.is_live(), egui::Button::new("Run again without the cap").small()).on_hover_text("Streams every row of the result; large results take memory in the grid.").clicked() {
                        actions.push(ResultsAction::RerunUncapped);
                    }
                });
            }
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
                        let show_header = n > 1 || run.export_target.is_some();
                        let header_h = if show_header { 20.0 } else { 0.0 };
                        let grid_h = (each - bar_h - header_h - if k + 1 < n { 6.0 } else { 0.0 }).max(80.0);
                        if show_header {
                            let rs = &run.result_sets[set].rs;
                            ui.horizontal(|ui| {
                                ui.add_space(6.0);
                                let rows_label = if rs.visible_count() != rs.row_count() {
                                    format!("{} of {} rows", crate::state::fmt_count(rs.visible_count() as u64), crate::state::fmt_count(rs.row_count() as u64))
                                } else {
                                    format!("{} rows", crate::state::fmt_count(rs.row_count() as u64))
                                };
                                let rows_label = match &run.export_target {
                                    Some(target) => format!("preview of the first {} rows  ·  {} {target}", crate::state::fmt_count(rs.row_count() as u64), if run.is_live() { "streaming to" } else { "exported to" }),
                                    None => rows_label,
                                };
                                ui.label(RichText::new(format!("Result {}  ·  {rows_label}", set + 1)).size(11.0).color(theme.text_muted));
                            });
                        }
                        let view = &mut run.result_sets[set];
                        // keep the totals row current when rows arrive or the view changes
                        if let Some(t) = view.grid.totals.as_mut() {
                            let g = view.rs.generation();
                            if t.generation != g {
                                t.values = profile::totals(&view.rs, t.kind);
                                t.generation = g;
                            }
                        }
                        let find_h = if view.grid.find.is_some() { 28.0 } else { 0.0 };
                        if view.grid.find.is_some() {
                            find_bar(ui, view, theme, args.fmt);
                        }
                        let grid_h = (grid_h - find_h).max(60.0);
                        let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), grid_h), egui::Sense::hover());
                        let mut child = ui.new_child(egui::UiBuilder::new().id_salt(("rs-child", run.id, set)).max_rect(rect).layout(egui::Layout::top_down(egui::Align::Min)));
                        child.set_clip_rect(rect.intersect(ui.clip_rect()));
                        let matrix = if set == 0 && view.chart.is_none() { tab.explore.as_deref().filter(|e| e.matrix).and_then(|e| e.pivot.clone()) } else { None };
                        let grid_actions = if let Some(pt) = matrix {
                            for a in matrix::show(&mut child, theme, &pt, "explore matrix") {
                                match a {
                                    matrix::MatrixAction::CopyTsv(s) => ui.ctx().copy_text(s),
                                }
                            }
                            Vec::new()
                        } else if view.chart.is_some() {
                            // the chart in place of the grid; the grid's state (selection, view) stays
                            for a in chart::show(&mut child, view, theme, args.fmt, set, false) {
                                match a {
                                    chart::ChartAction::Capture { save } => actions.push(ResultsAction::CaptureChart { set, purpose: if save { crate::state::CapturePurpose::SaveFile } else { crate::state::CapturePurpose::Clipboard } }),
                                    chart::ChartAction::Keep => {}
                                    chart::ChartAction::Close => view.chart = None,
                                }
                            }
                            Vec::new()
                        } else {
                            if view.edits.is_some() {
                                edit_strip(&mut child, view, theme, args.fmt, set, &mut actions);
                            }
                            grid::show(
                                &mut child,
                                grid::GridArgs {
                                    rs: &view.rs,
                                    grid: &mut view.grid,
                                    theme,
                                    fmt: args.fmt,
                                    font_size: args.settings.appearance.grid_font_size,
                                    id_salt: egui::Id::unique(("grid", tab.id, run.id, set)),
                                    show_row_numbers: args.settings.results.show_row_numbers,
                                    max_col_width: args.settings.results.max_column_width,
                                    sample_rows: args.settings.results.auto_size_sample_rows,
                                    row_tint: if diff_tab { grid::DiffTint::from_columns(&view.rs.columns) } else { None },
                                    edits: view.edits.as_mut(),
                                },
                            )
                        };
                        for ga in grid_actions {
                            match ga {
                                GridAction::ApplyView(spec) => actions.push(ResultsAction::ApplyView { set, spec }),
                                GridAction::OpenViewer(r, c) => actions.push(ResultsAction::OpenViewer { set, row: r, col: c }),
                                GridAction::ContextMenu(pos) => {
                                    view.grid.focused = true;
                                    ui.memory_mut(|m| m.data.insert_temp(egui::Id::unique(("grid-ctx", tab.id, set)), pos));
                                }
                                GridAction::Copy => actions.push(ResultsAction::Copy { set, kind: CopyKind::Tsv }),
                            }
                        }
                        // Escape in a maximized grid brings the editor back
                        let restore = view.grid.focused && view.grid.filter_popup.is_none() && view.grid.find.is_none() && view.grid.viewer.is_none() && ui.input(|i| i.key_pressed(egui::Key::Escape));
                        if restore && run.maximized.is_some() {
                            run.maximized = None;
                        }
                        // context menu
                        let ctx_id = egui::Id::unique(("grid-ctx", tab.id, set));
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
                                    if ui.button("View row as record").on_hover_text("Every column of this row, one per line — handy for wide tables").clicked() {
                                        if let Some((r, c)) = view.grid.anchor {
                                            actions.push(ResultsAction::OpenRecord { set, row: r, col: c });
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
                                    if ui.button(format!("{} Pin to dashboard…", icons::PUSH_PIN)).on_hover_text("A tile with this query, its connection and its chart, refreshed on an interval").clicked() {
                                        actions.push(ResultsAction::PinToDashboard { set });
                                        close = true;
                                    }
                                    if ui.button("Find in results…").on_hover_text("Ctrl+F while the grid has focus; match case, whole word and regex toggles").clicked() {
                                        view.grid.find.get_or_insert_with(Default::default);
                                        close = true;
                                    }
                                    ui.menu_button("Totals row", |ui| {
                                        let current = view.grid.totals.as_ref().map(|t| t.kind);
                                        if ui.selectable_label(current.is_none(), "None").clicked() {
                                            actions.push(ResultsAction::SetTotals { set, kind: None });
                                            ui.close();
                                        }
                                        for k in crate::state::TotalKind::ALL {
                                            if ui.selectable_label(current == Some(k), k.label()).clicked() {
                                                actions.push(ResultsAction::SetTotals { set, kind: Some(k) });
                                                ui.close();
                                            }
                                        }
                                    });
                                    if ui.button("Profile columns…").on_hover_text("Nulls, distinct values, min/max/avg, top values and distribution per column").clicked() {
                                        actions.push(ResultsAction::Profile { set });
                                        close = true;
                                    }
                                    if ui.button("Save as table…").on_hover_text("Load these rows into a new or existing table on any connected tab's database").clicked() {
                                        actions.push(ResultsAction::SaveAsTable { set, selection_only: false });
                                        close = true;
                                    }
                                    if ui.button("Open in Excel").on_hover_text("Writes a temporary .xlsx and opens it").clicked() {
                                        actions.push(ResultsAction::OpenInExcel { set, selection_only: false });
                                        close = true;
                                    }
                                    ui.separator();
                                    // filter every column the selection spans to the selected values
                                    let single = matches!(view.grid.selection, Selection::Cells { r0, c0, r1, c1 } if r0 == r1 && c0 == c1);
                                    let sel_filter = filter_to_selection(&view.grid, &view.rs, args.fmt);
                                    if ui.add_enabled(sel_filter.is_some(), egui::Button::new(if single { "Filter to this value" } else { "Filter to selected values" })).on_hover_text("Keep only rows whose values in the selected columns match the selected cells").clicked() {
                                        if let Some(spec) = sel_filter {
                                            actions.push(ResultsAction::ApplyView { set, spec });
                                        }
                                        close = true;
                                    }
                                    let excl_filter = exclude_selection(&view.grid, &view.rs, args.fmt);
                                    if ui.add_enabled(excl_filter.is_some(), egui::Button::new(if single { "Exclude this value" } else { "Exclude selected values" })).on_hover_text("Hide rows whose values in the selected columns match the selected cells").clicked() {
                                        if let Some(spec) = excl_filter {
                                            actions.push(ResultsAction::ApplyView { set, spec });
                                        }
                                        close = true;
                                    }
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
                                        actions.push(ResultsAction::FetchMore { rows: Some(cap) });
                                    }
                                    if ui.small_button("Fetch all").clicked() {
                                        actions.push(ResultsAction::FetchMore { rows: None });
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
            let frame = egui::Frame::new().fill(theme.bg_editor).inner_margin(8.0).show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                egui::ScrollArea::vertical().id_salt(("messages", tab.id)).auto_shrink([false, false]).stick_to_bottom(run.is_live()).show(ui, |ui| {
                    ui.style_mut().override_font_id = Some(egui::FontId::monospace(args.settings.appearance.grid_font_size));
                    for m in &run.messages {
                        let color = if m.is_error { theme.error } else if m.is_batch_header { theme.text_muted } else { theme.text };
                        crate::fonts::note(ui.ctx(), &m.text);
                        let text = RichText::new(&m.text).color(color);
                        if let Some(line) = m.line {
                            let r = ui.add(egui::Label::new(text.underline()).sense(egui::Sense::click()));
                            if r.clicked() {
                                actions.push(ResultsAction::JumpToLine(line));
                            }
                            r.on_hover_text(format!("Go to line {line}"));
                        } else if let Some(path) = &m.path {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(text);
                                let b = ui.small_button(format!("{} Open folder", icons::FOLDER_OPEN));
                                b.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "open export folder"));
                                if b.clicked() {
                                    if let Some(parent) = path.parent() {
                                        let _ = open::that(parent);
                                    }
                                }
                            });
                        } else {
                            ui.label(text);
                        }
                    }
                    if run.messages.is_empty() {
                        ui.label(RichText::new("No messages.").color(theme.text_faint));
                    }
                });
            });
            // Only the messages pane itself takes the context-menu click. `ui.min_rect()` would
            // also cover the Results/Messages tab strip above and swallow its clicks.
            let resp = ui.interact(frame.response.rect, egui::Id::unique(("messages-ctx", tab.id)), egui::Sense::click());
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
    // column-profile windows (one per set that has one open)
    for (set, view) in run.result_sets.iter_mut().enumerate() {
        profile_window(ui.ctx(), tab.id, set, view, theme);
    }
    actions
}

/// Scan every visible cell of the result set for the matcher on a thread, reporting matches in
/// blocks of rows (the first block lands within milliseconds, so the first hit is instant while
/// a million-row set keeps scanning). The receiver's drop cancels the thread.
fn spawn_find_scan(rs: std::sync::Arc<cobalt_results::ResultSet>, fmt: CellFormatter, is_match: profile::FindMatcher, egui: egui::Context) -> crate::state::FindScan {
    use crate::state::FindMsg;
    use std::sync::atomic::{AtomicBool, Ordering};
    let (tx, rx) = crossbeam_channel::unbounded::<FindMsg>();
    let cancel = std::sync::Arc::new(AtomicBool::new(false));
    let c2 = cancel.clone();
    std::thread::Builder::new()
        .name("results-find".into())
        .spawn(move || {
            let cols = rs.column_count().max(1);
            // about 50,000 cells per block: a few ms of work, one message
            let block = (50_000 / cols).clamp(64, 5_000);
            let mut stopped = false;
            rs.find_cells(&fmt, &*is_match, block, &c2, |found, scanned_rows| {
                if tx.send(FindMsg::Block { matches: found, scanned_rows }).is_err() {
                    stopped = true;
                    return false;
                }
                egui.request_repaint();
                true
            });
            if !stopped && !c2.load(Ordering::Relaxed) {
                let _ = tx.send(FindMsg::Done);
                egui.request_repaint();
            }
        })
        .ok();
    crate::state::FindScan { rx, cancel }
}

/// Find-in-results bar. The scan over the whole result set runs on a thread and reports in
/// blocks; the status says how far it got until it has looked at every cell.
fn find_bar(ui: &mut Ui, view: &mut crate::state::ResultSetView, theme: &Theme, fmt: &CellFormatter) {
    let rs = view.rs.clone();
    let Some(find) = view.grid.find.as_mut() else { return };
    let mut close = false;
    let mut step: i32 = 0;
    let mut recompute = false;
    egui::Frame::new().fill(theme.bg_sidebar).inner_margin(egui::Margin::symmetric(8, 3)).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(icons::MAGNIFYING_GLASS);
            let r = ui.add(egui::TextEdit::singleline(&mut find.text).hint_text("Find in results").desired_width(220.0).id(egui::Id::unique(("grid-find", rs.index, std::sync::Arc::as_ptr(&rs) as usize))));
            r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, "find in results"));
            if !r.has_focus() && find.matches.is_empty() && find.text.is_empty() {
                r.request_focus();
            }
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                step = if ui.input(|i| i.modifiers.shift) { -1 } else { 1 };
                r.request_focus();
            }
            recompute |= r.changed();
            let toggle = |ui: &mut Ui, on: &mut bool, label: &str, tip: &str, a11y: &str| {
                let b = ui.add(egui::Button::selectable(*on, RichText::new(label).size(12.0)).small());
                b.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, a11y));
                if b.on_hover_text(tip).clicked() {
                    *on = !*on;
                    true
                } else {
                    false
                }
            };
            recompute |= toggle(ui, &mut find.case_sensitive, "Aa", "Match case", "find match case");
            recompute |= toggle(ui, &mut find.whole_word, "ab", "Whole word", "find whole word");
            recompute |= toggle(ui, &mut find.use_regex, ".*", "Regular expression", "find regex");
            let gen = rs.generation();
            if recompute || find.dirty || find.generation != gen {
                find.generation = gen;
                find.dirty = false;
                find.scan = None; // drops the receiver: a running scan stops
                find.matches.clear();
                find.match_set = Default::default();
                find.current = 0;
                find.error = None;
                find.scanned_rows = 0;
                find.total_rows = rs.visible_count();
                if !find.text.is_empty() {
                    match profile::find_matcher(&find.text, find.case_sensitive, find.use_regex, find.whole_word) {
                        Ok(is_match) => find.scan = Some(spawn_find_scan(rs.clone(), fmt.clone(), is_match, ui.ctx().clone())),
                        Err(e) => find.error = Some(e),
                    }
                }
            }
            // blocks from the scan: the first match selects and scrolls, the rest accumulate
            let mut done = false;
            if let Some(scan) = &find.scan {
                while let Ok(m) = scan.rx.try_recv() {
                    match m {
                        crate::state::FindMsg::Block { matches, scanned_rows } => {
                            find.scanned_rows = scanned_rows;
                            if matches.is_empty() {
                                continue;
                            }
                            let first = find.matches.is_empty();
                            std::sync::Arc::make_mut(&mut find.match_set).extend(matches.iter().copied());
                            find.matches.extend(matches);
                            if first {
                                let (r0, c0) = find.matches[0];
                                view.grid.anchor = Some((r0, c0));
                                view.grid.selection = Selection::Cells { r0, c0, r1: r0, c1: c0 };
                                view.grid.scroll_to = Some((r0, c0));
                            }
                        }
                        crate::state::FindMsg::Done => done = true,
                    }
                }
            }
            if done {
                find.scan = None;
                find.scanned_rows = find.total_rows;
            }
            if ui.small_button(icons::CARET_UP).on_hover_text("Previous (Shift+Enter)").clicked() {
                step = -1;
            }
            if ui.small_button(icons::CARET_DOWN).on_hover_text("Next (Enter)").clicked() {
                step = 1;
            }
            let complete = find.complete();
            let progress = if complete || find.total_rows == 0 { String::new() } else { format!(" · scanning {}%", (find.scanned_rows * 100 / find.total_rows).min(99)) };
            let (label, color) = match (&find.error, find.text.is_empty(), find.matches.is_empty()) {
                (Some(e), _, _) => (e.clone(), theme.error),
                (None, true, _) => (String::new(), theme.text_muted),
                (None, false, true) if complete => ("No matches".into(), theme.text_muted),
                (None, false, true) => (format!("No match yet{progress}"), theme.text_muted),
                (None, false, false) => (format!("{} of {}{progress}", find.current + 1, find.matches.len()), theme.text_muted),
            };
            let l = ui.label(RichText::new(label).size(12.0).color(color));
            l.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, "find status"));
            if !complete {
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
            }
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

/// The column-profile window of a result set.
pub fn profile_window(ctx: &egui::Context, tab_id: cobalt_core::TabId, set: usize, view: &mut crate::state::ResultSetView, theme: &Theme) {
    let Some(p) = view.profile.as_mut() else { return };
    if !p.open {
        return;
    }
    let mut open = true;
    crate::ui::chrome::Window::new(format!("Column profile · result {}", set + 1))
        .id(egui::Id::unique(("profile", tab_id, set)))
        .open(&mut open)
        .default_width(860.0)
        .default_height(420.0)
        .resizable(true)
        .show(ctx, theme, |ui| {
            ui.label(RichText::new(format!("{} rows scanned{}", crate::state::fmt_count(p.rows_scanned as u64), if p.rows_scanned >= profile::SCAN_CAP { " (capped)" } else { "" })).size(11.0).color(theme.text_muted));
            egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
                egui::Grid::new(("profile-grid", tab_id, set)).striped(true).spacing([14.0, 4.0]).show(ui, |ui| {
                    for h in ["Column", "Type", "Rows", "Nulls", "Distinct", "Min", "Max", "Avg", "Top values", "Distribution"] {
                        ui.label(RichText::new(h).small().strong().color(theme.text_muted));
                    }
                    ui.end_row();
                    for c in &p.columns {
                        let s = &c.summary;
                        ui.label(RichText::new(&c.name).monospace());
                        ui.label(RichText::new(&c.type_label).monospace().color(theme.text_muted));
                        ui.label(crate::state::fmt_count(s.count as u64));
                        let pct = if s.count > 0 { 100.0 * s.nulls as f64 / s.count as f64 } else { 0.0 };
                        ui.label(RichText::new(format!("{} ({pct:.0}%)", crate::state::fmt_count(s.nulls as u64))).color(if s.nulls > 0 { theme.warning } else { theme.text }));
                        ui.label(crate::state::fmt_count(s.distinct as u64));
                        ui.label(s.min.map(profile::fmt_num).unwrap_or_default());
                        ui.label(s.max.map(profile::fmt_num).unwrap_or_default());
                        ui.label(s.avg.map(profile::fmt_num).unwrap_or_default());
                        let top: Vec<String> = c.top.iter().map(|(v, n)| format!("{} ×{}", if v.chars().count() > 24 { format!("{}…", v.chars().take(24).collect::<String>()) } else { v.to_string() }, n)).collect();
                        ui.label(RichText::new(top.join("  ")).size(11.0));
                        if c.histogram.is_empty() {
                            ui.label("");
                        } else {
                            let (rect, r) = ui.allocate_exact_size(Vec2::new(150.0, 26.0), egui::Sense::hover());
                            let max = c.histogram.iter().copied().max().unwrap_or(1).max(1) as f32;
                            let w = rect.width() / c.histogram.len() as f32;
                            for (i, n) in c.histogram.iter().enumerate() {
                                let h = (rect.height() - 2.0) * (*n as f32 / max);
                                let bar = egui::Rect::from_min_max(egui::pos2(rect.left() + i as f32 * w + 1.0, rect.bottom() - h), egui::pos2(rect.left() + (i + 1) as f32 * w - 1.0, rect.bottom()));
                                ui.painter().rect_filled(bar, 1.0, theme.accent);
                            }
                            if let Some((lo, hi)) = c.hist_range {
                                r.on_hover_text(format!("{} … {}\n{} bins; tallest {}", profile::fmt_num(lo), profile::fmt_num(hi), c.histogram.len(), crate::state::fmt_count(max as u64)));
                            }
                        }
                        ui.end_row();
                    }
                });
            });
        });
    if !open {
        p.open = false;
    }
}


/// A view spec that hides the selected values: one `NotEquals` filter per distinct selected value
/// per spanned column (at most 50 values per column), keeping existing filters and sorts.
fn exclude_selection(grid: &crate::state::GridState, rs: &std::sync::Arc<cobalt_results::ResultSet>, fmt: &cobalt_results::CellFormatter) -> Option<cobalt_results::ViewSpec> {
    use cobalt_results::{ColumnFilter, FilterOp};
    if !matches!(grid.selection, Selection::Cells { .. } | Selection::Rows { .. }) {
        return None;
    }
    let (rows, cols) = grid.selection.resolve(rs.visible_count(), rs.column_count())?;
    if rows.end() - rows.start() >= 10_000 {
        return None;
    }
    let mut spec = grid.view.clone();
    for c in cols {
        let mut seen = std::collections::HashSet::new();
        for r in rows.clone() {
            let v = rs.cell_text(r, c, fmt).to_string();
            if seen.insert(v.clone()) {
                if seen.len() > 50 {
                    return None;
                }
                spec.filters.push(ColumnFilter { column: c, op: FilterOp::NotEquals(v) });
            }
        }
    }
    Some(spec)
}

/// A view spec that adds an `In` filter per column spanned by the grid selection, whose set is the
/// selected cells' display values in that column (existing filters on those columns are replaced,
/// other filters and sorts are kept). `None` when nothing useful is selected.
fn filter_to_selection(grid: &crate::state::GridState, rs: &std::sync::Arc<cobalt_results::ResultSet>, fmt: &cobalt_results::CellFormatter) -> Option<cobalt_results::ViewSpec> {
    use cobalt_results::{ColumnFilter, FilterOp};
    if !matches!(grid.selection, Selection::Cells { .. } | Selection::Rows { .. }) {
        return None;
    }
    let (rows, cols) = grid.selection.resolve(rs.visible_count(), rs.column_count())?;
    if rows.end() - rows.start() >= 10_000 {
        return None;
    }
    let mut spec = grid.view.clone();
    for c in cols {
        let set: std::collections::HashSet<String> = rows.clone().map(|r| rs.cell_text(r, c, fmt).to_string()).collect();
        spec.filters.retain(|f| f.column != c);
        spec.filters.push(ColumnFilter { column: c, op: FilterOp::In(set) });
    }
    Some(spec)
}

/// The Pending changes strip over an editable grid: count, add / delete rows, Save, Discard,
/// the statements, the last error.
fn edit_strip(ui: &mut Ui, view: &mut crate::state::ResultSetView, theme: &Theme, fmt: &CellFormatter, set: usize, actions: &mut Vec<ResultsAction>) {
    let n = view.edits.as_ref().map(|e| e.pending_count()).unwrap_or(0);
    let saving = view.edits.as_ref().map(|e| e.saving).unwrap_or(false);
    let err = view.edits.as_ref().and_then(|e| e.error.clone());
    let show_sql = view.edits.as_ref().map(|e| e.show_sql).unwrap_or(false);
    let table = view.edits.as_ref().map(|e| e.table.clone()).unwrap_or_default();
    let stmts = if show_sql { crate::editq::statements(view, fmt) } else { Vec::new() };
    let cols_n = view.rs.column_count();
    let data_rows = view.rs.visible_count();
    let sel_rows: Vec<usize> = match view.grid.selection {
        Selection::Rows { r0, r1 } | Selection::Cells { r0, r1, .. } => (r0.min(r1)..=r0.max(r1)).collect(),
        _ => view.grid.anchor.map(|(r, _)| vec![r]).unwrap_or_default(),
    };
    egui::Frame::new().fill(theme.bg_panel).inner_margin(egui::Margin::symmetric(8, 4)).show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(format!("{} Edit Data · {table}", icons::PENCIL_SIMPLE)).strong().color(theme.accent)).on_hover_text("Double-click or F2 edits a cell, Tab moves on, Delete marks the selected rows. Nothing is written until Save.");
            let r = ui.label(RichText::new(if n == 0 { "no pending changes".to_string() } else { format!("{n} pending change{}", if n == 1 { "" } else { "s" }) }).color(if n == 0 { theme.text_muted } else { theme.warning }));
            r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, format!("edit pending {n}")));
            ui.add_space(8.0);
            let r = ui.add_enabled(!saving, egui::Button::new(format!("{} Row", icons::PLUS))).on_hover_text("A new row at the bottom; fill its cells, Save inserts it");
            r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "edit add row"));
            if r.clicked() {
                if let Some(e) = view.edits.as_mut() {
                    e.inserted.push(vec![None; cols_n]);
                }
                view.grid.scroll_to = Some((data_rows + view.edits.as_ref().map(|e| e.inserted.len()).unwrap_or(1) - 1, 0));
            }
            let r = ui.add_enabled(!saving && !sel_rows.is_empty(), egui::Button::new(format!("{} Delete row{}", icons::TRASH, if sel_rows.len() == 1 { "" } else { "s" }))).on_hover_text("Mark the selected rows for deletion (again to unmark); a new row is removed");
            r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "edit delete rows"));
            if r.clicked() {
                if let Some(e) = view.edits.as_mut() {
                    for &row in sel_rows.iter().rev() {
                        if row >= data_rows {
                            if row - data_rows < e.inserted.len() {
                                e.inserted.remove(row - data_rows);
                            }
                        } else if !e.deleted.remove(&row) {
                            e.deleted.insert(row);
                        }
                    }
                    e.editing = None;
                }
            }
            ui.separator();
            let r = ui.add_enabled(n > 0 && !saving, egui::Button::new(RichText::new(format!("{} Save", icons::CHECK)).color(theme.success))).on_hover_text("Run the statements in one transaction, then run the query again");
            r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "edit save"));
            if r.clicked() {
                actions.push(ResultsAction::EditSave { set });
            }
            let r = ui.add_enabled(!saving, egui::Button::new(format!("{} Discard", icons::ARROW_COUNTER_CLOCKWISE))).on_hover_text("Drop every pending change");
            r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "edit discard"));
            if r.clicked() {
                actions.push(ResultsAction::EditDiscard { set, off: false });
            }
            let r = ui.add(egui::Button::selectable(show_sql, "SQL")).on_hover_text("The statements Save will run");
            r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "edit show sql"));
            if r.clicked() {
                if let Some(e) = view.edits.as_mut() {
                    e.show_sql = !e.show_sql;
                }
            }
            if saving {
                ui.spinner();
                ui.label(RichText::new("Saving…").color(theme.text_muted));
            }
            if let Some(e) = err {
                ui.label(RichText::new(e).color(theme.error));
            }
        });
        if show_sql {
            egui::ScrollArea::vertical().id_salt(("edit-sql", set)).max_height(120.0).show(ui, |ui| {
                if stmts.is_empty() {
                    ui.label(RichText::new("No statements yet.").color(theme.text_muted));
                }
                for st in &stmts {
                    ui.label(RichText::new(st).monospace().size(12.0));
                }
            });
        }
    });
}
