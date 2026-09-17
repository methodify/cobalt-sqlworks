//! Small shared widgets: icon buttons, tree rows, section headers, spinners.

use super::theme::Theme;
use egui::{Color32, Response, RichText, Sense, Ui, Vec2};

/// A flat icon button with a tooltip (toolbar style).
pub fn icon_button(ui: &mut Ui, icon: &str, tooltip: &str, enabled: bool) -> Response {
    let r = ui.add_enabled(enabled, egui::Button::new(RichText::new(icon).size(16.0)).frame(false).min_size(Vec2::new(26.0, 24.0)));
    r.on_hover_text(tooltip)
}

/// Icon + label toolbar button.
pub fn tool_button(ui: &mut Ui, icon: &str, label: &str, tooltip: &str, enabled: bool) -> Response {
    let r = ui.add_enabled(enabled, egui::Button::new(format!("{icon} {label}")).frame(false).min_size(Vec2::new(0.0, 24.0)));
    if tooltip.is_empty() {
        r
    } else {
        r.on_hover_text(tooltip)
    }
}

/// Accent-filled primary button.
pub fn primary_button(ui: &mut Ui, theme: &Theme, label: &str, enabled: bool) -> Response {
    ui.add_enabled(enabled, egui::Button::new(RichText::new(label).color(theme.text_on_accent)).fill(theme.accent).min_size(Vec2::new(80.0, 26.0)))
}

/// A tree row: indent, caret, icon, label. Returns (row response, caret clicked).
pub struct TreeRow<'a> {
    pub depth: usize,
    pub expandable: bool,
    pub expanded: bool,
    pub loading: bool,
    pub icon: &'a str,
    pub icon_color: Option<Color32>,
    pub label: &'a str,
    pub detail: Option<&'a str>,
    pub selected: bool,
    pub color_dot: Option<Color32>,
    pub id_salt: &'a str,
}

pub struct TreeRowResponse {
    pub response: Response,
    pub toggle: bool,
}

pub fn tree_row(ui: &mut Ui, theme: &Theme, row: TreeRow<'_>) -> TreeRowResponse {
    let row_h = 22.0;
    let indent = 14.0 * row.depth as f32 + 4.0;
    let desired = Vec2::new(ui.available_width().max(100.0), row_h);
    let (rect, response) = ui.allocate_exact_size(desired, Sense::click());
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    let hovered = response.hovered();
    let painter = ui.painter();
    if row.selected {
        painter.rect_filled(rect, 3.0, theme.bg_selection);
    } else if hovered {
        painter.rect_filled(rect, 3.0, theme.bg_hover);
    }
    let mut x = rect.left() + indent;
    let mut toggle = false;
    // caret
    let caret_rect = egui::Rect::from_min_size(egui::pos2(x, rect.top()), Vec2::new(16.0, row_h));
    if row.expandable {
        let caret = if row.loading {
            egui_phosphor::regular::CIRCLE_NOTCH
        } else if row.expanded {
            egui_phosphor::regular::CARET_DOWN
        } else {
            egui_phosphor::regular::CARET_RIGHT
        };
        let caret_resp = ui.interact(caret_rect, response.id.with("caret"), Sense::click());
        if caret_resp.clicked() {
            toggle = true;
        }
        painter.text(caret_rect.center(), egui::Align2::CENTER_CENTER, caret, egui::FontId::proportional(12.0), theme.text_muted);
        if row.loading {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(120));
        }
    }
    x += 16.0;
    if let Some(dot) = row.color_dot {
        painter.circle_filled(egui::pos2(x + 4.0, rect.center().y), 4.0, dot);
        x += 12.0;
    }
    painter.text(egui::pos2(x + 8.0, rect.center().y), egui::Align2::CENTER_CENTER, row.icon, egui::FontId::proportional(14.0), row.icon_color.unwrap_or(theme.text_muted));
    x += 20.0;
    let label_pos = egui::pos2(x, rect.center().y);
    let galley = painter.layout_no_wrap(row.label.to_owned(), egui::FontId::proportional(13.0), theme.text);
    let label_w = galley.size().x;
    painter.galley(egui::pos2(label_pos.x, label_pos.y - galley.size().y / 2.0), galley, theme.text);
    if let Some(d) = row.detail {
        painter.text(egui::pos2(x + label_w + 6.0, rect.center().y), egui::Align2::LEFT_CENTER, d, egui::FontId::proportional(11.0), theme.text_faint);
    }
    if response.double_clicked() && row.expandable {
        toggle = true;
    }
    TreeRowResponse { response, toggle }
}

/// A little animated spinner glyph.
pub fn spinner(ui: &mut Ui, theme: &Theme) {
    ui.add(egui::Spinner::new().size(14.0).color(theme.accent));
}

/// Section title in sidebars.
pub fn section_title(ui: &mut Ui, theme: &Theme, text: &str) {
    ui.label(RichText::new(text.to_uppercase()).size(11.0).color(theme.text_muted).strong());
}

/// Muted small text.
pub fn muted(ui: &mut Ui, theme: &Theme, text: impl Into<String>) -> Response {
    ui.label(RichText::new(text.into()).size(12.0).color(theme.text_muted))
}

/// Keyboard shortcut chip.
pub fn key_chip(ui: &mut Ui, theme: &Theme, text: &str) {
    if text.is_empty() {
        return;
    }
    egui::Frame::new().fill(theme.bg_sidebar).stroke(egui::Stroke::new(1.0, theme.border)).corner_radius(3.0).inner_margin(egui::Margin::symmetric(5, 1)).show(ui, |ui| {
        ui.label(RichText::new(text).size(11.0).color(theme.text_muted));
    });
}

pub fn icon_for_object(kind: cobalt_core::ObjectKind) -> &'static str {
    use cobalt_core::ObjectKind::*;
    use egui_phosphor::regular as i;
    match kind {
        Database => i::DATABASE,
        Schema => i::FOLDER_SIMPLE,
        Table => i::TABLE,
        View => i::EYE,
        Procedure => i::GEAR_SIX,
        ScalarFunction | AggregateFunction => i::FUNCTION,
        TableFunction => i::FUNCTION,
        Synonym => i::LINK,
        Sequence => i::LIST_NUMBERS,
        TableType => i::BRACKETS_CURLY,
        Column => i::COLUMNS,
        Parameter => i::AT,
        Index => i::LIST_MAGNIFYING_GLASS,
        Key => i::KEY,
        Constraint => i::CHECK_SQUARE,
        Trigger => i::LIGHTNING,
    }
}
