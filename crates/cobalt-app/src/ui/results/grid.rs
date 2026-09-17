//! The results grid: `egui_table` over a `ResultSet`, with a row-number gutter, selection,
//! sorting, header filters, and keyboard navigation.

use crate::state::{FilterPopup, GridState, Selection};
use crate::ui::theme::Theme;
use cobalt_results::{CellFormatter, ColumnFilter, FilterOp, ResultSet, SortKey, ViewSpec};
use egui::{Align2, Color32, FontId, Key, Modifiers, Pos2, Rect, Sense, Stroke, Ui, Vec2};
use egui_table::{CellInfo, Column, HeaderCellInfo, HeaderRow, Table, TableDelegate};
use std::sync::Arc;

pub const ROW_H: f32 = 24.0;
pub const HEADER_H: f32 = 26.0;
pub const GUTTER_W: f32 = 56.0;

/// Things the grid asks the app to do.
#[derive(Debug, Clone)]
pub enum GridAction {
    ApplyView(ViewSpec),
    OpenViewer(usize, usize),
    ContextMenu(Pos2),
    Copy,
    CopyWithHeaders,
}

pub struct GridArgs<'a> {
    pub rs: &'a Arc<ResultSet>,
    pub grid: &'a mut GridState,
    pub theme: &'a Theme,
    pub fmt: &'a CellFormatter,
    pub font_size: f32,
    pub id_salt: egui::Id,
    pub show_row_numbers: bool,
    pub max_col_width: f32,
    pub sample_rows: usize,
}

struct Delegate<'a> {
    rs: &'a Arc<ResultSet>,
    grid: &'a mut GridState,
    theme: &'a Theme,
    fmt: &'a CellFormatter,
    font: FontId,
    header_font: FontId,
    rows: usize,
    cols: usize,
    gutter: bool,
    actions: Vec<GridAction>,
    click: Option<(usize, usize, bool, bool)>,
    drag_to: Option<(usize, usize)>,
    header_click: Option<(usize, bool)>,
    corner_click: bool,
    gutter_click: Option<(usize, bool)>,
    funnel_click: Option<(usize, Pos2)>,
    double_click: Option<(usize, usize)>,
    secondary: Option<Pos2>,
}

impl Delegate<'_> {
    fn data_col(&self, table_col: usize) -> Option<usize> {
        if self.gutter {
            if table_col == 0 { None } else { Some(table_col - 1) }
        } else {
            Some(table_col)
        }
    }
}

impl TableDelegate for Delegate<'_> {
    fn header_cell_ui(&mut self, ui: &mut Ui, cell: &HeaderCellInfo) {
        let rect = ui.max_rect();
        // egui_table paints the header row once per scroll region (sticky and scrolling columns),
        // so every cell is visited twice and one visit is fully clipped away. Interacting in the
        // clipped visit registers a second identical widget, which stacks a duplicate tooltip.
        if !ui.clip_rect().intersects(rect) {
            return;
        }
        let painter = ui.painter();
        painter.rect_filled(rect, 0.0, self.theme.bg_grid_header);
        painter.line_segment([rect.left_bottom(), rect.right_bottom()], Stroke::new(1.0, self.theme.border));
        painter.line_segment([rect.right_top(), rect.right_bottom()], Stroke::new(1.0, self.theme.border));
        let Some(col) = self.data_col(cell.col_range.start) else {
            // gutter header: select-all corner (like ADS/Excel)
            let resp = ui.interact(rect, ui.id().with("corner"), Sense::click());
            resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "select all cells"));
            if resp.hovered() {
                ui.painter().rect_filled(rect.shrink(2.0), 2.0, self.theme.bg_selection_inactive);
            }
            if resp.clicked() {
                self.grid.selection = Selection::All;
                self.grid.anchor = Some((0, 0));
                self.grid.focused = true;
                self.corner_click = true;
            }
            resp.on_hover_text("Select all");
            return;
        };
        let Some(info) = self.rs.columns.get(col) else { return };
        let selected_col = matches!(&self.grid.selection, Selection::Cols { c0, c1 } if col >= *c0 && col <= *c1) || self.grid.selection == Selection::All;
        if selected_col {
            painter.rect_filled(rect.shrink2(Vec2::new(0.0, 1.0)), 0.0, self.theme.bg_selection);
        }
        let sort = self.grid.view.sort.iter().position(|k| k.column == col).map(|i| (i, self.grid.view.sort[i].descending));
        let filtered = self.grid.view.filters.iter().any(|f| f.column == col);
        let name = if info.name.is_empty() { "(No column name)".to_string() } else { info.name.clone() };
        let label_rect = Rect::from_min_max(rect.min + Vec2::new(6.0, 0.0), rect.max - Vec2::new(22.0, 0.0));
        let galley = painter.layout_no_wrap(name, self.header_font.clone(), self.theme.text);
        let clip = painter.with_clip_rect(label_rect);
        clip.galley(Pos2::new(label_rect.left(), rect.center().y - galley.size().y / 2.0), galley, self.theme.text);
        if let Some((i, desc)) = sort {
            let icon = if desc { egui_phosphor::regular::SORT_DESCENDING } else { egui_phosphor::regular::SORT_ASCENDING };
            let x = rect.right() - 36.0;
            painter.text(Pos2::new(x, rect.center().y), Align2::CENTER_CENTER, icon, FontId::proportional(12.0), self.theme.accent);
            if self.grid.view.sort.len() > 1 {
                painter.text(Pos2::new(x + 8.0, rect.center().y - 6.0), Align2::CENTER_CENTER, (i + 1).to_string(), FontId::proportional(8.0), self.theme.accent);
            }
        }
        // funnel
        let funnel_rect = Rect::from_center_size(Pos2::new(rect.right() - 11.0, rect.center().y), Vec2::new(18.0, 18.0));
        let funnel_resp = ui.interact(funnel_rect, ui.id().with(("funnel", col)), Sense::click());
        let funnel_color = if filtered { self.theme.accent } else if funnel_resp.hovered() { self.theme.text } else { self.theme.text_faint };
        painter.text(funnel_rect.center(), Align2::CENTER_CENTER, egui_phosphor::regular::FUNNEL, FontId::proportional(13.0), funnel_color);
        funnel_resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("filter {}", info.name)));
        if funnel_resp.clicked() {
            self.funnel_click = Some((col, funnel_rect.left_bottom()));
        }
        let head_resp = ui.interact(Rect::from_min_max(rect.min, Pos2::new(funnel_rect.left(), rect.max.y)), ui.id().with(("head", col)), Sense::click());
        head_resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("column {}", info.name)));
        if head_resp.clicked() {
            let shift = ui.input(|i| i.modifiers.shift);
            self.header_click = Some((col, shift));
        }
        if head_resp.secondary_clicked() {
            self.grid.selection = Selection::Cols { c0: col, c1: col };
            self.secondary = ui.input(|i| i.pointer.interact_pos());
        }
        head_resp.on_hover_ui(|ui| {
            ui.label(egui::RichText::new(&info.name).strong());
            ui.label(info.type_label());
            ui.label(egui::RichText::new("Click: sort · Shift+click: add sort · funnel: filter").small().weak());
        });
    }

    fn cell_ui(&mut self, ui: &mut Ui, cell: &CellInfo) {
        let rect = ui.max_rect();
        let row = cell.row_nr as usize;
        let painter = ui.painter();
        let Some(col) = self.data_col(cell.col_nr) else {
            // row number gutter
            let selected_row = self.grid.selection.contains(row, 0) && matches!(self.grid.selection, Selection::Rows { .. } | Selection::All);
            painter.rect_filled(rect, 0.0, if selected_row { self.theme.bg_selection } else { self.theme.bg_grid_header });
            painter.line_segment([rect.right_top(), rect.right_bottom()], Stroke::new(1.0, self.theme.border));
            painter.text(Pos2::new(rect.right() - 6.0, rect.center().y), Align2::RIGHT_CENTER, (row + 1).to_string(), FontId::proportional((self.font.size - 1.0).max(9.0)), self.theme.text_muted);
            let resp = ui.interact(rect, ui.id().with(("gutter", row)), Sense::click());
            if resp.clicked() {
                self.gutter_click = Some((row, ui.input(|i| i.modifiers.shift)));
            }
            if resp.secondary_clicked() {
                self.grid.selection = Selection::Rows { r0: row, r1: row };
                self.secondary = ui.input(|i| i.pointer.interact_pos());
            }
            return;
        };
        if col >= self.cols {
            return;
        }
        let selected = self.grid.selection.contains(row, col);
        let is_anchor = self.grid.anchor == Some((row, col));
        let bg = if selected {
            if self.grid.focused { self.theme.bg_selection } else { self.theme.bg_selection_inactive }
        } else if row % 2 == 1 {
            self.theme.bg_grid_alt
        } else {
            self.theme.bg_grid
        };
        painter.rect_filled(rect, 0.0, bg);
        painter.line_segment([rect.right_top(), rect.right_bottom()], Stroke::new(1.0, self.theme.border));
        painter.line_segment([rect.left_bottom(), rect.right_bottom()], Stroke::new(1.0, self.theme.border));
        if is_anchor && self.grid.focused {
            painter.rect_stroke(rect.shrink(1.0), 0.0, Stroke::new(1.5, self.theme.accent), egui::StrokeKind::Inside);
        }
        let is_null = self.rs.is_null(row, col);
        let text = self.rs.cell_text(row, col, self.fmt);
        let color = if is_null { self.theme.null_text } else { self.theme.text };
        let right = self.rs.columns[col].sql_type.right_align();
        let text_rect = rect.shrink2(Vec2::new(6.0, 0.0));
        let clip = painter.with_clip_rect(text_rect);
        let mut font = self.font.clone();
        if is_null {
            font = FontId::new(font.size, egui::FontFamily::Proportional);
        }
        let single_line: String = if text.contains('\n') || text.contains('\r') { text.replace(['\r', '\n'], " ") } else { text.to_string() };
        let galley = clip.layout_no_wrap(single_line, font, color);
        let y = rect.center().y - galley.size().y / 2.0;
        let x = if right { text_rect.right() - galley.size().x } else { text_rect.left() };
        clip.galley(Pos2::new(x.max(text_rect.left()), y), galley, color);

        if !ui.clip_rect().intersects(rect) {
            return; // clipped duplicate visit (see header_cell_ui)
        }
        let resp = ui.interact(rect, ui.id().with(("cell", row, col)), Sense::click_and_drag());
        if resp.clicked() || resp.drag_started() {
            let (shift, ctrl) = ui.input(|i| (i.modifiers.shift, i.modifiers.command));
            self.click = Some((row, col, shift, ctrl));
        }
        if resp.dragged() {
            if let Some(pos) = ui.input(|i| i.pointer.interact_pos()) {
                if rect.contains(pos) {
                    self.drag_to = Some((row, col));
                }
            }
        }
        if resp.hovered() && ui.input(|i| i.pointer.primary_down()) && self.grid.anchor.is_some() {
            self.drag_to = Some((row, col));
        }
        if resp.double_clicked() {
            self.double_click = Some((row, col));
        }
        if resp.secondary_clicked() {
            if !selected {
                self.grid.selection = Selection::Cells { r0: row, c0: col, r1: row, c1: col };
                self.grid.anchor = Some((row, col));
            }
            self.secondary = ui.input(|i| i.pointer.interact_pos());
        }
        if resp.hovered() && text.len() > 60 {
            resp.on_hover_text(if text.len() > 2000 { format!("{}…", &text[..2000]) } else { text.to_string() });
        }
    }

    fn default_row_height(&self) -> f32 {
        ROW_H
    }
}

fn measure(ui: &mut Ui, font: &FontId, s: &str) -> f32 {
    ui.fonts_mut(|f| f.layout_no_wrap(s.to_owned(), font.clone(), Color32::WHITE).size().x)
}

/// Initialize column widths from header + a sample of rows.
fn init_widths(ui: &mut Ui, args: &mut GridArgs<'_>, font: &FontId, header_font: &FontId) {
    let rs = args.rs;
    let n = rs.column_count();
    let sample = args.sample_rows.min(rs.visible_count());
    let mut widths = Vec::with_capacity(n);
    for c in 0..n {
        let name = if rs.columns[c].name.is_empty() { "(No column name)" } else { rs.columns[c].name.as_str() };
        let mut w = measure(ui, header_font, name) + 40.0;
        for r in 0..sample {
            let t = rs.cell_text(r, c, args.fmt);
            let first = t.split('\n').next().unwrap_or("");
            let shown = if first.chars().count() > 120 { first.chars().take(120).collect::<String>() } else { first.to_string() };
            w = w.max(measure(ui, font, &shown) + 14.0);
        }
        widths.push(w.clamp(48.0, args.max_col_width));
    }
    args.grid.col_widths = widths;
    args.grid.widths_initialized = true;
}

/// Draw the grid; returns actions for the caller.
pub fn show(ui: &mut Ui, mut args: GridArgs<'_>) -> Vec<GridAction> {
    let font = FontId::monospace(args.font_size);
    let header_font = FontId::proportional(args.font_size);
    let rs = args.rs.clone();
    let rows = rs.visible_count();
    let cols = rs.column_count();
    let mut seed_state = false;
    if !args.grid.widths_initialized && (rows > 0 || !rs.state().is_live()) {
        init_widths(ui, &mut args, &font, &header_font);
        seed_state = true;
    } else if args.grid.col_widths.len() != cols {
        args.grid.col_widths.resize(cols, 120.0);
        seed_state = true;
    }
    let gutter = args.show_row_numbers;

    // Keyboard focus: the grid owns a focusable id so a click on a cell takes focus away from the
    // editor's text box (otherwise Ctrl+A / Ctrl+C keep acting on the SQL text).
    let focus_id = args.id_salt.with("kb-focus");
    let kb_focus = ui.memory(|m| m.has_focus(focus_id));
    if args.grid.focused && !kb_focus && ui.memory(|m| m.focused().is_some()) {
        args.grid.focused = false; // something else (the editor, a dialog field) took the keyboard
    }

    // keyboard navigation when focused
    let mut actions = Vec::new();
    if kb_focus && ui.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy))) {
        actions.push(GridAction::Copy);
    }
    if args.grid.focused && rows > 0 && cols > 0 {
        let (anchor_r, anchor_c) = args.grid.anchor.unwrap_or((0, 0));
        let mut mv: Option<(isize, isize)> = None;
        let mut shift = false;
        let mut select_all = false;
        let page = 20isize;
        ui.input_mut(|i| {
            shift = i.modifiers.shift;
            if i.consume_key(Modifiers::NONE, Key::ArrowDown) || i.consume_key(Modifiers::SHIFT, Key::ArrowDown) {
                mv = Some((1, 0));
            } else if i.consume_key(Modifiers::NONE, Key::ArrowUp) || i.consume_key(Modifiers::SHIFT, Key::ArrowUp) {
                mv = Some((-1, 0));
            } else if i.consume_key(Modifiers::NONE, Key::ArrowLeft) || i.consume_key(Modifiers::SHIFT, Key::ArrowLeft) {
                mv = Some((0, -1));
            } else if i.consume_key(Modifiers::NONE, Key::ArrowRight) || i.consume_key(Modifiers::SHIFT, Key::ArrowRight) {
                mv = Some((0, 1));
            } else if i.consume_key(Modifiers::NONE, Key::PageDown) || i.consume_key(Modifiers::SHIFT, Key::PageDown) {
                mv = Some((page, 0));
            } else if i.consume_key(Modifiers::NONE, Key::PageUp) || i.consume_key(Modifiers::SHIFT, Key::PageUp) {
                mv = Some((-page, 0));
            } else if i.consume_key(Modifiers::COMMAND, Key::Home) {
                mv = Some((-(rows as isize), -(cols as isize)));
            } else if i.consume_key(Modifiers::COMMAND, Key::End) {
                mv = Some((rows as isize, cols as isize));
            } else if i.consume_key(Modifiers::NONE, Key::Home) {
                mv = Some((0, -(cols as isize)));
            } else if i.consume_key(Modifiers::NONE, Key::End) {
                mv = Some((0, cols as isize));
            } else if i.consume_key(Modifiers::COMMAND, Key::A) {
                // Ctrl/Cmd+A: select every cell (copy/export then take the whole set)
                select_all = true;
            }
            if i.consume_key(Modifiers::NONE, Key::Enter) {
                mv = None;
                // open viewer
            }
        });
        if ui.input(|i| i.key_pressed(Key::Enter)) {
            if let Some((r, c)) = args.grid.anchor {
                actions.push(GridAction::OpenViewer(r, c));
            }
        }
        if select_all {
            args.grid.selection = Selection::All;
            if args.grid.anchor.is_none() {
                args.grid.anchor = Some((0, 0));
            }
        }
        if let Some((dr, dc)) = mv {
            let nr = (anchor_r as isize + dr).clamp(0, rows as isize - 1) as usize;
            let nc = (anchor_c as isize + dc).clamp(0, cols as isize - 1) as usize;
            if shift {
                extend_selection(args.grid, nr, nc);
            } else {
                args.grid.selection = Selection::Cells { r0: nr, c0: nc, r1: nr, c1: nc };
                args.grid.anchor = Some((nr, nc));
            }
            args.grid.scroll_to = Some((nr, nc));
        }
    }

    let mut columns: Vec<Column> = Vec::with_capacity(cols + 1);
    if gutter {
        columns.push(Column::new(GUTTER_W).range(GUTTER_W..=GUTTER_W).resizable(false));
    }
    for (i, w) in args.grid.col_widths.iter().enumerate() {
        columns.push(Column::new(*w).range(32.0..=4000.0).resizable(true).id(args.id_salt.with(("col", i))));
    }
    let sticky = if gutter { 1 + args.grid.frozen_cols } else { args.grid.frozen_cols };

    let mut delegate = Delegate {
        rs: &rs,
        grid: args.grid,
        theme: args.theme,
        fmt: args.fmt,
        font,
        header_font,
        rows,
        cols,
        gutter,
        actions: Vec::new(),
        click: None,
        drag_to: None,
        header_click: None,
        corner_click: false,
        gutter_click: None,
        funnel_click: None,
        double_click: None,
        secondary: None,
    };
    let mut table = Table::new()
        .id_salt(args.id_salt)
        .num_rows(rows as u64)
        .columns(columns)
        .num_sticky_cols(sticky)
        .headers([HeaderRow::new(HEADER_H)])
        .auto_size_mode(egui_table::AutoSizeMode::Never);
    if let Some((r, c)) = delegate.grid.scroll_to.take() {
        table = table.scroll_to_row(r as u64, None).scroll_to_column(if gutter { c + 1 } else { c }, None);
    }
    let table_id = table.get_id(ui);
    if seed_state || egui_table::TableState::load(ui.ctx(), table_id).is_none() {
        // egui_table auto-sizes brand-new tables from cell content (which we paint, not lay out),
        // so seed its state with our measured widths.
        let mut st = egui_table::TableState::default();
        for (i, w) in delegate.grid.col_widths.iter().enumerate() {
            st.col_widths.insert(args.id_salt.with(("col", i)), *w);
        }
        st.store(ui.ctx(), table_id);
    }
    let response = table.show(ui, &mut delegate);

    // persist resized widths back into our state
    if let Some(state) = egui_table::TableState::load(ui.ctx(), table_id) {
        for i in 0..cols {
            let cid = args.id_salt.with(("col", i));
            if let Some(w) = state.col_widths.get(&cid) {
                delegate.grid.col_widths[i] = *w;
            }
        }
    }

    // keep the focus id alive every frame (egui drops focus for ids it does not see)
    ui.interact(response.rect, focus_id, Sense::focusable_noninteractive());

    // resolve interactions
    if response.clicked() || response.drag_started() || delegate.click.is_some() || delegate.gutter_click.is_some() || delegate.corner_click {
        delegate.grid.focused = true;
        ui.memory_mut(|m| m.request_focus(focus_id));
    }
    if let Some((row, col, shift, _ctrl)) = delegate.click {
        if shift && delegate.grid.anchor.is_some() {
            extend_selection(delegate.grid, row, col);
        } else {
            delegate.grid.selection = Selection::Cells { r0: row, c0: col, r1: row, c1: col };
            delegate.grid.anchor = Some((row, col));
        }
    }
    if let Some((row, col)) = delegate.drag_to {
        if delegate.grid.anchor.is_some() {
            extend_selection(delegate.grid, row, col);
        }
    }
    if let Some((row, shift)) = delegate.gutter_click {
        match (&delegate.grid.selection, shift) {
            (Selection::Rows { r0, .. }, true) => {
                let a = *r0;
                delegate.grid.selection = Selection::Rows { r0: a.min(row), r1: a.max(row) };
            }
            _ => {
                delegate.grid.selection = Selection::Rows { r0: row, r1: row };
                delegate.grid.anchor = Some((row, 0));
            }
        }
    }
    if let Some((col, shift)) = delegate.header_click {
        let mut spec = delegate.grid.view.clone();
        if shift {
            if let Some(k) = spec.sort.iter_mut().find(|k| k.column == col) {
                k.descending = !k.descending;
            } else {
                spec.sort.push(SortKey { column: col, descending: false });
            }
        } else {
            let existing = spec.sort.iter().find(|k| k.column == col).map(|k| k.descending);
            spec.sort = match existing {
                None => vec![SortKey { column: col, descending: false }],
                Some(false) => vec![SortKey { column: col, descending: true }],
                Some(true) => vec![],
            };
        }
        delegate.actions.push(GridAction::ApplyView(spec));
    }
    if let Some((col, pos)) = delegate.funnel_click {
        let (values, truncated) = rs.distinct_values(col, 2000, args.fmt);
        let checked = match delegate.grid.view.filters.iter().find(|f| f.column == col) {
            Some(ColumnFilter { op: FilterOp::In(set), .. }) => values.iter().filter(|(v, _)| set.contains(&**v)).map(|(v, _)| v.clone()).collect(),
            _ => values.iter().map(|(v, _)| v.clone()).collect(),
        };
        delegate.grid.filter_popup = Some(FilterPopup { column: col, search: String::new(), values, truncated, checked, condition_op: 0, condition_value: String::new(), condition_value2: String::new() });
        delegate.grid.focused = true;
        let _ = pos;
    }
    if let Some((r, c)) = delegate.double_click {
        delegate.actions.push(GridAction::OpenViewer(r, c));
    }
    if let Some(pos) = delegate.secondary {
        delegate.actions.push(GridAction::ContextMenu(pos));
    }
    actions.extend(delegate.actions.drain(..));

    // filter popup
    if let Some(action) = filter_popup(ui, delegate.grid, args.theme, &rs) {
        actions.push(action);
    }
    actions
}

fn extend_selection(grid: &mut GridState, row: usize, col: usize) {
    let (ar, ac) = grid.anchor.unwrap_or((row, col));
    grid.selection = Selection::Cells { r0: ar.min(row), c0: ac.min(col), r1: ar.max(row), c1: ac.max(col) };
}

const CONDITION_OPS: &[&str] = &["(none)", "contains", "not contains", "equals", "not equals", "starts with", "ends with", ">", ">=", "<", "<=", "between", "is NULL", "is not NULL"];

fn filter_popup(ui: &mut Ui, grid: &mut GridState, theme: &Theme, rs: &Arc<ResultSet>) -> Option<GridAction> {
    let mut result = None;
    let mut close = false;
    let Some(popup) = grid.filter_popup.as_mut() else { return None };
    let col = popup.column;
    let name = rs.columns.get(col).map(|c| c.name.clone()).unwrap_or_default();
    let id = egui::Id::new(("filter-popup", rs.index, col));
    egui::Window::new(format!("Filter: {name}"))
        .id(id)
        .collapsible(false)
        .resizable(true)
        .default_width(300.0)
        .default_height(420.0)
        .show(ui.ctx(), |ui| {
            ui.horizontal(|ui| {
                ui.label("Condition");
                egui::ComboBox::from_id_salt(id.with("op")).selected_text(CONDITION_OPS[popup.condition_op]).show_ui(ui, |ui| {
                    for (i, op) in CONDITION_OPS.iter().enumerate() {
                        ui.selectable_value(&mut popup.condition_op, i, *op);
                    }
                });
            });
            if (1..=11).contains(&popup.condition_op) {
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut popup.condition_value).desired_width(120.0));
                    if popup.condition_op == 11 {
                        ui.label("and");
                        ui.add(egui::TextEdit::singleline(&mut popup.condition_value2).desired_width(120.0));
                    }
                });
            }
            ui.separator();
            ui.horizontal(|ui| {
                ui.label(egui_phosphor::regular::MAGNIFYING_GLASS);
                ui.add(egui::TextEdit::singleline(&mut popup.search).hint_text("Search values").desired_width(f32::INFINITY));
            });
            let search = popup.search.to_lowercase();
            let visible: Vec<usize> = popup.values.iter().enumerate().filter(|(_, (v, _))| search.is_empty() || v.to_lowercase().contains(&search)).map(|(i, _)| i).collect();
            ui.horizontal(|ui| {
                if ui.small_button("Select all").clicked() {
                    for &i in &visible {
                        popup.checked.insert(popup.values[i].0.clone());
                    }
                }
                if ui.small_button("Clear").clicked() {
                    for &i in &visible {
                        popup.checked.remove(&popup.values[i].0);
                    }
                }
                if popup.truncated {
                    ui.label(egui::RichText::new("showing first 2,000 values").small().color(theme.warning));
                }
            });
            egui::ScrollArea::vertical().max_height(240.0).auto_shrink([false, false]).show_rows(ui, 20.0, visible.len(), |ui, range| {
                for vi in range {
                    let i = visible[vi];
                    let (v, count) = &popup.values[i];
                    let mut on = popup.checked.contains(v);
                    let label = if v.is_empty() { "(Blank)".to_string() } else { v.to_string() };
                    let shown = if label.chars().count() > 80 { format!("{}…", label.chars().take(80).collect::<String>()) } else { label };
                    if ui.checkbox(&mut on, format!("{shown}   ({count})")).changed() {
                        if on {
                            popup.checked.insert(v.clone());
                        } else {
                            popup.checked.remove(v);
                        }
                    }
                }
            });
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("OK").clicked() {
                    let mut spec = grid_view_without(&grid_view_spec_clone(&popup_view(rs, grid.view.clone())), col);
                    let all_checked = popup.checked.len() == popup.values.len();
                    if !all_checked {
                        spec.filters.push(ColumnFilter { column: col, op: FilterOp::In(popup.checked.iter().map(|s| s.to_string()).collect()) });
                    }
                    let v1 = popup.condition_value.trim().to_string();
                    let v2 = popup.condition_value2.trim().to_string();
                    let cond = match popup.condition_op {
                        1 => Some(FilterOp::Contains(v1)),
                        2 => Some(FilterOp::NotContains(v1)),
                        3 => Some(FilterOp::Equals(v1)),
                        4 => Some(FilterOp::NotEquals(v1)),
                        5 => Some(FilterOp::StartsWith(v1)),
                        6 => Some(FilterOp::EndsWith(v1)),
                        7 => Some(FilterOp::Gt(v1)),
                        8 => Some(FilterOp::Gte(v1)),
                        9 => Some(FilterOp::Lt(v1)),
                        10 => Some(FilterOp::Lte(v1)),
                        11 => Some(FilterOp::Between(v1, v2)),
                        12 => Some(FilterOp::IsNull),
                        13 => Some(FilterOp::NotNull),
                        _ => None,
                    };
                    if let Some(op) = cond {
                        spec.filters.push(ColumnFilter { column: col, op });
                    }
                    result = Some(GridAction::ApplyView(spec));
                    close = true;
                }
                if ui.button("Clear filter").clicked() {
                    let spec = grid_view_without(&grid.view, col);
                    result = Some(GridAction::ApplyView(spec));
                    close = true;
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
        });
    if ui.input(|i| i.key_pressed(Key::Escape)) {
        close = true;
    }
    if close {
        grid.filter_popup = None;
    }
    result
}

fn popup_view(_rs: &Arc<ResultSet>, v: ViewSpec) -> ViewSpec {
    v
}
fn grid_view_spec_clone(v: &ViewSpec) -> ViewSpec {
    v.clone()
}
fn grid_view_without(v: &ViewSpec, col: usize) -> ViewSpec {
    let mut spec = v.clone();
    spec.filters.retain(|f| f.column != col);
    spec
}
