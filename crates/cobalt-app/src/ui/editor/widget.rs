//! The code editor widget: a syntax-coloured galley with a multi-cursor selection model
//! (see `core.rs` and `docs/design/editor_multicursor.md`). Replaces egui's `TextEdit`, which
//! owns a single cursor and decides double-clicks on release.
//!
//! Per frame: handle keyboard events (when focused) → layout → allocate → mouse → paint.
//! Positions are char indices; the galley converts them to pixels and back.

use super::core::*;
use crate::ui::theme::TokenColors;
use egui::text::CCursor;
use egui::text::CCursorRange;
use egui::text_selection::visuals::paint_text_selection;
use egui::{Color32, Event, EventFilter, FontId, Id, ImeEvent, Key, Modifiers, Pos2, Rect, Response, Sense, Stroke, Ui, Vec2, WidgetInfo, WidgetType};
use egui::epaint::text::cursor::LayoutCursor;
use egui::epaint::text::CharIndex;
use egui::Galley;
use std::sync::Arc;

/// Presses closer than this in time and space count as the same click sequence.
const MULTI_PRESS_SECS: f64 = 0.4;
const MULTI_PRESS_DIST: f32 = 6.0;

#[derive(Clone, Copy, Debug, PartialEq)]
enum DragMode {
    Char,
    Word,
    Line,
    Column,
}

#[derive(Clone, Copy, Debug)]
struct Drag {
    mode: DragMode,
    /// The selection made on press (a point, a word or a line); drags extend from it.
    anchor: Sel,
    /// Galley-relative press position (column mode).
    anchor_pos: Pos2,
}

/// Transient widget state kept in egui memory between frames.
#[derive(Clone, Default)]
struct Mem {
    last_press: Option<(f64, Pos2)>,
    press_count: u8,
    drag: Option<Drag>,
    last_interaction: f64,
    /// Match mode adopted by the first Ctrl+D from an empty cursor (whole word, match case).
    ctrl_d_mode: Option<MatchMode>,
    /// Keyboard column selection in progress: (anchor, head) in galley coordinates.
    col_box: Option<(Pos2, Pos2)>,
    /// Set by a key handler; surfaced once as a toast.
    notice: Option<&'static str>,
}

/// The clipboard text of the last single-cursor whole-line copy (app-global, like VS Code's
/// in-memory clipboard metadata).
#[derive(Clone, Default)]
struct LineClipboard(Option<String>);

pub struct CodeEditor<'a> {
    pub id: Id,
    pub text: &'a mut String,
    pub cursors: &'a mut Cursors,
    pub undo: &'a mut UndoStack,
    /// Snippet placeholders Tab walks through (ended by Escape, a click, or the last stop).
    pub snippet: &'a mut Option<SnippetSession>,
    pub font: FontId,
    pub colors: &'a TokenColors,
    pub text_color: Color32,
    pub cursor_color: Color32,
    pub word_wrap: bool,
    pub tab_size: usize,
    pub insert_spaces: bool,
    /// Fill at least this much of the parent (the scroll viewport).
    pub min_size: Vec2,
    pub margin: egui::Margin,
    /// Scroll so the primary caret is visible this frame (after a programmatic move).
    pub scroll_to_cursor: bool,
    /// Ctrl+D / Ctrl+Shift+L matching when seeded from a selection (the find bar's toggles).
    pub find_mode: MatchMode,
    /// Rows a PageUp/PageDown moves (the viewport height in rows).
    pub page_rows: usize,
}

pub struct CodeEditorOutput {
    pub response: Response,
    pub galley: Arc<Galley>,
    pub galley_pos: Pos2,
    pub row_height: f32,
    pub changed: bool,
    /// Something to tell the user ("No more matches").
    pub notice: Option<&'static str>,
    /// Text was typed (Text / IME commit), as opposed to pasted, indented or undone.
    pub typed: bool,
    pub cursor_moved: bool,
    pub focused: bool,
}

fn ci(n: usize) -> CCursor {
    CCursor::new(n)
}

/// Row rectangle in galley coordinates with a real height even for the empty rows egui lays
/// out with zero height (empty text, the row after a trailing newline).
pub fn row_rect(galley: &Galley, row: usize, row_h: f32) -> Rect {
    match galley.rows.get(row) {
        Some(r) => {
            let rect = r.rect();
            Rect::from_min_size(rect.min, Vec2::new(rect.width(), rect.height().max(row_h)))
        }
        None => Rect::from_min_size(Pos2::ZERO, Vec2::new(0.0, row_h)),
    }
}

/// Caret rectangle (1 px wide, a full row tall) in galley coordinates.
pub fn caret_rect(galley: &Galley, at: usize, row_h: f32) -> Rect {
    if galley.rows.is_empty() {
        return Rect::from_min_size(Pos2::ZERO, Vec2::new(1.0, row_h));
    }
    let lc = galley.layout_from_cursor(ci(at));
    let rr = row_rect(galley, lc.row, row_h);
    let x = galley.rows.get(lc.row).map(|r| r.pos.x + r.row.x_offset(lc.column)).unwrap_or(rr.left());
    Rect::from_min_size(Pos2::new(x, rr.top()), Vec2::new(1.0, row_h))
}

/// Char index of the first char of `row`.
fn row_start_index(galley: &Galley, row: usize) -> usize {
    galley.rows.iter().take(row).map(|r| r.char_count_including_newline().0).sum()
}

fn first_nonblank_in_row(galley: &Galley, text_chars: &[char], row: usize) -> usize {
    let start = row_start_index(galley, row);
    let n = galley.rows.get(row).map(|r| r.row.char_count_excluding_newline().0).unwrap_or(0);
    let end = (start + n).min(text_chars.len());
    text_chars[start..end].iter().position(|c| !c.is_whitespace()).map(|p| start + p).unwrap_or(end)
}

impl<'a> CodeEditor<'a> {
    pub fn show(self, ui: &mut Ui) -> CodeEditorOutput {
        let CodeEditor { id, text, cursors, undo, snippet, font, colors, text_color, cursor_color, word_wrap, tab_size, insert_spaces, min_size, margin, scroll_to_cursor, find_mode, page_rows } = self;
        let mut mem: Mem = ui.data_mut(|d| d.get_temp(id).unwrap_or_default());
        let row_h = ui.fonts_mut(|f| f.row_height(&font));
        let char_w = ui.fonts_mut(|f| f.glyph_width(&font, '0')).max(1.0);
        let now = ui.input(|i| i.time);
        mem.notice = None;
        let filter = EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true };

        let margin_w = (margin.left + margin.right) as f32;
        let margin_h = (margin.top + margin.bottom) as f32;
        let wrap_width = if word_wrap { (ui.available_width() - margin_w).max(40.0) } else { f32::INFINITY };
        let layout = |ui: &Ui, text: &str| -> Arc<Galley> {
            let job = super::layout_job(text, colors, font.clone(), wrap_width);
            ui.fonts_mut(|f| f.layout_job(job))
        };
        let mut galley = layout(ui, text);

        // ---- keyboard ----------------------------------------------------------------------
        let has_focus = ui.memory(|m| m.has_focus(id));
        let mut changed = false;
        let mut typed = false;
        let mut cursor_moved = false;
        if has_focus {
            ui.memory_mut(|m| m.set_focus_lock_filter(id, filter));
            let events = ui.input(|i| i.filtered_events(&filter));
            for ev in &events {
                let mut relayout = false;
                let before_len = text.chars().count();
                let edit_pos = cursors.primary().min();
                match ev {
                    Event::Copy => {
                        let (s, line) = copy_text(text, cursors);
                        ui.ctx().copy_text(s.clone());
                        ui.data_mut(|d| d.insert_temp(Id::new("cobalt-line-clipboard"), LineClipboard(line.then_some(s))));
                    }
                    Event::Cut => {
                        let (s, line) = copy_text(text, cursors);
                        ui.ctx().copy_text(s.clone());
                        ui.data_mut(|d| d.insert_temp(Id::new("cobalt-line-clipboard"), LineClipboard(line.then_some(s))));
                        undo.record(EditKind::Other, text, cursors);
                        if cursors.has_selection() {
                            delete_at_cursors(text, cursors, false, false);
                        } else {
                            delete_lines(text, cursors);
                        }
                        relayout = true;
                    }
                    Event::Paste(s) if !s.is_empty() => {
                        let line = ui.data(|d| d.get_temp::<LineClipboard>(Id::new("cobalt-line-clipboard"))).and_then(|l| l.0).map(|l| l == *s).unwrap_or(false);
                        undo.record(EditKind::Other, text, cursors);
                        paste_at_cursors(text, cursors, s, line);
                        relayout = true;
                    }
                    Event::Text(s) if !s.is_empty() && s != "\n" && s != "\r" => {
                        undo.record(EditKind::Typing, text, cursors);
                        insert_at_cursors(text, cursors, s);
                        relayout = true;
                        typed = true;
                    }
                    Event::Ime(ImeEvent::Commit(s)) if !s.is_empty() => {
                        undo.record(EditKind::Typing, text, cursors);
                        insert_at_cursors(text, cursors, s);
                        relayout = true;
                        typed = true;
                    }
                    Event::Key { key, pressed: true, modifiers, .. } => {
                        let r = handle_key(*key, modifiers, text, cursors, undo, snippet, &galley, &mut mem, tab_size, insert_spaces, find_mode, page_rows.max(1), row_h, char_w);
                        match r {
                            KeyOutcome::Ignored => {}
                            KeyOutcome::Moved => cursor_moved = true,
                            KeyOutcome::Edited => relayout = true,
                        }
                    }
                    _ => {}
                }
                if relayout {
                    changed = true;
                    cursor_moved = true;
                    let after_len = text.chars().count();
                    cursors.clamp(after_len);
                    if let Some(s) = snippet.as_mut() {
                        if cursors.is_multi() {
                            *snippet = None;
                        } else {
                            s.adjust(edit_pos, after_len as isize - before_len as isize);
                        }
                    }
                    galley = layout(ui, text);
                }
            }
            if changed || cursor_moved {
                mem.last_interaction = now;
            }
        }

        // ---- allocate ----------------------------------------------------------------------
        let galley_size = galley.size();
        let w = if word_wrap { min_size.x.max(galley_size.x + margin_w) } else { (galley_size.x + margin_w + 40.0).max(min_size.x) };
        let h = (galley_size.y.max(row_h) + margin_h).max(min_size.y);
        let (rect, _) = ui.allocate_exact_size(Vec2::new(w, h), Sense::hover());
        // interact under our own id so egui keeps keyboard focus on it across frames
        let mut response = ui.interact(rect, id, Sense::click_and_drag() | Sense::FOCUSABLE);
        response.widget_info(|| WidgetInfo::labeled(WidgetType::TextEdit, true, "editor"));
        let origin = rect.min + Vec2::new(margin.left as f32, margin.top as f32);

        // ---- mouse -------------------------------------------------------------------------
        if response.hovered() {
            ui.set_cursor_icon(egui::CursorIcon::Text);
        }
        if let Some(pos) = response.interact_pointer_pos() {
            let gp = pos - origin; // galley-relative
            let at = galley.cursor_from_pos(gp).index.0;
            let (pressed, released, mods) = ui.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_released(), i.modifiers));
            if pressed && response.contains_pointer() {
                mem.col_box = None;
                *snippet = None;
                let same = mem.last_press.map(|(t, p)| now - t < MULTI_PRESS_SECS && p.distance(pos) < MULTI_PRESS_DIST).unwrap_or(false);
                mem.press_count = if same { (mem.press_count + 1).min(4) } else { 1 };
                mem.last_press = Some((now, pos));
                ui.memory_mut(|m| m.request_focus(id));
                undo.break_merge();
                let chars: Vec<char> = text.chars().collect();
                let alt = mods.alt && !mods.shift;
                let shift_alt = mods.alt && mods.shift;
                if shift_alt {
                    // column select from the primary's anchor to here
                    let anchor_pos = galley_pos_of(&galley, cursors.primary().anchor, row_h).to_pos2();
                    mem.drag = Some(Drag { mode: DragMode::Column, anchor: Sel::cursor(cursors.primary().anchor), anchor_pos });
                    column_select(&galley, cursors, mem.drag.unwrap().anchor_pos, gp.to_pos2(), row_h);
                } else if alt && mem.press_count == 1 && cursors.remove_at(at) {
                    mem.drag = None;
                } else {
                    let sel = match mem.press_count {
                        1 => Sel::cursor(at),
                        2 => {
                            let (a, b) = word_at(&chars, at);
                            Sel::range(a, b)
                        }
                        3 => {
                            let (a, b) = line_at(&chars, at);
                            Sel::range(a, b)
                        }
                        _ => Sel::range(0, chars.len()),
                    };
                    let mode = match mem.press_count {
                        2 => DragMode::Word,
                        3 => DragMode::Line,
                        _ => DragMode::Char,
                    };
                    if alt {
                        cursors.add(sel);
                    } else if mods.shift && mem.press_count == 1 {
                        let p = cursors.primary_mut();
                        p.head = at;
                        p.h_pos = None;
                        cursors.normalize();
                    } else {
                        cursors.set_single(sel);
                    }
                    let anchor = if mods.shift && mem.press_count == 1 { Sel::cursor(cursors.primary().anchor) } else { sel };
                    mem.drag = Some(Drag { mode, anchor, anchor_pos: gp.to_pos2() });
                }
                cursor_moved = true;
                mem.last_interaction = now;
            } else if response.dragged() {
                if let Some(drag) = mem.drag {
                    let chars: Vec<char> = text.chars().collect();
                    match drag.mode {
                        DragMode::Char => {
                            let p = cursors.primary_mut();
                            p.anchor = drag.anchor.anchor;
                            p.head = at;
                            p.h_pos = None;
                        }
                        DragMode::Word => {
                            let (wa, wb) = (drag.anchor.min(), drag.anchor.max());
                            let (pa, pb) = word_at(&chars, at);
                            let p = cursors.primary_mut();
                            *p = if at >= wa { Sel::range(wa, pb.max(wb)) } else { Sel::range(wb, pa.min(wa)) };
                        }
                        DragMode::Line => {
                            let (la, lb) = (drag.anchor.min(), drag.anchor.max());
                            let (pa, pb) = line_at(&chars, at);
                            let p = cursors.primary_mut();
                            *p = if at >= la { Sel::range(la, pb.max(lb)) } else { Sel::range(lb, pa.min(la)) };
                        }
                        DragMode::Column => {
                            column_select(&galley, cursors, drag.anchor_pos, gp.to_pos2(), row_h);
                        }
                    }
                    cursors.normalize();
                    cursor_moved = true;
                    mem.last_interaction = now;
                }
            }
            if released {
                mem.drag = None;
            }
        }
        if !ui.input(|i| i.pointer.primary_down()) {
            mem.drag = None;
        }

        // ---- paint -------------------------------------------------------------------------
        let has_focus = ui.memory(|m| m.has_focus(id));
        let painter = ui.painter_at(rect.expand(2.0));
        let mut painted = galley.clone();
        for s in &cursors.sels {
            if !s.is_empty() {
                paint_text_selection(&mut painted, ui.visuals(), &CCursorRange::two(ci(s.min()), ci(s.max())), None);
            }
        }
        painter.galley(origin, painted, text_color);

        let primary_caret = caret_rect(&galley, cursors.primary().head, row_h).translate(origin.to_vec2());
        if has_focus && (ui.input(|i| i.focused) || cfg!(feature = "agent")) {
            let v = ui.visuals();
            let (visible, wake) = if v.text_cursor.blink {
                let on = v.text_cursor.on_duration;
                let off = v.text_cursor.off_duration;
                let total = on + off;
                let t = ((now - mem.last_interaction) % total as f64) as f32;
                if t < on {
                    (true, on - t)
                } else {
                    (false, total - t)
                }
            } else {
                (true, f32::INFINITY)
            };
            if visible {
                for s in &cursors.sels {
                    let r = caret_rect(&galley, s.head, row_h).translate(origin.to_vec2());
                    painter.line_segment([r.center_top(), r.center_bottom()], Stroke::new(2.0, cursor_color));
                }
            }
            if wake.is_finite() {
                ui.ctx().request_repaint_after_secs(wake);
            }
            // IME window next to the primary caret
            let to_global = ui.ctx().layer_transform_to_global(ui.layer_id()).unwrap_or_default();
            ui.output_mut(|o| {
                o.ime = Some(egui::output::IMEOutput { purpose: egui::IMEPurpose::Normal, rect: to_global * rect, cursor_rect: to_global * primary_caret, should_interrupt_composition: false });
            });
        }

        if (cursor_moved || scroll_to_cursor) && has_focus {
            ui.scroll_to_rect(primary_caret.expand2(Vec2::new(24.0, row_h)), None);
        }

        if changed {
            response.mark_changed();
        }
        let notice = mem.notice.take();
        ui.data_mut(|d| d.insert_temp(id, mem));
        CodeEditorOutput { response, galley, galley_pos: origin, row_height: row_h, changed, notice, typed, cursor_moved, focused: has_focus }
    }
}

fn galley_pos_of(galley: &Galley, at: usize, row_h: f32) -> Vec2 {
    let r = caret_rect(galley, at, row_h);
    Vec2::new(r.left(), r.center().y)
}

/// Column (box) selection between two galley-relative points, VS Code rules: one selection per
/// row between the two; rows whose text does not reach the box's left edge are skipped; if every
/// row is skipped, an empty cursor at each row's end.
fn column_select(galley: &Galley, cursors: &mut Cursors, a: Pos2, b: Pos2, row_h: f32) {
    if galley.rows.is_empty() {
        return;
    }
    let row_of = |y: f32| -> usize {
        let mut best = 0usize;
        let mut best_d = f32::INFINITY;
        for (i, _) in galley.rows.iter().enumerate() {
            let r = row_rect(galley, i, row_h);
            let d = if r.top() <= y && y <= r.bottom() { 0.0 } else { (r.top() - y).abs().min((r.bottom() - y).abs()) };
            if d < best_d {
                best_d = d;
                best = i;
            }
        }
        best
    };
    let (r0, r1) = (row_of(a.y), row_of(b.y));
    let (lo, hi) = (r0.min(r1), r0.max(r1));
    let (x_left, x_right) = (a.x.min(b.x), a.x.max(b.x));
    let forward = b.x >= a.x;
    let mut sels: Vec<Sel> = Vec::new();
    let mut start = row_start_index(galley, lo);
    for ri in lo..=hi {
        let row = &galley.rows[ri];
        let n_excl = row.row.char_count_excluding_newline();
        let width = row.pos.x + row.row.x_offset(n_excl);
        if width >= x_left {
            let ca = row.row.char_at(x_left - row.pos.x).0.min(n_excl.0);
            let cb = row.row.char_at(x_right - row.pos.x).0.min(n_excl.0);
            let (anchor, head) = if forward { (start + ca, start + cb) } else { (start + cb, start + ca) };
            sels.push(Sel::range(anchor, head));
        }
        start += row.char_count_including_newline().0;
    }
    if sels.is_empty() {
        let mut start = row_start_index(galley, lo);
        for ri in lo..=hi {
            let row = &galley.rows[ri];
            sels.push(Sel::cursor(start + row.row.char_count_excluding_newline().0));
            start += row.char_count_including_newline().0;
        }
    }
    let primary = if r1 >= r0 { sels.len() - 1 } else { 0 };
    *cursors = Cursors { sels, primary };
    cursors.normalize();
}

enum KeyOutcome {
    Ignored,
    Moved,
    Edited,
}

#[allow(clippy::too_many_arguments)]
fn handle_key(key: Key, m: &Modifiers, text: &mut String, cursors: &mut Cursors, undo: &mut UndoStack, snippet: &mut Option<SnippetSession>, galley: &Galley, mem: &mut Mem, tab_size: usize, insert_spaces: bool, find_mode: MatchMode, page_rows: usize, row_h: f32, char_w: f32) -> KeyOutcome {
    use KeyOutcome::*;
    let ctrl = m.ctrl || m.command;
    let alt = m.alt;
    let shift = m.shift;
    let len = text.chars().count();
    // inside a snippet: Tab / Shift+Tab walk the placeholders, Escape leaves
    if let Some(s) = snippet.as_mut() {
        if key == Key::Tab && !ctrl && !alt {
            return match s.step(shift) {
                Some((a, b)) => {
                    cursors.set_single(Sel::range(a.min(len), b.min(len)));
                    undo.break_merge();
                    Moved
                }
                None => {
                    *snippet = None;
                    if shift {
                        Ignored
                    } else {
                        let end = s_end(text, cursors);
                        cursors.set_single(Sel::cursor(end));
                        Moved
                    }
                }
            };
        }
        if key == Key::Escape {
            *snippet = None;
            return Moved;
        }
    }
    // keyboard column select: Ctrl+Shift+Alt+arrows grow a box from the primary's anchor
    if ctrl && shift && alt && matches!(key, Key::ArrowUp | Key::ArrowDown | Key::ArrowLeft | Key::ArrowRight) {
        let p = cursors.primary();
        let (anchor, mut head) = mem.col_box.unwrap_or_else(|| {
            let a = caret_rect(galley, p.anchor, row_h);
            let h = caret_rect(galley, p.head, row_h);
            (Pos2::new(a.left(), a.center().y), Pos2::new(h.left(), h.center().y))
        });
        match key {
            Key::ArrowUp => head.y -= row_h,
            Key::ArrowDown => head.y += row_h,
            Key::ArrowLeft => head.x = (head.x - char_w).max(0.0),
            _ => head.x += char_w,
        }
        let max_y = galley.rows.len().saturating_sub(1) as f32 * row_h + row_h / 2.0;
        head.y = head.y.clamp(row_h / 2.0, max_y.max(row_h / 2.0));
        mem.col_box = Some((anchor, head));
        column_select(galley, cursors, anchor, head, row_h);
        undo.break_merge();
        return Moved;
    }
    mem.col_box = None;

    // helper: move every head with a function of (head, h_pos) → (head, h_pos)
    let move_all = |cursors: &mut Cursors, extend: bool, f: &dyn Fn(usize, Option<f32>) -> (usize, Option<f32>)| {
        for s in &mut cursors.sels {
            let (h, hp) = f(s.head, s.h_pos);
            s.move_head(h, extend);
            s.h_pos = hp;
        }
        cursors.normalize();
    };

    match key {
        Key::Escape => {
            if cursors.is_multi() {
                cursors.collapse_to_primary();
                Moved
            } else if cursors.has_selection() {
                let h = cursors.primary().head;
                cursors.primary_mut().collapse_to(h);
                Moved
            } else {
                Ignored
            }
        }
        Key::ArrowUp | Key::ArrowDown if ctrl && alt => {
            // add a cursor above / below every cursor (translate anchor and head separately)
            let up = key == Key::ArrowUp;
            let mut added = Vec::new();
            for s in &cursors.sels {
                let mv = |i: usize, hp: Option<f32>| if up { galley.cursor_up_one_row(&ci(i), hp) } else { galley.cursor_down_one_row(&ci(i), hp) };
                let (h, hp) = mv(s.head, s.h_pos);
                if h.index.0 == s.head {
                    continue; // first / last line
                }
                let a = if s.is_empty() { h.index.0 } else { mv(s.anchor, None).0.index.0 };
                added.push(Sel { anchor: a, head: h.index.0, h_pos: hp });
            }
            if added.is_empty() {
                return Ignored;
            }
            for s in added {
                cursors.add(s);
            }
            undo.break_merge();
            Moved
        }
        Key::ArrowUp | Key::ArrowDown if alt && !ctrl => {
            undo.record(EditKind::Other, text, cursors);
            if shift {
                duplicate_lines(text, cursors, key == Key::ArrowDown);
            } else {
                move_lines(text, cursors, key == Key::ArrowDown);
            }
            Edited
        }
        Key::ArrowUp | Key::ArrowDown => {
            if ctrl {
                let target = if key == Key::ArrowUp { 0 } else { len };
                move_all(cursors, shift, &|_, _| (target, None));
            } else {
                let up = key == Key::ArrowUp;
                move_all(cursors, shift, &|h, hp| {
                    let (c, hp2) = if up { galley.cursor_up_one_row(&ci(h), hp) } else { galley.cursor_down_one_row(&ci(h), hp) };
                    (c.index.0, hp2)
                });
            }
            undo.break_merge();
            Moved
        }
        Key::PageUp | Key::PageDown => {
            let up = key == Key::PageUp;
            move_all(cursors, shift, &|h, hp| {
                let mut c = ci(h);
                let mut hp = hp;
                for _ in 0..page_rows {
                    let (nc, nhp) = if up { galley.cursor_up_one_row(&c, hp) } else { galley.cursor_down_one_row(&c, hp) };
                    c = nc;
                    hp = nhp;
                }
                (c.index.0, hp)
            });
            undo.break_merge();
            Moved
        }
        Key::ArrowLeft | Key::ArrowRight => {
            let left = key == Key::ArrowLeft;
            if !shift && !ctrl && cursors.has_selection() {
                for s in &mut cursors.sels {
                    if !s.is_empty() {
                        let t = if left { s.min() } else { s.max() };
                        s.collapse_to(t);
                    } else {
                        let t = if left { s.head.saturating_sub(1) } else { (s.head + 1).min(len) };
                        s.collapse_to(t);
                    }
                }
                cursors.normalize();
            } else if ctrl {
                let chars: Vec<char> = text.chars().collect();
                move_all(cursors, shift, &|h, _| (if left { prev_word_start(&chars, h) } else { next_word_end(&chars, h) }, None));
            } else {
                move_all(cursors, shift, &|h, _| (if left { h.saturating_sub(1) } else { (h + 1).min(len) }, None));
            }
            undo.break_merge();
            Moved
        }
        Key::Home => {
            if ctrl {
                move_all(cursors, shift, &|_, _| (0, None));
            } else {
                let chars: Vec<char> = text.chars().collect();
                move_all(cursors, shift, &|h, _| {
                    let lc: LayoutCursor = galley.layout_from_cursor(ci(h));
                    let rs = row_start_index(galley, lc.row);
                    let fnb = first_nonblank_in_row(galley, &chars, lc.row);
                    (if h == fnb { rs } else { fnb }, None)
                });
            }
            undo.break_merge();
            Moved
        }
        Key::End => {
            if ctrl {
                move_all(cursors, shift, &|_, _| (len, None));
            } else {
                move_all(cursors, shift, &|h, _| (galley.cursor_end_of_row(&ci(h)).index.0, None));
            }
            undo.break_merge();
            Moved
        }
        Key::Backspace => {
            undo.record(EditKind::Deleting, text, cursors);
            delete_at_cursors(text, cursors, false, ctrl);
            Edited
        }
        Key::Delete => {
            undo.record(EditKind::Deleting, text, cursors);
            delete_at_cursors(text, cursors, true, ctrl);
            Edited
        }
        Key::Enter if !ctrl => {
            undo.record(EditKind::Other, text, cursors);
            newline_at_cursors(text, cursors);
            Edited
        }
        Key::Tab => {
            undo.record(EditKind::Other, text, cursors);
            let indent: String = if insert_spaces { " ".repeat(tab_size.max(1)) } else { "\t".into() };
            let chars: Vec<char> = text.chars().collect();
            let multi_line = cursors.sels.iter().any(|s| !s.is_empty() && (chars[s.min()..s.max()].contains(&'\n') || (line_start(&chars, s.min()) == s.min() && line_end(&chars, s.max()) == s.max())));
            if shift {
                indent_lines(text, cursors, &indent, tab_size.max(1), true);
            } else if multi_line {
                indent_lines(text, cursors, &indent, tab_size.max(1), false);
            } else if insert_spaces {
                let ts = tab_size.max(1);
                let texts: Vec<String> = cursors.sels.iter().map(|s| " ".repeat(ts - (s.min() - line_start(&chars, s.min())) % ts)).collect();
                insert_per_cursor(text, cursors, &texts);
            } else {
                insert_at_cursors(text, cursors, "\t");
            }
            Edited
        }
        Key::A if ctrl && !shift && !alt => {
            cursors.set_single(Sel::range(0, len));
            Moved
        }
        Key::D if ctrl && !shift && !alt => {
            let mode = mem.ctrl_d_mode.unwrap_or(find_mode);
            let had_selection = cursors.has_selection();
            match add_next_occurrence(text, cursors, mode) {
                Some(m) => {
                    if !had_selection {
                        mem.ctrl_d_mode = Some(m);
                    }
                    undo.break_merge();
                    Moved
                }
                None => {
                    mem.notice = Some("No more matches");
                    Ignored
                }
            }
        }
        Key::L if ctrl && shift && !alt => {
            select_all_occurrences(text, cursors, mem.ctrl_d_mode.unwrap_or(find_mode));
            undo.break_merge();
            Moved
        }
        Key::I if shift && alt && !ctrl => {
            cursors_at_line_ends(text, cursors);
            undo.break_merge();
            Moved
        }
        Key::K if ctrl && shift && !alt => {
            undo.record(EditKind::Other, text, cursors);
            delete_lines(text, cursors);
            Edited
        }
        Key::Z if ctrl && !shift => {
            if undo.undo(text, cursors) {
                Edited
            } else {
                Ignored
            }
        }
        Key::Y if ctrl && !shift => {
            if undo.redo(text, cursors) {
                Edited
            } else {
                Ignored
            }
        }
        Key::Z if ctrl && shift => {
            if undo.redo(text, cursors) {
                Edited
            } else {
                Ignored
            }
        }
        _ => Ignored,
    }
}

/// Where the caret goes after the last snippet stop: the end of the current selection.
fn s_end(text: &str, cursors: &Cursors) -> usize {
    cursors.primary().max().min(text.chars().count())
}

/// Char index → (line, col), 1-based, for the status bar.
pub fn line_col_of(text: &str, at: usize) -> (usize, usize) {
    let mut line = 1;
    let mut col = 1;
    for (i, c) in text.chars().enumerate() {
        if i >= at {
            break;
        }
        if c == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

/// Status-bar text, VS Code style.
pub fn status_text(text: &str, cursors: &Cursors) -> String {
    let p = cursors.primary();
    if cursors.is_multi() {
        let selected: usize = cursors.sels.iter().map(|s| s.len()).sum();
        if selected > 0 {
            format!("{} selections ({} characters selected)", cursors.sels.len(), selected)
        } else {
            format!("{} selections", cursors.sels.len())
        }
    } else {
        let (line, col) = line_col_of(text, p.head);
        if p.is_empty() {
            format!("Ln {line}, Col {col}")
        } else {
            format!("Ln {line}, Col {col} ({} selected)", p.len())
        }
    }
}

// keep the CharIndex import used on every path
#[allow(dead_code)]
fn _char_index(n: usize) -> CharIndex {
    CharIndex(n)
}
