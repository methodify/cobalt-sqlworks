# Changelog

## Unreleased — 0.7.3

- **Cell run queue, visible and cancellable.** Running several cells (Run all, Run cells above,
  Shift+Enter in a row) queues them in notebook order; a waiting cell shows an hourglass in place
  of its play button, and clicking it (or "Cancel queued run" in the cell's menu) takes the cell
  back to inert. A running cell's button is Stop.
- **Per-cell run menu** (the small chevron under the play button): Run cell, Run selected code,
  Run all above this cell, Run this cell and all below.
- **Ctrl+Shift+Enter runs the selected code** in the focused cell (the whole cell when nothing is
  selected); the cell's source is untouched and its outputs show the selection's result. Ctrl+Enter
  runs the focused cell as before. In a query tab Ctrl+Shift+Enter runs the selection.
  Agent: `notebook {action: dequeue, index}`, `notebook {action: run_selection, index?, selection?}`.
- **PySpark cells are highlighted as Python**, not T-SQL: `#` comments (an apostrophe in one no
  longer opens a string that swallows the rest of the cell), string prefixes and triple quotes,
  numbers, keywords, common builtins (`spark`, `display`, `F`, `T`…), decorators and `%magics`.
  SQL completion and current-statement tracking stay on SQL cells; Markdown cells edit as plain
  text; Toggle Line Comment uses `#` in Python cells.

## 0.7.2 — 2026-10-06

- **Libraries for the Spark environment** (Settings → Notebooks & Spark → Libraries). Python
  packages as PyPI requirement specs or wheel files, installed into the runtime's environment
  with uv and importable in Spark sessions; Java libraries as jar files or Maven coordinates
  (`group:artifact:version`, fetched from Maven Central into the runtime folder), put on the Spark
  classpath when a session starts. The page shows what is installed or fetched; **Install
  libraries** saves the lists and runs the job with a log. Maven coordinates bring only the
  artifact itself — add its dependencies too.
- **Opening a notebook from Fabric shows a tab at once** with a spinner and the item's name while
  the definition is fetched; the explorer row spins too. A failed fetch stays in the tab with the
  reason, Retry and Close instead of a toast.
- **Session menu on the notebook toolbar.** The Local Spark chip's menu now manages the session —
  Start or Restart, Stop, Interrupt the running cell, Session log, Lakehouse shadows — alongside
  the kernel choice. The status-bar entry keeps the same menu.
- Agent: `runtime {action: libraries}`.

## 0.7.1 — 2026-10-06

- **Permissions that grow with the app.** A sign-in only ever carries the permissions consented
  at the time, so a refresh token from an older Cobalt could never pick up a permission the app
  registration gained later (Item.ReadWrite.All for notebooks was the first). The Fabric panel now
  checks the token's scopes against what Cobalt needs and shows a banner naming the missing ones
  with **Grant permissions…**, which runs the browser flow with `prompt=consent` so Entra shows the
  permissions screen again. Any 403 InsufficientScopes from Fabric raises the same banner, and the
  error toasts point at it. When a tenant requires an administrator to approve, the banner offers
  the admin-consent link to copy. **Sign out** now forgets the account for real (refresh token and
  cached tokens), and the sign-in after it always goes through the browser with the account
  picker. Agent: `fabric {action: sign_in|sign_out|grant|refresh}`, `fabric_scopes` reports
  `missing`.
- **Settings reorganised** into pages — Appearance, Editor, Query execution, Results & export,
  Connections, Notebooks & Spark, Keyboard, Advanced — with a navigation list on the left; "Reset
  this page to defaults" replaces the all-or-nothing reset; the last page is remembered.
- **Window chrome.** Secondary windows (Settings, cell viewer, column profile, session log,
  lakehouse shadows, About, shortcuts) share one look: a flat left-aligned title bar with a plain
  close button, rounded corners and a soft shadow, instead of egui's collapsible window with the
  centred title and triangle. Menus and popups got the softer shadow too.

## 0.7.0 — 2026-10-06 — Notebooks and local Spark

The notebooks release (`docs/design/notebooks_roadmap.md`, D009), built as three slates and
shipped together: SQL notebooks and the Cobalt-managed local Spark runtime, PySpark cells on
that runtime, and OneLake-bound sessions with Fabric notebooks in the explorer.

### SQL notebooks and the Spark runtime

- **Notebooks**: File → New Notebook (Ctrl+Shift+N), or open any `.ipynb` (Azure Data Studio SQL
  notebooks and Fabric notebooks included) or a Fabric Git `notebook-content.py`. Markdown cells
  render in place (double-click or Enter to edit, Shift+Enter to render); SQL cells use the Cobalt
  editor (IntelliSense against the tab's connection, multi-cursor, snippets) and run through the
  tab's connection with the full result grid under each cell: sort/filter, copy, find, profile
  columns, Save results as…, Save as table, Open in Excel, open in its own tab. Run cell
  (Ctrl+Enter), run and advance (Shift+Enter), run and insert (Alt+Enter), Run all (F5), Run cells
  above; a failing cell stops the queue. Jupyter's command-mode keys: ↑/↓, Enter, A, B, M, Y, D D,
  Z (undo delete). Outputs are saved in the file — an Arrow payload (Settings → Notebooks → rows
  per result set) plus HTML/Markdown/text previews — so grids come back on reopen and other tools
  still render something. Notebooks take part in hot exit, the Files sidebar (plus "New notebook
  here"), the command line and the `.ipynb` file association. Export as an HTML page or Markdown
  with results embedded.
- **Spark runtime (Settings → Spark runtime)**: Cobalt provisions a local Spark matching a Fabric
  runtime (`fabric-2.0` = Spark 4.1.1 / Delta 4.2.0 / Python 3.13, or `fabric-1.3`) into its own
  app-data folder: a pinned `uv`, a uv-managed Python (3.11 on Windows, SPARK-53759), a virtual
  environment with `local-spark-mcp` and its pyspark/delta-spark, and Microsoft Build of OpenJDK
  (or Temurin) — never Oracle. "Use what I have" adopts a JDK or uv already on the machine; every
  download is SHA-256 checked and resumable; a first Spark session pre-warms the Delta/Hadoop jars
  into a local Ivy cache; the smoke test runs `SELECT 1` through local-spark-mcp's worker protocol.
  Re-check, Remove runtime, log file. Nothing is downloaded until you click Install.
- Internals: new crates `cobalt-notebook` (nbformat 4 + Fabric metadata, Git `.py` form, HTML/MD
  export) and `cobalt-runtime` (manifest, detection, provisioning, worker protocol client); the
  editor widget is now hosted (`EditorHost`) so a query tab and a notebook cell share one editor.
  Agent verbs `notebook {…}` and `runtime {…}`; `state` tabs carry `kind` and `cells`.

### PySpark cells on the local Spark kernel

- **Local Spark kernel**: a notebook's kernel button (toolbar, right) now offers *Local Spark*
  next to the tab's connection; PySpark notebooks pick it by default. Code cells run on the
  runtime from Settings → Spark runtime through local-spark-mcp's worker: Python cells via
  `run_code` with a persistent IPython namespace (`spark`, `F`, `T`, `notebookutils`…), SQL and
  `%%sql` cells via `spark.sql` (several statements per cell; the last one is shown). The first
  cell starts the session (about 25 s here) and the status bar shows it: version, uptime, running;
  click for restart / stop / the session log (Spark, py4j and Ivy output). Query menu: Restart /
  Stop Local Spark Session, Local Spark Session Log.
- **DataFrames in the grid**: `display(df)`, a bare DataFrame expression, pandas DataFrames and
  `%%sql` results arrive as Arrow (typed, not JSON rows) and open in the normal results grid with
  everything that implies (sort/filter, copy, exports, Save as table…). `display()` shows 1,000 rows
  like Fabric unless given `limit=`; bare expressions and `%%sql` use Settings → Notebooks → "Rows a
  Spark DataFrame brings back" (10,000). Outputs save into the `.ipynb` like SQL results.
- **Errors and output**: stdout, stderr (Spark log noise filtered), IPython's traceback for the
  failing line, and a failing cell stops the queue. Stop / Alt+C interrupts the running cell — the
  worker protocol has no interrupt yet, so the session is killed and the next cell starts a fresh
  one (asked of local-spark-mcp in `docs/requests/local-spark-mcp.md`).
- Agent: `kernel {action: status|start|stop|restart|interrupt|log}`, `notebook set_kernel`,
  `state.kernel`.

### OneLake-bound sessions, shadows and Fabric notebooks

- **Lakehouse-bound Spark sessions.** A notebook on the Local Spark kernel can be bound to a
  Fabric workspace, a default lakehouse and a write mode (lakehouse button on the toolbar;
  Fabric notebooks pick it up from their metadata and write it back). The session registers every
  lakehouse in the workspace as a Spark database, so `test.sales_import` or `spark.table(...)`
  just work; OneLake is read through a token endpoint inside Cobalt that serves the signed-in
  Fabric account's storage token to the JVM (loopback only, per-start secret, no `az login`).
  The first touch of a table makes a Delta **shallow clone** under the runtime folder:
  **Sandbox** (default) reads and writes the clone and never touches OneLake; **Read only**
  refuses writes; **Write through** makes tables external OneLake tables so writes land in the
  lakehouse. A session keeps its binding until restarted; the toolbar warns when a notebook's
  binding differs and offers the restart.
- **Lakehouse shadows** (kernel menu, Query → Lakehouse Shadows…, or the lakehouse button): the
  clones in the running session with their state (read / written) and version; Discard all,
  Discard written, per-table Discard and Rewind to the clone's first version.
- **Fabric notebooks in the explorer.** Workspaces list their Notebook items; double-click opens
  one bound to the item (Save writes back after a confirmation through `updateDefinition`; Save
  As… makes a local copy and detaches), "Open a copy" opens it detached, plus Open in Fabric
  portal. Both the `.ipynb` and the Git `.py` definition forms are read. Needs the delegated
  permission `Item.ReadWrite.All` on the app registration (reading and writing item definitions);
  listing works with `Item.Read.All`, and the error says what to add when the scope is missing.
- Fabric client: generic item listing, POST with long-running-operation polling,
  `getDefinition` / `updateDefinition` / create notebook. Agent: `fabric_notebooks`,
  `notebook {open_fabric|save_fabric|set_lakehouse}`, `shadows`, `fabric_scopes`; `state.toasts`.

### Polish

- **Modal hotkeys**: the confirmation dialogs (unsaved changes, save to Fabric, delete
  connection/group, read-only guard) take Windows-style mnemonics — the underlined letter with
  Alt (Alt+S Save, Alt+N Don't save, Alt+D Delete, Alt+R Run anyway), Enter for the default
  action where it is safe (Save), Esc for Cancel — so closing a stack of dirty tabs is
  Ctrl+W, Alt+N, Ctrl+W, Alt+N.
- Reopen Closed Tab (Ctrl+Shift+T) brings a notebook back as a notebook, and a notebook opened
  from Fabric keeps its item binding across hot exit and reopen.
- A result grid in a notebook cell no longer paints over the tab strip and toolbar when the
  notebook is scrolled past it (the grid is clipped to the notebook's viewport; the same clip
  applies to the results pane). A notebook bound to a lakehouse resolves the workspace and
  lakehouse names itself instead of showing ids until the Fabric panel is opened. Agent:
  `pointer {action: scroll, x, y, dy}`.
- Notebook cell layout: the editor is a bordered box whose line-number band ends with the code
  (it used to spill into the output), and outputs sit in an indented block under it with a rule
  on the left — consecutive lines of PRINT/stdout as one text block, errors as a red block, result
  grids below with their row count and timing.


## 0.6.1 — 2026-10-05

- **Ctrl+F follows the focus**: with a result grid focused (click into it) Ctrl+F opens Find in
  results; in the editor it opens the editor's find bar as before. Find in results is also on the
  results toolbar and in the grid's context menu, so it is discoverable without the palette.

## 0.6.0 — 2026-10-05

The remaining V1.x backlog, plus two V2 items that fit:

- **Results**: the find bar gained *match case*, *whole word* and *regular expression* toggles, a
  live match count, Shift+Enter for the previous match, and every match is highlighted in the
  grid. A **totals row** (grid context menu → Totals row: Sum, Avg, Min, Max, Count or Distinct)
  sits under the headers and follows filters and new rows. **Profile columns** (toolbar, context
  menu, Results menu) opens a window with nulls, distinct, min/max/avg, top values and a
  distribution bar per column.
- **Editor**: drag a table, view or procedure from the Servers tree into the editor to insert its
  bracketed name at the drop point. **Format document** now follows Settings → Editor (keyword
  case, indent, blank lines between statements).
- **Execution**: the full session SET surface — ANSI_NULLS, ANSI_PADDING, ANSI_WARNINGS,
  QUOTED_IDENTIFIER, CONCAT_NULL_YIELDS_NULL, NUMERIC_ROUNDABORT, IMPLICIT_TRANSACTIONS,
  LOCK_TIMEOUT and DEADLOCK_PRIORITY — as tri-state options per tab (Query → Execution options)
  with defaults in Settings; "(default)" leaves the server's setting alone.
- **Query shortcuts** (Settings → Query shortcuts): Alt+F1 runs `sp_help` on the selection or the
  word at the caret, Ctrl+1 `sp_who`, Ctrl+2 `sp_lock`, Ctrl+3 `sp_helptext`; add your own with
  `{sel}` as the placeholder.
- **Editable keyboard shortcuts** (Settings → Keyboard shortcuts): every command's binding can be
  changed or cleared; invalid combinations are flagged, Reset restores the default.
- **Files sidebar** (left rail, View → Show Files): open a folder of .sql files and browse it as a
  tree; click opens, right-click reveals in the file manager or creates a new file; the folder is
  remembered.
- **Getting-started pane** beside a fresh empty query tab: connect, open, import, Fabric, recent
  connections, tips; hide it from the pane or Settings → Appearance; Help → Welcome brings it
  back. Optional **SPID in tab titles** (Settings → Appearance).
- **Plan viewer**: a *Highlight by* choice colours operators by cost, estimated rows, actual rows,
  elapsed time or logical reads, and *Tree* shows the operator tree as indented text with the key
  numbers (click selects the node; Find node highlights there too).
- **Authentication**: Entra service principals can use a **certificate** (a PEM with the
  certificate and its unencrypted RSA key; the client assertion is signed locally) instead of a
  secret, and a new **Managed identity** method takes a token from the Azure instance metadata
  service or the App Service identity endpoint (system- or user-assigned). Connection strings with
  `Authentication=Active Directory Managed Identity` map to it. Neither could be exercised against
  a live tenant from the dev box; the assertion builder and PEM parser are unit-tested.
- Dev: agent verbs `grid {totals|profile|find|find_state}`, `files_root`, `plan_view`.

## 0.5.0 — 2026-10-01

- **Save results as table** (grid context menu, results toolbar, Results menu): load a result set
  — or the selected cells — into a new or existing table on the database of *any connected tab*,
  not only the one the query ran on, so results can be copied across servers (prod → scratch,
  SQL Server → Fabric). Same dialog as Import Data: edit names and types, create or append, one
  transaction with progress and cancel. Agent: `results_to_table {table, schema?, existing?,
  target?}`.
- **Selection summary in the status bar**: select two or more cells and the status bar shows
  Count, and for numeric cells Sum, Avg, Min and Max, plus Distinct and Null counts (first 200,000
  cells).
- **Snippets with tab stops, and your own snippets.** Accepting a snippet (e.g. `sel`, `cte`)
  selects its first placeholder; Tab and Shift+Tab walk the placeholders, Escape leaves. A
  `snippets.toml` next to settings.toml (a commented template is created) adds your own, with the
  same `${1:placeholder}` / `$0` syntax, reloaded when the file changes. Suggestions do not pop up
  while placeholders are being filled in (Ctrl+Space still works).
- Editor: toggle comment, block comment and Run selection apply to every cursor (Run selection
  runs all selected ranges in document order); **keyboard column selection** with
  Ctrl+Shift+Alt+arrows; a "No more matches" notice when Ctrl+D runs out.
- Object explorer: **Filter…** on the Tables/Views/Procedures/… folders (context menu) narrows a
  single folder by name or schema, with "n of m" in the folder row; **Group objects by schema**
  (server context menu) nests a folder's objects under one row per schema.
- Describe hover shows the table's **row count and reserved size** (from the partition stats).
- Results: **Open in Excel** (toolbar, context menu, Results menu) writes a temporary .xlsx and
  opens it with the default app.
- **Import Data from File can write to any export target.** The Import dialog has a Destination
  choice: a table in the tab's database (bulk insert, as before) or *File or lakehouse…*, which
  hands the file to the export dialog: CSV/TSV, JSON, JSON Lines, XML, Markdown, Excel, Parquet,
  Arrow or Delta, to a local path or a OneLake lakehouse (Delta table or Files). Rows stream
  from the file straight to the writer with the column names, types and exclusions set in the
  dialog (a CSV of strings becomes a typed Parquet file, for instance). No connection is needed
  for file targets, so the dialog opens on a disconnected tab too. Agent: `import {…,
  destination: "file"}` + `import_to {format, path | lakehouse, name}`.
- Fixed: a tab whose remembered database had disappeared (a deleted lakehouse SQL endpoint, a
  dropped database) could not reconnect: Connect kept asking for the vanished database, failed,
  and the tab was dead until closed. A "database not found" login failure now forgets that
  database and connects to the profile's default once, with a toast saying so (a run queued for
  the old database is not replayed).

## 0.4.2 — 2026-09-30

- **Editor rewrite: multi-cursor editing and VS Code mouse selection.** The editor is now
  Cobalt's own widget (the model in `crates/cobalt-app/src/ui/editor/core.rs`, the design in
  `docs/design/editor_multicursor.md`) instead of egui's single-cursor `TextEdit`.
  - Multiple cursors: Ctrl+Alt+Up/Down adds a cursor above/below each cursor, Alt+Click adds
    (or removes) one, Shift+Alt+drag makes a column selection, Ctrl+D selects the word and then
    each next occurrence, Ctrl+Shift+L selects every occurrence, Shift+Alt+I puts a cursor at the
    end of every selected line, Escape drops the extra cursors. Every motion (Home/End, words,
    Up/Down with a sticky column, PageUp/Down, Shift variants) and every edit (typing, Backspace,
    Delete, Enter with auto-indent, Tab/Shift+Tab indent, paste) applies to all cursors, and one
    multi-cursor edit is one undo step. Copy of N selections pastes one line per cursor; a copy
    from a single empty cursor takes the whole line and pastes above.
  - Also new: Alt+Up/Down moves lines, Shift+Alt+Up/Down duplicates them, Ctrl+Shift+K deletes
    them, Home toggles between the first non-blank and column 1, Ctrl+Backspace/Delete delete
    words.
  - Mouse: selection happens on press. Double-press and drag extends by whole words, triple-press
    and drag by whole lines (the VS Code / ADS gesture); Shift+click extends.
  - The status bar reads "n selections (k characters selected)" with more than one cursor.
- Fixed: an empty editor drew its line number on the top edge of the gutter (egui reports an empty
  text as a zero-height row); the caret was invisible on an empty last line after clicking there.
- Fixed: keyboard shortcuts matched with extra modifiers (Ctrl+Shift+L ran "estimated plan" as if
  it were Ctrl+L); bindings now require the exact modifiers.
- Completion popup opens on typed text only (not after indent, paste or undo), and only plain
  Tab/Enter/arrows drive it (Shift+Tab outdents again).
- Release: every published file now carries a **GitHub artifact attestation** (Sigstore, keyless,
  from the workflow identity). Verify a download with
  `gh attestation verify <file> --repo methodify/cobalt-sqlworks`.
- Dev: agent `pointer` gained `alt`, `tripleclick`, `dbldrag` and `tripledrag`; new `paste` and
  `focus` verbs; `state` tabs carry `cursors`.

## 0.4.1 — 2026-09-24

- **Sessions survive long idle stretches.** Leaving a tab open for hours (a Fabric or Azure
  gateway, or a NAT, drops the idle TCP session) used to end in a run that spun, then failed with
  no message, a tab that still said "connected", a reconnect into the wrong database, and an empty
  database list whose Refresh button never worked. Now:
  - the TCP socket has keepalive on (30 s, as SqlClient sets it), so gateways and NATs keep the
    mapping and a vanished peer is noticed within seconds;
  - a tab idle for more than a minute pings the server before its next run; a dead connection is
    replaced in place, silently refreshing an expired Entra token from the stored refresh token,
    and the run proceeds — Messages says "The connection had been closed while idle (6 h 12 min);
    reconnected to *db* as SPID *n*". If the sign-in itself has expired, the tab reconnects through
    the normal sign-in and the run is repeated once connected;
  - transport-level errors and command timeouts appear in Messages (they only set the red badge
    before), and a session ended mid-run (severity 20+ errors such as "session is in the kill
    state", or an I/O error) flips the tab to disconnected at once instead of at the next run;
  - a reconnect goes back to the database the tab was in, not the profile's default;
  - the object explorer / database-list connection heals itself: an idle-dropped or expired
    metadata session is reopened with fresh credentials and the request retried, and the tree
    always holds the newest credentials from a tab connect.
- Dev: `COBALT_IDLE_PING_SECS` (default 60) and `COBALT_TEST_EXPIRED_TOKENS=1` knobs, agent verb
  `break_connection`, `databases` in the agent `state` tab JSON.

## 0.4.0 — 2026-09-22

- **Windows installer and zip now include a software renderer**: Mesa llvmpipe as a single
  `opengl32.dll` (from mmozeiko/build-mesa 26.2.3, MIT/Apache-2.0; licences ship next to the exe).
  Cobalt uses it only when the machine has no GPU, so VMs and RDP sessions go from ~4 fps to
  full speed; desktops with a GPU are unchanged. Also published separately as
  `cobalt-software-rendering-<version>-windows-x86_64.zip` for existing installs. Adds ~16 MB to
  the download and ~58 MB on disk.
- **Import Data from File** (File menu; a database's context menu in Servers): CSV/TSV/delimited
  text, Parquet and Arrow IPC into a new or an existing table. The file's shape is inferred
  (delimiter, header, column types) with a preview; every column's SQL type and nullability can be
  edited before loading; existing tables take their types from the server. Rows stream from the
  file through the tab's connection as a TDS bulk insert inside one transaction, with progress and
  Cancel (rollback). Measured: 50,000 mixed-type rows in 0.2 s locally.
- Editor: **per-batch timings in the gutter** after a run (red for a batch that failed), shown
  while the text is unchanged.
- Plan tab: a **warning badge** counts warnings and missing indexes; the plan header can insert
  the suggested CREATE INDEX at the cursor.
- Connection editor: paste an ADO.NET / SqlClient **connection string** and Apply to fill the
  fields (server, port, database, auth, encryption, options). The Server field no longer steals
  focus every frame.
- Results grid: **Exclude this value / Exclude selected values** next to Filter to selected values.
- History: a one-click **Run** button on every entry.
- **Renderer choice at start-up**. Without a GPU the only Direct3D adapter is Windows' WARP, which
  drew a frame in ~280 ms (3–4 fps). Cobalt now picks: a GPU → wgpu as before; no GPU plus a Mesa
  llvmpipe `opengl32.dll` next to `cobalt.exe` → OpenGL through Mesa (~5 ms a frame, measured);
  no GPU and no Mesa → WARP with a warning toast and a status-bar badge. Settings → Advanced →
  Renderer overrides the choice (`COBALT_RENDERER` too). See docs/alpha_notes.md → Running without
  a GPU.
- Adapter selection now prefers discrete > integrated > virtual > CPU explicitly and logs the pick.
- Dev: `COBALT_PERF=1` frame stats, `perf` / `spin` / `viewport` agent verbs, `COBALT_ADAPTER` and
  `COBALT_SPANS` diagnostics.

## 0.3.1 — 2026-09-21

- Object explorer: **Describe on hover** — hover a table, view or table type to see its columns
  with types and nullability (identity columns highlighted). Columns load on first hover and are
  cached on the node.
- **Go to Object** (Ctrl+Shift+O, or `#` in the command palette): fuzzy-find any table, view or
  procedure the app knows about — catalogs of connected tabs plus whatever the Servers tree has
  expanded — and open it (SELECT TOP 1000, or a script for procedures) in one keystroke.
- **Command-line launch**: `cobalt file.sql other.sqlplan -S <connection> -d <database>`. Files
  open in tabs; `-S` matching a saved connection (name or server) opens a query tab on it, an
  unknown server opens the connection editor pre-filled. The installers register `.sql` and
  `.sqlplan` so double-clicking a file opens it in Cobalt.
- Read-only guard: connections with the guard now show a **lock badge** on the tab title and the
  Servers row, not only in the toolbar.
- Save results / Run to File: the dialog **remembers the last format** you chose (as well as the
  folder), and the completion message in Messages has an **Open folder** button for local targets.

## 0.3.0 — 2026-09-21

- **Run to File** (Query menu, toolbar, Ctrl+Shift+F5): run a query straight into a file or a
  OneLake lakehouse without filling the grid. Every result set streams from the wire to the
  target as it arrives (CSV, TSV, JSON, JSON Lines, XML, Markdown, Excel, Parquet, Arrow, Delta;
  local file or lakehouse Delta table / Files upload), with back-pressure so memory stays flat.
  The grid keeps a 1,000-row preview of each set, the status bar shows rows written, the outcome
  lands in Messages, and cancelling the query discards the partial output. A second result set
  gets a `_2` suffix, and so on.
- Results grid: **View row as record** (context menu) opens the viewer in Record mode — every
  column of the row as name/value lines with previous/next row, copy the row as JSON, and click a
  value to open it. The cell viewer has a Record toggle too. Made for wide Fabric tables.
- File menu: **Export Connections…** / **Import Connections…** — the connection library as JSON
  (groups and connections; never passwords or secrets), merged on import.
- macOS: the app bundle is now ad-hoc code-signed and the dmg built with `hdiutil`. Apple Silicon
  refused the unsigned 0.2.2 bundle as "damaged"; it now shows the standard unidentified-developer
  prompt (right-click → Open) until the app is notarized. The fixed dmg was re-uploaded to v0.2.2.
- Dev: `run_to_export` and `library` agent verbs.

## 0.2.2 — 2026-09-18

- Results grid: **click-and-drag selects a range** again. Pressing a cell selects it at once and
  dragging extends from it; previously a press flashed a range from the old cell, then collapsed to
  a single cell and ignored the drag. Shift+click extends as before.
- Results grid: the column **filter popup opens beside its column header** instead of the window's
  top-left corner (and stays where you drag it afterwards).
- Results: **Maximize** now gives the result set the whole tab by hiding the editor; the same button,
  the grid context menu, or Escape in the grid restores it. Before, with one result set it did nothing.
- Results grid: new context-menu action **Filter to selected values** — every column the selection
  spans gets an "in (…)" filter of the selected cells' values (other filters and sorts are kept).
- Results: a filtered result set's header reads "n of m rows".
- Fabric explorer: expanding an item now shows **that item's database** (Tables, Views,
  Programmability, Schemas…) directly. Previously the inline explorer treated the item like a
  server and listed every database on the workspace's shared SQL endpoint, duplicating the
  workspace's own item list one level down. "Refresh objects" added to the item context menu.
- Fabric explorer: a SQL database's analytics-endpoint child row appears when the database is
  expanded, rather than always.
- Fabric explorer: pinned and recent items resolve on sign-in (their workspaces' items load
  eagerly) instead of reading "not found" until you expand the workspace.
- Export to OneLake: the dialog **remembers the last lakehouse** you exported to.
- Dev: `pointer` agent verb (click / right-click / double-click / drag in screenshot pixels) fed
  through eframe's raw-input hook, so agent runs can exercise real mouse interaction.

## 0.2.1 — 2026-09-18

- Fabric explorer: chevron on an item opens an **inline object explorer** (databases → tables →
  columns/keys/indexes, same tree as Servers) without leaving the panel.
- Fabric explorer: **Recent** section (last six items you opened) above Pinned.
- Fabric explorer: a SQL database's **SQL analytics endpoint** appears as a child row and opens
  against the workspace's warehouse host.
- Fabric explorer: capacity SKU next to the region when the registration grants
  `Capacity.Read.All` (silently skipped otherwise).
- Lakehouse context menu: **Export results here…** opens Save-results with that lakehouse
  preselected (Delta, schema pre-filled).

## 0.2.0 — 2026-09-18

- **Fabric explorer**: a Fabric icon on the left rail (Ctrl+Shift+B). Sign in once with your Entra
  account (an existing Entra connection is adopted silently), browse every workspace you can reach
  and its warehouses, lakehouse SQL endpoints, SQL databases and mirrored databases. Double-click
  opens a connected query tab, no saved connection needed. Pin favourites, search, *Save to
  Servers*, copy the connection string, open in the Fabric portal.
- **Export to OneLake**: the Save-results dialog can target a lakehouse. Delta tables land in
  `Tables/` (queryable from the lakehouse SQL endpoint right away); Parquet, CSV, Excel and the other
  formats land in `Files/`. Schema-enabled lakehouses are detected (`defaultSchema`) and Delta
  tables go under `Tables/<schema>/`, defaulting to the lakehouse's schema.
- Entra: one refresh token now serves SQL, the Fabric REST API and OneLake once the app
  registration's permissions are consented to (Power BI Service → Workspace.Read.All,
  Item.Read.All, OneLake.ReadWrite.All; optionally Azure Storage → user_impersonation).
- Agent verbs: `fabric_state`, `fabric {action}`, `export {lakehouse, name}`.

## 0.1.1 — 2026-09-18

- Check for updates: Help → Check for Updates…, plus a once-per-start check (Settings → Updates to turn
  it off or stop skipping a version).
- Database switcher: the toolbar combo is always a switcher when connected. A failed list keeps the
  current database and offers *Refresh list* (with the error on hover); engines without `USE` switch
  by reconnecting.
- The tab follows a `USE` run from the editor (title, status bar, completion catalog).
- Fabric Warehouse's per-statement "Statement ID / Query hash" info lines are shown muted, without
  the Msg/Level header.
- Release builds no longer contain the dev-only egui_agent channel.
- Build: faster release workflow (prebuilt packager), Linux-only CI, macOS dmg named without spaces.

## 0.1.0 — 2026-09-17

First release. Connection library, object explorer, editor with completion, streaming results grid,
exports (CSV/Excel/JSON/XML/Markdown/Parquet/Arrow/Delta), estimated and actual plans, history,
light/dark, Entra ID sign-in to Fabric Warehouse, SQL analytics endpoints and SQL database in Fabric,
Windows integrated auth. Installers for Windows and Linux; experimental macOS dmg.
