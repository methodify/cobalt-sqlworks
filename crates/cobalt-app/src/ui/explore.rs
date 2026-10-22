//! The Explore tab: a pivot-table-style builder on a semantic model. Fields on the left (the
//! model's tables with their columns and measures), the wells (Rows, Columns, Values, Filters)
//! and the generated DAX above the results, which show as the matrix or the flat grid.
//! Design: `docs/design/semantic_models.md` §3.1 (5).

use crate::exploreq::{self as ops, DragField, Well};
use crate::state::{Focus, Loadable, ResultsTab, ToastKind};
use crate::ui::results::{self, ResultsAction};
use crate::ui::shell::Frame;
use crate::ui::theme::Theme;
use crate::ui::widgets::{icon_button, tool_button};
use cobalt_core::chart::Agg;
use cobalt_core::explore::{DateUnit, FieldRef, FilterOp, SortBy, SortSpec};
use cobalt_core::tabular::{ModelCatalog, TabularDataType};
use egui::{RichText, Sense, Ui, Vec2};
use egui_phosphor::regular as icons;
use std::sync::Arc;

const FIELDS_W: f32 = 240.0;
const AGGS: [(Agg, &str); 6] = [(Agg::Sum, "Sum"), (Agg::Count, "Count"), (Agg::CountDistinct, "Count distinct"), (Agg::Avg, "Average"), (Agg::Min, "Min"), (Agg::Max, "Max")];

enum Act {
    Add(FieldRef, Well),
    /// A drag-and-drop: field, the well it came from, the well it landed on, the chip it landed before.
    Drop(FieldRef, Option<Well>, Well, Option<FieldRef>),
    Remove(FieldRef),
    Move(FieldRef, Well, i32),
    Agg(usize, Agg),
    Sort(Option<SortSpec>),
    TopN(Option<u64>),
    Subtotals(bool),
    EditFilter(usize),
    RemoveFilter(usize),
    ApplyFilter,
    CancelFilter,
    LoadDistinct(String, String),
    Run,
    Cancel,
    OpenDax,
    Save(bool),
    Pin,
    Refresh,
    /// Switch the tab to another semantic model.
    ChooseModel,
}

pub fn show(ui: &mut Ui, f: &mut Frame<'_>, idx: usize) {
    let theme = f.theme;
    let mut acts: Vec<Act> = Vec::new();
    let cat: Option<Arc<ModelCatalog>> = f.state.tabs[idx].model_catalog.clone();
    toolbar(ui, f, idx, &mut acts);
    let avail = ui.available_rect_before_wrap();
    let tab = &mut f.state.tabs[idx];
    let results_visible = tab.run.is_some();
    let frac = tab.results_fraction.clamp(0.1, 0.92);
    let sep_h = 6.0;
    let top_h = if results_visible { (avail.height() * (1.0 - frac) - sep_h / 2.0).max(120.0) } else { avail.height() };
    let top_rect = egui::Rect::from_min_size(avail.min, Vec2::new(avail.width(), top_h));
    // ---- builder: fields | wells + DAX ----
    {
        let mut top = ui.new_child(egui::UiBuilder::new().max_rect(top_rect).layout(egui::Layout::left_to_right(egui::Align::Min)));
        top.set_clip_rect(top_rect);
        let fields_rect = egui::Rect::from_min_size(top_rect.min, Vec2::new(FIELDS_W, top_h));
        let wells_rect = egui::Rect::from_min_max(egui::pos2(top_rect.left() + FIELDS_W + 1.0, top_rect.top()), top_rect.max);
        let mut fui = top.new_child(egui::UiBuilder::new().max_rect(fields_rect).layout(egui::Layout::top_down(egui::Align::Min)));
        fui.set_clip_rect(fields_rect);
        fields_pane(&mut fui, theme, tab, cat.as_deref(), &mut acts);
        top.painter().vline(fields_rect.right() + 0.5, fields_rect.y_range(), egui::Stroke::new(1.0, theme.border));
        let mut wui = top.new_child(egui::UiBuilder::new().max_rect(wells_rect).layout(egui::Layout::top_down(egui::Align::Min)));
        wui.set_clip_rect(wells_rect);
        wells_pane(&mut wui, theme, tab, cat.as_deref(), &mut acts);
        // a chip dropped back on the field list leaves its well; the floating label follows the drag
        if let Some(p) = egui::DragAndDrop::payload::<DragField>(ui.ctx()) {
            let pointer = ui.ctx().input(|i| i.pointer.latest_pos());
            if ui.ctx().input(|i| i.pointer.any_released()) {
                if p.from.is_some() && pointer.map(|pos| fields_rect.contains(pos)).unwrap_or(false) {
                    acts.push(Act::Remove(p.field.clone()));
                }
                egui::DragAndDrop::clear_payload(ui.ctx());
            } else {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                if let Some(pos) = pointer {
                    egui::Area::new(egui::Id::new(("explore-drag-label", tab.id))).order(egui::Order::Tooltip).fixed_pos(pos + Vec2::new(12.0, 8.0)).interactable(false).show(ui.ctx(), |ui| {
                        egui::Frame::new().fill(theme.bg_panel).stroke(egui::Stroke::new(1.0, theme.accent)).corner_radius(10.0).inner_margin(egui::Margin::symmetric(8, 3)).show(ui, |ui| {
                            ui.add(egui::Label::new(RichText::new(format!("{} {}", if p.field.is_measure() { icons::SIGMA } else { icons::COLUMNS }, p.field.label())).size(12.0)).extend());
                        });
                    });
                }
            }
        }
    }
    // ---- results ----
    let mut res_actions: Vec<ResultsAction> = Vec::new();
    if results_visible {
        let sep_rect = egui::Rect::from_min_size(egui::pos2(avail.left(), top_rect.bottom()), Vec2::new(avail.width(), sep_h));
        let sep = ui.interact(sep_rect, egui::Id::new(("explore-split", tab.id)), Sense::drag());
        ui.painter().rect_filled(sep_rect, 0.0, if sep.hovered() || sep.dragged() { theme.accent } else { theme.border });
        if sep.hovered() || sep.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
        }
        if sep.dragged() {
            let dy = sep.drag_delta().y;
            let new_h = (top_h + dy).clamp(120.0, avail.height() - 80.0);
            tab.results_fraction = 1.0 - (new_h + sep_h / 2.0) / avail.height();
        }
        let res_rect = egui::Rect::from_min_max(egui::pos2(avail.left(), sep_rect.bottom()), avail.max);
        let mut res_ui = ui.new_child(egui::UiBuilder::new().max_rect(res_rect).layout(egui::Layout::top_down(egui::Align::Min)));
        res_ui.set_clip_rect(res_rect);
        tab.results_tab = if tab.results_tab == ResultsTab::Plan { ResultsTab::Results } else { tab.results_tab };
        res_actions = results::show(&mut res_ui, results::ResultsArgs { tab, theme, settings: f.cx.settings, fmt: &f.state.formatter });
        if tab.run.as_ref().map(|r| r.result_sets.iter().any(|s| s.grid.focused)).unwrap_or(false) {
            f.state.focus = Focus::Results;
        }
    }
    // ---- the filter editor window ----
    filter_window(ui, f, idx, cat.as_deref(), &mut acts);
    // ---- apply ----
    for a in res_actions {
        crate::ops::results_action(f.state, f.cx, idx, a);
    }
    for a in acts {
        match a {
            Act::Add(field, well) => ops::add_field(f.state, f.cx, idx, field, well),
            Act::Drop(field, from, to, before) => ops::drop_field(f.state, f.cx, idx, field, from, to, before),
            Act::Remove(field) => ops::remove_field(f.state, f.cx, idx, &field),
            Act::Move(field, well, d) => ops::move_field(f.state, f.cx, idx, &field, well, d),
            Act::Agg(i, agg) => ops::set_agg(f.state, f.cx, idx, i, agg),
            Act::Sort(s) => ops::set_sort(f.state, f.cx, idx, s),
            Act::TopN(n) => ops::set_top_n(f.state, f.cx, idx, n),
            Act::Subtotals(on) => ops::set_subtotals(f.state, f.cx, idx, on),
            Act::EditFilter(i) => ops::edit_filter(f.state, idx, i),
            Act::RemoveFilter(i) => ops::remove_filter(f.state, f.cx, idx, i),
            Act::ApplyFilter => ops::apply_filter(f.state, f.cx, idx),
            Act::CancelFilter => {
                if let Some(ex) = f.state.tabs[idx].explore.as_deref_mut() {
                    ex.filter_editor = None;
                }
            }
            Act::LoadDistinct(t, c) => ops::load_distinct(f.state, f.cx, idx, &t, &c),
            Act::Run => ops::run(f.state, f.cx, idx),
            Act::Cancel => crate::ops::cancel(f.state, f.cx, idx),
            Act::OpenDax => ops::open_in_query_tab(f.state, f.cx, idx),
            Act::Save(save_as) => {
                ops::save(f.state, f.cx, idx, save_as);
            }
            Act::Pin => crate::dashboard::open_pin_dialog(f.state, f.cx, idx, 0),
            Act::Refresh => {
                if let Some(target) = f.state.tabs[idx].tabular.as_ref().map(|tt| tt.target.clone()) {
                    crate::tabularq::load_catalog(f.state, f.cx, &target, true);
                } else {
                    crate::localq::refresh_all_catalogs(f.state, f.cx);
                }
            }
            Act::ChooseModel => {
                if f.state.tabs[idx].tabular.is_some() {
                    crate::tabularq::open_chooser(f.state, f.cx, Some(idx), true);
                } else if f.state.tabs[idx].explore.as_ref().and_then(|e| e.spec.source.as_ref()).and_then(|s| s.sql_table()).is_some() {
                    f.cx.toast(ToastKind::Info, "This Explore is over a table on a connection; right-click another table in the Servers tree › Explore.");
                } else {
                    f.cx.toast(ToastKind::Info, "This Explore is over a local table; pick another one in the Local sidebar (right-click › Explore).");
                }
            }
        }
    }
}

fn toolbar(ui: &mut Ui, f: &mut Frame<'_>, idx: usize, acts: &mut Vec<Act>) {
    let theme = f.theme;
    let tab = &mut f.state.tabs[idx];
    let running = tab.is_running();
    let local_table = tab.explore.as_ref().and_then(|e| e.spec.source.as_ref()).and_then(|s| s.local_table()).map(str::to_string);
    let sql_table = tab.explore.as_ref().and_then(|e| e.spec.source.as_ref()).and_then(|s| s.sql_table()).map(|(_, db, s, t)| (db.to_string(), format!("{s}.{t}")));
    let is_local = local_table.is_some() || sql_table.is_some();
    let model = tab.tabular.as_ref().map(|tt| tt.target.model().to_string()).or_else(|| local_table.clone()).or_else(|| sql_table.as_ref().map(|(_, t)| t.clone())).unwrap_or_default();
    let host = tab.tabular.as_ref().map(|tt| tt.target.host_label()).or_else(|| sql_table.as_ref().map(|(db, _)| format!("{} · {db}", tab.profile.as_ref().and_then(|p| p.name.clone()).unwrap_or_else(|| tab.profile.as_ref().map(|p| p.server.clone()).unwrap_or_default())))).unwrap_or_else(|| "a local table".into());
    let is_sql = sql_table.is_some();
    let sql_state = if is_sql { Some(match &tab.conn { crate::state::ConnState::Connected { .. } => "reading the columns", crate::state::ConnState::Connecting => "connecting", _ => "not connected" }) } else { None };
    let local_cols = if is_local { tab.model_catalog.as_ref().and_then(|c| c.tables.first()).map(|t| t.columns.len()) } else { None };
    let cat_state = tab.tabular.as_ref().and_then(|tt| f.state.model_catalogs.get(&tt.model_key()));
    let Some(ex) = tab.explore.as_deref_mut() else { return };
    egui::Frame::new().fill(theme.bg_sidebar).inner_margin(egui::Margin::symmetric(8, 4)).show(ui, |ui| {
        ui.horizontal(|ui| {
            let r = ui.add(egui::TextEdit::singleline(&mut ex.spec.title).hint_text("Explore title").desired_width(200.0));
            r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, true, "explore title"));
            ui.label(RichText::new(if is_local { icons::TABLE } else { icons::CUBE }).color(theme.accent));
            let r = ui.add(egui::Button::new(RichText::new(&model).strong()).frame(false)).on_hover_text(if is_sql { format!("Table {model} on {host}\nThe builder writes T-SQL and runs it on this connection") } else if is_local { format!("Local table {model}") } else { format!("Semantic model {model} in {host}\nClick to explore another model") });
            r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "explore model"));
            if r.clicked() {
                acts.push(Act::ChooseModel);
            }
            match cat_state {
                Some(Loadable::Loaded(c)) => {
                    ui.label(RichText::new(format!("{} tables · {} measures", c.tables.len(), c.measure_count())).size(12.0).color(theme.text_muted));
                }
                Some(Loadable::Loading) => {
                    ui.spinner();
                    ui.label(RichText::new("loading the model…").size(12.0).color(theme.text_muted));
                }
                Some(Loadable::Failed(e)) => {
                    ui.label(RichText::new(format!("{} model not loaded", icons::WARNING)).size(12.0).color(theme.warning)).on_hover_text(e);
                    if ui.small_button("Retry").clicked() {
                        acts.push(Act::Refresh);
                    }
                }
                None if local_cols.is_some() => {
                    ui.label(RichText::new(format!("{} · {} columns", if is_sql { "table" } else { "local table" }, local_cols.unwrap_or(0))).size(12.0).color(theme.text_muted));
                }
                None if is_sql => {
                    ui.spinner();
                    ui.label(RichText::new(format!("{}…", sql_state.unwrap_or_default())).size(12.0).color(theme.text_muted)).on_hover_text("Connecting and reading the table's columns");
                }
                _ => {}
            }
            ui.separator();
            if running {
                if tool_button(ui, icons::STOP, "Stop", "Stop the query", true).clicked() {
                    acts.push(Act::Cancel);
                }
            } else {
                let dirty = ex.dirty && !ex.spec.is_empty();
                let r = tool_button(ui, icons::PLAY, if dirty { "Run" } else { "Refresh" }, "Run the query for the current wells (F5)", !ex.spec.is_empty());
                r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "explore run"));
                if r.clicked() {
                    acts.push(Act::Run);
                }
            }
            let r = ui.checkbox(&mut ex.auto_run, "Auto").on_hover_text("Run again whenever a well changes");
            r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Checkbox, true, "explore auto run"));
            ui.separator();
            ui.label(RichText::new("Top").size(12.0).color(theme.text_muted));
            let mut n = ex.spec.top_n.unwrap_or(0);
            let r = ui.add(egui::DragValue::new(&mut n).range(0..=1_000_000).speed(1.0).custom_formatter(|v, _| if v == 0.0 { "all".to_string() } else { format!("{v:.0}") }));
            r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::DragValue, true, "explore top n"));
            if r.changed() {
                acts.push(Act::TopN(if n == 0 { None } else { Some(n) }));
            }
            let mut sub = ex.spec.subtotals;
            let r = ui.checkbox(&mut sub, "Subtotals").on_hover_text("Subtotal rows per grouping field (ROLLUPADDISSUBTOTAL)");
            r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Checkbox, true, "explore subtotals"));
            if r.changed() {
                acts.push(Act::Subtotals(sub));
            }
            let has_cols = !ex.spec.columns.is_empty();
            if has_cols {
                let r = ui.selectable_label(ex.matrix, "Matrix").on_hover_text("Column fields across the top (a local pivot of the result)");
                r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "explore matrix"));
                if r.clicked() {
                    ex.matrix = true;
                }
                let r = ui.selectable_label(!ex.matrix, "Flat");
                r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "explore flat"));
                if r.clicked() {
                    ex.matrix = false;
                }
            }
            ui.separator();
            let r = ui.selectable_label(ex.show_dax, format!("{} {}", icons::CODE, if is_local { "SQL" } else { "DAX" })).on_hover_text("Show the generated query");
            r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "explore show dax"));
            if r.clicked() {
                ex.show_dax = !ex.show_dax;
            }
            if tool_button(ui, icons::ARROW_SQUARE_OUT, "Open in query tab", if is_sql { "The generated T-SQL in a query tab on this connection, to edit and run" } else if is_local { "The generated SQL in a Local query tab, to edit and run" } else { "The generated DAX in a DAX query tab, to edit and run" }, ex.dax.is_some()).clicked() {
                acts.push(Act::OpenDax);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if icon_button(ui, icons::PUSH_PIN, "Pin to dashboard", !ex.spec.is_empty()).clicked() {
                    acts.push(Act::Pin);
                }
                if icon_button(ui, icons::FLOPPY_DISK, "Save (.cobalt-explore.json)", !ex.spec.is_empty()).clicked() {
                    acts.push(Act::Save(false));
                }
            });
        });
    });
}

fn fields_pane(ui: &mut Ui, theme: &Theme, tab: &mut crate::state::EditorTab, cat: Option<&ModelCatalog>, acts: &mut Vec<Act>) {
    let Some(ex) = tab.explore.as_deref_mut() else { return };
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.add_space(6.0);
        let r = ui.add(egui::TextEdit::singleline(&mut ex.field_filter).hint_text(format!("{} Find a field", icons::MAGNIFYING_GLASS)).desired_width(FIELDS_W - 16.0));
        r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, true, "explore field filter"));
    });
    let Some(cat) = cat else {
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(RichText::new("The model's fields appear here once it is loaded.").size(12.0).color(theme.text_muted));
        });
        return;
    };
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        ui.label(RichText::new("drag a field onto a well, or click it").size(10.5).italics().color(theme.text_faint));
    });
    let filter = ex.field_filter.trim().to_lowercase();
    egui::ScrollArea::vertical().id_salt(("explore-fields", tab.id)).auto_shrink([false, false]).show(ui, |ui| {
        for t in cat.tables.iter().filter(|t| !t.hidden) {
            let cols: Vec<&cobalt_core::tabular::Column> = t.columns.iter().filter(|c| !c.hidden && (filter.is_empty() || c.name.to_lowercase().contains(&filter))).collect();
            let meas: Vec<&cobalt_core::tabular::Measure> = t.measures.iter().filter(|m| !m.hidden && (filter.is_empty() || m.name.to_lowercase().contains(&filter))).collect();
            if !filter.is_empty() && cols.is_empty() && meas.is_empty() && !t.name.to_lowercase().contains(&filter) {
                continue;
            }
            let open = !filter.is_empty() || ex.field_expanded.contains(&t.name);
            let r = crate::ui::fabric::tree_row(ui, theme, 0, open, icons::TABLE, &t.name, &format!("{} · {}", cols.len(), meas.len()), &format!("explore table {}", t.name));
            if r.clicked() && filter.is_empty() && !ex.field_expanded.remove(&t.name) {
                ex.field_expanded.insert(t.name.clone());
            }
            if !open {
                continue;
            }
            for m in &meas {
                let field = FieldRef::Measure { name: m.name.clone() };
                let used = ex.spec.contains(&field);
                let r = crate::ui::fabric::tree_leaf(ui, theme, 1, icons::SIGMA, &m.name, if used { "in Values" } else { "measure" }, &format!("explore field {}", field.qualified()));
                r.dnd_set_drag_payload(DragField { field: field.clone(), from: None });
                if r.clicked() || r.double_clicked() {
                    if used { acts.push(Act::Remove(field.clone())) } else { acts.push(Act::Add(field.clone(), Well::Values)) }
                }
                r.context_menu(|ui| {
                    if ui.button("Add to Values").clicked() {
                        acts.push(Act::Add(field.clone(), Well::Values));
                        ui.close();
                    }
                    if used && ui.button("Remove").clicked() {
                        acts.push(Act::Remove(field.clone()));
                        ui.close();
                    }
                });
            }
            for c in &cols {
                let field = FieldRef::Column { table: t.name.clone(), column: c.name.clone() };
                let used = ex.spec.contains(&field);
                let icon = match c.data_type {
                    TabularDataType::String => icons::TEXT_AA,
                    TabularDataType::DateTime => icons::CALENDAR_BLANK,
                    TabularDataType::Boolean => icons::TOGGLE_LEFT,
                    TabularDataType::Int64 | TabularDataType::Double | TabularDataType::Decimal => icons::HASH,
                    _ => icons::COLUMNS,
                };
                let sub = if used { "in use" } else { c.data_type.label() };
                let r = crate::ui::fabric::tree_leaf(ui, theme, 1, icon, &c.name, sub, &format!("explore field {}", field.qualified()));
                r.dnd_set_drag_payload(DragField { field: field.clone(), from: None });
                if r.clicked() || r.double_clicked() {
                    if used {
                        acts.push(Act::Remove(field.clone()));
                    } else if c.data_type.is_numeric() && c.summarize_by.as_deref().map(|s| !s.eq_ignore_ascii_case("none")).unwrap_or(true) {
                        acts.push(Act::Add(field.clone(), Well::Values));
                    } else {
                        acts.push(Act::Add(field.clone(), Well::Rows));
                    }
                }
                r.context_menu(|ui| {
                    if ui.button("Add to Rows").clicked() {
                        acts.push(Act::Add(field.clone(), Well::Rows));
                        ui.close();
                    }
                    if ui.button("Add to Columns").clicked() {
                        acts.push(Act::Add(field.clone(), Well::Columns));
                        ui.close();
                    }
                    if ui.button("Add to Values").clicked() {
                        acts.push(Act::Add(field.clone(), Well::Values));
                        ui.close();
                    }
                    if ui.button("Filter on…").clicked() {
                        acts.push(Act::Add(field.clone(), Well::Filters));
                        ui.close();
                    }
                    if used && ui.button("Remove").clicked() {
                        acts.push(Act::Remove(field.clone()));
                        ui.close();
                    }
                });
            }
        }
    });
}

fn wells_pane(ui: &mut Ui, theme: &Theme, tab: &mut crate::state::EditorTab, cat: Option<&ModelCatalog>, acts: &mut Vec<Act>) {
    let Some(ex) = tab.explore.as_deref_mut() else { return };
    let dragging: Option<Arc<DragField>> = egui::DragAndDrop::payload::<DragField>(ui.ctx());
    egui::ScrollArea::vertical().id_salt(("explore-wells", tab.id)).auto_shrink([false, false]).show(ui, |ui| {
        ui.add_space(4.0);
        if let Some(a) = well(ui, theme, "Rows", icons::ROWS, "explore well rows", Well::Rows, dragging.as_deref(), |ui| {
            let n = ex.spec.rows.len();
            for (i, f) in ex.spec.rows.clone().iter().enumerate() {
                field_chip(ui, theme, f, Well::Rows, i, n, &ex.spec, acts);
            }
            if ex.spec.rows.is_empty() {
                hint(ui, theme, if dragging.is_some() { "drop here" } else { "drag a column here, or click it in the list" });
            }
        }) {
            acts.push(a);
        }
        if let Some(a) = well(ui, theme, "Columns", icons::COLUMNS, "explore well columns", Well::Columns, dragging.as_deref(), |ui| {
            let n = ex.spec.columns.len();
            for (i, f) in ex.spec.columns.clone().iter().enumerate() {
                field_chip(ui, theme, f, Well::Columns, i, n, &ex.spec, acts);
            }
            if ex.spec.columns.is_empty() {
                hint(ui, theme, if dragging.is_some() { "drop here" } else { "optional: a field across the top makes a matrix" });
            }
        }) {
            acts.push(a);
        }
        if let Some(a) = well(ui, theme, "Values", icons::SIGMA, "explore well values", Well::Values, dragging.as_deref(), |ui| {
            for (i, v) in ex.spec.values.clone().iter().enumerate() {
                value_chip(ui, theme, i, v, &ex.spec, acts);
            }
            if ex.spec.values.is_empty() {
                hint(ui, theme, if dragging.is_some() { "drop here" } else { "a measure, or a column with Sum / Count / Average…" });
            }
        }) {
            acts.push(a);
        }
        if let Some(a) = well(ui, theme, "Filters", icons::FUNNEL, "explore well filters", Well::Filters, dragging.as_deref(), |ui| {
            for (i, fl) in ex.spec.filters.clone().iter().enumerate() {
                egui::Frame::new().fill(theme.bg_hover).corner_radius(10.0).inner_margin(egui::Margin::symmetric(8, 3)).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let r = ui.add(egui::Label::new(RichText::new(fl.summary()).size(12.0)).selectable(false).sense(Sense::click()));
                        r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("explore filter {}", fl.column)));
                        if r.clicked() {
                            acts.push(Act::EditFilter(i));
                        }
                        if ui.add(egui::Button::new(RichText::new(icons::X).size(11.0)).frame(false)).on_hover_text("Remove").clicked() {
                            acts.push(Act::RemoveFilter(i));
                        }
                    });
                });
            }
            if ex.spec.filters.is_empty() {
                hint(ui, theme, if dragging.is_some() { "drop a column here to filter on it" } else { "drag a column here, or right-click it › Filter on…" });
            }
        }) {
            acts.push(a);
        }
        if let Some(e) = &ex.dax_error {
            ui.horizontal_wrapped(|ui| {
                ui.add_space(8.0);
                ui.label(RichText::new(format!("{} {e}", icons::WARNING)).size(12.0).color(theme.warning));
            });
        }
        if ex.show_dax {
            let is_sql_text = ex.spec.source.as_ref().map(|s| s.local_table().is_some() || s.sql_table().is_some()).unwrap_or(false);
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.label(RichText::new(if is_sql_text { "SQL" } else { "DAX" }).size(11.0).strong().color(theme.text_muted));
                if let Some(q) = &ex.dax {
                    if ui.small_button("Copy").clicked() {
                        ui.ctx().copy_text(q.text.clone());
                    }
                }
            });
            let text = ex.dax.as_ref().map(|q| q.text.clone()).unwrap_or_else(|| "— add fields to the wells —".into());
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                let mut t = text;
                ui.add(egui::TextEdit::multiline(&mut t).code_editor().desired_width(f32::INFINITY).desired_rows(4).interactive(false));
            });
        }
        let _ = cat;
    });
}

/// A well: its chips in a frame that is a drop zone while a field is being dragged (outlined
/// when it takes the field, lit when the pointer is over it, dimmed when it does not).
#[allow(clippy::too_many_arguments)]
fn well(ui: &mut Ui, theme: &Theme, title: &str, icon: &str, a11y: &str, which: Well, dragging: Option<&DragField>, body: impl FnOnce(&mut Ui)) -> Option<Act> {
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        ui.label(RichText::new(format!("{icon} {title}")).size(11.0).strong().color(theme.text_muted));
    });
    let takes = dragging.map(|d| ops::accepts(which, &d.field)).unwrap_or(false);
    let mut frame = egui::Frame::new().inner_margin(egui::Margin { left: 6, right: 6, top: 2, bottom: 2 }).outer_margin(egui::Margin { left: 4, right: 6, top: 0, bottom: 0 }).corner_radius(6.0).begin(ui);
    {
        let ui = &mut frame.content_ui;
        ui.set_min_width(ui.available_width());
        // a steady height: an empty well is a target the size of a chip, and nothing moves
        // when a drag begins
        ui.set_min_height(24.0);
        ui.horizontal_wrapped(|ui| {
            body(ui);
        });
    }
    let r = frame.allocate_space(ui);
    r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, a11y.to_string()));
    if dragging.is_some() {
        let over = takes && r.contains_pointer();
        frame.frame.stroke = if !takes { egui::Stroke::new(1.0, theme.border) } else if over { egui::Stroke::new(1.5, theme.accent) } else { egui::Stroke::new(1.0, theme.accent.gamma_multiply(0.55)) };
        frame.frame.fill = if over { theme.accent.gamma_multiply(0.12) } else { egui::Color32::TRANSPARENT };
    }
    frame.paint(ui);
    let dropped = if takes { r.dnd_release_payload::<DragField>().map(|p| Act::Drop(p.field.clone(), p.from, which, None)) } else { None };
    ui.add_space(4.0);
    dropped
}

fn hint(ui: &mut Ui, theme: &Theme, text: &str) {
    ui.label(RichText::new(text).size(11.0).italics().color(theme.text_faint));
}

#[allow(clippy::too_many_arguments)]
fn field_chip(ui: &mut Ui, theme: &Theme, f: &FieldRef, well: Well, i: usize, n: usize, spec: &cobalt_core::explore::ExploreSpec, acts: &mut Vec<Act>) {
    egui::Frame::new().fill(theme.bg_hover).corner_radius(10.0).inner_margin(egui::Margin::symmetric(8, 3)).show(ui, |ui| {
        ui.horizontal(|ui| {
            let by = SortBy::Field { field: f.clone() };
            let sorted = ops::sort_label(spec, &by);
            let label = if sorted.is_empty() { f.label() } else { format!("{} {sorted}", f.label()) };
            let r = ui.add(egui::Label::new(RichText::new(label).size(12.0)).selectable(false).sense(Sense::click_and_drag()));
            r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("explore chip {}", f.qualified())));
            r.dnd_set_drag_payload(DragField { field: f.clone(), from: Some(well) });
            if let Some(p) = r.dnd_release_payload::<DragField>() {
                if p.field != *f {
                    acts.push(Act::Drop(p.field.clone(), p.from, well, Some(f.clone())));
                }
            }
            r.on_hover_text(format!("{}\nright-click: sort, move · drag to reorder or to another well · ✕ removes", f.qualified())).context_menu(|ui| chip_menu(ui, f, well, i, n, spec, acts));
            if ui.add(egui::Button::new(RichText::new(icons::X).size(11.0)).frame(false)).on_hover_text("Remove").clicked() {
                acts.push(Act::Remove(f.clone()));
            }
        });
    });
}

fn chip_menu(ui: &mut Ui, f: &FieldRef, well: Well, i: usize, n: usize, spec: &cobalt_core::explore::ExploreSpec, acts: &mut Vec<Act>) {
    let by = SortBy::Field { field: f.clone() };
    if ui.button("Sort ascending").clicked() {
        acts.push(Act::Sort(Some(SortSpec { by: by.clone(), desc: false })));
        ui.close();
    }
    if ui.button("Sort descending").clicked() {
        acts.push(Act::Sort(Some(SortSpec { by, desc: true })));
        ui.close();
    }
    if spec.sort.is_some() && ui.button("No sort").clicked() {
        acts.push(Act::Sort(None));
        ui.close();
    }
    ui.separator();
    if i > 0 && ui.button(format!("{} Move left", icons::ARROW_LEFT)).clicked() {
        acts.push(Act::Move(f.clone(), well, -1));
        ui.close();
    }
    if i + 1 < n && ui.button(format!("{} Move right", icons::ARROW_RIGHT)).clicked() {
        acts.push(Act::Move(f.clone(), well, 1));
        ui.close();
    }
    let other = if well == Well::Rows { Well::Columns } else { Well::Rows };
    if ui.button(format!("Move to {}", if other == Well::Rows { "Rows" } else { "Columns" })).clicked() {
        acts.push(Act::Add(f.clone(), other));
        ui.close();
    }
    if let FieldRef::Column { .. } = f {
        if ui.button("Filter on…").clicked() {
            acts.push(Act::Add(f.clone(), Well::Filters));
            ui.close();
        }
    }
    ui.separator();
    if ui.button("Remove").clicked() {
        acts.push(Act::Remove(f.clone()));
        ui.close();
    }
}

fn value_chip(ui: &mut Ui, theme: &Theme, i: usize, v: &cobalt_core::explore::ValueRef, spec: &cobalt_core::explore::ExploreSpec, acts: &mut Vec<Act>) {
    egui::Frame::new().fill(theme.bg_hover).corner_radius(10.0).inner_margin(egui::Margin::symmetric(8, 3)).show(ui, |ui| {
        ui.horizontal(|ui| {
            let by = SortBy::Value { index: i };
            let sorted = ops::sort_label(spec, &by);
            let label = if sorted.is_empty() { v.name() } else { format!("{} {sorted}", v.name()) };
            let r = ui.add(egui::Label::new(RichText::new(label).size(12.0)).selectable(false).sense(Sense::click_and_drag()));
            r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("explore value {}", v.name())));
            r.dnd_set_drag_payload(DragField { field: v.field.clone(), from: Some(Well::Values) });
            if let Some(p) = r.dnd_release_payload::<DragField>() {
                if p.field != v.field {
                    acts.push(Act::Drop(p.field.clone(), p.from, Well::Values, Some(v.field.clone())));
                }
            }
            let r = r.on_hover_text(format!("{}\nright-click: aggregation, sort · drag to reorder · ✕ removes", v.field.qualified()));
            r.context_menu(|ui| {
                if let FieldRef::Column { .. } = v.field {
                    for (agg, label) in AGGS {
                        if ui.radio(v.agg == agg, label).clicked() {
                            acts.push(Act::Agg(i, agg));
                            ui.close();
                        }
                    }
                    ui.separator();
                }
                if ui.button("Sort descending").clicked() {
                    acts.push(Act::Sort(Some(SortSpec { by: by.clone(), desc: true })));
                    ui.close();
                }
                if ui.button("Sort ascending").clicked() {
                    acts.push(Act::Sort(Some(SortSpec { by: by.clone(), desc: false })));
                    ui.close();
                }
                if spec.sort.is_some() && ui.button("No sort").clicked() {
                    acts.push(Act::Sort(None));
                    ui.close();
                }
                ui.separator();
                if ui.button("Remove").clicked() {
                    acts.push(Act::Remove(v.field.clone()));
                    ui.close();
                }
            });
            if ui.add(egui::Button::new(RichText::new(icons::X).size(11.0)).frame(false)).on_hover_text("Remove").clicked() {
                acts.push(Act::Remove(v.field.clone()));
            }
        });
    });
}

/// The filter editor: operator, then values (a picker of the column's distinct values for
/// "is one of", text boxes otherwise).
fn filter_window(ui: &mut Ui, f: &mut Frame<'_>, idx: usize, cat: Option<&ModelCatalog>, acts: &mut Vec<Act>) {
    let theme = f.theme;
    let tab_id = f.state.tabs[idx].id;
    let Some(ex) = f.state.tabs[idx].explore.as_deref_mut() else { return };
    let Some(ed) = ex.filter_editor.as_mut() else { return };
    let dtype = ops::column_type(cat, &ed.table, &ed.column);
    let key = (ed.table.clone(), ed.column.clone());
    let distinct = ex.distinct.get(&key);
    if matches!(ed.op, FilterOp::In { .. } | FilterOp::NotIn { .. }) && distinct.is_none() {
        acts.push(Act::LoadDistinct(ed.table.clone(), ed.column.clone()));
    }
    let mut open = true;
    let mut apply = false;
    let mut cancel = false;
    crate::ui::chrome::Window::new(format!("Filter: {}", ed.column)).id(egui::Id::new(("explore-filter", tab_id))).open(&mut open).default_size([360.0, 420.0]).resizable(true).show(ui.ctx(), theme, |ui: &mut egui::Ui| {
        ui.label(RichText::new(format!("{}[{}] · {}", ed.table, ed.column, dtype.label())).size(12.0).color(theme.text_muted));
        ui.add_space(4.0);
        // operator
        let ops_list: Vec<(&str, FilterOp)> = {
            let mut v = vec![("is one of", FilterOp::In { values: Vec::new() }), ("is not one of", FilterOp::NotIn { values: Vec::new() })];
            if dtype.is_numeric() || dtype == TabularDataType::DateTime || dtype == TabularDataType::Unknown {
                v.push(("between", FilterOp::Between { lo: None, hi: None }));
            }
            if dtype == TabularDataType::String || dtype == TabularDataType::Unknown {
                v.push(("contains", FilterOp::Contains { text: String::new() }));
                v.push(("starts with", FilterOp::StartsWith { text: String::new() }));
            }
            if dtype == TabularDataType::DateTime {
                v.push(("in the last…", FilterOp::RelativeDate { unit: DateUnit::Days, last: 30 }));
            }
            v.push(("is blank", FilterOp::IsBlank));
            v.push(("is not blank", FilterOp::NotBlank));
            v
        };
        let current = ops_list.iter().find(|(_, o)| std::mem::discriminant(o) == std::mem::discriminant(&ed.op)).map(|(l, _)| *l).unwrap_or("?");
        egui::ComboBox::from_id_salt("explore-filter-op").selected_text(current).show_ui(ui, |ui| {
            for (label, op) in &ops_list {
                if ui.selectable_label(*label == current, *label).clicked() && std::mem::discriminant(op) != std::mem::discriminant(&ed.op) {
                    ed.op = op.clone();
                    if matches!(ed.op, FilterOp::In { .. } | FilterOp::NotIn { .. }) {
                        acts.push(Act::LoadDistinct(ed.table.clone(), ed.column.clone()));
                    }
                }
            }
        });
        ui.add_space(6.0);
        match &mut ed.op {
            FilterOp::In { values } | FilterOp::NotIn { values } => {
                ui.add(egui::TextEdit::singleline(&mut ed.search).hint_text("Find a value").desired_width(f32::INFINITY));
                let picked = values.len();
                ui.label(RichText::new(if picked == 0 { "no value picked yet".to_string() } else { format!("{picked} picked") }).size(11.0).color(theme.text_muted));
                match distinct {
                    Some(Loadable::Loaded(list)) => {
                        let search = ed.search.to_lowercase();
                        egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                            for v in list.iter().filter(|v| search.is_empty() || v.to_lowercase().contains(&search)) {
                                let mut on = values.contains(v);
                                let r = ui.checkbox(&mut on, if v.is_empty() { "(blank)" } else { v.as_str() });
                                r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Checkbox, true, format!("explore filter value {v}")));
                                if r.changed() {
                                    if on {
                                        values.push(v.clone());
                                    } else {
                                        values.retain(|x| x != v);
                                    }
                                }
                            }
                            if list.len() >= 500 {
                                ui.label(RichText::new("first 500 values").size(11.0).color(theme.text_faint));
                            }
                        });
                    }
                    Some(Loadable::Failed(e)) => {
                        ui.colored_label(theme.warning, e);
                    }
                    _ => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(RichText::new("loading values…").size(12.0).color(theme.text_muted));
                        });
                    }
                }
                ui.horizontal(|ui| {
                    ui.label(RichText::new("or type one:").size(11.0).color(theme.text_muted));
                    let r = ui.add(egui::TextEdit::singleline(&mut ed.text_a).desired_width(160.0));
                    if (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))) || ui.small_button("Add").clicked() {
                        let v = ed.text_a.trim().to_string();
                        if !v.is_empty() && !values.contains(&v) {
                            values.push(v);
                        }
                        ed.text_a.clear();
                    }
                });
            }
            FilterOp::Between { lo, hi } => {
                ui.horizontal(|ui| {
                    ui.label("from");
                    if ui.add(egui::TextEdit::singleline(&mut ed.text_a).desired_width(120.0).hint_text(if dtype == TabularDataType::DateTime { "YYYY-MM-DD" } else { "number" })).changed() {
                        *lo = Some(ed.text_a.trim().to_string()).filter(|s| !s.is_empty());
                    }
                    ui.label("to");
                    if ui.add(egui::TextEdit::singleline(&mut ed.text_b).desired_width(120.0).hint_text(if dtype == TabularDataType::DateTime { "YYYY-MM-DD" } else { "number" })).changed() {
                        *hi = Some(ed.text_b.trim().to_string()).filter(|s| !s.is_empty());
                    }
                });
                ui.label(RichText::new("either end may stay empty").size(11.0).color(theme.text_faint));
            }
            FilterOp::Contains { text } | FilterOp::StartsWith { text } => {
                if ui.add(egui::TextEdit::singleline(&mut ed.text_a).desired_width(f32::INFINITY)).changed() {
                    *text = ed.text_a.clone();
                }
            }
            FilterOp::RelativeDate { unit, last } => {
                ui.horizontal(|ui| {
                    ui.label("last");
                    if ui.add(egui::DragValue::new(&mut ed.last_n).range(1..=3650)).changed() {
                        *last = ed.last_n;
                    }
                    for (u, l) in [(DateUnit::Days, "days"), (DateUnit::Months, "months"), (DateUnit::Years, "years")] {
                        if ui.radio(*unit == u, l).clicked() {
                            *unit = u;
                        }
                    }
                });
            }
            FilterOp::IsBlank | FilterOp::NotBlank => {}
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let r = ui.add(egui::Button::new(RichText::new("Apply").color(egui::Color32::WHITE)).fill(theme.accent));
            r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "explore filter apply"));
            if r.clicked() {
                apply = true;
            }
            if ui.button("Cancel").clicked() {
                cancel = true;
            }
        });
    });
    if apply {
        acts.push(Act::ApplyFilter);
    } else if cancel || !open {
        acts.push(Act::CancelFilter);
    }
    let _ = ToastKind::Info;
}
