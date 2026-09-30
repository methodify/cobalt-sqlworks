# Editor: multi-cursor editing and mouse selection

Status: design for the 0.4.x editor rework (2026-09-30). Implements the interaction pattern VS
Code calls **multiple selections (multi-cursor)** and **column (box) selection**, which Azure Data
Studio inherited unchanged. Sources: VS Code docs (Basic Editing) and source on `main`
(`multicursor.ts`, `coreCommands.ts`, `cursorCollection.ts`, `cursorMoveCommands.ts`,
`cursorColumnSelection.ts`, `cursorTypeEditOperations.ts`, `viewController.ts`, `mouseHandler.ts`,
`clipboardUtils.ts`, `viewCursors.ts`, `editorStatus.ts`).

## Why a custom editor widget

Cobalt's editor was egui's `TextEdit` with a syntax-colouring layouter. `TextEdit` owns exactly one
cursor and all of its mouse and keyboard handling; there is no hook to run edits at several
positions, and its mouse code decides "double-click" on the *release* of the second click, so the
press-and-drag word selection people use in VS Code is impossible. Two founder-reported rendering
bugs come from the same place: egui lays an empty text out as a zero-height row, so the line
number of an empty editor sits on the top edge and the caret on an empty trailing line has no
height. Owning the widget fixes all four reports with one model.

The rework keeps everything outside the widget: `tab.text` stays the source of truth, the
`PendingEdit` mechanism that commands use (Replace / SetCursor / Select / SetText) is applied to
the new cursor model, the gutter, completion popup, find/replace and go-to-line bars are unchanged,
and the agent verbs (`set_query`, `type_text`, `press`, `pointer`) keep working because they inject
egui events, which the widget now consumes directly.

## Model

- A **selection** is `anchor`..`head` in char indices (egui galley cursors are char-indexed);
  `head` is the caret. An empty selection is a **cursor**. Each selection remembers `h_pos`, the x
  the caret had before a vertical move, so Up/Down through shorter lines keep the column (VS Code's
  sticky column).
- The **cursor set** is a `Vec<Sel>` plus the index of the **primary** selection (the one the
  status bar reports and the one Escape keeps). After every change the set is **normalized**:
  sorted by position, and merged exactly as VS Code's `normalize()` does — a cursor touching or
  inside a selection folds into it, two non-empty selections merge only when they overlap
  (touching ranges stay separate). The merged selection takes the direction of the primary
  (VS Code: of the last-added cursor). The set is capped at 10,000 (`editor.multiCursorLimit`).
- **Edits** are `Replace { start, end, text }` batches, sorted and non-overlapping, applied in one
  pass front to back with a running offset; each cursor lands after its inserted text. This is the
  same observable result as VS Code's last-first application. Overlapping deletions (two cursors a
  character apart both backspacing) are merged first.
- **Undo** is a snapshot stack: the whole text plus the cursor set before each edit. One
  multi-cursor edit is one step and Ctrl+Z restores every cursor. Consecutive typing (or
  consecutive deleting) within 0.9 s merges into one step; a cursor move breaks the merge.
  Snapshots are cheap for scripts; the stack holds 300 steps.

## Keyboard

Windows bindings, matching VS Code. None collide with Cobalt's existing commands (checked against
`commands.rs`; Ctrl+L stays "estimated plan", Ctrl+Enter stays "run current statement").

| Keys | Action |
|---|---|
| Ctrl+Alt+Up / Down | Add a cursor above / below **each** cursor (a selection adds the same column range on the next line); at the first/last line that cursor adds nothing |
| Shift+Alt+I | One cursor at the end of every line under the selection |
| Ctrl+D | Empty cursor: select the word at each cursor (whole word, match case from then on). Selection: add the next occurrence after the last selection, wrapping; nothing when none is left |
| Ctrl+Shift+L | Select every occurrence (primary stays where invoked) |
| Escape | With several cursors: keep the primary only (its selection intact). With one selection: collapse it. (The completion popup and the find bar take Escape first, as before) |
| Arrows, Home, End, PageUp/Down, Ctrl+Left/Right (word), Shift variants | Applied to every cursor. Home toggles between the first non-blank and column 1; End is line end; Ctrl+Home/End collapse everything to the document ends |
| Ctrl+Up/Down | Document start / end (egui convention kept) |
| Typing, Backspace, Delete, Ctrl+Backspace/Delete (word), Enter | Per cursor. Enter copies the line's leading whitespace |
| Tab / Shift+Tab | Tab inserts an indent (spaces to the next stop, or a tab per settings) at empty cursors and single-line selections; with a multi-line selection it indents those lines. Shift+Tab always outdents the lines of each selection |
| Ctrl+C / Ctrl+X / Ctrl+V | See Clipboard |
| Ctrl+Z / Ctrl+Y / Ctrl+Shift+Z | Undo / redo (all cursors restored) |
| Alt+Up / Alt+Down | Move the lines under each selection |
| Shift+Alt+Up / Down | Copy the lines under each selection above / below (cursor lands on the copy) |
| Ctrl+Shift+K | Delete the lines under each selection |
| Ctrl+A | Select all (one selection) |

Not implemented in this pass (noted for later): Ctrl+U cursor undo, Ctrl+K Ctrl+D, Shift+Alt+Left/Right smart select, keyboard column select (Ctrl+Shift+Alt+Arrows), `editor.multiCursorModifier = ctrlCmd`.

## Mouse

Selection happens on **press**, never on release. The widget counts presses itself: a press
within 400 ms and a few pixels of the previous one raises the count (1 → 2 → 3, never skipping).

| Gesture | Effect |
|---|---|
| Press | Cursor at the point; Shift+press extends the primary's head there |
| Press 2 (double) | Select the word under the pointer; **dragging extends by whole words** from the anchor word (the founder's gesture) |
| Press 3 (triple) | Select the whole line; dragging extends by whole lines |
| Drag | Extends the head at the current granularity (char / word / line) |
| Alt+press | Add a cursor at the point; dragging extends only that new cursor. Alt+press inside an existing selection removes that cursor (VS Code's toggle), only while two or more exist |
| Shift+Alt+drag | Column (box) selection: one selection per row between the anchor row and the pointer row, between the two x positions. Rows whose text does not reach the box's left edge are skipped; if every row is skipped, an empty cursor goes to each row's end |

## Clipboard

VS Code's `emptySelectionClipboard` and `multiCursorPaste: spread`:

- **Copy / Cut**: one piece per selection, joined with newlines. An empty selection contributes its
  whole line plus newline; cut deletes that line. A copy from a *single* empty cursor is remembered
  as a "line copy".
- **Paste**: a line copy pasted at empty cursors goes in above each cursor's line. Otherwise the
  clipboard (one trailing newline stripped) is split into lines: with more than one cursor and as
  many lines as cursors, one line per cursor; else the whole text at every cursor. The system
  clipboard carries plain text only, so N-cursor copy → N-cursor paste round-trips through the
  line count, as VS Code does when its in-memory metadata is missing.

## Rendering

- All cursors blink in phase; the blink restarts solid on every interaction. Secondary cursors
  are drawn like the primary (VS Code differentiates only by theme colour).
- Every selection is painted; the current-statement highlight follows the primary.
- Status bar: `Ln n, Col m`, `Ln n, Col m (k selected)`, `n selections`, `n selections (k characters selected)`.
- Row geometry uses the font's row height for every row, including the zero-height rows egui
  reports for empty text and for the empty row after a trailing newline. That fixes the
  line-number-in-the-margin and invisible-caret-on-an-empty-line reports.

## Compatibility notes

- `EditorState.cursor` / `selection` still mirror the primary selection each frame; commands that
  act on "the selection" (Run selection, toggle comment, block comment, Insert CREATE INDEX) act
  on the primary. Making those multi-cursor-aware is a follow-on.
- Hot-exit snapshots keep saving the primary cursor.
- IME: commit text is inserted at every cursor; pre-edit text is not drawn inline (egui's
  `TextEdit` did, minimally). The IME candidate window is positioned at the primary caret.
- Accessibility: the widget reports itself as a text edit named "editor"; the text is not
  mirrored into the AccessKit tree every frame (it was not before either for large scripts).
