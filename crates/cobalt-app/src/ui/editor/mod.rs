//! The SQL editor: the multi-cursor code editor widget (`widget.rs` over the model in
//! `core.rs`) with a lexer-driven layout, line-number gutter, current-statement highlight,
//! completion popup, find/replace and go-to-line.

pub mod core;
pub mod widget;

use self::core::{EditKind, Sel, SnippetSession};
use cobalt_sql::snippets::UserSnippet;
use std::sync::RwLock;

/// The user's snippets (`snippets.toml` in the config folder), loaded by the app and reloaded
/// when the file changes. Global because completion runs deep inside the editor widget.
pub static USER_SNIPPETS: RwLock<Vec<UserSnippet>> = RwLock::new(Vec::new());
use super::theme::{Theme, TokenColors};
use crate::state::{CompletionEntry, CompletionPopup, EditorState, EditorTab, Loadable, PendingEdit};
use cobalt_core::DatabaseInfo;
use cobalt_core::Settings;
use cobalt_sql::lexer::{tokenize, TokenKind};
use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId, Key, Modifiers, Pos2, Rect, Sense, Shape, Stroke, TextEdit, Ui, Vec2};

pub const GUTTER_W: f32 = 52.0;

pub struct EditorOutput {
    pub changed: bool,
    /// Something to tell the user (a toast): "No more matches" after Ctrl+D, etc.
    pub notice: Option<&'static str>,
    /// Byte offset of the cursor.
    pub cursor_byte: usize,
    /// Byte range of the selection, if any (sorted).
    pub selection_bytes: Option<(usize, usize)>,
    pub focused: bool,
}

/// What the editor is editing: decides highlighting, completion and statement tracking.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Syntax {
    #[default]
    Sql,
    Python,
    /// Markdown and anything else: no highlighting.
    Plain,
}

/// Build a highlighted layout job for `text` in the given syntax.
pub fn layout_job_for(syntax: Syntax, text: &str, colors: &TokenColors, font: FontId, wrap_width: f32) -> LayoutJob {
    match syntax {
        Syntax::Sql => layout_job(text, colors, font, wrap_width),
        Syntax::Python => python_layout_job(text, colors, font, wrap_width),
        Syntax::Plain => {
            let mut job = LayoutJob::default();
            job.wrap.max_width = wrap_width;
            job.append(text, 0.0, TextFormat { font_id: font, color: colors.identifier, ..Default::default() });
            job
        }
    }
}

const PY_KEYWORDS: &[&str] = &["False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import", "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while", "with", "yield", "match", "case"];
const PY_BUILTINS: &[&str] = &["print", "len", "range", "int", "str", "float", "bool", "list", "dict", "set", "tuple", "type", "isinstance", "enumerate", "zip", "map", "filter", "sorted", "sum", "min", "max", "abs", "round", "open", "display", "spark", "sc", "F", "T", "Window", "notebookutils", "mssparkutils", "self"];

/// A small Python tokenizer for highlighting: comments, strings (with prefixes and triple
/// quotes), numbers, keywords, builtins, decorators. Good enough for cells; not a parser.
pub fn python_layout_job(text: &str, colors: &TokenColors, font: FontId, wrap_width: f32) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = wrap_width;
    let push = |job: &mut LayoutJob, s: &str, color: Color32, italics: bool| {
        if !s.is_empty() {
            job.append(s, 0.0, TextFormat { font_id: font.clone(), color, italics, ..Default::default() });
        }
    };
    let bytes = text.as_bytes();
    let n = bytes.len();
    let mut i = 0;
    let mut plain_start = 0;
    while i < n {
        let c = bytes[i];
        // comment to end of line
        if c == b'#' {
            push(&mut job, &text[plain_start..i], colors.identifier, false);
            let end = text[i..].find('\n').map(|k| i + k).unwrap_or(n);
            push(&mut job, &text[i..end], colors.comment, true);
            i = end;
            plain_start = i;
            continue;
        }
        // string, possibly with a prefix (r, b, f, u, rb, br, fr, rf)
        if c == b'\'' || c == b'"' || (c.is_ascii_alphabetic() && i + 1 < n && {
            let mut j = i;
            while j < n && j - i < 2 && bytes[j].is_ascii_alphabetic() && b"rbfuRBFU".contains(&bytes[j]) {
                j += 1;
            }
            j > i && j < n && (bytes[j] == b'\'' || bytes[j] == b'"') && (i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_'))
        }) {
            let mut q = i;
            while bytes[q] != b'\'' && bytes[q] != b'"' {
                q += 1;
            }
            let quote = bytes[q];
            let triple = q + 2 < n && bytes[q + 1] == quote && bytes[q + 2] == quote;
            let mut k = if triple { q + 3 } else { q + 1 };
            let mut closed = false;
            while k < n {
                if bytes[k] == b'\\' {
                    k += 2;
                    continue;
                }
                if triple {
                    if bytes[k] == quote && k + 2 < n + 0 && k + 2 <= n - 1 && bytes[k + 1] == quote && bytes[k + 2] == quote {
                        k += 3;
                        closed = true;
                        break;
                    }
                } else if bytes[k] == quote {
                    k += 1;
                    closed = true;
                    break;
                } else if bytes[k] == b'\n' {
                    break; // an unterminated single-line string stops at the line end
                }
                k += 1;
            }
            let _ = closed;
            let k = k.min(n);
            // keep on a char boundary
            let mut end = k;
            while end < n && !text.is_char_boundary(end) {
                end += 1;
            }
            push(&mut job, &text[plain_start..i], colors.identifier, false);
            push(&mut job, &text[i..end], colors.string, false);
            i = end;
            plain_start = i;
            continue;
        }
        // numbers
        if c.is_ascii_digit() && (i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_')) {
            let mut k = i + 1;
            while k < n && (bytes[k].is_ascii_alphanumeric() || bytes[k] == b'.' || bytes[k] == b'_') {
                k += 1;
            }
            push(&mut job, &text[plain_start..i], colors.identifier, false);
            push(&mut job, &text[i..k], colors.number, false);
            i = k;
            plain_start = i;
            continue;
        }
        // words
        if c.is_ascii_alphabetic() || c == b'_' {
            let mut k = i + 1;
            while k < n && (bytes[k].is_ascii_alphanumeric() || bytes[k] == b'_') {
                k += 1;
            }
            let word = &text[i..k];
            let color = if PY_KEYWORDS.contains(&word) {
                Some(colors.keyword)
            } else if PY_BUILTINS.contains(&word) {
                Some(colors.function)
            } else if i > 0 && bytes[i - 1] == b'@' {
                Some(colors.variable)
            } else {
                None
            };
            if let Some(col) = color {
                push(&mut job, &text[plain_start..i], colors.identifier, false);
                push(&mut job, word, col, false);
                plain_start = k;
            }
            i = k;
            continue;
        }
        if c == b'%' && (i == 0 || bytes[i - 1] == b'\n') {
            // a cell or line magic
            push(&mut job, &text[plain_start..i], colors.identifier, false);
            let end = text[i..].find('\n').map(|k| i + k).unwrap_or(n);
            push(&mut job, &text[i..end], colors.variable, false);
            i = end;
            plain_start = i;
            continue;
        }
        i += 1;
    }
    push(&mut job, &text[plain_start..], colors.identifier, false);
    job
}

/// Build a highlighted layout job for `text` (T-SQL).
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

/// Consume a key press with no modifiers at all (egui's `consume_key` would also take Shift+Tab
/// for a Tab pattern, which the editor needs for outdent).
fn consume_plain_key(i: &mut egui::InputState, key: Key) -> bool {
    let mut hit = false;
    i.events.retain(|e| match e {
        egui::Event::Key { key: k, pressed: true, modifiers, .. } if *k == key && modifiers.matches_exact(Modifiers::NONE) => {
            hit = true;
            false
        }
        _ => true,
    });
    hit
}

/// Apply a pending edit (from a command: completion, format, Go to line, Insert CREATE INDEX…)
/// to the text and the cursor set before the widget is drawn. Text edits are undoable.
pub fn apply_pending(h: &mut EditorHost<'_>) {
    let Some(edit) = h.editor.pending_edit.take() else { return };
    let ed = &mut *h.editor;
    match edit {
        PendingEdit::SetCursor(c) => ed.cursors.set_single(Sel::cursor(c)),
        PendingEdit::Select(a, b) => ed.cursors.set_single(Sel::range(a, b)),
        PendingEdit::Replace { start, end, text, cursor_after } => {
            ed.undo.record(EditKind::Other, &h.text, &ed.cursors);
            let start = start.min(h.text.len());
            let end = end.clamp(start, h.text.len());
            h.text.replace_range(start..end, &text);
            let after = cursor_after.unwrap_or(byte_to_char(&h.text, start + text.len()));
            ed.cursors.set_single(Sel::cursor(after));
            ed.snippet = None;
            if let Some(stops) = ed.pending_snippet.take() {
                // a snippet: select its first placeholder and let Tab walk the rest
                let stops: Vec<(usize, usize)> = stops.iter().map(|&(a, b)| (byte_to_char(&h.text, a), byte_to_char(&h.text, b))).collect();
                if let Some(&(a, b)) = stops.first() {
                    ed.cursors.set_single(Sel::range(a, b));
                    if stops.len() > 1 || a != b {
                        ed.snippet = Some(SnippetSession { stops, index: 0 });
                    }
                }
            }
        }
        PendingEdit::SetText { text, cursor } => {
            ed.undo.record(EditKind::Other, &h.text, &ed.cursors);
            *h.text = text;
            ed.cursors.set_single(Sel::cursor(cursor));
            ed.snippet = None;
        }
    }
    ed.cursors.clamp(h.text.chars().count());
    ed.request_focus = true;
    ed.scroll_to_cursor = true;
}

/// Draw the editor into the available space. Returns cursor/selection info.
/// "12 ms", "1.3 s", "2m 05s" — compact, for the gutter.
fn fmt_batch_time(d: std::time::Duration) -> String {
    let ms = d.as_secs_f64() * 1000.0;
    if ms < 1000.0 {
        format!("{ms:.0} ms")
    } else if ms < 60_000.0 {
        format!("{:.1} s", ms / 1000.0)
    } else {
        let s = d.as_secs();
        format!("{}m {:02}s", s / 60, s % 60)
    }
}

/// Where the editor borrows its text, cursor state and completion context from: a query tab,
/// or one notebook cell.
pub struct EditorHost<'a> {
    pub id: egui::Id,
    pub text: &'a mut String,
    pub editor: &'a mut EditorState,
    pub catalog: Option<&'a cobalt_core::DatabaseCatalog>,
    pub databases: &'a Loadable<Vec<DatabaseInfo>>,
    pub syntax: Syntax,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EditorLayout {
    /// Fill the available space and scroll inside it (query tabs).
    Fill,
    /// Grow with the text, at least `min_rows` tall; the surrounding UI scrolls (notebook cells).
    Auto { min_rows: usize },
}

impl EditorTab {
    pub fn host(&mut self) -> EditorHost<'_> {
        EditorHost { id: egui::Id::new(("cobalt-editor", self.id)), text: &mut self.text, editor: &mut self.editor, catalog: self.catalog.as_deref(), databases: &self.databases, syntax: Syntax::Sql }
    }
}

/// Draw a query tab's editor into the available space. Returns cursor/selection info.
pub fn show(ui: &mut Ui, tab: &mut EditorTab, theme: &Theme, settings: &Settings) -> EditorOutput {
    // per-batch timings from the last run, shown while the text is unchanged
    let timings: Vec<(u32, String, bool)> = match &tab.run {
        Some(r) if !r.batch_times.is_empty() && r.script_hash == crate::state::hash_text(&tab.text) => r.batch_times.iter().filter_map(|(line, el, err)| el.map(|d| (*line, fmt_batch_time(d), *err))).collect(),
        _ => Vec::new(),
    };
    show_host(ui, &mut tab.host(), timings, EditorLayout::Fill, theme, settings)
}

pub fn show_host(ui: &mut Ui, h: &mut EditorHost<'_>, timings: Vec<(u32, String, bool)>, layout: EditorLayout, theme: &Theme, settings: &Settings) -> EditorOutput {
    apply_pending(h);
    let focus_pending = h.editor.request_focus;
    let id = h.id;
    let font = FontId::monospace(settings.appearance.editor_font_size);
    let row_h = ui.fonts_mut(|f| f.row_height(&font));
    let colors = theme.tokens.clone();
    let word_wrap = settings.editor.word_wrap;

    // completion popup key handling happens before the TextEdit consumes keys
    let mut accept: Option<usize> = None;
    let mut close_popup = false;
    if let Some(p) = &mut h.editor.completion {
        let n = p.items.len();
        ui.input_mut(|i| {
            if consume_plain_key(i, Key::ArrowDown) {
                p.selected = (p.selected + 1) % n.max(1);
            }
            if consume_plain_key(i, Key::ArrowUp) {
                p.selected = (p.selected + n.max(1) - 1) % n.max(1);
            }
            if consume_plain_key(i, Key::PageDown) {
                p.selected = (p.selected + 8).min(n.saturating_sub(1));
            }
            if consume_plain_key(i, Key::PageUp) {
                p.selected = p.selected.saturating_sub(8);
            }
            if consume_plain_key(i, Key::Enter) || consume_plain_key(i, Key::Tab) {
                accept = Some(p.selected);
            }
            if consume_plain_key(i, Key::Escape) {
                close_popup = true;
            }
        });
    }
    if let Some(sel) = accept {
        accept_completion(h, sel);
        apply_pending(h);
    }
    if close_popup {
        h.editor.completion = None;
    }

    // find / replace bar
    if h.editor.find_open {
        find_bar(ui, h, theme);
    }
    if h.editor.goto_line_open {
        goto_bar(ui, h, theme);
    }

    let mut avail = ui.available_size();
    let auto_rows = match layout {
        EditorLayout::Fill => None,
        EditorLayout::Auto { min_rows } => {
            let rows = (h.text.matches('\n').count() + 1).max(min_rows);
            avail.y = rows as f32 * row_h + 10.0;
            Some(rows)
        }
    };
    let mut out = EditorOutput { changed: false, notice: None, cursor_byte: 0, selection_bytes: None, focused: false };
    let statement_bg = theme.bg_current_statement;
    let highlight_statement = settings.editor.highlight_current_statement;

    egui::Frame::new().fill(theme.bg_editor).show(ui, |ui| {
        ui.set_min_size(avail);
        let scroll = if auto_rows.is_some() {
            egui::ScrollArea::neither().id_salt(id.with("scroll")).auto_shrink([false, true])
        } else {
            egui::ScrollArea::both().id_salt(id.with("scroll")).auto_shrink([false, false])
        };
        scroll.show(ui, |ui| {
            ui.set_min_height(avail.y);
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                let gutter_w = GUTTER_W + if timings.is_empty() { 0.0 } else { 48.0 };
                // gutter placeholder; painted after the galley is known
                let (gutter_rect, _) = ui.allocate_exact_size(Vec2::new(gutter_w, avail.y.max(row_h)), Sense::hover());
                let gutter_painter = ui.painter().clone();
                let bg_idx = ui.painter().add(Shape::Noop);

                if h.editor.request_focus {
                    ui.ctx().memory_mut(|m| m.request_focus(id));
                    h.editor.request_focus = false;
                }
                let min_size = Vec2::new((ui.available_width()).max(0.0), avail.y);
                let scroll_to_cursor = std::mem::take(&mut h.editor.scroll_to_cursor);
                let find_mode = core::MatchMode { case_sensitive: h.editor.find_case, whole_word: false };
                let ed = &mut *h.editor;
                let syntax = h.syntax;
                let output = widget::CodeEditor {
                    id,
                    syntax,
                    text: &mut *h.text,
                    cursors: &mut ed.cursors,
                    undo: &mut ed.undo,
                    snippet: &mut ed.snippet,
                    font: font.clone(),
                    colors: &colors,
                    text_color: theme.text,
                    cursor_color: theme.text,
                    word_wrap,
                    tab_size: settings.editor.tab_size as usize,
                    insert_spaces: settings.editor.insert_spaces,
                    pairs: self::core::PairPolicy { surround: settings.editor.auto_surround, auto_close: settings.editor.auto_close_brackets },
                    min_size,
                    margin: egui::Margin { left: 6, right: 8, top: 4, bottom: 4 },
                    scroll_to_cursor,
                    find_mode,
                    page_rows: auto_rows.unwrap_or(((avail.y / row_h).floor() as usize).saturating_sub(1)).max(1),
                }
                .show(ui);
                out.changed = output.changed;
                out.notice = output.notice;
                out.focused = output.focused || focus_pending;
                let text: &str = h.text;
                let galley = &output.galley;
                let gpos = output.galley_pos;

                // cursor info (the primary selection)
                let p = h.editor.cursors.primary();
                out.cursor_byte = char_to_byte(text, p.head);
                if !p.is_empty() {
                    out.selection_bytes = Some((char_to_byte(text, p.min()), char_to_byte(text, p.max())));
                }
                h.editor.cursor = p.head;
                h.editor.selection = (!p.is_empty()).then_some((p.min(), p.max()));
                let (line, col) = line_col(text, out.cursor_byte);
                h.editor.line = line;
                h.editor.col = col;
                h.editor.line_count = text.matches('\n').count() + 1;

                // current statement highlight (SQL only)
                h.editor.statement_range = if syntax == Syntax::Sql { cobalt_sql::statements::statement_at(text, out.cursor_byte).map(|s| (s.start, s.end)) } else { None };
                if highlight_statement && out.focused && syntax == Syntax::Sql {
                    if let Some((s, e)) = h.editor.statement_range {
                        let cs = byte_to_char(text, s);
                        let ce = byte_to_char(text, e);
                        let r0 = widget::caret_rect(galley, cs, row_h);
                        let r1 = widget::caret_rect(galley, ce, row_h);
                        let full = Rect::from_min_max(
                            Pos2::new(gutter_rect.right(), gpos.y + r0.min.y - 1.0),
                            Pos2::new(ui.clip_rect().right().max(gutter_rect.right() + galley.size().x + 40.0), gpos.y + r1.max.y + 1.0),
                        );
                        ui.painter().set(bg_idx, Shape::rect_filled(full, 0.0, statement_bg));
                    }
                }

                // gutter: line numbers
                // the band spans the editor; the query tab's scroll area gets a little slack below
                let band_h = galley.size().y.max(avail.y) + if auto_rows.is_some() { 0.0 } else { 8.0 };
                gutter_painter.rect_filled(Rect::from_min_size(gutter_rect.min, Vec2::new(gutter_w, band_h)), 0.0, theme.bg_sidebar);
                gutter_painter.line_segment([Pos2::new(gutter_rect.right(), gutter_rect.top()), Pos2::new(gutter_rect.right(), gutter_rect.top() + band_h)], Stroke::new(1.0, theme.border));
                let mut line_no = 1usize;
                let cur_line = h.editor.line;
                let mut new_line = true;
                let small = FontId::monospace((settings.appearance.editor_font_size - 1.0).max(9.0));
                for (ri, row) in galley.rows.iter().enumerate() {
                    if new_line {
                        let y = gpos.y + widget::row_rect(galley, ri, row_h).center().y;
                        let color = if line_no == cur_line { theme.text } else { theme.text_faint };
                        gutter_painter.text(Pos2::new(gutter_rect.right() - 10.0, y), egui::Align2::RIGHT_CENTER, line_no.to_string(), small.clone(), color);
                        if let Some((_, label, err)) = timings.iter().find(|(l, _, _)| *l as usize == line_no) {
                            let tiny = FontId::proportional((settings.appearance.editor_font_size - 3.0).max(8.0));
                            gutter_painter.text(Pos2::new(gutter_rect.left() + 4.0, y), egui::Align2::LEFT_CENTER, label, tiny, if *err { theme.error } else { theme.success });
                        }
                    }
                    new_line = row.ends_with_newline;
                    if new_line {
                        line_no += 1;
                    }
                }

                // completion: trigger / update / draw
                if out.changed {
                    h.editor.completion = None;
                    // not while a snippet's placeholders are being filled in: Tab must stay the
                    // way to the next stop (Ctrl+Space still opens suggestions on demand)
                    if output.typed && syntax == Syntax::Sql && h.editor.snippet.is_none() && settings.editor.completion_enabled && settings.editor.completion_on_type {
                        let before = text[..out.cursor_byte].chars().next_back();
                        if matches!(before, Some(c) if c.is_alphanumeric() || c == '_' || c == '.' || c == '@' || c == '#') {
                            let anchor = gpos + widget::caret_rect(galley, h.editor.cursor, row_h).left_bottom().to_vec2();
                            open_completion(h, out.cursor_byte, anchor, before == Some('.'));
                        }
                    }
                } else if h.editor.completion.is_some() && !out.focused && !focus_pending {
                    h.editor.completion = None;
                }
                if let Some(anchor) = h.editor.completion.as_ref().map(|p| p.anchor) {
                    draw_completion(ui, h, theme, anchor, row_h);
                }

                let _ = Rect::NOTHING; // (scrolling to the caret is done by the widget)
            });
        });
    });
    out
}

/// Ctrl+Space or typing: compute completions at the cursor.
pub fn open_completion(h: &mut EditorHost<'_>, cursor_byte: usize, anchor: Pos2, force: bool) {
    let dbs: Vec<String> = h.databases.get().map(|d| d.iter().map(|x| x.name.clone()).collect()).unwrap_or_default();
    let user = USER_SNIPPETS.read().map(|v| v.clone()).unwrap_or_default();
    let req = cobalt_sql::completion::CompletionRequest { text: &h.text, cursor: cursor_byte, catalog: h.catalog, databases: &dbs, max_items: 60, user_snippets: &user };
    let c = cobalt_sql::completion::complete(&req);
    let prefix_len = c.replace_end.saturating_sub(c.replace_start);
    if c.items.is_empty() || (!force && prefix_len == 0) {
        h.editor.completion = None;
        return;
    }
    let items = c
        .items
        .into_iter()
        .map(|i| CompletionEntry { label: i.label, insert: i.insert, detail: i.detail, icon: completion_icon(i.kind) })
        .collect();
    h.editor.completion = Some(CompletionPopup { items, selected: 0, replace_start: c.replace_start, replace_end: c.replace_end, anchor });
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

fn accept_completion(h: &mut EditorHost<'_>, sel: usize) {
    let Some(p) = h.editor.completion.take() else { return };
    let Some(item) = p.items.get(sel) else { return };
    let (text, cursor_after) = if item.insert.contains("${") || item.insert.contains('$') && item.icon == egui_phosphor::regular::SCISSORS {
        let (expanded, stops) = cobalt_sql::snippets::expand(&item.insert);
        let c = stops.first().map(|(s, _)| p.replace_start + s).unwrap_or(p.replace_start + expanded.len());
        // absolute byte ranges in the text once the replacement is in; apply_pending selects the first
        h.editor.pending_snippet = Some(stops.iter().map(|&(s, e)| (p.replace_start + s, p.replace_start + e)).collect());
        (expanded, Some(c))
    } else {
        h.editor.pending_snippet = None;
        (item.insert.clone(), None)
    };
    let cursor_after_chars = cursor_after.map(|b| {
        // compute after replacement
        let mut t = h.text.clone();
        t.replace_range(p.replace_start.min(t.len())..p.replace_end.min(t.len()), &text);
        byte_to_char(&t, b)
    });
    h.editor.pending_edit = Some(PendingEdit::Replace { start: p.replace_start, end: p.replace_end, text, cursor_after: cursor_after_chars });
}

fn draw_completion(ui: &mut Ui, h: &mut EditorHost<'_>, theme: &Theme, anchor: Pos2, row_h: f32) {
    let mut accept: Option<usize> = None;
    let id = h.id.with("completion");
    let Some(p) = h.editor.completion.as_mut() else { return };
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
        accept_completion(h, i);
    }
}

fn find_bar(ui: &mut Ui, h: &mut EditorHost<'_>, theme: &Theme) {
    let mut do_find = false;
    let mut do_replace = false;
    let mut do_replace_all = false;
    let mut close = false;
    egui::Frame::new().fill(theme.bg_sidebar).inner_margin(egui::Margin::symmetric(8, 4)).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(egui_phosphor::regular::MAGNIFYING_GLASS);
            let r = ui.add(TextEdit::singleline(&mut h.editor.find_text).desired_width(220.0).hint_text("Find").id(h.id.with("find")));
            if h.editor.request_focus {
                // keep editor focus request
            }
            if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                do_find = true;
                r.request_focus();
            }
            if ui.small_button("Next").clicked() {
                do_find = true;
            }
            ui.checkbox(&mut h.editor.find_case, "Aa").on_hover_text("Match case");
            ui.separator();
            ui.add(TextEdit::singleline(&mut h.editor.replace_text).desired_width(200.0).hint_text("Replace"));
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
    if ui.input_mut(|i| consume_plain_key(i, Key::Escape)) {
        close = true;
    }
    if do_replace {
        if let Some((a, b)) = h.editor.selection {
            let ab = char_to_byte(&h.text, a);
            let bb = char_to_byte(&h.text, b);
            let sel = &h.text[ab..bb];
            let matches = if h.editor.find_case { sel == h.editor.find_text } else { sel.eq_ignore_ascii_case(&h.editor.find_text) };
            if matches {
                let rep = h.editor.replace_text.clone();
                let after = byte_to_char(&h.text, ab) + rep.chars().count();
                h.editor.pending_edit = Some(PendingEdit::Replace { start: ab, end: bb, text: rep, cursor_after: Some(after) });
                h.editor.cursor = after;
            }
        }
        do_find = true;
    }
    if do_replace_all && !h.editor.find_text.is_empty() {
        let needle = h.editor.find_text.clone();
        let rep = h.editor.replace_text.clone();
        let new_text = if h.editor.find_case {
            h.text.replace(&needle, &rep)
        } else {
            replace_case_insensitive(&h.text, &needle, &rep)
        };
        let cursor = h.editor.cursor.min(new_text.chars().count());
        h.editor.pending_edit = Some(PendingEdit::SetText { text: new_text, cursor });
    }
    if do_find && !h.editor.find_text.is_empty() {
        find_next(h);
    }
    if close {
        h.editor.find_open = false;
        h.editor.request_focus = true;
    }
}

pub fn find_next(h: &mut EditorHost<'_>) {
    let needle = h.editor.find_text.clone();
    if needle.is_empty() {
        return;
    }
    let text: &str = h.text;
    let start_char = h.editor.selection.map(|(_, b)| b).unwrap_or(h.editor.cursor);
    let start = char_to_byte(text, start_char);
    let (hay, nd) = if h.editor.find_case { (text.to_string(), needle.clone()) } else { (text.to_lowercase(), needle.to_lowercase()) };
    // lowercase can change byte lengths for non-ASCII; fall back to char-wise search in that case
    let found = if hay.len() == text.len() {
        hay[start..].find(&nd).map(|i| start + i).or_else(|| hay[..start].find(&nd))
    } else {
        text.to_lowercase().find(&nd)
    };
    if let Some(b) = found {
        let a = byte_to_char(text, b);
        let e = a + needle.chars().count();
        h.editor.pending_edit = Some(PendingEdit::Select(a, e));
        h.editor.selection = Some((a, e));
        h.editor.cursor = e;
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

fn goto_bar(ui: &mut Ui, h: &mut EditorHost<'_>, theme: &Theme) {
    let mut go = false;
    let mut close = false;
    egui::Frame::new().fill(theme.bg_sidebar).inner_margin(egui::Margin::symmetric(8, 4)).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label("Go to line:");
            let r = ui.add(TextEdit::singleline(&mut h.editor.goto_line_text).desired_width(80.0).id(h.id.with("goto")));
            // Enter makes the field surrender focus; check before re-requesting it, or the bar
            // never closes on Enter
            if (r.has_focus() || r.lost_focus()) && ui.input_mut(|i| consume_plain_key(i, Key::Enter)) {
                go = true;
            }
            if !go {
                r.request_focus();
            }
            if ui.small_button("Go").clicked() {
                go = true;
            }
            if ui.input_mut(|i| consume_plain_key(i, Key::Escape)) {
                close = true;
            }
        });
    });
    if go {
        if let Ok(n) = h.editor.goto_line_text.trim().parse::<usize>() {
            let n = n.max(1);
            let byte = h.text.split_inclusive('\n').take(n - 1).map(|l| l.len()).sum::<usize>();
            let c = byte_to_char(&h.text, byte);
            h.editor.pending_edit = Some(PendingEdit::SetCursor(c));
        }
        close = true;
    }
    if close {
        h.editor.goto_line_open = false;
        h.editor.request_focus = true;
    }
}

/// Toggle `--` on every line touched by each selection (or each cursor's line). One undo step.
pub fn toggle_line_comment(h: &mut EditorHost<'_>) {
    use self::core::{apply_replacements, line_end, line_start, Replace};
    let chars: Vec<char> = h.text.chars().collect();
    // one block of whole lines per selection, merged when they touch
    let mut blocks: Vec<(usize, usize)> = h
        .editor
        .cursors
        .sels
        .iter()
        .map(|s| {
            let end = if !s.is_empty() && s.max() > 0 && chars.get(s.max() - 1) == Some(&'\n') { s.max() - 1 } else { s.max() };
            (line_start(&chars, s.min()), line_end(&chars, end))
        })
        .collect();
    blocks.sort();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for b in blocks {
        match merged.last_mut() {
            Some(l) if b.0 <= l.1 => l.1 = l.1.max(b.1),
            _ => merged.push(b),
        }
    }
    let marker = if h.syntax == Syntax::Python { "#" } else { "--" };
    let all_commented = merged.iter().flat_map(|&(a, b)| chars[a..b].iter().collect::<String>().lines().map(str::to_string).collect::<Vec<_>>()).filter(|l| !l.trim().is_empty()).all(|l| l.trim_start().starts_with(marker));
    let reps: Vec<Replace> = merged
        .iter()
        .map(|&(a, b)| {
            let block: String = chars[a..b].iter().collect();
            let text: String = block
                .split_inclusive('\n')
                .map(|l| {
                    let (body, nl) = match l.strip_suffix('\n') {
                        Some(b) => (b, "\n"),
                        None => (l, ""),
                    };
                    if all_commented {
                        let trimmed = body.trim_start();
                        if let Some(rest) = trimmed.strip_prefix(marker) {
                            let indent = &body[..body.len() - trimmed.len()];
                            format!("{indent}{}{nl}", rest.strip_prefix(' ').unwrap_or(rest))
                        } else {
                            format!("{body}{nl}")
                        }
                    } else if body.trim().is_empty() {
                        format!("{body}{nl}")
                    } else {
                        format!("{marker} {body}{nl}")
                    }
                })
                .collect();
            Replace { start: a, end: b, text }
        })
        .collect();
    h.editor.undo.record(EditKind::Other, &h.text, &h.editor.cursors);
    let ends = apply_replacements(&mut *h.text, &reps);
    let sels: Vec<Sel> = reps.iter().zip(ends).map(|(r, e)| Sel::range(e - r.text.chars().count(), e)).collect();
    let primary = sels.len() - 1;
    h.editor.cursors = core::Cursors { sels, primary };
    h.editor.cursors.normalize();
    h.editor.request_focus = true;
    h.editor.scroll_to_cursor = true;
}

/// Wrap every non-empty selection in `/* … */` (or unwrap it). One undo step.
pub fn toggle_block_comment(h: &mut EditorHost<'_>) {
    use self::core::{apply_replacements, Replace};
    if !h.editor.cursors.has_selection() {
        return;
    }
    let chars: Vec<char> = h.text.chars().collect();
    let reps: Vec<Replace> = h
        .editor
        .cursors
        .sels
        .iter()
        .filter(|s| !s.is_empty())
        .map(|s| {
            let sel: String = chars[s.min()..s.max()].iter().collect();
            let text = if sel.trim_start().starts_with("/*") && sel.trim_end().ends_with("*/") {
                let t = sel.trim();
                t[2..t.len() - 2].trim().to_string()
            } else {
                format!("/* {sel} */")
            };
            Replace { start: s.min(), end: s.max(), text }
        })
        .collect();
    h.editor.undo.record(EditKind::Other, &h.text, &h.editor.cursors);
    let ends = apply_replacements(&mut *h.text, &reps);
    let sels: Vec<Sel> = reps.iter().zip(ends).map(|(r, e)| Sel::range(e - r.text.chars().count(), e)).collect();
    let primary = sels.len() - 1;
    h.editor.cursors = core::Cursors { sels, primary };
    h.editor.cursors.normalize();
    h.editor.request_focus = true;
    h.editor.scroll_to_cursor = true;
}

/// The text to run for "Run selection": every non-empty selection, in document order, joined
/// with newlines, plus the editor line the first one starts on. None when nothing is selected.
pub fn selected_script(tab: &EditorTab) -> Option<(String, u32)> {
    let sels: Vec<&Sel> = tab.editor.cursors.sels.iter().filter(|s| !s.is_empty()).collect();
    if sels.is_empty() {
        return None;
    }
    let text = &tab.text;
    let parts: Vec<String> = sels.iter().map(|s| text[char_to_byte(text, s.min())..char_to_byte(text, s.max())].to_string()).collect();
    let first = char_to_byte(text, sels[0].min());
    Some((parts.join("\n"), cobalt_sql::statements::line_of(text, first)))
}

/// Replace the whole text keeping the cursor line.
pub fn set_text_keep_line(h: &mut EditorHost<'_>, new_text: String) {
    let line = h.editor.line.max(1);
    let byte = new_text.split_inclusive('\n').take(line - 1).map(|l| l.len()).sum::<usize>();
    let c = byte_to_char(&new_text, byte);
    h.editor.pending_edit = Some(PendingEdit::SetText { text: new_text, cursor: c });
}

#[cfg(test)]
mod syntax_tests {
    use super::*;

    fn colors() -> TokenColors {
        crate::ui::theme::Theme::for_choice(cobalt_core::ThemeChoice::Light, false).tokens.clone()
    }

    /// The colour of the section that contains byte `at`.
    fn color_at(job: &LayoutJob, at: usize) -> Color32 {
        job.sections.iter().find(|s| s.byte_range.start.0 <= at && at < s.byte_range.end.0).map(|s| s.format.color).expect("section")
    }

    #[test]
    fn python_comment_with_apostrophe_does_not_open_a_string() {
        let text = r#"# One display value (PO 2026-10-01): F&O receives a value's registry LABEL
rollup = spark.sql(f"""SELECT 1""")
x = 'done'"#;
        let c = colors();
        let job = python_layout_job(text, &c, FontId::monospace(12.0), f32::INFINITY);
        assert_eq!(color_at(&job, 10), c.comment);
        let rollup = text.find("rollup").unwrap();
        assert_eq!(color_at(&job, rollup), c.identifier);
        let sel = text.find("SELECT").unwrap();
        assert_eq!(color_at(&job, sel), c.string);
        let done = text.find("'done'").unwrap();
        assert_eq!(color_at(&job, done + 1), c.string);
        assert_eq!(color_at(&job, text.find("spark").unwrap()), c.function);
    }

    #[test]
    fn plain_syntax_has_one_section() {
        let job = layout_job_for(Syntax::Plain, "# not a comment 'x", &colors(), FontId::monospace(12.0), f32::INFINITY);
        assert_eq!(job.sections.len(), 1);
    }
}
