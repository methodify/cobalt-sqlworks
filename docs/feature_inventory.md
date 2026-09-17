# Cobalt SQL Works — Consolidated Feature Inventory & Proposed Disposition

*Status: DRAFT for discussion, 2026-09-16. Nothing here is decided until we say so.*

This consolidates everything Azure Data Studio did (see `docs/research/01_*` and `02_*`) into one
list, and proposes where each item lands for Cobalt. The tiers:

| Tier | Meaning |
|---|---|
| **V1** | The founder's stated goal: a working query-runner surface with a connection library, run queries, grid, exports (incl. Parquet/Arrow/Delta), plans + viewer, light/dark. Ship this first. |
| **V1.x** | Cheap, high-value follow-ons that make V1 feel finished. Same release train. |
| **V2** | Real features that need their own design pass. Architecture must not preclude them. |
| **Later** | Plausible someday; no design effort now. |
| **Out** | Deliberately not doing. |

A `★` marks something Cobalt does that ADS did not.

---

## 1. Connections & connection library

| Feature | ADS | Proposed | Notes |
|---|---|---|---|
| Connection dialog: server, auth type, user/pw, database, group, friendly name | ✓ | **V1** | Form + "Advanced" section |
| Auth: SQL login | ✓ | **V1** | |
| Auth: Microsoft Entra ID — interactive (MFA), via system browser | ✓ | **V1** | We build auth-code+PKCE loopback ourselves (Rust SDK has none). Device-code fallback for RDP/headless. Refresh tokens in OS keychain. **Decision needed: who owns the Entra app registration** (see §10). |
| Auth: Entra via `az login` (Azure CLI credential) | ✗ | **V1** ★ | Free from `azure_identity`; great fallback for people who already `az login` |
| Auth: Windows Integrated (SSPI/Kerberos) | ✓ | **V1** (Windows) / V1.x (Linux/mac Kerberos) | Driver does SSPI on Windows |
| Auth: service principal (client secret / cert), managed identity | ✗ (conn string only) | **V1.x** ★ | Trivial with `azure_identity` |
| Advanced props: encrypt (strict/mandatory/optional), trust cert, hostname-in-cert, app name, connect/command timeout, app intent, MARS, packet size, port, retry | ✓ | **V1** (core subset) | Encrypt + trust cert on front page like ADS 1.40 |
| Remember password (OS credential store) | ✓ | **V1** | `keyring` crate |
| Connection string input mode | ✓ | **V1.x** | Parse SqlClient-style strings |
| Server groups with color, description; drag-drop | ✓ | **V1** | People explicitly mourn this. Color propagates to tab + status bar (ADS never did status bar ★). |
| Recent connections | ✓ | **V1** | |
| Browse Azure: accounts → tenants → subscriptions → SQL servers/DBs/MI/Synapse | ✓ | **V2** | Nice; not needed to connect. |
| Browse **Fabric** workspaces → Warehouse / SQL analytics endpoint / SQL DB / mirrored DB | ✗ (SSMS 22 / VS Code have it) | **V2** ★ | Uses Fabric REST API with the same Entra token. Strong for the target audience. |
| Firewall-rule prompt on Azure SQL connect failure | ✓ | Later | |
| Change-password-on-login dialog | ✓ | Later | |
| Connection pooling / per-tab connection + separate metadata connection | ✓ (1.45) | **V1** | One session per editor tab (SPID shown), one metadata connection per server for the tree so browsing never blocks a query |
| Connection import/export (JSON) | ✗ | **V1.x** ★ | A named gap at ADS retirement; also our migration path *from* ADS (`datasource.connections` in its settings.json) ★ |
| Read-only flag per connection ("production guard") | ✗ | **V1.x** ★ | Cheap, prevents disasters |
| Multiple engines: PostgreSQL, MySQL, DuckDB, SQLite, Kusto | via ext | **V2** (driver trait in V1) | Decide which. See §10. |
| Central Management Servers / multi-server query | ✓ (preview) | Out | |

## 2. Object Explorer

| Feature | ADS | Proposed | Notes |
|---|---|---|---|
| Tree: server → databases → tables/views/programmability/security/… (full SMO tree) | ✓ | **V1** (core: Databases, Tables, Views, Procs, Functions, Columns, Keys, Indexes, Schemas, Synonyms, Sequences, UDTs) | Lazy-load on expand; folder gating by engine edition (Fabric DW lacks many nodes) |
| Group by schema toggle | ✓ | **V1.x** | |
| Context: Select Top 1000 (editable N ★), New Query, Refresh, Script as Create/Drop/Alter/Execute/Select | ✓ | **V1** | Scripting requires our own DDL scripter (SMO did it in ADS) — real work; start with tables/views/procs/functions |
| Filter dialog (property/operator/value per folder) | ✓ | **V1.x** | Simpler: inline type-to-filter first (V1), full dialog later |
| Global object search across connections (Ctrl+P style) | partial | **V1.x** ★ | Cheap once catalog is cached |
| Drag table/column name into editor | ✓ | V1.x | |
| Edit Data (grid edit of table rows) | ✓ | **V2** | Needs PK detection, staged edits, review-SQL-before-commit (TablePlus model ★) |
| Table Designer | ✓ | Later | |
| New/Drop/Attach/Detach Database, Properties dialogs, User Management | ✓ (preview) | Later / Out | DBA tooling — see §8 |
| Object "Describe" popup / hover (columns, types, row count) | ✗ | **V1.x** ★ | Toad F4-style; very cheap with the catalog cache |

## 3. Query editor

| Feature | ADS | Proposed | Notes |
|---|---|---|---|
| Multiple tabs, each with its own connection, DB dropdown, Connect/Disconnect/Change | ✓ | **V1** | |
| Run (F5) selection-or-all; Cancel; GO batch splitting; Run current statement (Ctrl+F5 → **Ctrl+Enter** ★ too) | ✓ | **V1** | Attention-packet cancel via driver |
| Syntax highlighting (T-SQL) | ✓ | **V1** | Own lexer → `LayoutJob` |
| Autocomplete: keywords, objects, columns, alias-aware | ✓ (STS) | **V1** basic (keywords + catalog objects + columns of tables in the statement) → V1.x alias resolution | Pure Rust. **Decision: SqlToolsService or not** (§10) |
| Error squiggles from server parse (`SET PARSEONLY`) / "Parse" button | ✓ | **V1.x** | |
| Snippets with tab stops; user snippets | ✓ | **V1.x** | |
| Format document/selection | ✓ | **V1.x** | `sqlformat` crate; options later |
| Find/replace, multi-cursor, folding, minimap, bracket match, comment toggle | ✓ (VS Code) | **V1**: find/replace, comment toggle, bracket match. **V2**: multi-cursor, folding. **Out**: minimap | egui TextEdit is single-cursor; multi-cursor is custom work |
| Peek/Go to definition | ✓ | V2 | |
| Estimated plan (Ctrl+L) / Enable actual plan (Ctrl+M) | ✓ | **V1** | `SET SHOWPLAN_XML` / `SET STATISTICS XML` |
| SQLCMD mode (`:setvar`, `:r`, `:connect`) | ✓ (partial, preview) | Later | |
| Per-connection execution options (rowcount, timeout, isolation, ARITHABORT, NOCOUNT, STATISTICS IO/TIME…) | ✓ | **V1** subset (timeout, rowcount, isolation) → V1.x rest | |
| Query shortcuts (Alt+F2 = sp_help selection etc.) | ✓ | V1.x | |
| Query History panel | ✓ (ext) | **V1** ★ upgraded | Local SQLite: every execution with server/db/duration/rows/error, full-text search, star, restore closed tabs. SQL Prompt-class, not a log. |
| Session restore / hot exit (unsaved tabs survive restart) | ✓ | **V1** | |
| Tab: rename, pin, color by group, SPID in title | partial | **V1.x** | |
| To Notebook | ✓ | V2 (with notebooks) | |
| Inline per-statement execution time in gutter | ✗ | V1.x ★ | DataGrip 2026.2 |
| Parameters / variables prompt (`@p` dialog, `-- @limit` magics) | ✗ | V2 ★ | |
| Copilot / AI | ✓ (completions only) | **V2** ★ BYO-model | See §9 |

## 4. Results

| Feature | ADS | Proposed | Notes |
|---|---|---|---|
| Results/Messages tabs; multiple result sets stacked; maximize one; toggle pane; focus toggle | ✓ | **V1** | Also: result sets as tabs and side-by-side ★ (top VS Code complaint) |
| **Streaming rows into the grid as they arrive**; live row count + elapsed | ✓ | **V1** | Non-negotiable "feels fast" feature |
| Virtualized grid to millions of rows | ✓ (disk-backed in STS) | **V1** | `egui_table` over Arrow `RecordBatch` chunks; spill to Arrow IPC temp files past a memory budget ★ |
| Row cap / "stop after N, fetch more / all" | partial (`rowCount`) | **V1** ★ | Default e.g. 10k with a one-click continue bar; Fabric cost control |
| Column resize, auto-size, max width; row number column; NULL styling; font settings | ✓ | **V1** | |
| Sort + Excel-style filter per column (distinct-value list, search) | ✓ | **V1** | Local, over Arrow — no 5k-row limit ★ |
| Find in results | ✗ | V1.x ★ | |
| Selection: cell/range/row/column/all; keyboard nav; Shift+click | ✓ | **V1** | Get this *right* — the VS Code grid's failure |
| Copy / Copy with headers / Copy headers | ✓ | **V1** | Plus ★ Copy as Markdown / JSON / CSV / TSV / INSERT / IN-list |
| Selection summary in status bar (sum/avg/count/distinct/null) | ✓ | **V1.x** | |
| Save as CSV / JSON / Excel / XML / Markdown (whole set or selection) | ✓ | **V1** | Options: headers, delimiter, encoding, Excel freeze/autofilter |
| **Save as Parquet / Arrow IPC (Feather)** | ✗ | **V1** ★ | Direct from RecordBatches |
| **Save as Delta table** (local path) | ✗ | **V1** ★ | `deltalake` crate; append/overwrite modes |
| Delta/Parquet to **OneLake / ADLS / S3** | ✗ | V1.x ★ | `object_store` with the same Entra token. **Decision** (§10) |
| Streaming export (wire → file, never touching the grid) with progress | ✗ | V1.x ★ | Beekeeper model; for the "50M rows to Parquet" case |
| Cell viewers: JSON, XML, long text, hex/binary | ✓ | **V1** (JSON/XML/text) / V1.x hex | No 65k truncation; lazy full-value fetch ★ |
| Transposed / record view of a row | ✗ | V1.x ★ | Wide Fabric tables |
| Column profile panel (null %, distinct, min/max, histogram) | ✗ | **V2** ★ | DuckDB-UI-style; computed over Arrow in background. A flagship "beyond" feature. |
| Chart viewer (bar/hbar/line/pie/doughnut/scatter/time series/count/table/image) + save as PNG | ✓ | **V2** | `egui_plot`; "Create Insight" only if dashboards come |
| "Query this result locally" (SQL over result sets, join two tabs, diff two runs) | ✗ | **V2** ★ | DataFusion (pure Rust) — falls out of the Arrow model. Flagship "beyond". |
| Messages pane: batch start lines (clickable), rows affected, PRINT, errors (clickable line), total time | ✓ | **V1** | |
| Copy Query With Results (HTML) | ✓ | Later | |
| Query Magics (`__format__link` etc.) | ✗ | Later ★ | Cute; cheap; not now |
| SandDance / Visualizer | ✓ (ext) | Out | |

## 5. Execution plans

| Feature | ADS (1.40) | Proposed | Notes |
|---|---|---|---|
| Graphical estimated & actual plans, one per statement, SSMS icon set, cost %, edge width by rows | ✓ | **V1** | Custom egui painter + layered layout; parse showplan XML with `quick-xml`. Icons: we need our own set (SSMS icons are not ours to ship) |
| Zoom/fit/pan; tooltips; Properties pane (sort/filter/copy) | ✓ | **V1** | |
| Top Operations grid (sortable, filter, jump-to-node) | ✓ | **V1** | Reuse the results grid |
| Plan Tree text view | ✓ | V1.x | |
| Highlight most expensive operator by chosen metric | ✓ | V1.x | |
| Find node | ✓ | V1.x | |
| Open/save `.sqlplan`; show plan XML | ✓ | **V1** | |
| Warnings badges (missing index, spills, implicit conversion) + missing-index DDL surfaced | partial | **V1.x** ★ | VS Code 1.45.1 added it; users asked ADS for years |
| Plan comparison (side by side, equivalent props collapsed) | ✓ | V2 | |
| Plan Explorer-class: statements tree with per-stmt metrics, cost gradient, est-vs-actual discrepancy highlighting, Table I/O, wait stats, index analysis, anonymize | ✗ | **V2** ★ | The thing SQL Server pros would switch for; Plan Explorer is dead. Stage it. |
| Live Query Statistics | ✗ | Later | |

## 6. Notebooks, dashboards, charts

| Feature | ADS | Proposed | Notes |
|---|---|---|---|
| SQL notebooks (`.ipynb`, SQL kernel, cached results, markdown cells) | ✓ | **V2** | Most-mourned ADS feature. SQL + Markdown only — **no Python/Jupyter/PowerShell kernels** (Out). Chained cells over local DataFusion ★ is the upgrade. **Decision** (§10) |
| Jupyter Books, Notebook Views, Run with Parameters, Papermill, URI params, Agent notebook jobs | ✓ | Out | |
| Server/database dashboards ("Manage"), insight widgets (JSON + T-SQL), Tasks/Explorer widgets | ✓ | **V2** (lightweight) | "Pin any query as a tile" is the cheap 80%. **Decision** (§10) |
| Server Reports, SQL Assessment, MI dashboard | ✓ (ext) | Out | |

## 7. Shell, settings, UX

| Feature | ADS | Proposed | Notes |
|---|---|---|---|
| Light / dark themes; high contrast | ✓ | **V1** (light/dark) | Own palette; theme follows OS |
| Command palette (Ctrl+Shift+P) | ✓ | **V1** | Also drives egui_agent-friendly actions |
| Keyboard shortcuts, editable | ✓ | **V1** defaults (ADS/SSMS-compatible: F5, Ctrl+L, Ctrl+M, Ctrl+Shift+C…) / V1.x editable | |
| Settings UI + JSON file | ✓ | **V1** (TOML/JSON file + minimal UI) | |
| Docking: sidebar, editor tabs, results pane, panel; resizable; tear-out | ✓ | **V1** (fixed IDE layout with resizable panes) → V2 full docking | `egui_dock`/`egui_tiles` — decide in architecture |
| Status bar: connection, engine, executing/elapsed, rows, selection summary, cursor pos | ✓ | **V1** | |
| Welcome page, feature tour | ✓ | V1.x minimal | |
| Files: open/save `.sql`, folder tree of queries (Explorer), auto-save, hot exit | ✓ | **V1** open/save + hot exit; **V1.x** folder tree ("queries in folders" was explicitly mourned) | |
| Integrated terminal | ✓ | Out | |
| Source control | ✓ | **Out** (founder decision) | |
| Extension marketplace / VSIX | ✓ | **Out** (founder decision) | A *plugin trait* for engines/exporters internally is fine; no marketplace |
| Settings sync | ✗ | Out | |
| Localization | ✓ (10 langs) | Later | Design strings for it; don't translate |
| Accessibility (screen reader) | ✓ | V1 baseline via AccessKit (egui built-in) | Also what egui_agent introspects |
| Auto-update / installers (MSI, DMG, deb/AppImage) | ✓ | **V1** installers (Win → Linux → mac) / V1.x update check | `cargo-dist` / `cargo-packager` |
| Telemetry | ✓ | Out | OSS; none |
| Command-line / URI launch (`-S server -D db file.sql`) | ✓ | V1.x | |

## 8. DBA tooling (extensions in ADS)

| Feature | Proposed | Notes |
|---|---|---|
| Backup / Restore dialogs | Later | Fabric/Azure SQL don't even have them |
| Import flat file wizard (CSV → table with type inference) | **V2** | Natural fit: we already have Arrow; "load a Parquet/CSV into a table" ★ |
| dacpac/bacpac wizard | Out | `SqlPackage` exists |
| Schema Compare | Later | Big; SSMS/VS Code ship it |
| SQL Database Projects | Out | |
| SQL Agent (jobs/alerts/operators) | Out | Microsoft sent users back to SSMS too |
| Profiler (XEvents) | Later | |
| Query Store UI | Later ★ | ADS never had it; a focused "top queries / regressed plans" view is cheap and valuable. Not V1. |
| Table Designer | Later | |
| Machine Learning, Arc, BDC, migration, data virtualization, Kusto/Monitor, Cosmos/Mongo, PostgreSQL/MySQL extensions | Out (engines revisited in V2 via driver trait) | |

## 9. Beyond ADS — candidates for the "reach past" section

Ranked from `docs/research/04_*` §5 and my own judgment. ★ everywhere. Those I'd make **flagship** are bold.

1. **Arrow-native result engine**: streaming, columnar, disk-spilling, memory budget, no cell-size cap, every exporter reads the same batches. (V1 — it's the foundation, not a feature.)
2. **Parquet / Arrow / Delta export**, later to OneLake/ADLS/S3. (V1 / V1.x)
3. **SQL history as a database**: searchable, starred, versioned tab contents, restore closed tabs. (V1)
4. **Column profiler** on every result (null %, distinct, min/max, histograms; click to filter). (V2)
5. **Local re-query with DataFusion**: SQL over result tabs, join prod vs dev, diff two runs, pivot. (V2)
6. **Plan Explorer-class plan analysis** (statements tree, cost gradient, est-vs-actual, index analysis). (V2, staged from V1 viewer)
7. **Fabric-native**: workspace browsing, "this is Fabric" capability hints, Query Insights (CU/duration of past runs), OneLake Delta preview. (V2)
8. **Safe mode + staged edits** with review-the-SQL-diff before commit; read-only connections. (V2 with Edit Data; read-only flag V1.x)
9. **BYO-model AI**: explain/fix/optimize, NL→SQL grounded in the catalog, explain-plan; OpenAI-compatible/Anthropic/Ollama endpoints; read-only classifier + explicit approval for writes; per-connection instructions file. (V2)
10. **Notebook mode** with chained cells (previous result = local table). (V2)
11. **Copy-as everything** + Open in Excel + execute-current-statement + inline timings. (V1/V1.x polish)
12. **Transposed/record view, aggregate view, totals row.** (V1.x)
13. **Global object search + Describe popup.** (V1.x)
14. **Streaming export from the wire** with progress; **Save as table (CTAS)** action. (V1.x)
15. **Import Parquet/CSV/Arrow into a table** (bulk insert from Arrow). (V2)
16. **Session isolation**: metadata connection never blocks queries; "keep running when tab closed, notify me." (V1 / V1.x)
17. **Agent-drivable UI** via egui_agent — Cobalt is also a tool Claude can operate: run a query, read the grid, export a Parquet. (V1 — a real differentiator for people who work with agents.)
18. Query Store view; lightweight XEvents profiler; Query Hint trial runner. (Later)
19. ER diagram from selected tables (auto layout, FK edges, Mermaid export). (Later)
20. One-click local SQL Server container. (Later)

## 10. Open decisions — these are yours

| # | Decision | Options | My recommendation |
|---|---|---|---|
| D1 | **egui version** | (a) Hold at 0.35 with prior crate versions; (b) bump egui_agent to 0.36 and start current | **(b)** — small change in a crate you own; avoids starting a new project one version behind a fast-moving ecosystem |
| D2 | **TDS driver** | `tiberius-ng` / Microsoft `mssql-tds` 0.1.0 / `mssql-client` / ODBC | **`tiberius-ng` behind our own `Driver` trait**; re-evaluate Microsoft's driver quarterly; ODBC as an opt-in connection type later |
| D3 | **Entra interactive auth — app registration** | (a) Register a multi-tenant public-client app owned by the project; (b) users supply their own client ID; (c) `az login` only | **(a) with (b) as override and (c) as fallback.** (a) needs someone to own an Entra tenant registration for the OSS project — can be yours to start. Conditional-access policies requiring device compliance may block (a); (c) covers those. |
| D4 | **IntelliSense engine** | (a) Pure Rust: own lexer + catalog cache + `sqlparser` for aliases; (b) bundle SqlToolsService (.NET, ~100 MB) as LSP subprocess | **(a)**, with an LSP client behind a feature flag so (b) stays possible. Pure-Rust is the identity of the project; (a) is good enough for V1 and improves incrementally. |
| D5 | **Notebooks** | Out / V2 SQL-only / V2 with Python | **V2, SQL + Markdown only, `.ipynb`-compatible**, chained cells via DataFusion. Never Jupyter kernels. |
| D6 | **Dashboards / insight widgets** | Out / V2 "pin a query as a tile" / full ADS model | **V2 lightweight.** Design the query model so a saved query can render as a tile. |
| D7 | **Edit Data** | V1.x / V2 / Out | **V2** with staged-edit safe mode. Did you use it in ADS? |
| D8 | **Other engines in V2** | PostgreSQL / MySQL / DuckDB / SQLite / Kusto | Driver trait in V1; **DuckDB + SQLite first** in V2 (pure local, pairs with the Arrow story), then PostgreSQL. Which do you actually need? |
| D9 | **Delta export targets** | Local only / + OneLake & ADLS / + S3 | **Local in V1, OneLake+ADLS in V1.x** (same Entra token), S3 when someone asks |
| D10 | **AI features** | Out / V2 BYO-model | **V2 BYO-model** (OpenAI-compatible + Anthropic + Ollama); never a required cloud dependency |
| D11 | **Layout engine** | Fixed IDE layout / `egui_dock` / `egui_tiles` | Decide in architecture; lean **`egui_dock`** (tear-out tabs = ADS/VS Code muscle memory) |
| D12 | **License** | MIT / Apache-2.0 / dual / MPL | **MIT OR Apache-2.0** (Rust convention; matches egui_agent) |
| D13 | **Plan-viewer icon set** | Draw our own / Phosphor glyphs / commission | Own simple set, SSMS-*shaped* not SSMS-*copied* |
| D14 | **What did you actually use weekly in ADS?** | — | This decides V1's edges more than anything above |
