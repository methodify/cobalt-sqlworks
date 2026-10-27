# Editor groups: tab reordering, split views, and what comes with them

*Status: design for the founder's alignment, 2026-10-27. Requested by the team: drag tabs to
reorder, and split the editor area side by side (A | B) or stacked (A / B), as VS Code and
Azure Data Studio did. Decisions for the founder are in §6; slices in §5.*

## 1. The idea in one paragraph

Today Cobalt has one strip of tabs in a fixed order and one editor area; every query, notebook,
Explore, Diff and dashboard is a tab in that strip. The team is reaching for the two things
every editor since VS Code taught them to expect from a strip: that a tab can be dragged to
where it belongs, and that two tabs can be looked at at once. The second one is the real
feature. In a SQL client the pairs people hold in their head are concrete: the query and the
procedure it calls; the same query on dev and on prod; a query and the Explore pivot of its
result; a monitoring query that refreshes while the real work happens next to it; a notebook
beside the Local query that joins its cells. This slate turns the single strip into **editor
groups**: a tree of groups, each with its own strip and its own editor-plus-results, split
left/right or top/bottom to any depth, created by dragging a tab to an edge, from a tab's menu,
or with a key, and put back together just as easily. Around that core it sweeps in the things
the same hands reach for next: tabs that stay where you put them across restarts, results
beside the editor instead of under it, preview tabs that do not pile up when browsing files and
history, keys for every move, and an agent that can drive all of it.

## 2. What exists that this builds on (taking stock, 0.11.4)

- **One strip, one list.** `AppState.tabs: Vec<EditorTab>` in creation order, `active_tab` an
  index into it; the strip (`ui/shell.rs::tab_strip`) draws every tab with its kind glyph,
  connection colour stripe, running spinner, pin, close, middle-click close, double-click rename,
  a context menu (Rename, Pin, Close, Close others, Close all), horizontal scrolling with the
  scroll bar hidden, and the `+` menu with every tab kind and the configured default. Indices
  are used throughout `ops.rs`, the agent (`activate_tab {tab: index}`) and the a11y labels
  (`tab <title>`).
- **Per-tab editor / results split.** `editor_area` lays out the active tab: toolbar, editor,
  a draggable separator (`results_fraction` per tab), the results pane, and "Maximize this
  result set" which hides the editor. Every kind of tab (SQL, Spark, DAX, Local, Explore, Diff,
  notebook, dashboard, plan) renders through this one function from `f.state.active_tab`.
- **Hot exit.** `ops::snapshot_tabs` writes every open tab to `tab_snapshots`; restore reads
  them back in row order, so order survives a restart only because nothing reorders.
- **Drag and drop precedent.** Explore's field chips use `egui::DragAndDrop` with a payload type,
  a drag threshold and drop targets; the agent drives drags with stepped pointer moves (a known
  gotcha: egui's drag threshold needs real motion).
- **Keys.** Ctrl+Tab / Ctrl+Shift+Tab (next / previous tab), Ctrl+W, Ctrl+Shift+T (reopen).
  Ctrl+\, Ctrl+1..3, Alt+1..9, Ctrl+Alt+arrows and Ctrl+Shift+PageUp/PageDown are all free.
- **Windows.** One native viewport; egui 0.36 can open more (`ViewportBuilder`), unused so far.

## 3. Product shape

### 3.1 Reordering

Press and drag a tab along its strip: the other tabs part to show an insertion caret, the tab
lands where it is dropped. Pinned tabs stay at the left and reorder among themselves; a pinned
tab cannot be dragged past the first unpinned one, and vice versa. The order is the order the
strip, Ctrl+Tab and the snapshots use, so it survives a restart. Keyboard: Ctrl+Shift+PageUp /
PageDown move the active tab one place left / right (VS Code's keys). The `∨` tab list at the
right of the strip keeps listing tabs in strip order.

### 3.2 Editor groups

The editor area becomes a **tree of groups**. A group is a strip plus the editor and results of
its active tab, exactly what the whole area is today. A split is a row or a column of children
with draggable gutters between them; a child is a group or another split, so A | B, A / B, and
(A | B) / C are all one model. The group that has the keyboard is the **active group**: its
strip shows the accent underline, its active tab is `state.active_tab`, and every command that
says "the active tab" (Run, Format, Save, Change connection, Explore this result…) keeps meaning
exactly that. New tabs open in the active group (§6.3 says where in its strip).

**Making a group.** Four ways, all ending in the same `move_tab(tab, target)`:

- Drag a tab over the editor area of any group. Five drop zones appear (VS Code's): the middle
  means "into this group's strip", the four edges mean "split this group on that side and put
  the tab there". The zone under the pointer tints; release to drop.
- Drag a tab onto another group's strip: an insertion caret, like reordering.
- The tab's context menu: *Split right*, *Split down*, *Move to group ›* (the groups listed by
  their active tab's title), and, on a strip's `+` menu, *New query here*.
- Keys: Ctrl+\ splits the active group to the right and moves the active tab there;
  Ctrl+Shift+\ splits down. Ctrl+Alt+→ / ← / ↓ / ↑ move the active tab to the neighbouring
  group, creating one at the edge when there is none (VS Code's "move editor into next group").

A split **moves** the tab; it does not open a second view of the same document (§6.1). An
empty group that would be left behind closes, and a split with one child collapses into its
parent, so the tree never holds an empty group or a redundant split. The last group is never
removed: with no tabs it shows the welcome pane.

**Living in groups.** Gutters drag; double-click a gutter to equalise its row or column.
*Maximize group* (Ctrl+Shift+M, and a button at the right of every strip) gives one group the
whole area and keeps the tree; the same key or button restores it. *Close group* on the strip's
`∨` menu closes its tabs with the usual confirmation for unsaved ones. Ctrl+1, Ctrl+2, Ctrl+3
focus the first, second and third group in reading order; clicking anywhere in a group makes it
active. Alt+1 … Alt+9 activate the n-th tab of the active group. Ctrl+Tab cycles within the
active group. The empty group's welcome pane offers the same *New query / notebook / Spark /
DAX / Local* buttons as the global one, so any group can start work. Dashboards are tabs like
any other: a dashboard beside a query is a natural pair.

### 3.3 Results beside the editor

The reason to split is often "I want to see two things"; sometimes the second thing is the
result of the first. A per-tab toggle on the results toolbar, *Results beside the editor*, turns
the editor/results split sideways (editor | results) using the same draggable separator and the
same `results_fraction`. It is remembered in the tab's snapshot; Settings › Appearance has the
default for new tabs. Combined with groups this gives the wide-monitor layout people build in
SSMS by hand: query and grid side by side on the left, a second query on the right.

### 3.4 Preview tabs

Browsing the Files sidebar or History opens a new tab per click, and the strip fills with
things nobody meant to keep. A **preview tab** (italic title, as in VS Code and ADS) opens on a
single click and is reused by the next single click; it becomes a normal tab when it is edited,
run, pinned, double-clicked, or dragged. Off by default or on (§6.6); the Files sidebar's
double-click and *Open* always open a kept tab.

### 3.5 Keys and commands

| Action | Key | Where else |
| --- | --- | --- |
| Move tab left / right in its strip | Ctrl+Shift+PageUp / PageDown | drag |
| Split right / down (moves the active tab) | Ctrl+\ / Ctrl+Shift+\ | tab menu, drop zones |
| Move tab to the group left / right / up / down | Ctrl+Alt+← → ↑ ↓ | tab menu › Move to group |
| Focus group 1 / 2 / 3 | Ctrl+1 / 2 / 3 | click |
| Tab n of the active group | Alt+1 … Alt+9 | click |
| Maximize / restore group | Ctrl+Shift+M | strip button |
| Equalise a split | | double-click a gutter |
| Close group | | strip `∨` menu |
| Results beside the editor | | results toolbar, Settings › Appearance |

All of them are commands in the palette and the keymap, so they can be rebound; the Alt-digit
keys rely on the 0.11.4 rule that a consumed shortcut takes its text with it.

### 3.6 Persistence

The layout tree (splits, ratios, group membership and order, active group, maximized group,
each tab's results-beside flag) is written with the tab snapshots on every hot-exit tick and at
close, and restored with them. Tab snapshots gain a position so reordering survives. An older
build reading a newer store ignores the layout and shows every tab in one group; a newer build
reading an older store does the same. Nothing else changes in the store.

### 3.7 Agent

`state.layout` describes the tree: `{root: {split: "row"|"col", children: [...], ratios}
| {group: n}}`, `groups: [{id, tabs: [indices], active, maximized}]`, `active_group`. Tab
indices stay global (they index `state.tabs` as today), so every existing script keeps working.
New verbs: `tab {action: move, tab?, group?, index?}`, `tab {action: split, tab?, side:
left|right|up|down}`, `group {action: focus|close|maximize|equalize, group}`, `tab {action:
results_side, on}`. Labels: the strip's tabs stay `tab <title>`; when the same title is in two
groups the a11y label is `tab <title> (group n)`; drop zones are `drop centre group n`, `drop
left group n`, …; gutters `gutter n`. Drags are the stepped pointer sequence Explore already
uses.

## 4. Architecture

**The model lives in cobalt-app** (`layout.rs`), next to `state.rs`:

```rust
pub struct GroupId(u32);
pub enum Node { Group(GroupId), Split { axis: Axis, children: Vec<Node>, ratios: Vec<f32> } }
pub struct Group { tabs: Vec<TabId>, active: Option<TabId> }
pub struct Layout { root: Node, groups: BTreeMap<GroupId, Group>, active: GroupId, maximized: Option<GroupId>, next_id: u32 }
```

`state.tabs` stays the flat owner of every `EditorTab`, in strip order of the whole app (the
order used by snapshots and by the `∨` list of one group is the group's own `tabs` order). The
layout holds ids, never indices, so moving a tab between groups touches nothing else.
`state.active_tab` is kept as the derived index of the active group's active tab and set in one
place (`layout.sync_active(&tabs)`), so the hundreds of sites that read it are untouched. The
layout is normalised after every mutation: empty groups go (the last one stays), single-child
splits collapse, ratios renormalise, `active` always names a live group. Every mutation is one
of `move_tab(tab, Target::Strip { group, index } | Target::Split { group, side })`,
`close_group`, `set_active_group`, `toggle_maximize`, `equalize`; the drag, menu, key and agent
paths all call these.

**Rendering.** `shell::show` replaces the single `tab_strip + editor_area` with
`editor_groups(ui, f)`, which walks the tree: a `Split` allocates its rect, cuts it by the
ratios along its axis with 6 px gutters (each a `Sense::drag` interact that adjusts the two
neighbouring ratios), and recurses into child `Ui`s built on those rects; a `Group` draws its
strip (today's `tab_strip`, taking the group) and `editor_area(ui, f, idx)` for its active tab.
`editor_area` already does everything inside the rect it is given (its own separator, its own
scroll areas), so it needs only its tab index as a parameter instead of reading `active_tab`,
plus the group in a few widget ids that are keyed by tab today and stay unique. The welcome pane
becomes the empty group's content. Only visible groups render, and a maximized group renders
alone; each visible group with results streams its own grid exactly as the single area does now.

**Drag and drop.** The tab rect senses click-and-drag; past egui's threshold it sets a
`DragTab { tab, from }` payload and draws the title in a floating label at the pointer. While a
payload exists, each strip computes the insertion index from the pointer's x and paints the
caret; each group's editor rect computes which of the five zones the pointer is in and paints
the tint. On release the hovered target wins; releasing anywhere else is a no-op. Pin rules are
enforced in `move_tab`, not in the strips.

**Store.** `tab_snapshots` gains `position INTEGER`; a `layout` row (JSON) in the settings
table of the SQLite store, written and read with the snapshots. Restore maps snapshot tab ids
to the new tabs and drops ids it cannot find.

**Why not egui_tiles or egui_dock.** Both give a tree, gutters and drags, but with their own
strips and tab semantics; Cobalt's strip has pins, kinds, connection stripes, a11y labels and
menus that would have to be reimplemented inside their tab renderers, and both track egui's
release train, which Cobalt pins for `egui_agent`. The tree Cobalt needs is two node kinds and
five operations, a few hundred lines that it owns.

**Second windows.** egui 0.36's native viewports can host a group in another OS window
(tear a tab off onto a second monitor). The state model above makes that possible later: a
viewport is a root with its own tree. It is not in this slate (§6.7).

## 5. Slices

- **A. Order and groups.** The layout model and normalisation with unit tests; the strip per
  group; drag to reorder within a strip; Split right / down and Move to group from the tab menu
  and keys; gutters and equalise; maximize and close group; focus keys; empty-group welcome;
  snapshots with position and layout; agent `state.layout`, `tab move/split`, `group`. Verified
  by script against the dev build, and the streaming-grid probe repeated with two groups each
  holding a million-row result.
- **B. Drag everywhere, results beside.** The five drop zones over editor areas and drag between
  strips; drop a file from the Files sidebar onto a group; *Results beside the editor* per tab
  with the setting; stepped-drag agent verification.
- **C. Preview tabs and strip polish.** Preview tabs from Files and History; the `∨` list per
  group with Close group; scroll-to-active when a tab is activated by key; strip overflow that
  keeps the active tab visible.
- **D (optional, after the founder's call). Second windows.** A group in its own viewport.

## 6. Decisions for the founder (recommendations in bold)

1. **A split moves the tab; there is no second view of one document.** VS Code's Ctrl+\
   duplicates the editor; for a query tab that would mean two cursors over one text and one run,
   which the tab model does not have and which few people want for SQL. *New query here* on the
   new group's `+` covers "the same connection, beside it".
2. **A custom layout tree, not egui_tiles / egui_dock** (§4): keeps the strip, the a11y labels
   and the egui pin.
3. **New tabs open to the right of the active tab, not at the end** (VS Code's default), in the
   active group, with a setting to put them at the end instead. Today they go to the end, which
   with a long strip puts the new tab out of sight.
4. **Keys as in §3.5** (VS Code's where VS Code has one). Ctrl+Shift+M for maximize is Cobalt's
   own choice, since VS Code's chord (Ctrl+K Z) does not fit the keymap.
5. **Results beside the editor is a per-tab toggle with a global default, off by default.**
6. **Preview tabs on by default** for single clicks in Files and History, as ADS had them; a
   Settings › Appearance switch turns them off.
7. **Second windows are not in this slate.** The model leaves the door open; the win is real on
   two monitors, but it is a different kind of work (viewport lifecycle, focus, snapshots per
   window) and deserves its own decision once groups have been used.
8. **Any depth of nesting, but the UI only offers right / down and the four edge zones.** Grids
   like (A | B) / C come from two operations; nobody needs a "3 × 3" dialog.

## 7. Risks

- **Indices.** `active_tab` and tab indices are everywhere; the derived-index rule in §4 keeps
  them valid, and the unit tests pin the invariants (every tab in exactly one group, no empty
  group, no single-child split, active group live). Reordering `state.tabs` itself never
  happens; only group membership and the groups' own order change.
- **Widget ids.** Editors, grids and scroll areas use ids keyed by tab id, which stay unique
  across groups; anything keyed by "the editor" alone (the results separator id is `("split",
  tab.id)`, fine) must include the tab, else two groups fight over one state. The build will
  show them: egui warns on id clashes in debug builds.
- **Focus.** Keyboard focus moves with the active group; a click in a group's editor or grid
  activates the group before the editor takes the click, or Run would hit the wrong tab.
- **Performance.** Two or three visible grids streaming at once each cost what one costs now;
  the probe in slice A measures it, and only visible groups render.
- **Drag threshold and the agent.** Known; the stepped-move pattern from Explore covers it.
- **Store migration.** The `position` column is additive; the layout row is ignored by older
  builds, which show one group.

## 8. Before execution

Nothing from the founder's side except the decisions in §6. Verification needs only the dev
build and the Docker SQL Server; the perf probe exists.
