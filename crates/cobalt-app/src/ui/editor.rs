//! The SQL editor: egui `TextEdit` with a lexer-driven layouter, line-number gutter,
//! current-statement highlight, completion popup, find/replace and go-to-line.

use super::theme::{Theme, TokenColors};
use crate::state::{CompletionEntry, CompletionPopup, EditorTab, PendingEdit};
use cobalt_core::Settings;
use cobalt_sql::lexer::{tokenize, TokenKind};
use egui::text::{CCursor, CCursorRange, LayoutJob, TextFormat};
use egui::{Color32, FontId, Key, Modifiers, Pos2, Rect, Sense, Shape, Stroke, TextEdit, Ui, Vec2};
use std::sync::Arc;

pub const GUTTER_W: f32 = 52.0;

pub struct EditorOutput {
    pub changed: bool,
    /// Byte offset of the cursor.
    pub cursor_byte: usize,
    /// Byte range of the selection, if any (sorted).
    pub selection_bytes: Option<(usize, usize)>,
    pub focused: bool,
}

/// Build a highlighted layout job for `text`.
pub fn layout_job(text: &str, colors: &TokenColors, font: FontId, wrap_width: f32) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = wrap_width;
    let tokens = tokenize(text);
    let mut last = 0usize;
    for t in &tokens {
        if t.start > last {
            job.append(&text[last..t.start], 0.0, TextFormat { font_id: font.clone(), color: colors.identifier, ..Default::default() });
        }
        let color = match t.kind {
            TokenKind::Keyword => colors.keyword,
            TokenKind::Function => colors.function,
            TokenKind::Type => colors.type_,
            TokenKind::Identifier => colors.identifier,
            TokenKind::BracketedIdentifier | TokenKind::QuotedIdentifier => colors.bracketed,
            TokenKind::String => colors.string,
            TokenKind::Number => colors.number,
            TokenKind::LineComment | TokenKind::BlockComment => colors.comment,
            TokenKind::Variable => colors.variable,
            TokenKind::TempTable => colors.temp_table,
            TokenKind::Operator => colors.operator,
            TokenKind::Punct => colors.punct,
            _ => colors.identifier,
        };
        let italics = matches!(t.kind, TokenKind::LineComment | TokenKind::BlockComment);
        job.append(&text[t.start..t.end], 0.0, TextFormat { font_id: font.clone(), color, italics, ..Default::default() });
        last = t.end;
    }
    if last < text.len() {
        job.append(&text[last..], 0.0, TextFormat { font_id: font.clone(), color: colors.identifier, ..Default::default() });
    }
    job
}

pub fn char_to_byte(text: &str, char_idx: usize) -> usize {
    text.char_indices().nth(char_idx).map(|(b, _)| b).unwrap_or(text.len())
}

pub fn byte_to_char(text: &str, byte_idx: usize) -> usize {
    text[..byte_idx.min(text.len())].chars().count()
}

pub fn line_col(text: &str, byte_idx: usize) -> (usize, usize) {
    let upto = &text[..byte_idx.min(text.len())];
    let line = upto.matches('\n').count() + 1;
    let col = upto.rsplit('\n').next().map(|s| s.chars().count()).unwrap_or(0) + 1;
    (line, col)
}

fn editor_id(tab: &EditorTab) -> egui::Id {
    egui::Id::new(("cobalt-editor", tab.id))
}

/// Apply a pending edit to the text and cursor state before the widget is drawn.
fn apply_pending(ctx: &egui::Context, tab: &mut EditorTab) {
    let Some(edit) = tab.editor.pending_edit.take() else { return };
    let id = editor_id(tab);
    let mut state = TextEdit::load_state(ctx, id).unwrap_or_default();
    let set_cursor = |state: &mut egui::text_edit::TextEditState, a: usize, b: usize| {
        state.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(a), CCursor::new(b))));
    };
    match edit {
        PendingEdit::SetCursor(c) => set_cursor(&mut state, c, c),
        PendingEdit::Select(a, b) => set_cursor(&mut state, a, b),
        PendingEdit::Replace { start, end, text, cursor_after } => {
            let start = start.min(tab.text.len());
            let end = end.clamp(start, tab.text.len());
            tab.text.replace_range(start..end, &text);
            let after = cursor_after.unwrap_or(byte_to_char(&tab.text, start + text.len()));
            set_cursor(&mut state, after, after);
        }
        PendingEdit::SetText { text, cursor } => {
            tab.text = text;
            set_cursor(&mut state, cursor, cursor);
        }
    }
    TextEdit::store_state(ctx, id, state);
    tab.editor.request_focus = true;
    tab.editor.scroll_to_cursor = true;
}

/// Draw the editor into the available space. Returns cursor/selection info.
pub fn show(ui: &mut Ui, tab: &mut EditorTab, theme: &Theme, settings: &Settings) -> EditorOutput {
    let ctx = ui.ctx().clone();
    apply_pending(&ctx, tab);
    let id = editor_id(tab);
    let font = FontId::monospace(settings.appearance.editor_font_size);
    let row_h = ui.fonts_mut(|f| f.row_height(&font));
    let colors = theme.tokens.clone();
    let word_wrap = settings.editor.word_wrap;

    // completion popup key handling happens before the TextEdit consumes keys
    let mut accept: Option<usize> = None;
    let mut close_popup = false;
    if let Some(p) = &mut tab.editor.completion {
        let n = p.items.len();
        ui.input_mut(|i| {
            if i.consume_key(Modifiers::NONE, Key::ArrowDown) {
                p.selected = (p.selected + 1) % n.max(1);
            }
            if i.consume_key(Modifiers::NONE, Key::ArrowUp) {
                p.selected = (p.selected + n.max(1) - 1) % n.max(1);
            }
            if i.consume_key(Modifiers::NONE, Key::PageDown) {
                p.selected = (p.selected + 8).min(n.saturating_sub(1));
            }
            if i.consume_key(Modifiers::NONE, Key::PageUp) {
                p.selected = p.selected.saturating_sub(8);
            }
            if i.consume_key(Modifiers::NONE, Key::Enter) || i.consume_key(Modifiers::NONE, Key::Tab) {
                accept = Some(p.selected);
            }
            if i.consume_key(Modifiers::NONE, Key::Escape) {
                close_popup = true;
            }
        });
    }
    if let Some(sel) = accept {
        accept_completion(tab, sel);
        apply_pending(&ctx, tab);
    }
    if close_popup {
        tab.editor.completion = None;
    }

    // find / replace bar
    if tab.editor.find_open {
        find_bar(ui, tab, theme);
    }
    if tab.editor.goto_line_open {
        goto_bar(ui, tab, theme);
    }

    let avail = ui.available_size();
    let mut out = EditorOutput { changed: false, cursor_byte: 0, selection_bytes: None, focused: false };
    let statement_bg = theme.bg_current_statement;
    let highlight_statement = settings.editor.highlight_current_statement;

    egui::Frame::new().fill(theme.bg_editor).show(ui, |ui| {
        ui.set_min_size(avail);
        let scroll = egui::ScrollArea::both().id_salt(("editor-scroll", tab.id)).auto_shrink([false, false]);
        scroll.show(ui, |ui| {
            ui.set_min_height(avail.y);
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                // gutter placeholder; painted after the galley is known
                let (gutter_rect, _) = ui.allocate_exact_size(Vec2::new(GUTTER_W, avail.y.max(row_h)), Sense::hover());
                let gutter_painter = ui.painter().clone();
                let bg_idx = ui.painter().add(Shape::Noop);

                let mut layouter = |ui: &Ui, buf: &dyn egui::TextBuffer, wrap_width: f32| -> Arc<egui::Galley> {
                    let w = if word_wrap { wrap_width } else { f32::INFINITY };
                    let job = layout_job(buf.as_str(), &colors, font.clone(), w);
                    ui.fonts_mut(|f| f.layout_job(job))
                };
                let desired_w = if word_wrap { ui.available_width() } else { f32::INFINITY };
                let te = TextEdit::multiline(&mut tab.text)
                    .id(id)
                    .font(font.clone())
                    .code_editor()
                    .lock_focus(true)
                    .frame(egui::Frame::NONE)
                    .desired_width(desired_w)
                    .desired_rows(((avail.y / row_h).floor() as usize).max(10))
                    .margin(egui::Margin { left: 6, right: 8, top: 4, bottom: 4 })
                    .layouter(&mut layouter);
                let output = te.show(ui);
                let resp = &output.response.response;
                out.changed = resp.changed();
                out.focused = resp.has_focus();
                if tab.editor.request_focus {
                    resp.request_focus();
                    tab.editor.request_focus = false;
                }
                let text = tab.text.as_str();
                let galley = &output.galley;
                let gpos = output.galley_pos;

                // cursor info
                if let Some(range) = output.cursor_range {
                    let sorted = range.as_sorted_char_range();
                    let a = char_to_byte(text, sorted.start.into());
                    let b = char_to_byte(text, sorted.end.into());
                    out.cursor_byte = char_to_byte(text, range.primary.index.into());
                    if a != b {
                        out.selection_bytes = Some((a, b));
                    }
                    tab.editor.cursor = range.primary.index.into();
                    tab.editor.selection = if a != b { Some((sorted.start.into(), sorted.end.into())) } else { None };
                } else {
                    out.cursor_byte = char_to_byte(text, tab.editor.cursor);
                }
                let (line, col) = line_col(text, out.cursor_byte);
                tab.editor.line = line;
                tab.editor.col = col;
                tab.editor.line_count = text.matches('\n').count() + 1;

                // current statement highlight
                tab.editor.statement_range = cobalt_sql::statements::statement_at(text, out.cursor_byte).map(|s| (s.start, s.end));
                if highlight_statement && out.focused {
                    if let Some((s, e)) = tab.editor.statement_range {
                        let cs = byte_to_char(text, s);
                        let ce = byte_to_char(text, e);
                        let r0 = galley.pos_from_cursor(CCursor::new(cs));
                        let r1 = galley.pos_from_cursor(CCursor::new(ce));
                        let full = Rect::from_min_max(
                            Pos2::new(gutter_rect.right(), gpos.y + r0.min.y - 1.0),
                            Pos2::new(ui.clip_rect().right().max(gutter_rect.right() + galley.size().x + 40.0), gpos.y + r1.max.y + 1.0),
                        );
                        ui.painter().set(bg_idx, Shape::rect_filled(full, 0.0, statement_bg));
                    }
                }

                // gutter: line numbers
                gutter_painter.rect_filled(Rect::from_min_size(gutter_rect.min, Vec2::new(GUTTER_W, galley.size().y.max(avail.y) + 8.0)), 0.0, theme.bg_sidebar);
                gutter_painter.line_segment([Pos2::new(gutter_rect.right(), gutter_rect.top()), Pos2::new(gutter_rect.right(), gutter_rect.top() + galley.size().y.max(avail.y) + 8.0)], Stroke::new(1.0, theme.border));
                let mut line_no = 1usize;
                let cur_line = tab.editor.line;
                let mut new_line = true;
                let small = FontId::monospace((settings.appearance.editor_font_size - 1.0).max(9.0));
                for row in &galley.rows {
                    if new_line {
                        let y = gpos.y + row.rect().center().y;
                        let color = if line_no == cur_line { theme.text } else { theme.text_faint };
                        gutter_painter.text(Pos2::new(gutter_rect.right() - 10.0, y), egui::Align2::RIGHT_CENTER, line_no.to_string(), small.clone(), color);
                    }
                    new_line = row.ends_with_newline;
                    if new_line {
                        line_no += 1;
                    }
                }

                // completion: trigger / update / draw
                if out.changed {
                    tab.editor.completion = None;
                    if settings.editor.completion_enabled && settings.editor.completion_on_type {
                        let before = text[..out.cursor_byte].chars().next_back();
                        if matches!(before, Some(c) if c.is_alphanumeric() || c == '_' || c == '.' || c == '@' || c == '#') {
                            let anchor = gpos + galley.pos_from_cursor(CCursor::new(tab.editor.cursor)).left_bottom().to_vec2();
                            open_completion(tab, out.cursor_byte, anchor, before == Some('.'));
                        }
                    }
                } else if tab.editor.completion.is_some() && !out.focused {
                    tab.editor.completion = None;
                }
                if let Some(anchor) = tab.editor.completion.as_ref().map(|p| p.anchor) {
                    draw_completion(ui, tab, theme, anchor, row_h);
                }

                // scroll cursor into view after programmatic moves
                if tab.editor.scroll_to_cursor {
                    let r = galley.pos_from_cursor(CCursor::new(tab.editor.cursor));
                    let rect = Rect::from_min_max(gpos + r.min.to_vec2(), gpos + r.max.to_vec2()).expand(row_h * 2.0);
                    ui.scroll_to_rect(rect, None);
                    tab.editor.scroll_to_cursor = false;
                }
            });
        });
    });
    out
}

/// Ctrl+Space or typing: compute completions at the cursor.
pub fn open_completion(tab: &mut EditorTab, cursor_byte: usize, anchor: Pos2, force: bool) {
    let dbs: Vec<String> = tab.databases.get().map(|d| d.iter().map(|x| x.name.clone()).collect()).unwrap_or_default();
    let req = cobalt_sql::completion::CompletionRequest { text: &tab.text, cursor: cursor_byte, catalog: tab.catalog.as_deref(), databases: &dbs, max_items: 60 };
    let c = cobalt_sql::completion::complete(&req);
    let prefix_len = c.replace_end.saturating_sub(c.replace_start);
    if c.items.is_empty() || (!force && prefix_len == 0) {
        tab.editor.completion = None;
        return;
    }
    let items = c
        .items
        .into_iter()
        .map(|i| CompletionEntry { label: i.label, insert: i.insert, detail: i.detail, icon: completion_icon(i.kind) })
        .collect();
    tab.editor.completion = Some(CompletionPopup { items, selected: 0, replace_start: c.replace_start, replace_end: c.replace_end, anchor });
}

fn completion_icon(kind: cobalt_sql::completion::CompletionKind) -> &'static str {
    use cobalt_sql::completion::CompletionKind as K;
    use egui_phosphor::regular as i;
    match kind {
        K::Keyword => i::KEY_RETURN,
        K::Function | K::ScalarFunction | K::TableFunction => i::FUNCTION,
        K::Type => i::CUBE,
        K::Schema => i::FOLDER_SIMPLE,
        K::Table => i::TABLE,
        K::View => i::EYE,
        K::Procedure => i::GEAR_SIX,
        K::Column => i::COLUMNS,
        K::Alias => i::TAG,
        K::Variable => i::AT,
        K::Snippet => i::SCISSORS,
        K::Database => i::DATABASE,
    }
}

fn accept_completion(tab: &mut EditorTab, sel: usize) {
    let Some(p) = tab.editor.completion.take() else { return };
    let Some(item) = p.items.get(sel) else { return };
    let (text, cursor_after) = if item.insert.contains("${") || item.insert.contains('$') && item.icon == egui_phosphor::regular::SCISSORS {
        let (expanded, stops) = cobalt_sql::snippets::expand(&item.insert);
        let c = stops.first().map(|(s, _)| p.replace_start + s).unwrap_or(p.replace_start + expanded.len());
        (expanded, Some(c))
    } else {
        (item.insert.clone(), None)
    };
    let cursor_after_chars = cursor_after.map(|b| {
        // compute after replacement
        let mut t = tab.text.clone();
        t.replace_range(p.replace_start.min(t.len())..p.replace_end.min(t.len()), &text);
        byte_to_char(&t, b)
    });
    tab.editor.pending_edit = Some(PendingEdit::Replace { start: p.replace_start, end: p.replace_end, text, cursor_after: cursor_after_chars });
}

fn draw_completion(ui: &mut Ui, tab: &mut EditorTab, theme: &Theme, anchor: Pos2, row_h: f32) {
    let mut accept: Option<usize> = None;
    let id = egui::Id::new(("completion", tab.id));
    let Some(p) = tab.editor.completion.as_mut() else { return };
    let max_visible = 12usize;
    let item_h = 22.0;
    egui::Area::new(id).order(egui::Order::Foreground).fixed_pos(anchor + Vec2::new(0.0, 2.0)).constrain(true).show(ui.ctx(), |ui| {
        egui::Frame::popup(ui.style()).fill(theme.bg_panel).stroke(Stroke::new(1.0, theme.border_strong)).inner_margin(2.0).show(ui, |ui| {
            ui.set_min_width(260.0);
            ui.set_max_width(520.0);
            let h = (p.items.len().min(max_visible) as f32) * item_h;
            egui::ScrollArea::vertical().id_salt(id.with("scroll")).max_height(h).auto_shrink([false, true]).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for (i, it) in p.items.iter().enumerate() {
                    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width().max(256.0), item_h), Sense::click());
                    let selected = i == p.selected;
                    if selected {
                        ui.painter().rect_filled(rect, 3.0, theme.bg_selection);
                        if resp.rect.top() < ui.clip_rect().top() || resp.rect.bottom() > ui.clip_rect().bottom() {
                            ui.scroll_to_rect(rect, None);
                        }
                    } else if resp.hovered() {
                        ui.painter().rect_filled(rect, 3.0, theme.bg_hover);
                    }
                    let p0 = rect.left_center();
                    ui.painter().text(p0 + Vec2::new(12.0, 0.0), egui::Align2::CENTER_CENTER, it.icon, FontId::proportional(13.0), theme.accent);
                    let label_galley = ui.painter().layout_no_wrap(it.label.clone(), FontId::monospace(13.0), theme.text);
                    let lw = label_galley.size().x;
                    ui.painter().galley(p0 + Vec2::new(26.0, -label_galley.size().y / 2.0), label_galley, theme.text);
                    if let Some(d) = &it.detail {
                        ui.painter().text(p0 + Vec2::new(26.0 + lw + 10.0, 0.0), egui::Align2::LEFT_CENTER, d, FontId::proportional(11.0), theme.text_faint);
                    }
                    if resp.clicked() {
                        accept = Some(i);
                    }
                    if resp.hovered() && ui.input(|i| i.pointer.is_moving()) {
                        p.selected = i;
                    }
                }
            });
        });
    });
    let _ = row_h;
    if let Some(i) = accept {
        accept_completion(tab, i);
    }
}

fn find_bar(ui: &mut Ui, tab: &mut EditorTab, theme: &Theme) {
    let mut do_find = false;
    let mut do_replace = false;
    let mut do_replace_all = false;
    let mut close = false;
    egui::Frame::new().fill(theme.bg_sidebar).inner_margin(egui::Margin::symmetric(8, 4)).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(egui_phosphor::regular::MAGNIFYING_GLASS);
            let r = ui.add(TextEdit::singleline(&mut tab.editor.find_text).desired_width(220.0).hint_text("Find").id(egui::Id::new(("find", tab.id))));
            if tab.editor.request_focus {
                // keep editor focus request
            }
            if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                do_find = true;
                r.request_focus();
            }
            if ui.small_button("Next").clicked() {
                do_find = true;
            }
            ui.checkbox(&mut tab.editor.find_case, "Aa").on_hover_text("Match case");
            ui.separator();
            ui.add(TextEdit::singleline(&mut tab.editor.replace_text).desired_width(200.0).hint_text("Replace"));
            if ui.small_button("Replace").clicked() {
                do_replace = true;
            }
            if ui.small_button("All").clicked() {
                do_replace_all = true;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button(egui_phosphor::regular::X).clicked() {
                    close = true;
                }
            });
        });
    });
    if ui.input(|i| i.key_pressed(Key::Escape)) {
        close = true;
    }
    if do_replace {
        if let Some((a, b)) = tab.editor.selection {
            let ab = char_to_byte(&tab.text, a);
            let bb = char_to_byte(&tab.text, b);
            let sel = &tab.text[ab..bb];
            let matches = if tab.editor.find_case { sel == tab.editor.find_text } else { sel.eq_ignore_ascii_case(&tab.editor.find_text) };
            if matches {
                let rep = tab.editor.replace_text.clone();
                let after = byte_to_char(&tab.text, ab) + rep.chars().count();
                tab.editor.pending_edit = Some(PendingEdit::Replace { start: ab, end: bb, text: rep, cursor_after: Some(after) });
                tab.editor.cursor = after;
            }
        }
        do_find = true;
    }
    if do_replace_all && !tab.editor.find_text.is_empty() {
        let needle = tab.editor.find_text.clone();
        let rep = tab.editor.replace_text.clone();
        let new_text = if tab.editor.find_case {
            tab.text.replace(&needle, &rep)
        } else {
            replace_case_insensitive(&tab.text, &needle, &rep)
        };
        let cursor = tab.editor.cursor.min(new_text.chars().count());
        tab.editor.pending_edit = Some(PendingEdit::SetText { text: new_text, cursor });
    }
    if do_find && !tab.editor.find_text.is_empty() {
        find_next(tab);
    }
    if close {
        tab.editor.find_open = false;
        tab.editor.request_focus = true;
    }
}

pub fn find_next(tab: &mut EditorTab) {
    let needle = tab.editor.find_text.clone();
    if needle.is_empty() {
        return;
    }
    let text = &tab.text;
    let start_char = tab.editor.selection.map(|(_, b)| b).unwrap_or(tab.editor.cursor);
    let start = char_to_byte(text, start_char);
    let (hay, nd) = if tab.editor.find_case { (text.to_string(), needle.clone()) } else { (text.to_lowercase(), needle.to_lowercase()) };
    // lowercase can change byte lengths for non-ASCII; fall back to char-wise search in that case
    let found = if hay.len() == text.len() {
        hay[start..].find(&nd).map(|i| start + i).or_else(|| hay[..start].find(&nd))
    } else {
        text.to_lowercase().find(&nd)
    };
    if let Some(b) = found {
        let a = byte_to_char(text, b);
        let e = a + needle.chars().count();
        tab.editor.pending_edit = Some(PendingEdit::Select(a, e));
        tab.editor.selection = Some((a, e));
        tab.editor.cursor = e;
    }
}

fn replace_case_insensitive(text: &str, needle: &str, rep: &str) -> String {
    let lower = text.to_lowercase();
    let nl = needle.to_lowercase();
    if lower.len() != text.len() || nl.is_empty() {
        return text.replace(needle, rep);
    }
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while let Some(pos) = lower[i..].find(&nl) {
        out.push_str(&text[i..i + pos]);
        out.push_str(rep);
        i += pos + nl.len();
    }
    out.push_str(&text[i..]);
    out
}

fn goto_bar(ui: &mut Ui, tab: &mut EditorTab, theme: &Theme) {
    let mut go = false;
    let mut close = false;
    egui::Frame::new().fill(theme.bg_sidebar).inner_margin(egui::Margin::symmetric(8, 4)).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label("Go to line:");
            let r = ui.add(TextEdit::singleline(&mut tab.editor.goto_line_text).desired_width(80.0).id(egui::Id::new(("goto", tab.id))));
            r.request_focus();
            if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                go = true;
            }
            if ui.small_button("Go").clicked() {
                go = true;
            }
            if ui.input(|i| i.key_pressed(Key::Escape)) {
                close = true;
            }
        });
    });
    if go {
        if let Ok(n) = tab.editor.goto_line_text.trim().parse::<usize>() {
            let n = n.max(1);
            let byte = tab.text.split_inclusive('\n').take(n - 1).map(|l| l.len()).sum::<usize>();
            let c = byte_to_char(&tab.text, byte);
            tab.editor.pending_edit = Some(PendingEdit::SetCursor(c));
        }
        close = true;
    }
    if close {
        tab.editor.goto_line_open = false;
        tab.editor.request_focus = true;
    }
}

/// Toggle `--` on every line touched by the selection (or the cursor line).
pub fn toggle_line_comment(tab: &mut EditorTab) {
    let text = tab.text.clone();
    let (a, b) = match tab.editor.selection {
        Some((a, b)) => (char_to_byte(&text, a), char_to_byte(&text, b)),
        None => {
            let c = char_to_byte(&text, tab.editor.cursor);
            (c, c)
        }
    };
    let line_start = text[..a].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let line_end = text[b..].find('\n').map(|i| b + i).unwrap_or(text.len());
    let block = &text[line_start..line_end];
    let all_commented = block.lines().filter(|l| !l.trim().is_empty()).all(|l| l.trim_start().starts_with("--"));
    let new_block: String = block
        .split_inclusive('\n')
        .map(|l| {
            let (body, nl) = match l.strip_suffix('\n') {
                Some(b) => (b, "\n"),
                None => (l, ""),
            };
            if all_commented {
                let trimmed = body.trim_start();
                if let Some(rest) = trimmed.strip_prefix("--") {
                    let indent = &body[..body.len() - trimmed.len()];
                    format!("{indent}{}{nl}", rest.strip_prefix(' ').unwrap_or(rest))
                } else {
                    format!("{body}{nl}")
                }
            } else if body.trim().is_empty() {
                format!("{body}{nl}")
            } else {
                format!("-- {body}{nl}")
            }
        })
        .collect();
    let sel_start = byte_to_char(&text, line_start);
    let sel_end = sel_start + new_block.chars().count();
    tab.editor.pending_edit = Some(PendingEdit::Replace { start: line_start, end: line_end, text: new_block, cursor_after: Some(sel_end) });
    tab.editor.selection = Some((sel_start, sel_end));
}

pub fn toggle_block_comment(tab: &mut EditorTab) {
    let text = tab.text.clone();
    let Some((a, b)) = tab.editor.selection else { return };
    let (ab, bb) = (char_to_byte(&text, a), char_to_byte(&text, b));
    let sel = &text[ab..bb];
    let new = if sel.trim_start().starts_with("/*") && sel.trim_end().ends_with("*/") {
        let t = sel.trim();
        t[2..t.len() - 2].trim().to_string()
    } else {
        format!("/* {sel} */")
    };
    let end = a + new.chars().count();
    tab.editor.pending_edit = Some(PendingEdit::Replace { start: ab, end: bb, text: new, cursor_after: Some(end) });
    tab.editor.selection = Some((a, end));
}

/// Replace the whole text keeping the cursor line.
pub fn set_text_keep_line(tab: &mut EditorTab, new_text: String) {
    let line = tab.editor.line.max(1);
    let byte = new_text.split_inclusive('\n').take(line - 1).map(|l| l.len()).sum::<usize>();
    let c = byte_to_char(&new_text, byte);
    tab.editor.pending_edit = Some(PendingEdit::SetText { text: new_text, cursor: c });
}

pub fn color_hex(c: Color32) -> String {
    format!("#{:02X}{:02X}{:02X}", c.r(), c.g(), c.b())
}
