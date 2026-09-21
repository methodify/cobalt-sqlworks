//! Cell viewer: a side panel that shows one value in full — pretty JSON, indented XML,
//! wrapped text, or a hex dump — with copy. Its **Record** mode shows every column of the row
//! as name/value pairs (wide Fabric tables), with previous/next row navigation.

use crate::ui::theme::Theme;
use cobalt_results::{CellValue, ResultSet};
use egui::{RichText, Ui};
use std::sync::Arc;

pub struct ViewerState {
    pub rs: Arc<ResultSet>,
    pub row: usize,
    pub col: usize,
    pub mode: ViewMode,
    pub wrap: bool,
    pub text: String,
    pub pretty: Option<String>,
    /// Record mode: the whole row, one column per line.
    pub record: bool,
}

/// What the viewer wants the host to do after a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewerOutcome {
    Open,
    Close,
    /// Move to another cell (row navigation in record mode, or a value picked from the record).
    Goto { row: usize, col: usize, record: bool },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ViewMode {
    Auto,
    Text,
    Json,
    Xml,
    Hex,
}

impl ViewerState {
    pub fn new(rs: Arc<ResultSet>, row: usize, col: usize) -> Self {
        let value = rs.cell_value(row, col);
        let (text, pretty, mode) = match &value {
            CellValue::Null => ("NULL".to_string(), None, ViewMode::Text),
            CellValue::Text(t) => {
                let trimmed = t.trim_start();
                if trimmed.starts_with('{') || trimmed.starts_with('[') {
                    let pretty = serde_json::from_str::<serde_json::Value>(t).ok().and_then(|v| serde_json::to_string_pretty(&v).ok());
                    let mode = if pretty.is_some() { ViewMode::Json } else { ViewMode::Text };
                    (t.clone(), pretty, mode)
                } else if trimmed.starts_with('<') {
                    (t.clone(), Some(pretty_xml(t)), ViewMode::Xml)
                } else {
                    (t.clone(), None, ViewMode::Text)
                }
            }
            CellValue::Bytes(b) => (hex_dump(b), None, ViewMode::Hex),
            other => (other.to_json().to_string().trim_matches('"').to_string(), None, ViewMode::Text),
        };
        Self { rs, row, col, mode, wrap: true, text, pretty, record: false }
    }

    /// The row as a JSON object (column name → value).
    pub fn row_json(&self) -> String {
        let mut obj = serde_json::Map::new();
        for (c, col) in self.rs.columns.iter().enumerate() {
            obj.insert(col.name.clone(), self.rs.cell_value(self.row, c).to_json());
        }
        serde_json::to_string_pretty(&serde_json::Value::Object(obj)).unwrap_or_default()
    }

    pub fn shown(&self) -> &str {
        match (self.mode, &self.pretty) {
            (ViewMode::Json | ViewMode::Xml | ViewMode::Auto, Some(p)) => p,
            _ => &self.text,
        }
    }
}

pub fn show(ui: &mut Ui, theme: &Theme, v: &mut ViewerState) -> ViewerOutcome {
    let mut out = ViewerOutcome::Open;
    let col_name = v.rs.columns.get(v.col).map(|c| c.name.clone()).unwrap_or_default();
    let type_label = v.rs.columns.get(v.col).map(|c| c.type_label()).unwrap_or_default();
    let nrows = v.rs.visible_count();
    egui::Frame::new().fill(theme.bg_panel).inner_margin(8.0).show(ui, |ui| {
        ui.set_min_size(ui.available_size());
        ui.horizontal(|ui| {
            if v.record {
                ui.label(RichText::new(format!("Row {}", v.row + 1)).strong());
                ui.label(RichText::new(format!("of {}", crate::state::fmt_count(nrows as u64))).small().color(theme.text_muted));
                let prev = ui.add_enabled(v.row > 0, egui::Button::new(egui_phosphor::regular::CARET_LEFT).small());
                prev.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "record previous row"));
                if prev.on_hover_text("Previous row").clicked() {
                    out = ViewerOutcome::Goto { row: v.row - 1, col: v.col, record: true };
                }
                let next = ui.add_enabled(v.row + 1 < nrows, egui::Button::new(egui_phosphor::regular::CARET_RIGHT).small());
                next.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "record next row"));
                if next.on_hover_text("Next row").clicked() {
                    out = ViewerOutcome::Goto { row: v.row + 1, col: v.col, record: true };
                }
            } else {
                ui.label(RichText::new(format!("{col_name}")).strong());
                ui.label(RichText::new(format!("row {}  ·  {type_label}", v.row + 1)).small().color(theme.text_muted));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button(egui_phosphor::regular::X).on_hover_text("Close (Esc)").clicked() {
                    out = ViewerOutcome::Close;
                }
                if ui.small_button(egui_phosphor::regular::COPY).on_hover_text(if v.record { "Copy row as JSON" } else { "Copy value" }).clicked() {
                    ui.ctx().copy_text(if v.record { v.row_json() } else { v.shown().to_string() });
                }
                let rec = ui.selectable_label(v.record, "Record");
                rec.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "viewer record mode"));
                if rec.on_hover_text("Every column of this row, one per line").clicked() {
                    v.record = !v.record;
                }
                if !v.record {
                    ui.checkbox(&mut v.wrap, "Wrap");
                    for (m, label) in [(ViewMode::Text, "Text"), (ViewMode::Json, "JSON"), (ViewMode::Xml, "XML"), (ViewMode::Hex, "Hex")] {
                        let enabled = match m {
                            ViewMode::Json | ViewMode::Xml => v.pretty.is_some(),
                            ViewMode::Hex => true,
                            _ => true,
                        };
                        if ui.add_enabled_ui(enabled, |ui| ui.selectable_label(v.mode == m, label)).inner.clicked() {
                            if m == ViewMode::Hex && !matches!(v.rs.cell_value(v.row, v.col), CellValue::Bytes(_)) {
                                v.text = hex_dump(v.text.as_bytes());
                            }
                            v.mode = m;
                        }
                    }
                }
            });
        });
        ui.separator();
        if v.record {
            record_body(ui, theme, v, &mut out);
            return;
        }
        let shown_len = v.shown().len();
        ui.label(RichText::new(format!("{} characters", crate::state::fmt_count(v.shown().chars().count() as u64))).small().color(theme.text_faint));
        egui::ScrollArea::both().id_salt(("viewer", v.row, v.col)).auto_shrink([false, false]).show(ui, |ui| {
            let mut text: &str = v.shown();
            let mut truncated = String::new();
            if shown_len > 2_000_000 {
                truncated = format!("{}\n\n… (truncated for display; use Copy for the full value)", &text[..2_000_000]);
                text = &truncated;
            }
            let mut buf = text.to_string();
            let te = egui::TextEdit::multiline(&mut buf).font(egui::FontId::monospace(13.0)).code_editor().desired_width(if v.wrap { ui.available_width() } else { f32::INFINITY }).frame(egui::Frame::NONE).interactive(true);
            ui.add(te);
        });
    });
    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        out = ViewerOutcome::Close;
    }
    out
}

/// Record mode body: one line per column (name + type, value). Clicking a value opens it.
fn record_body(ui: &mut Ui, theme: &Theme, v: &ViewerState, out: &mut ViewerOutcome) {
    const MAX_CHARS: usize = 300;
    egui::ScrollArea::vertical().id_salt(("record", v.row)).auto_shrink([false, false]).show(ui, |ui| {
        egui::Grid::new(("record-grid", v.row)).num_columns(2).striped(true).spacing([14.0, 4.0]).min_col_width(140.0).show(ui, |ui| {
            for (c, col) in v.rs.columns.iter().enumerate() {
                ui.vertical(|ui| {
                    ui.label(RichText::new(&col.name).strong());
                    ui.label(RichText::new(col.type_label()).small().color(theme.text_faint));
                });
                let value = v.rs.cell_value(v.row, c);
                let (text, is_null) = match &value {
                    CellValue::Null => ("NULL".to_string(), true),
                    CellValue::Text(t) => (t.clone(), false),
                    CellValue::Bytes(b) => (format!("0x{}{} ({} bytes)", b.iter().take(16).map(|x| format!("{x:02x}")).collect::<String>(), if b.len() > 16 { "…" } else { "" }, b.len()), false),
                    other => (other.to_json().to_string().trim_matches('"').to_string(), false),
                };
                let one_line: String = text.replace(['\r', '\n'], " ");
                let shown = if one_line.chars().count() > MAX_CHARS { format!("{}…", one_line.chars().take(MAX_CHARS).collect::<String>()) } else { one_line };
                let label = egui::Label::new(RichText::new(shown).monospace().color(if is_null { theme.null_text } else { theme.text })).sense(egui::Sense::click()).truncate();
                let r = ui.add(label);
                r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("record value {}", col.name)));
                if r.on_hover_text("Open this value").clicked() {
                    *out = ViewerOutcome::Goto { row: v.row, col: c, record: false };
                }
                ui.end_row();
            }
        });
    });
}

pub fn pretty_xml(src: &str) -> String {
    let mut out = String::with_capacity(src.len() + src.len() / 4);
    let mut depth: usize = 0;
    let mut chars = src.trim().char_indices().peekable();
    let bytes = src.trim();
    let mut i = 0;
    let s = bytes;
    let _ = &mut chars;
    while i < s.len() {
        if s[i..].starts_with('<') {
            let end = s[i..].find('>').map(|e| i + e + 1).unwrap_or(s.len());
            let tag = &s[i..end];
            let is_close = tag.starts_with("</");
            let is_self = tag.ends_with("/>") || tag.starts_with("<?") || tag.starts_with("<!");
            if is_close {
                depth = depth.saturating_sub(1);
            }
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(&"  ".repeat(depth));
            out.push_str(tag);
            if !is_close && !is_self {
                depth += 1;
            }
            i = end;
            // inline text content up to the next tag
            let next = s[i..].find('<').map(|n| i + n).unwrap_or(s.len());
            let text = s[i..next].trim();
            if !text.is_empty() {
                out.push_str(text);
                // if followed by a closing tag, keep it on the same line
                if s[next..].starts_with("</") {
                    let cend = s[next..].find('>').map(|e| next + e + 1).unwrap_or(s.len());
                    out.push_str(&s[next..cend]);
                    depth = depth.saturating_sub(1);
                    i = cend;
                    continue;
                }
            }
            i = next;
        } else {
            i += 1;
        }
    }
    out
}

pub fn hex_dump(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 4);
    for (i, chunk) in bytes.chunks(16).enumerate() {
        out.push_str(&format!("{:08X}  ", i * 16));
        for (j, b) in chunk.iter().enumerate() {
            out.push_str(&format!("{b:02X} "));
            if j == 7 {
                out.push(' ');
            }
        }
        for _ in chunk.len()..16 {
            out.push_str("   ");
        }
        if chunk.len() <= 8 {
            out.push(' ');
        }
        out.push_str(" |");
        for b in chunk {
            out.push(if (0x20..0x7f).contains(b) { *b as char } else { '.' });
        }
        out.push_str("|\n");
        if i > 65536 {
            out.push_str("… (truncated)\n");
            break;
        }
    }
    out
}
