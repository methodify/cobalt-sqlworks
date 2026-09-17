# Cobalt SQL Works — Product Design

*Version 1.0 · 2026-09-16 · Status: aligned with founder; V1 build authorized*

Companion documents: `feature_inventory.md` (every ADS feature and its disposition),
`decisions/` (why), `architecture.md` (how), `research/` (evidence).

---

## 1. What Cobalt is

Cobalt SQL Works is a free, open-source desktop SQL client for people who spend their day
running queries against SQL Server, Azure SQL, and Microsoft Fabric — and who lost their tool
when Microsoft retired Azure Data Studio in February 2026.

It is **not** an IDE, a DBA console, or a notebook platform. It is the fast, focused query
workbench that ADS was at its best: connect, browse, write SQL, run it, look at the data, take
the data somewhere. It starts in under a second, never blocks its UI on the server, and treats
a fifty-million-row result the same way it treats fifty rows.

It is written in Rust with egui, one binary per platform (Windows first, then Linux, then macOS),
and it is designed from the first commit to be operated by AI agents as well as by people.

### 1.1 The name

*Cobalt* — a blue that isn't Azure. *SQL Works* — it's where you do the work.

### 1.2 Who it's for

- **The everyday querier**: analysts, data engineers, developers, "accidental DBAs" who open a
  SQL tool ten times a day to look at something. ADS's core audience.
- **The Fabric worker**: the same person, now pointed at a Fabric Data Warehouse or Lakehouse
  SQL endpoint with Entra MFA, wanting their results as Parquet or a Delta table rather than a
  spreadsheet.
- **The macOS/Linux SQL Server user** who has no SSMS and doesn't want VS Code to become a
  database tool.
- **AI agents** doing data work on the user's behalf, which need a client they can see and drive.

### 1.3 Who it's not for (yet)

DBAs managing Agent jobs, backups, replication, or Always On. Schema designers. People who need
Python in their notebooks. Those are SSMS's job, or a later Cobalt.

---

## 2. Principles

These decide arguments. When two features conflict, the earlier principle wins.

1. **Fast is the feature.** Cold start under 1 s. The UI thread never waits on the network,
   the disk, or a big export. First rows appear while the query is still running. If something
   takes time, it shows progress and can be cancelled.
2. **Focused.** Cobalt does query work. Every surface earns its place by serving connect →
   browse → write → run → look → take. No terminal, no source control, no marketplace.
3. **Honest with data.** Results are held in Arrow, typed, and never silently truncated. A cell
   with 2 MB of JSON is 2 MB of JSON. A million rows is a million rows. When we cap, we say so and
   offer more.
4. **Muscle memory is sacred.** ADS/SSMS keys work: F5, Ctrl+L, Ctrl+M, Ctrl+Shift+C,
   Ctrl+N, F1. Layout is the familiar tree-left / editor-top / results-bottom. Users switch
   without relearning.
5. **Keyboard first, mouse welcome.** Everything reachable from the command palette; the grid is
   navigable and copyable without a mouse; the mouse gets context menus and drag-resize.
6. **Cross-platform means equal.** One codebase, native dialogs, OS keychain, OS theme, HiDPI.
   No "best on Windows."
7. **Agent-native.** Every meaningful action has a semantic verb an agent can invoke, and every
   meaningful state is introspectable. Humans and agents use the same app.
8. **Pure Rust.** No bundled runtimes, no native SDKs we don't control. When that costs polish
   (IntelliSense), we accept "good-not-great" and improve.

---

## 3. The shape of the app

```
┌─────────────────────────────────────────────────────────────────────────────┐
│ File  Edit  View  Query  Help                        ⌘K  Search / Commands  │
├──────────┬──────────────────────────────────────────────────────────────────┤
│ SERVERS  │ [SQLQuery_1 · silver] [orders.sql · gold ●] [+]                  │
│ ▾ prod_  │ ▶ Run  ■ Cancel  ⇄ Connect  ⌬ Database: silver ▾  ⎇ Est. Plan ⧉ Actual │
│   ▾ silver│ 1  select top 100 *                                              │
│     ▸ Tab│ 2  from orderstatus                                              │
│     ▾ Vie│ 3  where …                                                       │
│       rep│                                                                  │
│       rep├──────────────────────────────────────────────────────────────────┤
│   ▸ gold │ Results │ Messages │ Plan                    ⤓ CSV ⤓ Parquet ⤓ Δ │
│ ▸ dev_box│ # │ COMPANY │ CUSTCOMPANY │ DEMANDTYPE │ CATEGORY │ CLASS │ …    │
│          │ 1 │ chh     │ chj         │ SALE       │ 03       │ 308   │       │
│ HISTORY  │ 2 │ chh     │ chj         │ SALE       │ 03       │ 308   │       │
│ ⌕ search │ … │                                                              │
├──────────┴──────────────────────────────────────────────────────────────────┤
│ ● silver @ prod_fabric (SPID 166)   00:00.41   100 rows   Ln 3, Col 7   MSSQL│
└─────────────────────────────────────────────────────────────────────────────┘
```

- **Left sidebar** — a vertical strip of views: **Servers** (the connection tree), **History**,
  **Files** (V1.x: a folder of `.sql` files). Collapsible; width remembered.
- **Editor area** — tabs. Each tab is one script with one connection (its own SPID). Tabs can
  be renamed, pinned, and carry their server group's color as a top border.
- **Results area** — under each editor, resizable; tabs for **Results**, **Messages**, and
  **Plan** (when present). Can be hidden (Ctrl+Shift+R), maximized, or popped into its own
  editor-area tab.
- **Status bar** — connection (group-colored), executing/elapsed, row count, selection summary,
  cursor position, engine.
- **Command palette** (Ctrl+Shift+P / F1) — every command, fuzzy-searched, with its shortcut.
- **Theme** — light and dark, following the OS by default, switchable.

---

## 4. V1 surfaces, in detail

### 4.1 Connection library

**Profiles.** A profile is: name (optional), server (host[,port] or `server.database.fabric.
microsoft.com`), authentication, user, database (optional), server group, color override, and
advanced options. Advanced: encrypt (Strict / Mandatory / Optional), trust server certificate,
host name in certificate, application name, connect timeout, command timeout, application intent
(ReadWrite/ReadOnly), MARS, packet size, **read-only guard** (Cobalt refuses to run statements
that aren't SELECT/WITH/EXEC-of-known-readers on this connection without confirmation).

**Authentication.**
| Method | How |
|---|---|
| SQL Login | user + password; password optionally saved in the OS keychain |
| Microsoft Entra ID — interactive | system browser, PKCE, loopback redirect; refresh token in keychain; silent refresh thereafter; tenant optional |
| Microsoft Entra ID — device code | for RDP/remote sessions without a local browser |
| Azure CLI | reuse `az login` |
| Windows Integrated | SSPI on Windows |
| Entra service principal | client ID + secret or certificate (V1.x) |

**Server groups.** Named, colored, nestable one level. Profiles drag between groups. Group color
shows on the tree, on the editor tab border, and on the status bar connection item. Default
palette of 8 colors; custom hex allowed.

**Connection dialog.** Opens as a modal from the Servers toolbar, Ctrl+Shift+N, the palette,
or "Connect" in an editor. Form on the left; on the right a **Recent** list. Test Connection
button. Enter connects; Esc cancels. Errors are shown inline with the server's message and,
for the usual suspects, a hint (certificate → "try Trust server certificate"; firewall; login).

**Import from ADS.** File → Import → Azure Data Studio settings: reads `datasource.connections`
and `datasource.connectionGroups` from ADS's `settings.json`, recreates groups and profiles
(passwords are not migratable — they're in the OS store under ADS's key). Also JSON export/
import of Cobalt's own library.

**Sessions.** Each editor tab opens its own connection on first run and keeps it (SPID shown).
The Servers tree uses a separate metadata connection per server, so browsing never waits on a
running query and vice versa. Disconnect from the tab toolbar; auto-reconnect on the next run
after a dropped connection, with a message.

### 4.2 Object Explorer (Servers view)

Tree: **Group → Server → Databases → Database → {Tables, Views, Programmability {Stored
Procedures, Functions {Table-valued, Scalar-valued}}, Schemas, Synonyms, Sequences, Types
{User-defined table types}} → object → {Columns, Keys, Indexes, Parameters}**. Nodes load on
expand; folders show a spinner and a count once loaded. Folders that don't apply to the
engine (Fabric SQL endpoint has no procedures, sequences, etc.) don't appear.

- **Type-to-filter** on any folder (inline box appears on typing when a folder is focused).
- **Group by schema** toggle (V1.x).
- **Context menu**: New Query (pre-connected to this database) · Select Top 1000 (N is a
  setting; the query opens in a new tab and runs) · Script as Create / Alter / Drop / Select /
  Execute (V1 covers tables, views, procedures, functions; the script opens in a new tab) ·
  Refresh · Copy name (bracketed, schema-qualified) · Properties (V1.x popover).
- **Server node menu**: Connect / Disconnect, Edit, Delete, New Query, Refresh, Manage (V2).
- **Drag** a table or column into the editor inserts its bracketed name.
- Double-click a table = Select Top N.

### 4.3 Query editor

- Tabs named `SQLQuery_1…` until saved; title shows `name · database` and the tab border
  carries the group color. Right-click: Rename, Pin, Close others, Close to the right, Copy
  path, Reveal in Files.
- **Toolbar**: Run · Cancel · Connect/Disconnect/Change · Database dropdown · Estimated Plan ·
  Actual Plan (toggle) · Parse · Format · (overflow) Execution options.
- **Language**: T-SQL lexer-driven highlighting (keywords, functions, strings, numbers,
  comments, identifiers, variables, temp tables, operators). Bracket matching. Line numbers.
  Current-statement highlighting (subtle background on the statement under the cursor).
- **Completion**: Ctrl+Space or on typing after `.`: keywords; schemas/tables/views/functions of
  the current database; columns of tables referenced in the current statement (alias-aware when
  `sqlparser` can parse it; falls back to "columns of all tables named in the statement");
  snippets (`sel`, `selt`, `cte`, `ins`, `upd`, `del`, `proc`). Catalog cache per connection,
  refreshed on connect and on demand (F7 / "Refresh IntelliSense").
- **Editing**: find/replace with regex (Ctrl+F / Ctrl+H), comment toggle (Ctrl+/), block comment
  (Shift+Alt+A), move line (Alt+↑/↓), duplicate line (Shift+Alt+↓), uppercase/lowercase keyword
  transform, go to line (Ctrl+G), undo/redo, auto-indent, auto-close brackets/quotes, word
  wrap toggle. (Multi-cursor, folding: V2.)
- **Execution keys**: F5 / Ctrl+E — run selection or all · **Ctrl+Enter / Ctrl+F5 — run the
  statement under the cursor** · Alt+Break / Esc-while-running — cancel · Ctrl+L — estimated
  plan · Ctrl+M — toggle actual plan · Shift+Alt+P — parse · Shift+Alt+F — format.
- **Per-tab execution options** (gear in toolbar): row cap, command timeout, isolation level,
  `SET NOCOUNT`, `ARITHABORT`, `STATISTICS IO/TIME`, `XACT_ABORT`.
- **Files**: Open/Save/Save As `.sql` (UTF-8, BOM preserved), auto-save option, **hot exit**
  (unsaved tabs restored after crash or restart), external-change detection.

### 4.4 Running queries

- Text is split into batches on `GO` (with `GO n`). Each batch runs on the tab's session in
  order; a batch error stops the run unless configured otherwise.
- **Streaming**: rows stream into the grid as the driver yields them; the first batch renders
  within tens of milliseconds; the grid stays scrollable during the fetch; the status bar
  shows elapsed time ticking and rows so far.
- **Row cap**: default 10,000 rows per result set (setting). When hit, fetching pauses and a
  bar appears under the grid: *"Showing 10,000 rows — Fetch 10,000 more · Fetch all · Stop."*
  The server-side query stays open until you choose (or the tab's timeout). This is how
  Fabric CU cost stays under control without a hard 10k wall.
- **Memory budget**: results live in Arrow batches in RAM up to a global budget (default
  1 GB, setting); beyond it batches spill to Arrow IPC files in the temp dir transparently. The
  status bar shows the tab's result size.
- **Messages** pane: batch start lines with timestamps (click → jump to statement), "(N rows
  affected)", PRINT/RAISERROR output as it arrives, errors in red with server line numbers
  (click → jump to the offending line in the editor), total execution time. Copy / Copy all.
- **Cancel** sends a TDS attention; the UI shows "Cancelling…" until the server acknowledges.
- **Multiple result sets**: stacked vertically (ADS style) by default; each with its own
  header, row count, and maximize button; a setting switches to result-set tabs; the
  divider between editor and results is draggable and remembered per tab.
- Results outlive the tab's connection: disconnecting or reconnecting keeps the grid.

### 4.5 Results grid

The grid is the center of the product and gets the most care.

- **Virtualized** over Arrow columns: millions of rows scroll at 60 fps; column widths auto-fit
  to the first N rows, resizable by drag, double-click header border to fit, max default width
  400 px with ellipsis; row-number gutter; sticky header; freeze first K columns (V1.x).
- **Types are visible**: header tooltip shows SQL type and nullability; numbers right-aligned;
  NULL rendered as italic `NULL` in a distinct color; dates in ISO with configurable precision;
  binary as `0x…` hex prefix; bit as 0/1 or true/false (setting).
- **Selection model**: click a cell; Shift+click / Shift+arrows extend a rectangle; click the
  row number selects the row; click the header selects the column; Ctrl+A selects all; Ctrl+click
  adds. Arrow keys move; Home/End; Ctrl+arrows jump; Page Up/Down. Selection is preserved across
  sort/filter where rows survive.
- **Copy**: Ctrl+C copies the selection as TSV (Excel-paste-ready); Ctrl+Shift+C with headers;
  Ctrl+Shift+H headers only. **Copy as ▸** CSV · TSV · Markdown table · JSON (array of objects)
  · SQL INSERT statements · IN-list `('a','b')` · Plain text (cell). Copy completes with a toast.
- **Sort**: click header to sort asc/desc/none (local, over Arrow, stable); Shift+click for
  secondary sort.
- **Filter**: funnel icon in header → distinct-value list with search, check/uncheck, `(NULL)`,
  `(Blank)`; plus a "matches…" text/number/date condition. Active filters show as chips above
  the grid with a "clear all." Filtered row count shown as "1,204 of 10,000".
- **Find in results** (Ctrl+F while grid focused): highlights matches, Enter jumps.
- **Cell viewer**: double-click or Enter on a cell opens a side panel: pretty-printed JSON with
  tree/collapse; XML pretty-printed; long text with wrap toggle; binary as hex dump. Values are
  fetched in full (no 64 KB cap; if the driver truncated, Cobalt re-fetches the single value
  by key when it can, otherwise says "truncated at N chars").
- **Selection summary** (status bar): numeric selections show Sum · Avg · Min · Max · Count;
  others show Count · Distinct · Nulls.
- **Transposed view** (V1.x): show the selected row as a two-column key/value list.
- **Maximize** a result set to the whole results area; **pop out** the result set to its own
  editor tab (survives re-running the query).
- **Charts** (V2): quick bar/line/pie/scatter from the selection.

### 4.6 Export

Right-click grid / results toolbar / palette → **Save results as ▸**. Applies to the whole
result set or the current selection. Every export streams from the Arrow batches with a progress
toast and cancel; the UI stays responsive.

| Format | Notes |
|---|---|
| CSV | delimiter, quote, line ending, header, encoding (UTF-8 / UTF-8 BOM / UTF-16), null as empty or `NULL` |
| TSV | as CSV |
| Excel `.xlsx` | header bold, freeze header, autofilter, auto column widths, native types (numbers, dates); warns above 1,048,576 rows |
| JSON | array of objects, or newline-delimited (JSONL); typed values; dates as ISO |
| XML | `<row>` elements or attribute style; formatted |
| Markdown | GFM table; alignment by type |
| **Parquet** | Snappy/ZSTD; typed schema from Arrow; row-group size setting |
| **Arrow IPC** | `.arrow` file (Feather v2); zero-copy loadable by pandas/polars/DuckDB |
| **Delta Lake** | writes a Delta table directory via delta-rs: **Create new** / **Overwrite** / **Append**; optional partition columns; schema from Arrow; verified readable by Fabric Spark and SQL endpoints. V1 targets local/UNC paths; OneLake/ADLS/S3 are on the roadmap |
| Clipboard | all of the text formats above via Copy as |

**"Export whole query"** (V1.x): run the query and stream every row straight to a file without
populating the grid — for the 50M-row extract.

### 4.7 Execution plans

- **Estimated** (Ctrl+L): runs `SET SHOWPLAN_XML ON` for the selection/statement; opens the
  Plan tab. **Actual** (Ctrl+M toggle): subsequent runs use `SET STATISTICS XML ON`; each
  statement's plan appears alongside its result set.
- **Plan tab** layout: graph canvas (left, most of the space) · **Properties** pane (right,
  collapsible) · **Top Operations** grid (bottom, collapsible).
- **Graph**: right-to-left tree, layered layout, straight-or-orthogonal edges whose thickness
  scales with actual (or estimated) rows; each node shows an operator icon, operator name,
  object (schema.table.index, truncated with tooltip), cost % of the statement, and — for actual
  plans — actual vs estimated rows with a colored delta when they diverge >10×. Warnings
  (spill, implicit conversion, missing index, no join predicate) as a badge. Parallelism
  glyph. Zoom with Ctrl+wheel, fit (Ctrl+0), pan by drag, keyboard navigation between nodes;
  hover tooltip with the key properties; click selects and fills Properties.
- **Properties**: full operator property set, grouped, searchable, copyable; long values
  expand.
- **Top Operations**: every operator sortable by Est. Cost, Subtree Cost, Est. Rows, Actual
  Rows, Actual Time, Reads; filter box; click → highlights the node.
- **Statements**: multi-statement batches show a statement selector with per-statement cost.
- **Missing index** hint from the plan shown as a banner with a "Copy CREATE INDEX" button.
- **Files**: Open `.sqlplan` (File → Open, or drag onto the window); Save plan as `.sqlplan`;
  Show plan XML in a new tab; Copy plan XML.
- **Icons** are Cobalt's own set (SSMS-shaped: scan, seek, lookup, nested loops, hash match,
  merge, sort, filter, compute scalar, stream/hash aggregate, top, concatenation, parallelism,
  spool, table insert/update/delete, etc.), monochrome, tinted by category, legible in both
  themes.
- V2: side-by-side plan comparison; Plan Explorer-class analysis (statement tree, cost gradients,
  Table I/O, wait stats, index analysis).

### 4.8 History

Every execution is recorded in a local SQLite database: text, server, database, started/ended,
duration, rows returned/affected, success or error text, the tab it ran in. The **History** view
lists entries newest first with full-text search (Ctrl+Shift+H from anywhere), filters by server
/ database / status / date, **star** to keep, and per-entry actions: Open in new tab · Re-run ·
Copy · Delete. Unsaved tab contents are also snapshotted on run and on close, so "reopen the
query I closed without saving" is always possible. Retention: 90 days or 10,000 entries by
default; starred entries are kept forever; capture can be paused (for sensitive work) and the
store can be cleared.

### 4.9 Settings, themes, keys

- Settings are a TOML file (`%APPDATA%\Cobalt SQL Works\settings.toml` and the XDG/macOS
  equivalents) with a searchable Settings tab in the app that edits the same file. Categories:
  Appearance (theme, UI scale, fonts), Editor, Execution (row cap, timeouts), Results (NULL
  style, date format, max column width), Export defaults, Connections (default auth, Entra client
  ID, tenant), History, Advanced (memory budget, temp dir, logging).
- **Themes**: Cobalt Light and Cobalt Dark; follow OS by default; consistent tokens across the
  editor, grid, and plan viewer. High-contrast variants later.
- **Key bindings**: ADS/SSMS defaults; a Keyboard Shortcuts tab lists every command and lets
  you rebind (V1.x); conflicts flagged.
- **UI scale**: Ctrl+= / Ctrl+- zooms the whole UI; grid and editor font sizes independently
  adjustable.

### 4.10 Agent surface

With `COBALT_AGENT=1` the app serves egui_agent on a named pipe (Windows) or Unix socket. Beyond
the automatic widget/AccessKit snapshot, Cobalt exposes semantic verbs:

```
connect          {profile | server, auth, database}      → session id
open_query       {text, connect_to?}                      → tab id
run              {tab, mode: all|selection|current}       → run id
cancel           {tab}
wait_complete    {run, timeout_ms}
results          {tab, set?, offset, limit, columns?}     → rows as JSON, schema
messages         {tab}
export           {tab, set?, format, path, options}
plan             {tab}                                    → plan summary (statements, top ops)
tree_expand      {path}                                   → children
catalog_search   {server, term}
snapshot_state   {}                                       → open tabs, connections, selection
```

This makes Cobalt usable by Claude as a data tool — "run this against dev, export to Parquet,
tell me the top operator" — and is also how the app is tested during development.

### 4.11 Fabric-specific behavior (V1)

- Detects engine edition on connect (`SERVERPROPERTY('EngineEdition')`, `@@VERSION`) and
  labels the server node: SQL Server, Azure SQL DB, Azure SQL MI, Synapse, **Fabric Warehouse**,
  **Fabric SQL endpoint** (read-only), Fabric SQL DB.
- Hides object-explorer folders the edition doesn't support; disables Actual Plan where
  `STATISTICS XML` isn't supported and says why.
- Entra-only auth is enforced for Fabric hosts (SQL login option hidden).
- Status bar shows the workspace/warehouse name parsed from the host.
- Row cap defaults on, with a gentle "Fabric bills by CU — Cobalt fetches in pages" hint the
  first time.

---

## 5. Beyond ADS — where Cobalt is going

ADS was a good tool built on a general-purpose IDE. Cobalt is built on a **result engine**, and
that changes what's possible. These are the directions we're drawn to, roughly in the order
we'd build them after V1. Each is a product bet, not a promise.

### 5.1 The result engine is the product

Every result is a typed, columnar, local dataset. Once that's true, the grid is just one view
of it:

- **Column profiler** — a panel beside any result: for each column, type, null %, distinct
  count, min/max/mean, top values, and a histogram; computed in the background over the Arrow
  batches and refined as rows stream in. Click a bar to filter. (What DuckDB UI and Navicat do;
  nothing in the SQL Server world does.)
- **Local re-query** — "Query this result": a DataFusion SQL prompt over the result tabs of the
  current window. Filter, aggregate, pivot, and — because two tabs are two tables — **join a
  prod result to a dev result, diff two runs of the same query, compare Fabric to on-prem**
  with zero server round trips.
- **Result diff** — pick two result sets with a key; see added / removed / changed rows.
- **Pivot and totals** — group-by and aggregate without rewriting SQL; totals row.
- **Streaming everything** — export, profile, and re-query all work on results that never fit in
  RAM, because the engine spills.

### 5.2 Plans people can actually read

SentryOne Plan Explorer is dead and SSMS's viewer hasn't changed in fifteen years. Cobalt's
V1 viewer is parity; V2 goes for the crown: a **statements tree** with per-statement cost and
duration for procs and batches, **cost heat gradients** on nodes, **estimate-vs-actual
discrepancy highlighting** (the single most useful signal in a plan), edge thickness selectable
by rows / data size / cost, **Table I/O** and **wait stats** tabs for actual plans, **index
analysis** ("this seek is a lookup because the index doesn't cover X, Y"), plan comparison, and
**anonymize plan** for sharing. Open any `.sqlplan` anyone sends you.

### 5.3 Fabric-native, not Fabric-tolerant

- Browse Fabric **workspaces → Warehouses / Lakehouse SQL endpoints / SQL DBs / mirrored DBs**
  in the connection dialog; no connection strings.
- **Query Insights** panel: recent runs with duration and CU from
  `queryinsights.exec_requests_history`, so you learn what your query cost.
- **OneLake browser**: see the Lakehouse's Delta tables and Files; preview a Delta table
  without a SQL endpoint; **export a result straight into OneLake as a Delta table** with the
  same Entra token.
- Capability hints inline: "this endpoint is read-only", "no transactions across batches",
  "CTAS available — Save as table."
- Cross-warehouse three-part-name helper.

### 5.4 History as memory

V1 records everything you ran. V2 makes it useful: **versioned tab contents** (every save and
run is a version; scrub back), **"queries I ran against this table"** from the object explorer,
**saved query library** with folders, tags, and parameters (`@customer_id` prompts on run,
`-- @limit 500` magics), and a **workspace** concept — a named set of connections, open tabs,
and saved queries you can switch between (prod investigation vs. dev work).

### 5.5 Notebooks, reborn

SQL + Markdown cells in an `.ipynb`-compatible file, with results cached in the document so a
runbook carries its evidence. No Jupyter, no Python. The upgrade: **cells chain** — a cell's
result is a local table the next cell can query with DataFusion, or a variable it can splice
into SQL. Export to HTML/Markdown (the thing ADS never had). Run from the command line for
scheduled reports.

### 5.6 Lightweight dashboards

Any saved query can be pinned as a **tile** — a number, a bar, a line, a small grid — on a
server or database home page, with an auto-refresh interval. ADS's insight widgets without the
JSON editing. A page of tiles is a shareable file.

### 5.7 Editing data, safely

Edit Data for tables with a key, with **staged edits**: every change is a pending statement
you can see; Save shows the SQL diff; connections marked production require confirmation;
read-only connections refuse. Result grids from simple single-table SELECTs become editable the
same way.

### 5.8 AI you bring

Explain this query · Fix this error · Optimize this plan · Write SQL from a sentence, grounded in
the live catalog · Describe this table. **Bring your own model**: any OpenAI-compatible endpoint,
Anthropic, Azure OpenAI, or a local Ollama — never a required cloud dependency, never a Cobalt
account. A read-only classifier gates generated SQL; writes need an explicit click. Per-connection
instruction files (`COBALT.md` in a folder, or a saved note on the connection) give the model
context. The agent surface (§4.10) means an *external* agent can also drive Cobalt directly —
Cobalt becomes an MCP-addressable data tool.

### 5.9 More engines

The driver trait is engine-agnostic. After SQL Server: **DuckDB and SQLite** (local files,
pairs with Parquet/Delta exports — open the file you just wrote), then **PostgreSQL** (the most
requested), then whatever the community brings. One tool for the Fabric warehouse and the local
Parquet.

### 5.10 Small things that make people smile

Execute-current-statement with inline timing in the gutter · Copy as Markdown for the Teams
message · Open in Excel · a "Describe" popover on any object · global object search across every
connection · rename and color tabs · "keep running when I close this tab, notify me" · Query
Magics (`url__format__link`) · one-click local SQL Server container · ER diagram of the
selected tables with Mermaid export · Query Store "regressed plans" view.

---

## 6. Non-goals

Extension marketplace · source control · integrated terminal · Python/PowerShell/Jupyter
kernels · SQL Agent · dacpac/bacpac/SQL projects · Schema Compare (SSMS and VS Code do it) ·
migration wizards · telemetry of any kind · accounts, cloud sync, or anything that phones home.

---

## 7. Roadmap

| Milestone | Contents | Exit criterion |
|---|---|---|
| **V1 alpha** | Everything in §4 at "works end to end" quality against SQL Server 2022 (Docker) and SQL auth; Entra flows implemented but unverified live; Windows build; Linux build compiles | Founder can connect to Fabric with Entra MFA, run a query, get results, export Parquet + Delta, view a plan |
| **V1** | Alpha feedback fixed; Windows installer (MSI) + Linux (AppImage/deb) via cargo-dist; macOS build from a Mac runner (unsigned); ADS import; polish pass on grid/editor; docs site | Founder uses it as daily driver for two weeks; three outside users do too |
| **V1.x** | Items marked V1.x in `feature_inventory.md`: read-only guard, connection import/export, freeze columns, find in results, transposed view, selection summary, snippets, format, statement timing, Fabric hints, whole-query export, OneLake Delta target, editable keys, Linux/mac parity | Each ships as it's ready |
| **V2** | §5.1–5.9 in the order the founder and users pull them | — |

---

## 8. V1 acceptance checklist

The alpha is done when all of these are true, verified by driving the app through egui_agent
against the Docker SQL Server and by the founder against Fabric:

- [ ] Cold start to interactive < 1 s on the founder's Windows box; binary < 40 MB.
- [ ] Create a group, create a profile with SQL auth and saved password, connect; tree shows
      databases and expands tables/views/procs/functions/columns lazily.
- [ ] Entra interactive login completes in the browser and connects to a Fabric warehouse
      (founder-verified); token refresh is silent on the next launch.
- [ ] `az login` credential connects to Fabric (founder-verified, if `az` present).
- [ ] New Query from a database node; editor highlights T-SQL; completion offers tables and
      columns; F5 runs; Ctrl+Enter runs the current statement; cancel works mid-fetch.
- [ ] A 5,000,000-row `SELECT` streams into the grid with the 10k cap; "Fetch all" completes
      without the UI dropping frames; memory stays under the budget (spill engaged); scrolling
      to row 4,999,999 is instant; sort and filter on a column work on the full set.
- [ ] Multiple result sets, PRINT messages, an error with a clickable line number, rows affected.
- [ ] Copy / Copy with headers / Copy as Markdown / JSON / INSERT produce correct text; paste
      into Excel lands in cells.
- [ ] Save as CSV / Excel / JSON / XML / Markdown / Parquet / Arrow / Delta each produce a file
      that round-trips (Parquet/Arrow/Delta re-read with a Rust reader in tests; Delta
      founder-verified in Fabric).
- [ ] JSON and XML cells open in the viewer pretty-printed; a 1 MB `nvarchar(max)` value is
      shown in full.
- [ ] Estimated plan renders as a graph with icons, cost %, properties, and Top Operations;
      actual plan shows actual vs estimated rows; `.sqlplan` opens and saves.
- [ ] History records every run; search finds it; a closed unsaved tab can be restored.
- [ ] Light and dark themes are complete (no unthemed surface); OS theme is followed.
- [ ] Every command is in the palette with its shortcut; ADS keys work.
- [ ] Hot exit: kill the process with unsaved tabs; relaunch; tabs and text are back.
- [ ] `COBALT_AGENT=1` + `egui-agent-cli` can connect, run, read results, and export.
- [ ] Linux build runs the same checklist (minus Entra) on Ubuntu 24.04 under X11 and Wayland.

---

## 9. Risks we're carrying knowingly

| Risk | Mitigation |
|---|---|
| TDS driver ecosystem is weeks old | `Driver` trait; integration tests; quarterly review; ODBC escape hatch later |
| Entra conditional-access may block a public client | device-code and `az login` paths; document the tenant-admin consent step |
| egui breaking releases every quarter | pin, bump quarterly, vendor any crate that stalls |
| Arrow version skew (`arrow` vs `deltalake` vs `datafusion`) | pin to delta-rs's arrow; exporters in isolated crates |
| Editor depth (multi-cursor, folding) is custom | ship without; V2 |
| Plan renderer is custom | bounded scope; parity first; icons are ours |
| macOS signing needs an Apple account + runner | unsigned builds until the founder sets it up |
