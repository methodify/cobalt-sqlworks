//! Editor core: the multi-cursor selection model, text edits applied at every cursor, and the
//! undo stack. No egui in here, so it is unit-testable; row-based motion (up/down, home/end
//! under word wrap) lives in the widget, which has the galley.
//!
//! Positions are **char indices** (what egui's galley cursors use), never bytes. A selection is
//! an `anchor` (where it started) and a `head` (the caret, which moves); an empty selection is a
//! plain cursor. The cursor set is always kept sorted by position with overlapping/touching
//! selections merged, so edits can be applied front to back with a running offset.

use std::time::{Duration, Instant};

/// VS Code's `editor.multiCursorLimit` default.
pub const MAX_CURSORS: usize = 10_000;

/// One selection: `anchor`..`head` in char indices, either order. `h_pos` remembers the x the
/// caret had before a vertical move so Up/Down through short lines keep the column (VS Code's
/// "sticky column").
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sel {
    pub anchor: usize,
    pub head: usize,
    pub h_pos: Option<f32>,
}

impl Sel {
    pub fn cursor(i: usize) -> Self {
        Self { anchor: i, head: i, h_pos: None }
    }
    pub fn range(anchor: usize, head: usize) -> Self {
        Self { anchor, head, h_pos: None }
    }
    pub fn min(&self) -> usize {
        self.anchor.min(self.head)
    }
    pub fn max(&self) -> usize {
        self.anchor.max(self.head)
    }
    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }
    pub fn len(&self) -> usize {
        self.max() - self.min()
    }
    pub fn is_forward(&self) -> bool {
        self.head >= self.anchor
    }
    pub fn collapse_to(&mut self, i: usize) {
        self.anchor = i;
        self.head = i;
        self.h_pos = None;
    }
    /// Move the caret, extending when `extend` (Shift) else collapsing.
    pub fn move_head(&mut self, i: usize, extend: bool) {
        self.head = i;
        if !extend {
            self.anchor = i;
        }
    }
}

/// The cursor set. `sels` is sorted by `min()` after [`Cursors::normalize`]; `primary` indexes the
/// selection that owns the caret shown in the status bar and that survives Escape.
#[derive(Clone, Debug, PartialEq)]
pub struct Cursors {
    pub sels: Vec<Sel>,
    pub primary: usize,
}

impl Default for Cursors {
    fn default() -> Self {
        Self::single(0)
    }
}

impl Cursors {
    pub fn single(i: usize) -> Self {
        Self { sels: vec![Sel::cursor(i)], primary: 0 }
    }
    pub fn one(sel: Sel) -> Self {
        Self { sels: vec![sel], primary: 0 }
    }
    pub fn primary(&self) -> Sel {
        self.sels[self.primary.min(self.sels.len() - 1)]
    }
    pub fn primary_mut(&mut self) -> &mut Sel {
        let i = self.primary.min(self.sels.len() - 1);
        &mut self.sels[i]
    }
    pub fn is_multi(&self) -> bool {
        self.sels.len() > 1
    }
    pub fn has_selection(&self) -> bool {
        self.sels.iter().any(|s| !s.is_empty())
    }
    pub fn set_single(&mut self, sel: Sel) {
        self.sels.clear();
        self.sels.push(sel);
        self.primary = 0;
    }
    /// Add a selection and make it primary (Alt+Click, Ctrl+D, Ctrl+Alt+Down).
    pub fn add(&mut self, sel: Sel) {
        self.sels.push(sel);
        self.primary = self.sels.len() - 1;
        self.normalize();
    }
    /// Escape: keep the primary selection only.
    pub fn collapse_to_primary(&mut self) {
        let p = self.primary();
        self.set_single(p);
    }
    pub fn clamp(&mut self, len: usize) {
        for s in &mut self.sels {
            s.anchor = s.anchor.min(len);
            s.head = s.head.min(len);
        }
        self.normalize();
    }
    /// Sort by position and merge, as VS Code's `normalize()` does: a cursor touching or inside a
    /// selection folds into it; two non-empty selections merge only when they overlap (touching
    /// ranges stay separate). The merged selection keeps the direction of the primary when it took
    /// part, else of the earlier one. The set is capped at [`MAX_CURSORS`].
    pub fn normalize(&mut self) {
        if self.sels.is_empty() {
            self.sels.push(Sel::cursor(0));
            self.primary = 0;
            return;
        }
        let mut order: Vec<usize> = (0..self.sels.len()).collect();
        order.sort_by_key(|&i| (self.sels[i].min(), self.sels[i].max()));
        let mut merged: Vec<(Sel, bool)> = Vec::with_capacity(self.sels.len());
        for i in order {
            let s = self.sels[i];
            let is_primary = i == self.primary;
            let mergeable = match merged.last() {
                Some((last, _)) => {
                    if s.is_empty() || last.is_empty() {
                        s.min() <= last.max()
                    } else {
                        s.min() < last.max()
                    }
                }
                None => false,
            };
            if mergeable {
                let (last, last_primary) = merged.last_mut().unwrap();
                let min = last.min().min(s.min());
                let max = last.max().max(s.max());
                let (forward, h_pos) = if is_primary { (s.is_forward(), s.h_pos) } else { (last.is_forward(), last.h_pos) };
                *last = if forward { Sel { anchor: min, head: max, h_pos } } else { Sel { anchor: max, head: min, h_pos } };
                *last_primary |= is_primary;
            } else {
                merged.push((s, is_primary));
            }
        }
        if merged.len() > MAX_CURSORS {
            let keep_primary = merged.iter().position(|(_, p)| *p).unwrap_or(0) < MAX_CURSORS;
            merged.truncate(MAX_CURSORS);
            if !keep_primary {
                merged.last_mut().unwrap().1 = true;
            }
        }
        self.primary = merged.iter().position(|(_, p)| *p).unwrap_or(0);
        self.sels = merged.into_iter().map(|(s, _)| s).collect();
    }
    /// Alt+Click on an existing cursor removes it (only while there is more than one).
    /// Returns true when a cursor was removed.
    pub fn remove_at(&mut self, i: usize) -> bool {
        if self.sels.len() < 2 {
            return false;
        }
        let Some(k) = self.sels.iter().position(|s| s.min() <= i && i <= s.max()) else { return false };
        self.sels.remove(k);
        if self.primary >= self.sels.len() {
            self.primary = self.sels.len() - 1;
        }
        true
    }
    /// Char ranges of every selection, sorted.
    pub fn ranges(&self) -> Vec<(usize, usize)> {
        self.sels.iter().map(|s| (s.min(), s.max())).collect()
    }
}

// ---------------------------------------------------------------------------------------------
// Text helpers (char-indexed)
// ---------------------------------------------------------------------------------------------

pub fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Byte offset of every char index (len + 1 entries), so many char→byte lookups cost one scan.
pub fn char_offsets(text: &str) -> Vec<usize> {
    let mut v: Vec<usize> = text.char_indices().map(|(b, _)| b).collect();
    v.push(text.len());
    v
}

/// Char index of the start of the line containing `i`.
pub fn line_start(chars: &[char], i: usize) -> usize {
    let i = i.min(chars.len());
    chars[..i].iter().rposition(|&c| c == '\n').map(|p| p + 1).unwrap_or(0)
}

/// Char index of the end of the line containing `i` (position of its `\n`, or text end).
pub fn line_end(chars: &[char], i: usize) -> usize {
    let i = i.min(chars.len());
    chars[i..].iter().position(|&c| c == '\n').map(|p| i + p).unwrap_or(chars.len())
}

/// First non-blank char of the line containing `i`.
pub fn line_first_nonblank(chars: &[char], i: usize) -> usize {
    let s = line_start(chars, i);
    let e = line_end(chars, i);
    chars[s..e].iter().position(|c| !c.is_whitespace()).map(|p| s + p).unwrap_or(e)
}

/// Word motion, VS Code style: Ctrl+Right stops at the end of the current/next word, Ctrl+Left at
/// the start of the current/previous word. Runs of punctuation count as words of their own.
pub fn next_word_end(chars: &[char], i: usize) -> usize {
    let n = chars.len();
    let mut i = i.min(n);
    while i < n && chars[i].is_whitespace() && chars[i] != '\n' {
        i += 1;
    }
    if i < n && chars[i] == '\n' {
        return i + 1;
    }
    if i >= n {
        return n;
    }
    let word = is_word_char(chars[i]);
    while i < n && !chars[i].is_whitespace() && is_word_char(chars[i]) == word {
        i += 1;
    }
    i
}

pub fn prev_word_start(chars: &[char], i: usize) -> usize {
    let mut i = i.min(chars.len());
    while i > 0 && chars[i - 1].is_whitespace() && chars[i - 1] != '\n' {
        i -= 1;
    }
    if i > 0 && chars[i - 1] == '\n' {
        return i - 1;
    }
    if i == 0 {
        return 0;
    }
    let word = is_word_char(chars[i - 1]);
    while i > 0 && !chars[i - 1].is_whitespace() && is_word_char(chars[i - 1]) == word {
        i -= 1;
    }
    i
}

/// The word around `i` (for double-click and Ctrl+D): a run of word chars, else the run of
/// non-blank punctuation, else the single char.
pub fn word_at(chars: &[char], i: usize) -> (usize, usize) {
    let n = chars.len();
    if n == 0 {
        return (0, 0);
    }
    let i = i.min(n);
    // prefer the char to the right, then the one to the left (like VS Code at a word boundary)
    let pick = if i < n && is_word_char(chars[i]) {
        i
    } else if i > 0 && is_word_char(chars[i - 1]) {
        i - 1
    } else if i < n && !chars[i].is_whitespace() {
        i
    } else if i > 0 && !chars[i - 1].is_whitespace() {
        i - 1
    } else {
        return (i, i);
    };
    let class = |c: char| if is_word_char(c) { 1 } else if c.is_whitespace() { 0 } else { 2 };
    let k = class(chars[pick]);
    let mut a = pick;
    while a > 0 && class(chars[a - 1]) == k {
        a -= 1;
    }
    let mut b = pick + 1;
    while b < n && class(chars[b]) == k {
        b += 1;
    }
    (a, b)
}

/// The line containing `i`, including its trailing newline (triple-click).
pub fn line_at(chars: &[char], i: usize) -> (usize, usize) {
    let s = line_start(chars, i);
    let e = line_end(chars, i);
    (s, (e + 1).min(chars.len()))
}

/// Leading whitespace of the line containing `i`.
pub fn line_indent(chars: &[char], i: usize) -> String {
    let s = line_start(chars, i);
    chars[s..].iter().take_while(|c| **c == ' ' || **c == '\t').collect()
}

// ---------------------------------------------------------------------------------------------
// Edits
// ---------------------------------------------------------------------------------------------

/// One replacement in char indices. A batch must be sorted and non-overlapping.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Replace {
    pub start: usize,
    pub end: usize,
    pub text: String,
}

/// Apply sorted, non-overlapping replacements in one pass. Returns, per replacement, the char
/// index just after its inserted text in the new string (in the same order).
pub fn apply_replacements(text: &mut String, reps: &[Replace]) -> Vec<usize> {
    if reps.is_empty() {
        return Vec::new();
    }
    let offsets = char_offsets(text);
    let nchars = offsets.len() - 1;
    let mut out = String::with_capacity(text.len() + reps.iter().map(|r| r.text.len()).sum::<usize>());
    let mut ends = Vec::with_capacity(reps.len());
    let mut prev = 0usize; // char index copied up to
    let mut delta: isize = 0;
    for r in reps {
        let start = r.start.min(nchars);
        let end = r.end.clamp(start, nchars);
        debug_assert!(start >= prev, "replacements must be sorted and non-overlapping");
        out.push_str(&text[offsets[prev]..offsets[start]]);
        out.push_str(&r.text);
        let inserted = r.text.chars().count();
        ends.push((start as isize + delta) as usize + inserted);
        delta += inserted as isize - (end - start) as isize;
        prev = end;
    }
    out.push_str(&text[offsets[prev]..]);
    *text = out;
    ends
}

/// What kind of edit an undo step holds; consecutive steps of the same kind close together are
/// merged so typing a word is one undo, not one per character.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditKind {
    Typing,
    Deleting,
    Other,
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub text: String,
    pub cursors: Cursors,
}

/// Snapshot undo: the whole text plus the cursor set before each edit. Scripts are small, and a
/// snapshot per step keeps multi-cursor edits trivially reversible.
#[derive(Default)]
pub struct UndoStack {
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    last_kind: Option<EditKind>,
    last_at: Option<Instant>,
}

const MERGE_WINDOW: Duration = Duration::from_millis(900);
const MAX_STEPS: usize = 300;

impl UndoStack {
    /// Call *before* mutating. `kind` decides whether it merges into the previous step.
    pub fn record(&mut self, kind: EditKind, text: &str, cursors: &Cursors) {
        let now = Instant::now();
        let mergeable = kind != EditKind::Other && self.last_kind == Some(kind) && self.last_at.map(|t| now.duration_since(t) < MERGE_WINDOW).unwrap_or(false);
        if !mergeable || self.undo.is_empty() {
            self.undo.push(Snapshot { text: text.to_string(), cursors: cursors.clone() });
            if self.undo.len() > MAX_STEPS {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
        self.last_kind = Some(kind);
        self.last_at = Some(now);
    }
    /// A cursor move breaks typing merges (so "type, move, type" is two undo steps).
    pub fn break_merge(&mut self) {
        self.last_kind = None;
    }
    pub fn undo(&mut self, text: &mut String, cursors: &mut Cursors) -> bool {
        let Some(snap) = self.undo.pop() else { return false };
        self.redo.push(Snapshot { text: std::mem::take(text), cursors: cursors.clone() });
        *text = snap.text;
        *cursors = snap.cursors;
        cursors.clamp(text.chars().count());
        self.last_kind = None;
        true
    }
    pub fn redo(&mut self, text: &mut String, cursors: &mut Cursors) -> bool {
        let Some(snap) = self.redo.pop() else { return false };
        self.undo.push(Snapshot { text: std::mem::take(text), cursors: cursors.clone() });
        *text = snap.text;
        *cursors = snap.cursors;
        cursors.clamp(text.chars().count());
        self.last_kind = None;
        true
    }
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.last_kind = None;
    }
}

/// Replace every selection with `s` (typing, paste of one block). Cursors land after the text.
pub fn insert_at_cursors(text: &mut String, cursors: &mut Cursors, s: &str) {
    let reps: Vec<Replace> = cursors.sels.iter().map(|sel| Replace { start: sel.min(), end: sel.max(), text: s.to_string() }).collect();
    let ends = apply_replacements(text, &reps);
    for (sel, e) in cursors.sels.iter_mut().zip(ends) {
        sel.collapse_to(e);
    }
    cursors.normalize();
}

/// One string per cursor (paste of N lines into N cursors). `texts.len()` must equal the cursor count.
pub fn insert_per_cursor(text: &mut String, cursors: &mut Cursors, texts: &[String]) {
    debug_assert_eq!(texts.len(), cursors.sels.len());
    let reps: Vec<Replace> = cursors.sels.iter().zip(texts).map(|(sel, t)| Replace { start: sel.min(), end: sel.max(), text: t.clone() }).collect();
    let ends = apply_replacements(text, &reps);
    for (sel, e) in cursors.sels.iter_mut().zip(ends) {
        sel.collapse_to(e);
    }
    cursors.normalize();
}

/// Enter: newline plus the current line's indentation, at every cursor.
pub fn newline_at_cursors(text: &mut String, cursors: &mut Cursors) {
    let chars: Vec<char> = text.chars().collect();
    let texts: Vec<String> = cursors.sels.iter().map(|s| format!("\n{}", line_indent(&chars, s.min()))).collect();
    insert_per_cursor(text, cursors, &texts);
}

/// Backspace / Delete. An empty cursor removes one char (or one word with `word`), a selection
/// removes itself. Ranges that would overlap after sorting are merged.
pub fn delete_at_cursors(text: &mut String, cursors: &mut Cursors, forward: bool, word: bool) {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut ranges: Vec<(usize, usize)> = cursors
        .sels
        .iter()
        .map(|s| {
            if !s.is_empty() {
                (s.min(), s.max())
            } else if forward {
                let e = if word { next_word_end(&chars, s.head) } else { (s.head + 1).min(n) };
                (s.head, e)
            } else {
                let a = if word { prev_word_start(&chars, s.head) } else { s.head.saturating_sub(1) };
                (a, s.head)
            }
        })
        .collect();
    // merge overlaps (two cursors one char apart both backspacing)
    ranges.sort();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for r in ranges {
        match merged.last_mut() {
            Some(last) if r.0 < last.1 => last.1 = last.1.max(r.1),
            _ => merged.push(r),
        }
    }
    let reps: Vec<Replace> = merged.iter().map(|&(a, b)| Replace { start: a, end: b, text: String::new() }).collect();
    let ends = apply_replacements(text, &reps);
    cursors.sels = ends.into_iter().map(Sel::cursor).collect();
    cursors.primary = cursors.primary.min(cursors.sels.len() - 1);
    cursors.normalize();
}

/// The distinct lines (start char index of each) touched by the selections, ascending.
fn touched_lines(chars: &[char], cursors: &Cursors) -> Vec<usize> {
    let mut lines: Vec<usize> = Vec::new();
    for s in &cursors.sels {
        let mut p = line_start(chars, s.min());
        // a selection ending at the very start of a line does not include that line
        let end = if !s.is_empty() && s.max() > 0 && chars.get(s.max() - 1) == Some(&'\n') { s.max() - 1 } else { s.max() };
        loop {
            if lines.last() != Some(&p) && !lines.contains(&p) {
                lines.push(p);
            }
            let e = line_end(chars, p);
            if e >= end || e >= chars.len() {
                break;
            }
            p = e + 1;
        }
    }
    lines.sort_unstable();
    lines.dedup();
    lines
}

/// Tab with a multi-line selection / Shift+Tab: add or remove one indent level at the start of every
/// touched line. Selections and cursors shift with the text.
pub fn indent_lines(text: &mut String, cursors: &mut Cursors, indent: &str, tab_size: usize, outdent: bool) {
    let chars: Vec<char> = text.chars().collect();
    let lines = touched_lines(&chars, cursors);
    let mut reps: Vec<Replace> = Vec::new();
    let mut shifts: Vec<(usize, isize)> = Vec::new(); // (line start, delta chars)
    for &ls in &lines {
        if outdent {
            let mut k = 0usize;
            let mut cols = 0usize;
            while ls + k < chars.len() && cols < tab_size {
                match chars[ls + k] {
                    ' ' => {
                        k += 1;
                        cols += 1;
                    }
                    '\t' => {
                        k += 1;
                        break;
                    }
                    _ => break,
                }
            }
            if k > 0 {
                reps.push(Replace { start: ls, end: ls + k, text: String::new() });
                shifts.push((ls, -(k as isize)));
            }
        } else {
            let e = line_end(&chars, ls);
            if e > ls || lines.len() == 1 {
                reps.push(Replace { start: ls, end: ls, text: indent.to_string() });
                shifts.push((ls, indent.chars().count() as isize));
            }
        }
    }
    if reps.is_empty() {
        return;
    }
    apply_replacements(text, &reps);
    let adjust = |i: usize| -> usize {
        let mut d: isize = 0;
        for &(ls, delta) in &shifts {
            if ls < i {
                // an outdent that ate chars past the cursor clamps to the line start
                if delta < 0 && i < ls + (-delta) as usize {
                    d += ls as isize - i as isize;
                } else {
                    d += delta;
                }
            } else if ls == i && delta > 0 {
                d += delta; // cursor at line start moves with the inserted indent
            }
        }
        (i as isize + d).max(0) as usize
    };
    for s in &mut cursors.sels {
        s.anchor = adjust(s.anchor);
        s.head = adjust(s.head);
    }
    cursors.normalize();
}

/// Alt+Up / Alt+Down: move the lines under each selection one line up or down (as one block per
/// selection). Cursors travel with their text.
pub fn move_lines(text: &mut String, cursors: &mut Cursors, down: bool) {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    // blocks: (start, end_exclusive_of_newline) per selection, merged when they share lines
    let mut blocks: Vec<(usize, usize)> = cursors
        .sels
        .iter()
        .map(|s| {
            let a = line_start(&chars, s.min());
            let end = if !s.is_empty() && s.max() > 0 && chars.get(s.max() - 1) == Some(&'\n') { s.max() - 1 } else { s.max() };
            (a, line_end(&chars, end))
        })
        .collect();
    blocks.sort();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for b in blocks {
        match merged.last_mut() {
            Some(l) if b.0 <= l.1 + 1 => l.1 = l.1.max(b.1),
            _ => merged.push(b),
        }
    }
    let mut reps: Vec<Replace> = Vec::new();
    let mut shifts: Vec<(usize, usize, isize)> = Vec::new(); // (block start, block end, delta for chars inside)
    for &(a, b) in &merged {
        if down {
            if b >= n {
                continue; // last line
            }
            let nb = line_end(&chars, b + 1); // end of the next line
            let block: String = chars[a..b].iter().collect();
            let next: String = chars[b + 1..nb].iter().collect();
            reps.push(Replace { start: a, end: nb, text: format!("{next}\n{block}") });
            shifts.push((a, b, (nb - b) as isize));
        } else {
            if a == 0 {
                continue;
            }
            let pa = line_start(&chars, a - 1);
            let prev: String = chars[pa..a - 1].iter().collect();
            let block: String = chars[a..b].iter().collect();
            reps.push(Replace { start: pa, end: b, text: format!("{block}\n{prev}") });
            shifts.push((a, b, -((a - pa) as isize)));
        }
    }
    if reps.is_empty() {
        return;
    }
    reps.sort_by_key(|r| r.start);
    apply_replacements(text, &reps);
    for s in &mut cursors.sels {
        for &(a, b, d) in &shifts {
            if s.min() >= a && s.max() <= b + 1 {
                s.anchor = (s.anchor as isize + d) as usize;
                s.head = (s.head as isize + d) as usize;
                break;
            }
        }
    }
    cursors.normalize();
}

/// Shift+Alt+Down / Up: duplicate the lines under each selection (below or above).
pub fn duplicate_lines(text: &mut String, cursors: &mut Cursors, down: bool) {
    let chars: Vec<char> = text.chars().collect();
    let mut reps: Vec<Replace> = Vec::new();
    let mut shifts: Vec<(usize, usize)> = Vec::new(); // (insert position, inserted chars)
    for s in &cursors.sels {
        let a = line_start(&chars, s.min());
        let end = if !s.is_empty() && s.max() > 0 && chars.get(s.max() - 1) == Some(&'\n') { s.max() - 1 } else { s.max() };
        let b = line_end(&chars, end);
        let block: String = chars[a..b].iter().collect();
        if down {
            reps.push(Replace { start: b, end: b, text: format!("\n{block}") });
            shifts.push((b, b - a + 1));
        } else {
            reps.push(Replace { start: a, end: a, text: format!("{block}\n") });
            shifts.push((a, b - a + 1));
        }
    }
    reps.sort_by_key(|r| r.start);
    reps.dedup();
    let starts: Vec<(usize, usize)> = cursors
        .sels
        .iter()
        .map(|s| line_start(&chars, s.min()))
        .zip(shifts.iter().map(|&(_, k)| k))
        .collect();
    apply_replacements(text, &reps);
    // below: the cursor lands on the copy (which follows the original); above: the copy goes in
    // front, so the original — with its cursor — moves down. Either way: +k for every block
    // that starts at or before the cursor.
    let adjust = |i: usize| -> usize {
        let mut d = 0usize;
        let mut seen: Vec<usize> = Vec::new();
        for &(a, k) in &starts {
            if a <= i && !seen.contains(&a) {
                d += k;
                seen.push(a);
            }
        }
        i + d
    };
    for s in &mut cursors.sels {
        s.anchor = adjust(s.anchor);
        s.head = adjust(s.head);
    }
    cursors.normalize();
}

/// Ctrl+Shift+K: delete the whole lines under each selection.
pub fn delete_lines(text: &mut String, cursors: &mut Cursors) {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut ranges: Vec<(usize, usize)> = cursors
        .sels
        .iter()
        .map(|s| {
            let a = line_start(&chars, s.min());
            let end = if !s.is_empty() && s.max() > 0 && chars.get(s.max() - 1) == Some(&'\n') { s.max() - 1 } else { s.max() };
            let b = line_end(&chars, end);
            if b < n {
                (a, b + 1)
            } else if a > 0 {
                (a - 1, b)
            } else {
                (a, b)
            }
        })
        .collect();
    ranges.sort();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for r in ranges {
        match merged.last_mut() {
            Some(l) if r.0 <= l.1 => l.1 = l.1.max(r.1),
            _ => merged.push(r),
        }
    }
    let reps: Vec<Replace> = merged.iter().map(|&(a, b)| Replace { start: a, end: b, text: String::new() }).collect();
    let ends = apply_replacements(text, &reps);
    let chars2: Vec<char> = text.chars().collect();
    cursors.sels = ends.into_iter().map(|e| Sel::cursor(line_start(&chars2, e.min(chars2.len())))).collect();
    cursors.primary = 0;
    cursors.normalize();
}

/// Copy: one piece per selection, joined with newlines. An empty selection contributes its whole
/// line plus newline (VS Code's `emptySelectionClipboard`). Returns (text, from_single_empty):
/// the flag marks a copy of one whole line from a single cursor, which pastes above the line.
pub fn copy_text(text: &str, cursors: &Cursors) -> (String, bool) {
    let chars: Vec<char> = text.chars().collect();
    let parts: Vec<String> = cursors
        .sels
        .iter()
        .map(|s| {
            if s.is_empty() {
                let ls = line_start(&chars, s.head);
                chars[ls..line_end(&chars, ls)].iter().collect::<String>() + "\n"
            } else {
                chars[s.min()..s.max()].iter().collect()
            }
        })
        .collect();
    let single_empty = cursors.sels.len() == 1 && cursors.sels[0].is_empty();
    let joined = if single_empty { parts.concat() } else { parts.join("\n") };
    (joined, single_empty)
}

/// Paste: `clip` split on newlines goes one line per cursor when the counts match (and there is more
/// than one cursor); otherwise the whole text goes to every cursor. A clipboard that came from a
/// whole-line copy (`line_copy`) is inserted above the cursor's line.
pub fn paste_at_cursors(text: &mut String, cursors: &mut Cursors, clip: &str, line_copy: bool) {
    let clip = clip.replace("\r\n", "\n");
    if line_copy && !cursors.has_selection() {
        let chars: Vec<char> = text.chars().collect();
        let mut starts: Vec<usize> = cursors.sels.iter().map(|s| line_start(&chars, s.head)).collect();
        starts.sort_unstable();
        starts.dedup();
        let reps: Vec<Replace> = starts.iter().map(|&ls| Replace { start: ls, end: ls, text: clip.clone() }).collect();
        apply_replacements(text, &reps);
        let k = clip.chars().count();
        for s in &mut cursors.sels {
            let shift = starts.iter().filter(|&&ls| ls <= s.head).count() * k;
            s.anchor += shift;
            s.head += shift;
        }
        cursors.normalize();
        return;
    }
    let body = clip.strip_suffix('\n').unwrap_or(&clip);
    let lines: Vec<&str> = body.split('\n').collect();
    if cursors.sels.len() > 1 && lines.len() == cursors.sels.len() {
        let texts: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
        insert_per_cursor(text, cursors, &texts);
    } else {
        insert_at_cursors(text, cursors, &clip);
    }
}

/// How Ctrl+D / Ctrl+Shift+L match. Seeded from an empty cursor VS Code uses whole word + match
/// case; seeded from a selection it uses the find widget's toggles (both off by default).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MatchMode {
    pub case_sensitive: bool,
    pub whole_word: bool,
}

fn find_all(chars: &[char], needle: &[char], mode: MatchMode) -> Vec<usize> {
    let eq = |a: char, b: char| if mode.case_sensitive { a == b } else { a.to_lowercase().eq(b.to_lowercase()) };
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + needle.len() <= chars.len() {
        let hit = needle.iter().enumerate().all(|(k, &c)| eq(chars[i + k], c));
        let whole = !mode.whole_word || ((i == 0 || !is_word_char(chars[i - 1])) && (i + needle.len() == chars.len() || !is_word_char(chars[i + needle.len()])));
        if hit && whole {
            out.push(i);
            i += needle.len();
        } else {
            i += 1;
        }
    }
    out
}

/// Ctrl+D: with a selection, add the next occurrence of the primary selection's text after the
/// last selection, wrapping around; without one, select the word at each cursor (returns the mode
/// to keep using: whole word + match case). Returns None when there was nothing to add.
pub fn add_next_occurrence(text: &str, cursors: &mut Cursors, mode: MatchMode) -> Option<MatchMode> {
    let chars: Vec<char> = text.chars().collect();
    if !cursors.has_selection() {
        let mut any = false;
        for s in &mut cursors.sels {
            let (a, b) = word_at(&chars, s.head);
            if a != b {
                *s = Sel::range(a, b);
                any = true;
            }
        }
        cursors.normalize();
        return any.then_some(MatchMode { case_sensitive: true, whole_word: true });
    }
    let p = cursors.primary();
    let needle: Vec<char> = chars[p.min()..p.max()].to_vec();
    if needle.is_empty() {
        return None;
    }
    let last_max = cursors.sels.iter().map(|s| s.max()).max().unwrap_or(0);
    let hits = find_all(&chars, &needle, mode);
    let taken = |i: usize| cursors.sels.iter().any(|s| s.min() == i && s.max() == i + needle.len());
    let next = hits.iter().copied().find(|&i| i >= last_max && !taken(i)).or_else(|| hits.iter().copied().find(|&i| !taken(i)));
    let i = next?;
    cursors.add(Sel::range(i, i + needle.len()));
    Some(mode)
}

/// Ctrl+Shift+L: select every occurrence of the primary selection's text (or of the word at the
/// cursor, whole word + match case). The primary stays where it was invoked.
pub fn select_all_occurrences(text: &str, cursors: &mut Cursors, mode: MatchMode) {
    let chars: Vec<char> = text.chars().collect();
    let p = cursors.primary();
    let (a, b, mode) = if p.is_empty() {
        let (a, b) = word_at(&chars, p.head);
        (a, b, MatchMode { case_sensitive: true, whole_word: true })
    } else {
        (p.min(), p.max(), mode)
    };
    let needle: Vec<char> = chars[a..b].to_vec();
    if needle.is_empty() {
        return;
    }
    let sels: Vec<Sel> = find_all(&chars, &needle, mode).into_iter().map(|i| Sel::range(i, i + needle.len())).collect();
    if sels.is_empty() {
        return;
    }
    let primary = sels.iter().position(|s| s.min() == a).unwrap_or(0);
    *cursors = Cursors { sels, primary };
    cursors.normalize();
}

/// Shift+Alt+I: one cursor at the end of every line under the selections.
pub fn cursors_at_line_ends(text: &str, cursors: &mut Cursors) {
    let chars: Vec<char> = text.chars().collect();
    let lines = touched_lines(&chars, cursors);
    let sels: Vec<Sel> = lines.iter().map(|&ls| Sel::cursor(line_end(&chars, ls))).collect();
    if !sels.is_empty() {
        let primary = sels.len() - 1;
        *cursors = Cursors { sels, primary };
        cursors.normalize();
    }
}

/// An active snippet: the tab stops (char ranges) of an expanded snippet and which one the
/// caret is on. Edits shift the stops so Tab keeps landing on the right placeholders.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnippetSession {
    pub stops: Vec<(usize, usize)>,
    pub index: usize,
}

impl SnippetSession {
    pub fn current(&self) -> Option<(usize, usize)> {
        self.stops.get(self.index).copied()
    }
    /// A text edit replaced something at `pos` and changed the length by `delta`: stops after it
    /// move, the current stop (which the edit happened inside) grows or shrinks.
    pub fn adjust(&mut self, pos: usize, delta: isize) {
        let sh = |v: usize| (v as isize + delta).max(pos as isize) as usize;
        for (i, (a, b)) in self.stops.iter_mut().enumerate() {
            if i == self.index && *a <= pos && pos <= *b {
                *b = sh(*b).max(*a);
            } else if *a >= pos {
                *a = sh(*a);
                *b = sh(*b).max(*a);
            } else if *b > pos {
                *b = sh(*b).max(*a);
            }
        }
    }
    /// Move to the next (or previous) stop; None when leaving the snippet.
    pub fn step(&mut self, back: bool) -> Option<(usize, usize)> {
        let next = if back { self.index.checked_sub(1)? } else { self.index + 1 };
        if next >= self.stops.len() {
            return None;
        }
        self.index = next;
        self.current()
    }
}


// ---------------------------------------------------------------------------------------------
// Bracket and quote pairs (VS Code's auto-closing / auto-surround / overtype / pair delete)
// ---------------------------------------------------------------------------------------------

/// The pairs kept balanced. Quotes pair with themselves; `[`…`]` doubles as T-SQL's identifier
/// quote, backticks as Spark SQL's.
pub const PAIRS: [(char, char); 6] = [('(', ')'), ('[', ']'), ('{', '}'), ('\'', '\''), ('"', '"'), ('`', '`')];

pub fn closer_of(open: char) -> Option<char> {
    PAIRS.iter().find(|(o, _)| *o == open).map(|(_, c)| *c)
}

pub fn is_closer(c: char) -> bool {
    PAIRS.iter().any(|(_, cl)| *cl == c)
}

fn is_quote(c: char) -> bool {
    matches!(c, '\'' | '"' | '`')
}

/// What the editor does with brackets and quotes (Settings → Editor).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PairPolicy {
    /// Wrap a selection when an opening bracket or a quote is typed.
    pub surround: bool,
    /// Insert the partner, overtype it, delete an empty pair with Backspace.
    pub auto_close: bool,
}

impl Default for PairPolicy {
    fn default() -> Self {
        Self { surround: true, auto_close: true }
    }
}

/// VS Code's `autoCloseBefore`: a pair is only inserted when the caret is followed by nothing,
/// whitespace, or one of these.
fn auto_close_before(next: Option<char>) -> bool {
    match next {
        None => true,
        Some(c) => c.is_whitespace() || matches!(c, ';' | ':' | '.' | ',' | '=' | '}' | ']' | ')' | '>'),
    }
}

/// Should typing `ch` (an opener) at `at` insert its partner too?
fn should_auto_close(ch: char, chars: &[char], at: usize) -> bool {
    let next = chars.get(at).copied();
    let prev = if at > 0 { chars.get(at - 1).copied() } else { None };
    if !auto_close_before(next) {
        return false;
    }
    if is_quote(ch) {
        // not right after a word (it's / don't / an identifier), and not inside a string on
        // this line (an odd number of the same quote before the caret means we are)
        if prev.map(is_word_char).unwrap_or(false) || prev == Some(ch) {
            return false;
        }
        let line_start = chars[..at].iter().rposition(|c| *c == '\n').map(|i| i + 1).unwrap_or(0);
        let count = chars[line_start..at].iter().filter(|c| **c == ch).count();
        if count % 2 == 1 {
            return false;
        }
    }
    true
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PairAct {
    /// `(` + selection + `)`, selection kept on the inner text.
    Wrap,
    /// `(` + `)` with the caret between.
    Pair,
    /// The closer is already there: step over it.
    Over,
    /// Just the character.
    Plain,
}

/// Type one character under the pairing rules, at every cursor. Returns `false` when the rules
/// do not apply (the caller inserts the text as usual).
pub fn type_pair_char(text: &mut String, cursors: &mut Cursors, ch: char, policy: PairPolicy) -> bool {
    let close = closer_of(ch);
    if close.is_none() && !is_closer(ch) {
        return false;
    }
    let chars: Vec<char> = text.chars().collect();
    let acts: Vec<PairAct> = cursors
        .sels
        .iter()
        .map(|s| {
            if !s.is_empty() {
                // a closing bracket typed over a selection replaces it, as in VS Code
                if policy.surround && close.is_some() { PairAct::Wrap } else { PairAct::Plain }
            } else if policy.auto_close && is_closer(ch) && chars.get(s.head).copied() == Some(ch) {
                PairAct::Over
            } else if policy.auto_close && close.is_some() && should_auto_close(ch, &chars, s.head) {
                PairAct::Pair
            } else {
                PairAct::Plain
            }
        })
        .collect();
    if acts.iter().all(|a| *a == PairAct::Plain) {
        return false;
    }
    let closer = close.unwrap_or(ch);
    let mut reps: Vec<Replace> = Vec::with_capacity(acts.len());
    let mut inner_lens: Vec<usize> = Vec::with_capacity(acts.len());
    for (s, a) in cursors.sels.iter().zip(&acts) {
        let (start, end) = (s.min(), s.max());
        let t = match a {
            PairAct::Wrap => {
                let inner: String = chars[start..end].iter().collect();
                inner_lens.push(end - start);
                format!("{ch}{inner}{closer}")
            }
            PairAct::Pair => {
                inner_lens.push(0);
                format!("{ch}{closer}")
            }
            PairAct::Over => {
                inner_lens.push(0);
                ch.to_string()
            }
            PairAct::Plain => {
                inner_lens.push(0);
                ch.to_string()
            }
        };
        let end = if *a == PairAct::Over { end + 1 } else { end };
        reps.push(Replace { start, end, text: t });
    }
    let ends = apply_replacements(text, &reps);
    for ((s, a), (e, inner)) in cursors.sels.iter_mut().zip(&acts).zip(ends.iter().zip(&inner_lens)) {
        match a {
            PairAct::Wrap => {
                let (lo, hi) = (e - 1 - inner, e - 1);
                let forward = s.is_forward();
                s.anchor = if forward { lo } else { hi };
                s.head = if forward { hi } else { lo };
                s.h_pos = None;
            }
            PairAct::Pair => s.collapse_to(e - 1),
            PairAct::Over | PairAct::Plain => s.collapse_to(*e),
        }
    }
    cursors.normalize();
    true
}

/// Backspace with the caret between an opener and its closer removes both (per cursor; other
/// cursors delete one character). Returns `false` when no cursor sits in an empty pair.
pub fn backspace_pair(text: &mut String, cursors: &mut Cursors) -> bool {
    if cursors.sels.iter().any(|s| !s.is_empty()) {
        return false;
    }
    let chars: Vec<char> = text.chars().collect();
    let in_pair = |at: usize| -> bool { at > 0 && chars.get(at - 1).and_then(|o| closer_of(*o)).is_some_and(|c| chars.get(at).copied() == Some(c)) };
    if !cursors.sels.iter().any(|s| in_pair(s.head)) {
        return false;
    }
    let reps: Vec<Replace> = cursors
        .sels
        .iter()
        .map(|s| if in_pair(s.head) { Replace { start: s.head - 1, end: s.head + 1, text: String::new() } } else { Replace { start: s.head.saturating_sub(1), end: s.head, text: String::new() } })
        .collect();
    let ends = apply_replacements(text, &reps);
    for (s, e) in cursors.sels.iter_mut().zip(ends) {
        s.collapse_to(e);
    }
    cursors.normalize();
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cur(text: &str, at: &[usize]) -> Cursors {
        let mut c = Cursors { sels: at.iter().map(|&i| Sel::cursor(i)).collect(), primary: at.len() - 1 };
        c.clamp(text.chars().count());
        c
    }

    #[test]
    fn replacements_shift_later_cursors() {
        let mut t = "ab cd ef".to_string();
        let mut c = cur(&t, &[0, 3, 6]);
        insert_at_cursors(&mut t, &mut c, "XY");
        assert_eq!(t, "XYab XYcd XYef");
        assert_eq!(c.sels.iter().map(|s| s.head).collect::<Vec<_>>(), vec![2, 7, 12]);
    }

    #[test]
    fn insert_replaces_selections() {
        let mut t = "hello world".to_string();
        let mut c = Cursors { sels: vec![Sel::range(0, 5), Sel::range(6, 11)], primary: 0 };
        insert_at_cursors(&mut t, &mut c, "x");
        assert_eq!(t, "x x");
        assert!(!c.has_selection());
    }

    #[test]
    fn normalize_merges_and_sorts() {
        let mut c = Cursors { sels: vec![Sel::range(5, 8), Sel::cursor(2), Sel::range(7, 10), Sel::cursor(2)], primary: 2 };
        c.normalize();
        assert_eq!(c.sels.len(), 2);
        assert_eq!(c.sels[0], Sel::cursor(2));
        assert_eq!((c.sels[1].min(), c.sels[1].max()), (5, 10));
        assert_eq!(c.primary, 1);
    }

    #[test]
    fn cursor_touching_selection_merges_but_touching_selections_do_not() {
        let mut c = Cursors { sels: vec![Sel::range(0, 3), Sel::cursor(3)], primary: 1 };
        c.normalize();
        assert_eq!(c.sels.len(), 1);
        let mut c = Cursors { sels: vec![Sel::range(0, 3), Sel::range(3, 5)], primary: 1 };
        c.normalize();
        assert_eq!(c.sels.len(), 2);
        // the primary's direction wins the merge
        let mut c = Cursors { sels: vec![Sel::range(0, 4), Sel::range(6, 2)], primary: 1 };
        c.normalize();
        assert_eq!(c.sels, vec![Sel::range(6, 0)]);
    }

    #[test]
    fn alt_click_removes_a_cursor() {
        let mut c = Cursors { sels: vec![Sel::cursor(1), Sel::range(4, 8)], primary: 1 };
        assert!(c.remove_at(6));
        assert_eq!(c.sels, vec![Sel::cursor(1)]);
        assert!(!c.remove_at(1));
    }

    #[test]
    fn backspace_merges_adjacent() {
        let mut t = "abcd".to_string();
        let mut c = cur(&t, &[2, 3]);
        delete_at_cursors(&mut t, &mut c, false, false);
        assert_eq!(t, "ad");
        assert_eq!(c.sels.len(), 1);
        assert_eq!(c.sels[0].head, 1);
    }

    #[test]
    fn delete_word_forward() {
        let mut t = "select * from t".to_string();
        let mut c = cur(&t, &[0]);
        delete_at_cursors(&mut t, &mut c, true, true);
        assert_eq!(t, " * from t");
    }

    #[test]
    fn word_motion() {
        let ch: Vec<char> = "select sys.tables\nfrom".chars().collect();
        assert_eq!(next_word_end(&ch, 0), 6);
        assert_eq!(next_word_end(&ch, 6), 10); // "sys"
        assert_eq!(next_word_end(&ch, 10), 11); // "."
        assert_eq!(prev_word_start(&ch, 17), 11); // "tables"
        assert_eq!(next_word_end(&ch, 17), 18); // newline
        assert_eq!(word_at(&ch, 12), (11, 17));
        assert_eq!(word_at(&ch, 6), (0, 6)); // boundary prefers the word to the left when right is blank
    }

    #[test]
    fn newline_keeps_indent() {
        let mut t = "  a\n    b".to_string();
        let mut c = cur(&t, &[3, 9]);
        newline_at_cursors(&mut t, &mut c);
        assert_eq!(t, "  a\n  \n    b\n    ");
    }

    #[test]
    fn indent_and_outdent_lines() {
        let mut t = "a\nb\nc".to_string();
        let mut c = Cursors::one(Sel::range(0, 5));
        indent_lines(&mut t, &mut c, "  ", 2, false);
        assert_eq!(t, "  a\n  b\n  c");
        assert_eq!((c.sels[0].min(), c.sels[0].max()), (2, 11));
        indent_lines(&mut t, &mut c, "  ", 2, true);
        assert_eq!(t, "a\nb\nc");
        assert_eq!((c.sels[0].min(), c.sels[0].max()), (0, 5));
    }

    #[test]
    fn move_lines_down_and_up() {
        let mut t = "one\ntwo\nthree".to_string();
        let mut c = cur(&t, &[1]);
        move_lines(&mut t, &mut c, true);
        assert_eq!(t, "two\none\nthree");
        assert_eq!(c.sels[0].head, 5);
        move_lines(&mut t, &mut c, false);
        assert_eq!(t, "one\ntwo\nthree");
        assert_eq!(c.sels[0].head, 1);
    }

    #[test]
    fn duplicate_line_down() {
        let mut t = "one\ntwo".to_string();
        let mut c = cur(&t, &[1]);
        duplicate_lines(&mut t, &mut c, true);
        assert_eq!(t, "one\none\ntwo");
        assert_eq!(c.sels[0].head, 5);
    }

    #[test]
    fn delete_whole_lines() {
        let mut t = "one\ntwo\nthree".to_string();
        let mut c = cur(&t, &[5]);
        delete_lines(&mut t, &mut c);
        assert_eq!(t, "one\nthree");
        assert_eq!(c.sels[0].head, 4);
    }

    #[test]
    fn copy_and_paste_multi() {
        let t = "aa\nbb\ncc".to_string();
        let c = Cursors { sels: vec![Sel::range(0, 2), Sel::range(3, 5)], primary: 0 };
        let (clip, line) = copy_text(&t, &c);
        assert_eq!(clip, "aa\nbb");
        assert!(!line);
        let mut t2 = "x\ny".to_string();
        let mut c2 = cur(&t2, &[1, 3]);
        paste_at_cursors(&mut t2, &mut c2, &clip, false);
        assert_eq!(t2, "xaa\nybb");
        // empty-selection copy takes whole lines and pastes above
        let (clip, line) = copy_text(&t, &cur(&t, &[4]));
        assert_eq!(clip, "bb\n");
        assert!(line);
        // two empty cursors: a line each, joined; not a line-copy
        let (clip2, line2) = copy_text(&t, &cur(&t, &[0, 4]));
        assert_eq!(clip2, "aa\n\nbb\n");
        assert!(!line2);
        let mut t3 = "one\ntwo".to_string();
        let mut c3 = cur(&t3, &[5]);
        paste_at_cursors(&mut t3, &mut c3, &clip, true);
        assert_eq!(t3, "one\nbb\ntwo");
        assert_eq!(c3.sels[0].head, 8);
    }

    #[test]
    fn next_occurrence_and_all() {
        let t = "id, Id, name, id".to_string();
        let mut c = cur(&t, &[1]);
        let loose = MatchMode { case_sensitive: false, whole_word: false };
        let mode = add_next_occurrence(&t, &mut c, loose).unwrap();
        assert_eq!(c.ranges(), vec![(0, 2)]);
        assert_eq!(mode, MatchMode { case_sensitive: true, whole_word: true });
        assert!(add_next_occurrence(&t, &mut c, mode).is_some());
        assert_eq!(c.ranges(), vec![(0, 2), (14, 16)]); // "Id" skipped: match case
        assert!(add_next_occurrence(&t, &mut c, mode).is_none());
        let mut c = Cursors::one(Sel::range(0, 2));
        assert!(add_next_occurrence(&t, &mut c, loose).is_some());
        assert_eq!(c.ranges(), vec![(0, 2), (4, 6)]);
        let mut c = cur(&t, &[15]);
        select_all_occurrences(&t, &mut c, loose);
        assert_eq!(c.ranges(), vec![(0, 2), (14, 16)]);
        assert_eq!(c.primary, 1);
    }

    #[test]
    fn line_end_cursors() {
        let t = "ab\ncde\nf".to_string();
        let mut c = Cursors::one(Sel::range(0, 7));
        cursors_at_line_ends(&t, &mut c);
        // the selection ends at column 1 of the last line, so that line is not included (VS Code)
        assert_eq!(c.sels.iter().map(|s| s.head).collect::<Vec<_>>(), vec![2, 6]);
        let mut c = Cursors::one(Sel::range(0, 8));
        cursors_at_line_ends(&t, &mut c);
        assert_eq!(c.sels.iter().map(|s| s.head).collect::<Vec<_>>(), vec![2, 6, 8]);
    }

    #[test]
    fn snippet_stops_follow_edits() {
        // "SELECT TOP (100) *\nFROM table" with stops at "table" (24..29) and $0 at 29
        let mut s = SnippetSession { stops: vec![(24, 29), (29, 29)], index: 0 };
        // typing "t" replaces the selected placeholder: 5 chars → 1
        s.adjust(24, -4);
        assert_eq!(s.stops, vec![(24, 25), (25, 25)]);
        s.adjust(25, 2); // "t" → "tbl"
        assert_eq!(s.stops, vec![(24, 27), (27, 27)]);
        assert_eq!(s.step(false), Some((27, 27)));
        assert_eq!(s.step(false), None);
    }

    #[test]
    fn undo_redo_roundtrip() {
        let mut t = "abc".to_string();
        let mut c = cur(&t, &[3]);
        let mut u = UndoStack::default();
        u.record(EditKind::Typing, &t, &c);
        insert_at_cursors(&mut t, &mut c, "d");
        u.record(EditKind::Typing, &t, &c); // merges into the same step
        insert_at_cursors(&mut t, &mut c, "e");
        assert_eq!(t, "abcde");
        assert!(u.undo(&mut t, &mut c));
        assert_eq!(t, "abc");
        assert_eq!(c.sels[0].head, 3);
        assert!(u.redo(&mut t, &mut c));
        assert_eq!(t, "abcde");
    }

    fn pairs_default() -> PairPolicy {
        PairPolicy::default()
    }

    #[test]
    fn surround_wraps_selection_and_keeps_it() {
        let mut t = String::from("select name from t");
        let mut c = Cursors { sels: vec![Sel::range(7, 11)], primary: 0 };
        assert!(type_pair_char(&mut t, &mut c, '[', pairs_default()));
        assert_eq!(t, "select [name] from t");
        assert_eq!((c.sels[0].anchor, c.sels[0].head), (8, 12));
        // typing another opener wraps again; a backwards selection keeps its direction
        c.sels[0] = Sel::range(12, 8);
        assert!(type_pair_char(&mut t, &mut c, '\'', pairs_default()));
        assert_eq!(t, "select ['name'] from t");
        assert_eq!((c.sels[0].anchor, c.sels[0].head), (13, 9));
    }

    #[test]
    fn surround_every_cursor_and_closer_replaces() {
        let mut t = String::from("a b");
        let mut c = Cursors { sels: vec![Sel::range(0, 1), Sel::range(2, 3)], primary: 0 };
        assert!(type_pair_char(&mut t, &mut c, '(', pairs_default()));
        assert_eq!(t, "(a) (b)");
        // a closing bracket over a selection replaces it (VS Code)
        let mut t = String::from("abc");
        let mut c = Cursors { sels: vec![Sel::range(0, 3)], primary: 0 };
        assert!(!type_pair_char(&mut t, &mut c, ')', pairs_default()));
        // surround off: the rules do not apply to a selection
        let mut c = Cursors { sels: vec![Sel::range(0, 3)], primary: 0 };
        assert!(!type_pair_char(&mut t, &mut c, '(', PairPolicy { surround: false, auto_close: true }));
    }

    #[test]
    fn auto_close_pairs_overtypes_and_respects_context() {
        let mut t = String::new();
        let mut c = Cursors::single(0);
        assert!(type_pair_char(&mut t, &mut c, '(', pairs_default()));
        assert_eq!((t.as_str(), c.sels[0].head), ("()", 1));
        // typing the closer steps over the auto-inserted one
        assert!(type_pair_char(&mut t, &mut c, ')', pairs_default()));
        assert_eq!((t.as_str(), c.sels[0].head), ("()", 2));
        // no pair before a word character
        let mut t = String::from("x");
        let mut c = Cursors::single(0);
        assert!(!type_pair_char(&mut t, &mut c, '(', pairs_default()));
        // quotes: not after a word (don't), not inside an open string on the line
        let mut t = String::from("don");
        let mut c = Cursors::single(3);
        assert!(!type_pair_char(&mut t, &mut c, '\'', pairs_default()));
        let mut t = String::from("'abc");
        let mut c = Cursors::single(4);
        assert!(!type_pair_char(&mut t, &mut c, '\'', pairs_default()));
        // a quote at a clean spot pairs
        let mut t = String::from("where x = ");
        let mut c = Cursors::single(10);
        assert!(type_pair_char(&mut t, &mut c, '\'', pairs_default()));
        assert_eq!((t.as_str(), c.sels[0].head), ("where x = ''", 11));
        // auto-close off: nothing happens
        let mut t = String::new();
        let mut c = Cursors::single(0);
        assert!(!type_pair_char(&mut t, &mut c, '(', PairPolicy { surround: true, auto_close: false }));
    }

    #[test]
    fn backspace_removes_an_empty_pair() {
        let mut t = String::from("f()");
        let mut c = Cursors::single(2);
        assert!(backspace_pair(&mut t, &mut c));
        assert_eq!((t.as_str(), c.sels[0].head), ("f", 1));
        let mut t = String::from("f(x)");
        let mut c = Cursors::single(2);
        assert!(!backspace_pair(&mut t, &mut c));
        // mixed cursors: the one in a pair deletes both, the other one character
        let mut t = String::from("() ab");
        let mut c = Cursors { sels: vec![Sel::cursor(1), Sel::cursor(5)], primary: 0 };
        assert!(backspace_pair(&mut t, &mut c));
        assert_eq!(t, " a");
    }

}
