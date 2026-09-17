# Competitive survey: what a next-gen SQL Server / Azure SQL / Fabric client should steal, and what it should fix

Researched 2026-09-16. Claims with a URL were checked against the cited page; claims marked **[memory]** are from prior knowledge and were not re-verified.

---

## 0. Executive summary

The tooling landscape after Azure Data Studio's retirement (end of support 2026-02-28) splits into three camps:

1. **Microsoft's two survivors.** SSMS 21/22 (Windows-only, Visual Studio shell, now with dark mode, Git, Copilot Ask/Agent modes, SQL formatter, schema compare, Database DevOps, a Query Hint Recommendation tool) and the VS Code MSSQL extension (cross-platform, moving fast: results grid with filter/sort/freeze, Query Plan viewer, Schema Designer, Schema Compare, local SQL containers, SQL Notebooks, Query Profiler, Fabric browsing, Copilot Ask/Agent/slash commands). Both are heavy hosts (VS shell / VS Code) and neither is a "fast native SQL client".
2. **Cross-database GUI clients.** DataGrip (the deepest data editor and refactoring), DBeaver (breadth, ERD, data transfer, now MCP/AI), TablePlus (speed, safe mode, native feel), Beekeeper (folders, editable results, query magics, streaming export), DbGate (perspectives, data archive, macros), HeidiSQL/Navicat/Toad/DbSchema (older but with specific gems: Navicat data profiling, Toad compare/sync, DbSchema HTML docs).
3. **Analytical / notebook / TUI tools.** DuckDB UI (column explorer with per-column profiles), Hex/Deepnote/Observable (chained SQL cells, column summary charts, table cells with conditional formatting and totals), Malloy/Bruin/dbt Power User (semantic models, lineage, rendered queries), Harlequin/lazysql/rainfrog (keyboard-first, history + favorites, instant start).

What people say they miss from ADS is consistent: a **simple, focused, fast-starting, cross-platform (Mac!) SQL tool**, notebooks, dashboards/insight widgets, server groups and the "queries as files in folders" model, charting of results, and the JSON cell viewer. What they hate in SSMS/VS Code is: slowness/crashes, 32-bit-era limits (65,535-char cell copy cap, grid memory blowups), cluttered general-purpose IDE, results grids that lose selection or don't copy cleanly, and the need to bounce between tools (Profiler, Agent, Plan Explorer).

The strongest differentiators for a new native client, in order of leverage: (a) a **result engine** that streams, spills to disk, profiles columns, and lets you re-query results locally with DuckDB; (b) a **Plan Explorer-class execution plan viewer** (nothing free has replaced it well); (c) **SQL history + session restore as a first-class database**, not a text log; (d) BYO-model AI with read-only guardrails; (e) Fabric-native connection/browsing.

---

## 1. Tool-by-tool survey

### 1.1 SSMS 21 / 22 (2024-2026)

Sources: [SSMS 21 GA post](https://techcommunity.microsoft.com/blog/sqlserver/sql-server-management-studio-ssms-21-is-now-generally-available-ga/4415230), [Brent Ozar on v21](https://www.brentozar.com/archive/2024/11/ssms-v21-dark-mode-git-vertical-tabs-and-more/), [SSMS 22 release notes](https://learn.microsoft.com/en-us/ssms/release-notes-22), [Copilot chat docs](https://learn.microsoft.com/en-us/ssms/github-copilot/chat), [Agent mode](https://learn.microsoft.com/en-us/ssms/github-copilot/agent-mode), [Query Hint tool](https://learn.microsoft.com/en-us/ssms/query-hint-tool/hint-tool-overview), [22.7 announcement](https://techcommunity.microsoft.com/blog/sqlserver/announcing-the-release-of-ssms-22-7-0---and-many-previews/4526908).

Distinctive/new features worth noting:

1. **64-bit VS 2022 shell, dark theme, vertical tabs, tab coloring by project/file type, min/max tab width** (v21). Dark mode was "one of two features users have been asking for for a very long time"; execution plans initially lacked dark support (Brent Ozar).
2. **Git integration** via the Code Tools workload; **Database DevOps workload (preview)** in 22.4+: SQL projects in Solution Explorer, Create Project From Database, 74 item templates, Publish with SQLCMD variables, `.scmp` and `.publish.xml` files, DACPAC/NuGet references.
3. **GitHub Copilot**: chat + inline chat (Alt+/) with diff-view apply; right-click Document/Explain/Fix/Optimize; code completions and Next Edit Suggestions; model picker + **Bring Your Own Model** (22.1); **custom instructions** and per-database instructions; **execution context via `CONSTITUTION.md`** in the database (custom login for Copilot queries); can read the **results pane, execution plan, messages, client statistics** as context (22.5); attach `.sqlplan` files and images; generates Mermaid ER diagrams with preview; **Agent Mode (preview, 22.7)** with skills like slow-query investigation, wait category analysis, IO pressure, memory issues; Ask mode has a read-only query classifier (explicitly "not a security boundary").
4. **Modern connection dialog**: Fabric browsing in Browse tab, Azure subscription search/filter, Managed Instance browsing, connection import/export, "auto-select most recent connection" setting, horizontal layout.
5. **Results grid**: quick export to **Excel, JSON, Markdown, XML** (22.4); **zoom the results grid independently** (22.0); column data type in header tooltip (22.6); **JSON viewer for cells** ("like Azure Data Studio", 22.0); JSON display setting for text columns.
6. **Execution plans**: open plan in a new tab; JSON/vector index operators.
7. **Query Hint Recommendation tool (preview)**: automated trial of Query Store hints inside a time budget, rejects hints worse than baseline.
8. **SQL Formatter (preview, 22.7+)** with before/after preview, leading/trailing comma, `AS` vs `=` alias style, JOIN/ON newline options, tabs vs spaces.
9. **Schema Compare (preview, 22.7)** for databases, dacpacs, projects; Group By action/schema/type.
10. **Object Explorer: group by schema** toggle (22.4); Registered Servers multi-server connect with Cancel.
11. Arm64 support; Unified Settings tab; "What's New" page; Rename Tabs dialog; Linked Server wizard; vector/JSON index dialogs.

Takeaway: SSMS is converging on Copilot + DevOps, but the core query/results loop is still the 2005-era grid with incremental fixes, and it's Windows-only.

### 1.2 VS Code MSSQL extension (v1.25 -> v1.45, Oct 2024 -> Aug 2026)

Sources: [CHANGELOG](https://github.com/microsoft/vscode-mssql/blob/main/CHANGELOG.md), [What's new/next](https://devblogs.microsoft.com/azure-sql/mssql-vs-code-whats-new-next/), [Feb 2026 post](https://devblogs.microsoft.com/azure-sql/vscode-mssql-feb-2026/), [Mar 2026 post](https://devblogs.microsoft.com/azure-sql/vscode-mssql-march-2026/), [June 2026 post](https://devblogs.microsoft.com/azure-sql/vscode-mssql-june-2026/), [Fabric preview](https://visualstudiomagazine.com/articles/2025/09/18/vs-code-sql-extension-previews-microsoft-fabric-connectivity.aspx), [Query Profiler docs](https://learn.microsoft.com/en-us/sql/tools/visual-studio-code-extensions/mssql/mssql-query-profiler?view=sql-server-ver17), [SQL Notebooks](https://learn.microsoft.com/en-us/sql/tools/visual-studio-code-extensions/mssql/mssql-sql-notebooks?view=sql-server-ver17).

1. **Modern connection dialog** with Entra sign-in, Azure/Fabric browse tree with search, connection string paste, **Connection Groups** (1.33), color-coded connections, Fabric "zero connection strings" browse.
2. **Object Explorer filtering** (by name/schema/etc.) and **Global Object Search** (1.40).
3. **Results grid**: open in new tab or panel, filter rows by value, sort, maximize a result set, **Text View** mode (1.35), copy / copy with headers / copy headers, save as CSV/JSON/Excel, **Beta Results Grid** with column show/hide, **freeze columns**, better cell selection, "executing" timer, state management (1.45).
4. **Query Plan visualizer** (interactive, with **missing index recommendations** shown, 1.45.1).
5. **Schema Designer** (visual ERD-style modeling, GA 1.35), later with Copilot natural-language schema creation, change diff, ORM script generation (Prisma/EF Core/etc.), validation for missing PKs (1.43).
6. **Schema Compare** GA (1.35), **Table Designer**, **Edit Data** (browse/modify table with filter/sort/export), **Data-tier Application** wizard (dacpac/bacpac), **Backup/Restore, Flat File Import, database dialogs** (1.40 -> GA 1.42).
7. **Local SQL Server containers** (create/start/stop Docker containers from the extension, GA 1.35).
8. **Copilot**: Ask mode, Agent mode, slash commands (`/explain`, `/fix`, `/optimize`, `/runQuery`, etc.) GA 1.37; MCP-style tools that read schema and run queries.
9. **SQL Notebooks** (Jupyter `.ipynb`, T-SQL kernel, IntelliSense, multi-kernel) GA 1.43 — the direct answer to "we lost ADS notebooks."
10. **Query Profiler** (Extended Events based, sessions with start/pause/resume, filter by text/db/duration, CSV export) GA 1.42.
11. **Data API builder** integration (REST/GraphQL/MCP endpoints from tables) and **Azure SQL provisioning** wizard.
12. **New SQL formatter (preview 1.45)**; ADS Migration Toolkit; static code analysis for SQL projects.

Complaints (GitHub issues): edit grid squeezed by the bottom panel ([#20404](https://github.com/microsoft/vscode-mssql/issues/20404)); "Query results always open in a new tab" ([#17503](https://github.com/microsoft/vscode-mssql/issues/17503)); lost ability to see two result sets side by side ([#18316](https://github.com/microsoft/vscode-mssql/issues/18316)); Ctrl+A selects the whole webview, then Ctrl+C copies nothing ([#18642](https://github.com/microsoft/vscode-mssql/issues/18642)); stale selection after sort/filter ([#22789](https://github.com/microsoft/vscode-mssql/issues/22789)); selection highlighting unreadable ([#18211](https://github.com/microsoft/vscode-mssql/issues/18211)). The recurring theme: a webview grid inside a general-purpose editor fights the host for keyboard, clipboard and layout.

### 1.3 DataGrip / JetBrains

Sources: [Data editor docs](https://www.jetbrains.com/help/datagrip/data-editor-and-viewer.html), [Explore data](https://www.jetbrains.com/help/datagrip/explore-data-in-data-editor.html), [2026.1 blog](https://blog.jetbrains.com/datagrip/2026/03/26/datagrip-2026-1-redesigned-query-files-data-source-templates-in-your-jetbrains-account-ai-agents-in-the-ai-chat-explain-plan-flow-enhancements-and-more/), [2026.2 blog](https://blog.jetbrains.com/datagrip/2026/07/16/datagrip-2026-2-ai-agent-skills-mcp-tools-and-cli-commands-for-data-source-management-bundled-jdbc-drivers-and-improved-session-control/), [Query consoles are coming back](https://blog.jetbrains.com/datagrip/2025/12/18/query-consoles-are-coming-back/).

1. **View modes on any result: Table, Tree, Text, Transposed, Record view** (single row in a side panel) — transposed is the killer for wide rows.
2. **Aggregate view**: select cells, get sum/avg/min/max/count/distinct etc.; user-scriptable aggregators.
3. **Local (client-side) filter and sort** per column, plus server-side WHERE/ORDER BY in the same toolbar; "clear all local filters" action.
4. **Data extractors**: copy/save results as CSV, TSV, JSON, Markdown, HTML, SQL INSERT/UPDATE, and custom scripted extractors; **Copy to Database** (push a result set into another data source).
5. **Compare data** of two tables/results with configurable tolerance; schema diff.
6. **Submit/revert edits with auto or manual transaction mode**; **SQL Expression mode** in cells (`NOW()` instead of a literal) (2026.2).
7. **Local History** of every query file (per-file undo timeline) **[memory]**; **Live templates** (snippet expansion with tab stops) **[memory]**; **rename refactoring** of aliases/columns across the file **[memory]**.
8. **Explain plan flame graph** (Total Cost / Startup Cost sub-tabs) + Operations Tree with side detail panel (2026.1).
9. **Query files vs consoles**: JetBrains tried removing consoles (2025.3), user backlash forced them back; files are attached to data sources and grouped in the explorer (2026.1). Lesson: users want both scratch consoles and durable files.
10. **Session control modes** (single shared, shared + separate introspection session, per-client sessions) so metadata refresh never blocks your query (2026.2).
11. **Execution time inline** next to each statement on its first line (2026.2); **console tab names from variables** (data source/db/schema).
12. **Data source templates in JetBrains account** (settings without credentials, syncable); CLI `datagrip dataSources list/manage` with JSON I/O; **MCP server + agent skills** (`database-tools`, `database-text-to-sql`) with four consent levels, and history of AI-generated queries.
13. **Geo viewer** for geometry columns and inline **charts** from a result set; Snowflake role/warehouse switcher dropdown.

### 1.4 DBeaver

Sources: [Features](https://dbeaver.com/features/), [26.1 release](https://dbeaver.com/2026/06/10/dbeaver-26-1/), [Spatial](https://dbeaver.com/docs/dbeaver/Working-with-Spatial-GIS-data/), [AI assistant](https://dbeaver.com/docs/dbeaver/AI-Smart-Assistance/).

1. **ERD** auto-generated per schema/table, editable, exportable.
2. **Data transfer wizard** (DB->DB, DB->file, file->DB) with column mapping, saved as **tasks** that can be scheduled.
3. **Data compare** and **schema compare** across connections (paid editions).
4. **Dashboards** of DB health metrics; **spatial viewer** (map for geometry).
5. **Grouping panel / calc panel** on results (group-by and aggregates without rewriting SQL) **[memory]**; **Query Manager** log of every executed statement with duration/rows **[memory]**.
6. **AI**: "Explain and Fix" on errors, send execution plan to AI chat, describe database object, external **MCP servers** in chat; `dbvr` CLI is itself an MCP server (26.1).
7. **Redesigned execution plan** as collapsible tree with cost indicators and detail side panel (26.1); native **Microsoft Fabric driver** (26.1).
8. **Perceived weaknesses**: Java/Eclipse heft and startup; many features gated to paid editions.

### 1.5 TablePlus

Sources: [Safe mode](https://docs.tableplus.com/gui-tools/code-review-and-safemode/safe-mode), [Streaming results](https://docs.tableplus.com/query-editor/streaming-results-and-async-loading), [Query history](https://docs.tableplus.com/query-editor/query-history), [Multiple results](https://tableplus.com/blog/2018/08/show-multiple-results-separately.html), [Shortcut keys](https://docs.tableplus.com/utilities/shortcut-keys), [11 tips](https://tableplus.com/blog/2018/05/11-tips-to-boost-productivity-with-tableplus.html).

1. **Native app, tiny footprint, instant start** — the thing ADS users on Mac cite most.
2. **Safe mode + Code Review**: every grid edit is staged; a diff of the generated SQL is shown before commit (Cmd+S), and safe mode blocks accidental writes on production.
3. **Streaming results** shown as they arrive; loading is async and never blocks the UI.
4. **Inline editing** of table rows and of query results; multi-row paste.
5. **Keyboard-centric**: Cmd+P open anything (tables, connections), Cmd+K quick switcher, Cmd+R run, Cmd+E/Cmd+F filter/find in grid, Cmd+Shift+F "filter this column by cell value" **[memory]**.
6. **Multi-tab and multi-window** per connection; **split panes**; **multiple result sets in tabs**.
7. Query history sidebar; connection **color tags** and **groups**; SSH tunnel built in; **BYOK AI** text-to-SQL.
8. **DBngin** companion: one-click local Postgres/MySQL/Redis servers on macOS ([dbngin.com](https://dbngin.com/)) — a model for "one-click local SQL Server container" (which VS Code MSSQL now has).

### 1.6 Beekeeper Studio (and Outerbase Studio)

Sources: [5.7 release](https://www.beekeeperstudio.io/blog/release-5.7-folders-editable-results), [Query magics](https://docs.beekeeperstudio.io/user_guide/query-magics/), [Data export](https://docs.beekeeperstudio.io/user_guide/data-export/), [Saved queries](https://docs.beekeeperstudio.io/user_guide/sql_editor/saving_queries/), [Pinned tables](https://github.com/beekeeper-studio/beekeeper-studio/issues/247), [Outerbase Studio](https://outerbase.com/blog/outerbase-studio-open-source-database-management/).

1. **Pinned tables** persisted per saved connection; **folders** for saved queries and connections with drag-drop, pin/unpin, and persisted expand state (5.7).
2. **Editable query results**: parses the SQL to map result columns back to source tables, integrated with manual-commit mode (5.7).
3. **Query Magics**: column aliases like `url__format__link`, `__format__image`, `__format__check`, `__format__money`, `__format__progress`, `__goto__` (jump to related table filtered by the value) — pure SQL, zero DSL, delightful.
4. **Streaming export** of a giant query directly to CSV/JSON/JSONL/SQL without materializing in memory.
5. Filter bar with an "<>" toggle into a raw WHERE-clause; **row as JSON with FK expansion**; JSON cell edit modal with syntax check; plugin system.
6. Outerbase Studio (formerly LibSQL Studio): browser-only, "data editor capable of handling thousands of rows and columns without overwhelming RAM", schema editor, connections stored locally. Notable as a design reference for a lightweight virtualized grid.

### 1.7 DbGate

Sources: [Features](https://www.dbgate.io/features/), [Perspectives](https://docs.dbgate.io/perspectives/index.html), [Data archive](https://docs.dbgate.io/dbgate/working-with-data/advanced-data-tools/data-deployer/index.html).

1. **Perspectives**: read-only nested master/detail views built by clicking through foreign keys (join-free exploration of related data).
2. **Data archive**: table data saved locally as NDJSON folders that you can browse/edit like tables, then **visually diff and deploy** back to a database.
3. **Macros** for batch edits in the grid; **query designer** that joins across SQL and NoSQL sources; FK lookups; charts with geo maps; multi-table import/export in many formats; ER diagrams; **AI chatbot that knows the schema**; DbGate Cloud for shared scripts.

### 1.8 HeidiSQL, Navicat, Toad, DbSchema, Sequel Ace, Sqlectron

- **HeidiSQL** ([site](https://www.heidisql.com/)): multiple query tabs each with **sub-tabs per batch result**; editable results; snippet files; query history with per-item delete; export grid rows to CSV/HTML/XML/SQL/Markdown/JSON **[memory]**; very fast Delphi native UI.
- **Navicat 17** ([Data profiling](https://www.navicat.com/en/company/aboutus/blog/2425-data-profiling-in-navicat-17), [Data dictionary](https://www.navicat.com/en/company/aboutus/blog/2426-create-a-data-dictionary-in-navicat-17.html)): **column profiling** (null %, distinct/unique counts, repeats, min/max, value-distribution chart with "spotlight" to highlight rows) on filtered or full data; generated **data dictionary** documentation; model designer.
- **Toad for SQL Server** ([Quest](https://www.quest.com/products/toad-for-sql-server)): schema/data/server **compare & sync with generated change scripts**, job scheduling, **transaction log reader** for recovery; Toad's classic "Describe" popup (F4) on any object **[memory]**.
- **DbSchema** ([features](https://dbschema.com/features.html)): offline design model (`.dbs` file), **interactive HTML5/PDF/Markdown docs from the diagram**, visual query builder, layouts of the same schema for different audiences.
- **Sequel Ace** ([favorites](https://sequel-ace.com/favorites.html)): **query favorites with tab-trigger expansion**, global vs per-connection favorites, bundles (scriptable actions on selection) — macOS-native speed.
- **Sqlectron** ([GitHub](https://github.com/sqlectron/sqlectron)): minimal; paginates results client-side during rendering. Mostly a warning about what happens when a small project stalls.

### 1.9 Azure Data Studio itself — what to preserve

Sources: [Retirement notice](https://devblogs.microsoft.com/azure-sql/azure-data-studio-retirement/), [What's happening](https://learn.microsoft.com/en-us/sql/tools/whats-happening-azure-data-studio?view=sql-server-ver17), [Dashboards](https://www.sqlshack.com/server-and-database-dashboards-in-azure-data-studio/), [Agent notebook jobs](https://learn.microsoft.com/en-us/shows/data-exposed/managing-sql-server-agent-jobs-with-notebook-jobs-in-azure-data-studio), [Server groups](https://learn.microsoft.com/en-us/azure-data-studio/server-groups), [Migration guide (jamsql)](https://jamsql.com/blog/azure-data-studio-retiring-migration-guide/).

1. **Server groups with colors** and connection tree; **fast startup**; Mac/Linux support.
2. **SQL notebooks** (`.ipynb` with SQL kernel), Jupyter Books, **Notebook Jobs** where each SQL Agent run's output is saved as a notebook.
3. **Dashboards with insight widgets** (custom JSON-configured chart widgets driven by queries) — no counterpart anywhere today.
4. **Chart viewer on any result** (bar/line/pie/scatter, "Create insight" from a query).
5. **JSON cell viewer**, results-to-CSV/Excel/JSON/XML/Markdown export, Copy with headers, "Copy query with results" for Teams/email.
6. **Query history extension**, **SQL Agent / Profiler / Import wizard / Schema Compare / Kusto / PostgreSQL** extensions.
7. Command palette + VS Code keybindings and integrated terminal.

### 1.10 SentryOne / SolarWinds Plan Explorer — the execution-plan gold standard

Sources: [Results docs](https://documentation.solarwinds.com/en/success_center/sqlsentry/content/planexplorer/results.htm), [Integrated overview](https://documentation.solarwinds.com/en/success_center/sqlsentry/content/planexplorer/integrated-planexplorer-overview.htm), [Product page](https://www.solarwinds.com/free-tools/plan-explorer).

What it does better than SSMS (still free, still Windows-only, effectively unmaintained **[memory]**):

1. **Statements tree** for multi-statement batches/procs with per-statement estimated/actual metrics; sortable to find the worst statement instantly; filters for control structures (IF/WHILE/EXEC).
2. **Plan diagram**: scale connector lines by rows / data size / cost; **yellow-to-red cost gradient**; per-node vs cumulative cost; zoom 6-400%; layout modes, rotation, flattening; condensed layout so nodes are never truncated.
3. **Top Operations** sortable grid (cost desc by default); **Plan Tree** with column chooser and **highlighting of estimate vs actual discrepancies**.
4. **Query Columns** tab: how each column is accessed, flags bookmark lookups and non-covering indexes; **Index Analysis** with scoring, recommendations and stats; create index from the plan.
5. **Join Diagram** showing how nested views resolve to base tables.
6. **Parameters** tab (compiled vs runtime) for parameter sniffing; **Expressions** tab; **Table I/O** breakdown by object; **Wait stats** tab for actual plans.
7. **Actual plan capture with live statistics and replay**, "actual cost recosting", and **Get Actual Plan discards results** to measure the query, not the rendering **[memory]**.
8. **Plan history** per session file (`.queryanalysis`), plan comparison, **anonymize plan** for sharing (strip object names) **[memory]**, deadlock graph analysis (paid).
9. Right-click any SSMS plan -> open in Plan Explorer.

### 1.11 Redgate SQL Prompt

Sources: [SQL History](https://documentation.red-gate.com/sp/ssms-tab-management/sql-history), [Product page](https://www.red-gate.com/products/sql-prompt/), [Keyboard](https://www.red-gate.com/hub/product-learning/sql-prompt/sql-prompt-by-keyboard/).

1. **SQL History**: every tab's content is versioned as you type; searchable; star favorites; rename; restore open tabs after crash; "reopen a query you closed without saving"; retention trimming (7-day default for old versions).
2. **Execute current statement** without selecting it (a repeated request in SSMS comment threads).
3. **Open in Excel** from the results grid; **Script as INSERT** from selected rows; **copy selected values into an IN list** **[memory]**.
4. **Formatting styles** shared across a team; **code analysis** rules inline; **tab coloring by environment**; snippets with placeholders; **Encapsulate as stored procedure / Inline EXEC / Qualify object names / Expand wildcards** refactorings **[memory]**; AI text-to-SQL and explain.

### 1.12 Fabric portal SQL query editor

Source: [Query using the SQL query editor](https://learn.microsoft.com/en-us/fabric/data-warehouse/sql-query-editor), [Copilot GA](https://blog.fabric.microsoft.com/en-US/blog/copilot-and-query-editor-in-sql-database-in-fabric-ga-update/), [SSMS 22 meets Fabric DW](https://blog.fabric.microsoft.com/en-us/blog/ssms-22-meets-fabric-data-warehouse-evolving-the-developer-experiences/).

1. **Save as view / Save as table (CTAS) / Open in Excel / Visualize results (Power BI) / Explore this data** directly from a selected SELECT.
2. Results capped at **10,000 rows** preview, **search-within-results** filter box, Copy with/without headers, multiple result sets dropdown.
3. **Autosave** of queries with indicator; My queries / Shared queries folders; SQL templates dropdown.
4. **Background execution on tab close** ("keep running, notify me when done").
5. Copilot: inline completions, chat, quick actions Fix/Explain with shortcuts (Ctrl+Alt+F/E), read-only vs read/write-with-approval modes.
6. Limits worth exploiting: each Run is a **separate session/batch** (no SET/session context/transactions across runs) — a desktop client with a persistent session is strictly better for Fabric DW.

### 1.13 DuckDB UI (2025)

Source: [DuckDB Local UI](https://duckdb.org/2025/03/12/duckdb-ui), [devclass note on licensing](https://devclass.com/2025/03/19/duckdb-project-releases-local-web-ui-but-not-as-open-source/).

1. **Column Explorer** panel next to every result: per-column type, null %, distinct count, min/max, **distribution histogram**; click a column for detail.
2. **Table summaries** on click (row count, column profiles, DDL, 100-row preview).
3. Notebook cells, run cell or selection, results returned in DuckDB's binary DataChunk form (no JSON round-trip); sort/filter/transform in the UI, export clipboard/file.
4. Local-only by default; UI itself is not open source (an opening for an OSS competitor).

### 1.14 Notebook-style tools: Hex, Deepnote, Observable, Querybook

Sources: [Hex SQL cells](https://learn.hex.tech/docs/explore-data/cells/sql-cells/sql-cells-introduction), [Hex table cells](https://learn.hex.tech/docs/explore-data/cells/visualization-cells/table-display-cells), [Deepnote SQL blocks](https://deepnote.com/docs/sql-cells), [Observable data table cell](https://observablehq.com/blog/introducing-data-table-cell), [Querybook](https://github.com/pinterest/querybook).

1. **Chained SQL**: each cell's result is a named variable; downstream SQL cells query it (Hex "dataframe SQL"; Deepnote "query preview mode" stores the SQL and inlines it as a CTE).
2. **Column summary charts in every column header** (Observable): mini-histograms/bars, hover shows range; hovering a row highlights its marks.
3. **Table cells** with conditional formatting rules, color scales, totals row computed over the full dataset, filters per data type, hidden/renamed columns (Hex).
4. **Querybook**: every executed query is parsed to learn which tables are hot, who uses them, and example queries; **templated queries with variables**; table metadata hover in the editor.

### 1.15 Semantic / pipeline tools: Malloy, Bruin, dbt Power User

Sources: [Malloy extension](https://docs.malloydata.dev/documentation/setup/extension), [Bruin extension](https://getbruin.com/docs/bruin/vscode-extension/overview.html), [dbt Power User](https://github.com/AltimateAI/vscode-dbt-power-user).

1. Malloy: nested results rendered as **nested tables/drill-downs** from a single query; built-in DuckDB so results can be re-queried locally; simple visualizations inline **[memory]**.
2. Bruin: **rendered-query preview** (Jinja resolved + materialization applied), "Preview selected query" CodeLens, column names/types pulled from the DB into asset definitions, lineage panel.
3. dbt Power User: **column-level lineage**, query results tab with **export/charts/groups**, **cost estimation** (BigQuery bytes-scanned dry run), AI doc generation, health checks.

### 1.16 TUIs: Harlequin, lazysql, rainfrog, gobang

Sources: [Harlequin](https://harlequin.sh/), [lazysql](https://github.com/jorgerojas26/lazysql), [rainfrog](https://github.com/achristmascarl/rainfrog), [rainfrog on HN](https://news.ycombinator.com/item?id=41563100).

1. Sub-second startup; keyboard-only; vim keys; `?` opens a contextual help overlay.
2. **History + favorites** (rainfrog), query history (lazysql, Harlequin), **external editor** hand-off, **yank** cell/row to clipboard, CSV export with batch size, WHERE-clause filter input, JSON viewer, data catalog sidebar, multiple adapters via plugins.

---

## 2. What users complain about (ADS, SSMS, VS Code MSSQL)

Sources: [HN "ADS is being killed"](https://news.ycombinator.com/item?id=42971640), [ADS issue #26289 "Please reconsider"](https://github.com/microsoft/azuredatastudio/issues/26289), [Kevin Chant's reflections + comments](https://www.kevinrchant.com/2025/02/07/thoughts-about-the-azure-data-studio-retirement-announcement/), [SQLServerCentral editorial](https://www.sqlservercentral.com/editorials/the-end-of-azure-data-studio), [InfoQ Copilot/color-codes reactions](https://infoq.com/news/2025/08/mssql-vscode-copilot-color-codes), [Beekeeper's ADS alternatives](https://www.beekeeperstudio.io/blog/azure-data-studio-alternatives-free), [MS Q&A slow SSMS](https://learn.microsoft.com/en-us/answers/questions/1511788/sql-server-management-studio-is-really-slow), [SSMS 21 crashes](https://learn.microsoft.com/en-us/answers/questions/5641978/most-recent-usable-version-of-ssms), [65,535 char limit](https://github.com/microsoft/azuredatastudio/issues/19480), [SSMS memory](https://sqldba.blog/how-to-increase-maximum-characters-displayed-in-ssms/).

**About ADS's retirement (what people will miss):**
- "A simple and focused environment" vs. VS Code "too cluttered when I add SQL tools on top of everything else"; on macOS ADS "was the only full-featured SQL tool" (issue #26289).
- The **Explorer pane of queries organized in folders with descriptive names** (issue #26289).
- **Notebooks** and Jupyter Books (Polyglot Notebooks in VS Code couldn't open them), **dashboards/insight widgets**, **charting**, **connection organization (server groups)**, multi-engine extensions (Postgres, Kusto) — Kevin Chant comments, jamsql migration guide, Beekeeper post.
- HN: "Agent, Profiler, DB administration won't be ported... go back to SSMS for the other stuff" — the fragmentation is the pain; "colour-coded connections are not a compelling replacement" (InfoQ). One reviewer: "the gap is astonishing... nearly non-functional for my workflow."
- Steve Jones's wish sums up the market: "a cross platform editor that was simple and fast, but not one based on VSCode. One that's written to just manage queries."
- Where people are going: VS Code MSSQL (the official path), SSMS on Windows, DBeaver (free/broad), DataGrip (paid/best editor), TablePlus (fast/native, Mac), DbGate, Beekeeper, DbVisualizer, JamSQL/Chat2DB/Mako (AI-first newcomers).

**About SSMS:**
- Random slowness/unresponsiveness when connecting, right-clicking Object Explorer, opening new queries (long-running MS Q&A thread); **SSMS 21 crashes on VPN reconnect** driving users back to v20.
- 32-bit legacy: **65,535 character cell copy limit**, grid runs out of memory on large results; "Results to text" as a workaround.
- Missing "execute current statement" shortcut (Brent Ozar comments); dark mode incomplete; SSIS/SSAS missing in 21 preview; Copilot "ate 50% of usage credits" (22.3.2 note).
- Tooling sprawl: Profiler, Plan Explorer, SQL Prompt, Agent, DTA are all separate.

**About VS Code MSSQL:** see 1.2 — grid viewport crushed by the terminal panel, results always in new tab, side-by-side result sets lost, Ctrl+A/Ctrl+C inconsistencies, stale selection after sort, selection styling; general "webview inside an IDE" friction; Fabric endpoint connection bugs ([#22099](https://github.com/microsoft/vscode-mssql/issues/22099)).

---

## 3. Large-result handling: what the best tools do and what nobody does yet

Evidence: TablePlus streams and never blocks ([docs](https://docs.tableplus.com/query-editor/streaming-results-and-async-loading)); Beekeeper streams exports to file ([docs](https://docs.beekeeperstudio.io/user_guide/data-export/)); DuckDB UI keeps results in DuckDB's binary chunk format and profiles columns ([blog](https://duckdb.org/2025/03/12/duckdb-ui)); Fabric caps at 10,000 preview rows ([docs](https://learn.microsoft.com/en-us/fabric/data-warehouse/sql-query-editor)); SSMS has a 65,535 char cap and memory limits; Observable/Navicat/DuckDB UI show column summaries; JDBC/SQLAlchemy literature on server-side cursors and fetch size ([MS JDBC caching sample](https://learn.microsoft.com/en-us/sql/connect/jdbc/caching-result-set-data-sample?view=sql-server-ver16)).

Design ideas, roughly ordered by impact:

1. **Stream rows into the grid as they arrive** (TDS is naturally streaming; render the first batch within ~50 ms; show a live row counter and elapsed time; keep the editor usable). Every native client that "feels fast" does this.
2. **Columnar, disk-spilling result store.** Store fetched rows as Arrow record batches; keep N MB in memory and spill batches to a temp file (or a local DuckDB table) beyond that. Grid is virtualized over batch offsets, so 10M rows scroll fine. Nothing in the SQL Server space does this; DuckDB UI's binary chunks are the closest.
3. **"Stop after N rows" and "peek" modes**: a global default (e.g., 10k) with a one-click "fetch more / fetch all / cancel" bar; a `-- @limit 1000` magic comment; a "SELECT TOP wrapper" toggle that rewrites the outer query for Fabric DW cost control. Fabric's portal only offers a hard cap — expose it as a choice.
4. **Memory budget per tab and per app**, visible in the status bar, with automatic spill and a warning before OOM (SSMS's failure mode is silent).
5. **Column profile panel** on every result (DuckDB UI/Navicat/Observable): type, null %, distinct count, min/max/mean, top-k values, histogram; computed lazily in a background thread over the Arrow batches, and refined as more rows stream in. Click a histogram bar to filter the grid.
6. **Header mini-charts** (Observable) as the compact version of the above; toggleable.
7. **Cell content limits done right**: show large text/JSON/XML/varbinary as truncated previews with an "open in viewer" affordance that re-fetches the full value on demand (`SUBSTRING`/`DATALENGTH` probe) rather than a hard 64k cap.
8. **Server-side paging option** for table browsing (OFFSET/FETCH or keyset) separate from ad-hoc query streaming; DataGrip's page size control is the model.
9. **Streaming export** straight from the TDS stream to CSV/Parquet/JSONL without touching the grid (Beekeeper), with progress and cancel.
10. **Local re-query of results**: because results already sit in Arrow/DuckDB, expose "Query this result with SQL" (DuckDB) — filter, aggregate, join two result tabs, pivot — with zero server round trips. This also enables **result diffing** and **cross-connection joins** (prod vs. dev, SQL Server vs. Fabric).

---

## 4. Modern query-workbench innovations worth adopting

Grouped by area, each with the best exemplar:

**History, favorites, restore**
- Versioned, searchable **SQL history** with star/rename/restore-closed-tab and crash recovery (SQL Prompt SQL History); every execution logged with server/db/duration/rows/error (DBeaver Query Manager **[memory]**, TablePlus history).
- **Folders for saved queries and connections**, pinned tables, persisted expand state (Beekeeper 5.7); ADS-style "queries as files in a folder tree" (issue #26289).
- **Session restore** on start (SQL Prompt, ADS/VS Code hot exit).
- **Tab management**: rename, color by environment (SQL Prompt, SSMS 21 tab coloring, VS Code connection colors), pin, vertical tabs, "keep running when closed" (Fabric).

**Editor**
- Execute current statement without selecting (SQL Prompt; requested on SSMS threads).
- Schema-aware autocomplete including aliases, JOIN ON suggestions from FKs, and `*` expansion (SQL Prompt/DataGrip **[memory]**); live templates with tab stops; snippets with tab-trigger expansion (Sequel Ace).
- Formatter with before/after preview and team-shareable styles (SSMS 22.7, SQL Prompt).
- **Inline execution time per statement** on its first line (DataGrip 2026.2).
- Parameterized queries / variables (`:name`, `@name` prompt dialogs; Querybook templates; Hex Jinja) and **query chaining/cells** (Hex/Deepnote; VS Code SQL Notebooks GA).
- Refactorings: rename alias, encapsulate as proc, inline view (SQL Prompt/DataGrip **[memory]**).

**Results grid**
- Excel-style column filters + local sort with "clear all filters" (DataGrip, VS Code beta grid); find-in-results (Fabric, TablePlus).
- **Transposed / record view**, **aggregate view** of selected cells (DataGrip); totals row (Hex); grouping/calc panel (DBeaver).
- Copy as Markdown / JSON / CSV / TSV / HTML / **SQL INSERT** / IN-list; **Copy with headers** as a keyboard shortcut (an ADS ask since 2018, [#3341](https://github.com/microsoft/azuredatastudio/issues/3341)); Open in Excel (SQL Prompt, Fabric).
- JSON/XML/hex/image/geo cell viewers (SSMS 22, ADS, Beekeeper, DBeaver spatial, DataGrip geo).
- **Editable results with reverse-mapping to source tables**, staged as a reviewable SQL diff with safe mode (Beekeeper + TablePlus code review).
- **Query Magics** style column-alias formatting (links, images, money, progress bars, enum labels) (Beekeeper).
- Quick charts from any result (ADS chart viewer, DataGrip, Fabric "Visualize results").
- **Result set diff** (two tabs or two runs) and **data compare** with tolerance (DataGrip, Toad, DBeaver).

**Schema / navigation**
- Global object search across all connected DBs (VS Code 1.40, Cmd+P in TablePlus); Object Explorer filtering; group by schema (SSMS 22.4).
- ER diagram generation with editing/export (DBeaver, DbGate, DbSchema, VS Code Schema Designer); Copilot-generated Mermaid ERD (SSMS).
- Perspectives / FK drill-through (DbGate, Beekeeper `__goto__`, DataGrip related rows).
- Schema compare with grouped results and generated scripts (SSMS 22.7, VS Code, Toad).
- Data dictionary / HTML documentation generation (Navicat 17, DbSchema).

**Execution plans & performance**
- Everything in 1.10 (Plan Explorer); plus DataGrip's flame graph, VS Code's missing-index callout, DBeaver's "send plan to AI", SSMS's Query Hint Recommendation tool (automated Query Store hint trials), and VS Code's XEvents Query Profiler.
- Query cost estimation: BigQuery dry-run bytes ([DataGrip plugin](https://plugins.jetbrains.com/plugin/15884-bigquery-query-size-estimator), dbt Power User). For Fabric DW there is no dry-run API; the equivalent is showing **Query Insights** (`queryinsights.exec_requests_history`) CU/duration for past runs and an estimated-plan row-count/data-size readout **[memory]**.

**AI**
- Ask vs Agent modes, slash commands, read-only classification with explicit approval for writes (SSMS, VS Code, Fabric Copilot); **BYOM** (SSMS 22.1), BYOK (TablePlus), Ollama/local endpoints (QueryDeck, Vanna per [Bytebase roundup](https://www.bytebase.com/blog/top-text-to-sql-query-tools/)); per-database instruction files (`CONSTITUTION.md`, custom instructions); "Explain and Fix" on error, explain a plan, describe an object (DBeaver); MCP server exposure of the client itself (DataGrip, DBeaver dbvr).

**Fabric-specific**
- Browse Fabric workspaces in the connection dialog (SSMS 22, VS Code 1.36+), workspace name in the tree, objects grouped by schema; connect to Warehouse, SQL analytics endpoint (Lakehouse), SQL database in Fabric, mirrored DBs.
- OneLake file/table browsing lives in the separate Fabric Data Engineering extension ([explore lakehouse in VS Code](https://learn.microsoft.com/en-us/fabric/data-engineering/explore-lakehouse-with-vs-code)); Delta tables are readable via the SQL endpoint or directly from OneLake as Delta/Parquet (DuckDB `delta_scan` + Entra token) — nobody has a desktop client that unifies these.
- Cross-warehouse three-part-name queries; Save as view / Save as table (CTAS) actions; Open in Excel; per-run session isolation in the portal (a desktop client's persistent session is an advantage to advertise).

---

## 5. Top 30 ideas that would make a next-gen SQL client better than ADS

Ranked by **value / effort** (V = user value 1-5, E = effort 1-5; score = V/E, ties broken by strategic weight). "Effort" assumes a native app with a Rust/C# core, an Arrow-based result store and TDS driver already in place.

| # | Idea | Exemplar | V | E | Why it wins |
|---|------|----------|---|---|-------------|
| 1 | **Stream rows into the grid as they arrive; never block the UI; live row/elapsed counter; cancel anytime** | TablePlus, DuckDB UI | 5 | 2 | The single most-felt difference between "fast native" and SSMS/VS Code. |
| 2 | **Copy-as menu: with headers (shortcut), Markdown table, JSON, CSV/TSV, SQL INSERT, IN-list, Open in Excel** | SQL Prompt, DataGrip, SSMS 22.4 | 4 | 1 | Cheap, used hourly, long-standing ADS/SSMS asks. |
| 3 | **Execute current statement (no selection) + inline per-statement timing** | SQL Prompt, DataGrip 2026.2 | 4 | 1 | Explicitly requested by SSMS users for years. |
| 4 | **SQL History as a local database: every execution + versioned tab contents, full-text search, star, rename, restore closed/unsaved tabs, crash-safe session restore** | SQL Prompt SQL History | 5 | 2 | Turns the tool into a memory; ADS users cite losing work. |
| 5 | **Saved-query and connection folders, pinned tables, colors/tags per environment, persisted tree state** | Beekeeper 5.7, ADS server groups | 4 | 1 | Directly answers the #26289 "folders of queries" and server-groups gaps. |
| 6 | **Excel-style column filters, local sort, find-in-results, clear-all-filters, freeze columns, hide/reorder columns** | DataGrip, VS Code beta grid | 4 | 2 | Table stakes for analysts; VS Code's version is buggy. |
| 7 | **Column profile panel + header mini-histograms on every result (null %, distinct, min/max, top-k), computed lazily on Arrow batches; click to filter** | DuckDB UI, Navicat 17, Observable | 5 | 3 | Nothing in the SQL Server ecosystem has it; huge "delight." |
| 8 | **Disk-spilling Arrow/DuckDB result store with memory budget; "stop after N / fetch more / fetch all"; no 64k cell cap (lazy full-value fetch)** | DuckDB UI (binary chunks); fixes SSMS limits | 5 | 3 | Removes the two most infamous SSMS failure modes. |
| 9 | **Transposed / record view and Aggregate view of selected cells; totals row** | DataGrip, Hex | 4 | 2 | Wide rows and quick sums are daily tasks. |
| 10 | **JSON / XML / hex / long-text / geo cell viewers with search and pretty-print** | SSMS 22, ADS, Beekeeper, DBeaver | 4 | 2 | Cited by SSMS users as an ADS feature they wanted back. |
| 11 | **Multiple result sets as tabs and side-by-side / vertical split; results in panel or in own tab, user's choice; results zoom** | TablePlus, SSMS 22, VS Code complaints | 4 | 2 | Fixes top vscode-mssql grievances. |
| 12 | **Plan Explorer-class execution plan viewer: statements tree, cost-gradient diagram scalable by rows/size, Top Operations grid, Plan Tree with est-vs-actual highlighting, Query Columns / index analysis, parameters tab, Table I/O, wait stats, open .sqlplan files, anonymize, compare two plans** | SentryOne Plan Explorer | 5 | 5 | The one thing every SQL Server pro would switch for; Plan Explorer is unmaintained. Ship in stages (diagram + top ops first). |
| 13 | **Query this result locally with DuckDB (filter/aggregate/pivot/join result tabs); result-set diff; cross-connection joins (prod vs dev, SQL Server vs Fabric)** | Malloy (embedded DuckDB), DataGrip compare, Hex chained SQL | 5 | 3 | Falls out of #8 almost for free; unique in this market. |
| 14 | **Quick charts from any result (bar/line/pie/scatter/histogram) with copy-as-image** | ADS chart viewer, DataGrip, Fabric Visualize | 4 | 2 | Explicitly missed after ADS. |
| 15 | **Safe mode + staged edits: editable results/tables reverse-mapped to source, shown as a reviewable SQL diff before commit; read-only flag per connection; confirm on prod** | TablePlus, Beekeeper 5.7 | 4 | 3 | Prevents the worst day of a DBA's life. |
| 16 | **Fabric-native connection: sign in with Entra, browse workspaces -> Warehouse / SQL analytics endpoint / SQL DB / mirrored DB, workspace shown in tree, objects grouped by schema, connection import/export** | SSMS 22, VS Code 1.36 | 5 | 3 | Core target audience; both MS tools have it, so it's a must. |
| 17 | **Global object search (Cmd+P) across all connections; Object Explorer filter; group by schema; jump-to-definition and "Describe" popup** | VS Code 1.40, TablePlus, Toad F4 | 4 | 2 | Keyboard-first navigation is what native clients are loved for. |
| 18 | **Streaming export to CSV/Parquet/JSONL/Excel directly from the wire with progress; Save as table (CTAS) / Save as view actions** | Beekeeper, Fabric editor | 4 | 2 | Big-result workflows without the grid. |
| 19 | **Query Magics-style formatting via column aliases (`__format__link/image/money/check/progress`, `__goto__`)** | Beekeeper | 3 | 1 | Pure delight, trivially cheap, works with any SQL. |
| 20 | **AI with BYO model (OpenAI-compatible endpoints, Ollama, Anthropic, Azure OpenAI): explain/fix/optimize, NL->SQL grounded in schema, explain-plan-to-AI, per-database instruction file, read-only classifier + explicit approval for writes, history of AI-generated queries** | SSMS 22 BYOM + CONSTITUTION.md, DBeaver, DataGrip consent levels, TablePlus BYOK | 4 | 3 | Table stakes in 2026; BYO + local model is the OSS differentiator. |
| 21 | **Notebook mode: cells of SQL + Markdown, results embedded, `.ipynb` compatible, chained cells (previous result as a local DuckDB table)** | ADS notebooks, VS Code SQL Notebooks GA, Hex/Deepnote | 4 | 4 | The most-cited ADS loss; chaining is the upgrade. |
| 22 | **Parameterized queries and variables: `@param` prompt dialogs, saved parameter sets, `-- @limit`, `-- @name` magics; snippet library with tab-trigger expansion and placeholders** | Querybook, Sequel Ace, SQL Prompt | 3 | 2 | Makes saved queries reusable. |
| 23 | **Schema-aware autocomplete: aliases, FK-driven JOIN ON suggestions, `*` expansion, insert column list, format on demand with before/after preview and shareable style** | SQL Prompt, DataGrip, SSMS 22.7 | 4 | 4 | Expected by DataGrip/SQL Prompt refugees; big but incremental. |
| 24 | **ER diagram from selected tables/schema (auto layout, FK edges, export SVG/Mermaid) + FK drill-through ("perspectives")** | DBeaver, DbGate, VS Code Schema Designer | 3 | 3 | Named gap in VS Code MSSQL vs ADS-era expectations. |
| 25 | **Session isolation: separate introspection connection so metadata refresh never blocks queries; per-tab sessions; "keep running when tab closed, notify me"** | DataGrip 2026.2, Fabric editor | 3 | 2 | Removes a classic "why is my editor frozen" cause. |
| 26 | **Dashboards / insight widgets: pin any query as a chart or KPI tile on a server/database home page, auto-refresh** | ADS dashboards | 3 | 3 | Unique ADS feature with zero replacements. |
| 27 | **Fabric extras: Query Insights panel (recent runs, duration, CU), OneLake table browser with Delta/Parquet preview via DuckDB `delta_scan`, cross-warehouse query helper, "this is a Fabric endpoint" capability hints (no TCL across batches, etc.)** | SSMS/Fabric portal (partial), none for OneLake | 4 | 4 | Nobody unifies SQL endpoint + OneLake in a desktop client. |
| 28 | **Schema compare + data compare with generated sync scripts** | SSMS 22.7, VS Code, Toad, DataGrip | 3 | 4 | Valued, but MS now ships it; do later. |
| 29 | **Local SQL Server container one-click (Docker) + connection auto-registration** | VS Code 1.35, DBngin | 3 | 2 | Dev-loop convenience; easy win via Docker CLI. |
| 30 | **XEvents-based lightweight profiler with filters and CSV export; Query Hint trial runner (Query Store hints)** | VS Code Query Profiler, SSMS Query Hint tool | 3 | 4 | Reduces tool-hopping; later phase. |

Reading the ranking: items 1-11 are mostly grid/editor/history polish that is cheap and makes the app *feel* like the fast ADS replacement people asked for; 12-16 are the strategic differentiators (plan viewer, local re-query engine, safe editing, Fabric); 17-30 round out parity with DataGrip/SSMS and the ADS features that were orphaned.

---

## 6. Source list

- SSMS: https://techcommunity.microsoft.com/blog/sqlserver/sql-server-management-studio-ssms-21-is-now-generally-available-ga/4415230 ; https://learn.microsoft.com/en-us/ssms/release-notes-22 ; https://learn.microsoft.com/en-us/ssms/github-copilot/chat ; https://learn.microsoft.com/en-us/ssms/github-copilot/agent-mode ; https://learn.microsoft.com/en-us/ssms/query-hint-tool/hint-tool-overview ; https://www.brentozar.com/archive/2024/11/ssms-v21-dark-mode-git-vertical-tabs-and-more/ ; https://techcommunity.microsoft.com/blog/sqlserver/announcing-the-release-of-ssms-22-7-0---and-many-previews/4526908
- VS Code MSSQL: https://github.com/microsoft/vscode-mssql/blob/main/CHANGELOG.md ; https://devblogs.microsoft.com/azure-sql/mssql-vs-code-whats-new-next/ ; https://devblogs.microsoft.com/azure-sql/vscode-mssql-feb-2026/ ; https://devblogs.microsoft.com/azure-sql/vscode-mssql-march-2026/ ; https://devblogs.microsoft.com/azure-sql/vscode-mssql-june-2026/ ; https://learn.microsoft.com/en-us/sql/tools/visual-studio-code-extensions/mssql/mssql-query-profiler?view=sql-server-ver17 ; issues #20404, #17503, #18316, #18642, #22789, #18211, #22099
- ADS retirement / sentiment: https://devblogs.microsoft.com/azure-sql/azure-data-studio-retirement/ ; https://learn.microsoft.com/en-us/sql/tools/whats-happening-azure-data-studio?view=sql-server-ver17 ; https://news.ycombinator.com/item?id=42971640 ; https://github.com/microsoft/azuredatastudio/issues/26289 ; https://www.kevinrchant.com/2025/02/07/thoughts-about-the-azure-data-studio-retirement-announcement/ ; https://www.sqlservercentral.com/editorials/the-end-of-azure-data-studio ; https://infoq.com/news/2025/08/mssql-vscode-copilot-color-codes ; https://www.beekeeperstudio.io/blog/azure-data-studio-alternatives-free ; https://jamsql.com/blog/azure-data-studio-retiring-migration-guide/ ; https://github.com/microsoft/azuredatastudio/issues/3341 ; https://github.com/microsoft/azuredatastudio/issues/19480
- SSMS complaints: https://learn.microsoft.com/en-us/answers/questions/1511788/sql-server-management-studio-is-really-slow ; https://learn.microsoft.com/en-us/answers/questions/5641978/most-recent-usable-version-of-ssms ; https://sqldba.blog/how-to-increase-maximum-characters-displayed-in-ssms/
- DataGrip: https://www.jetbrains.com/help/datagrip/data-editor-and-viewer.html ; https://www.jetbrains.com/help/datagrip/explore-data-in-data-editor.html ; https://blog.jetbrains.com/datagrip/2026/03/26/... ; https://blog.jetbrains.com/datagrip/2026/07/16/... ; https://blog.jetbrains.com/datagrip/2025/12/18/query-consoles-are-coming-back/
- DBeaver: https://dbeaver.com/features/ ; https://dbeaver.com/2026/06/10/dbeaver-26-1/ ; https://dbeaver.com/docs/dbeaver/AI-Smart-Assistance/
- TablePlus/DBngin: https://docs.tableplus.com/gui-tools/code-review-and-safemode/safe-mode ; https://docs.tableplus.com/query-editor/streaming-results-and-async-loading ; https://docs.tableplus.com/query-editor/query-history ; https://tableplus.com/blog/2018/08/show-multiple-results-separately.html ; https://dbngin.com/
- Beekeeper/Outerbase: https://www.beekeeperstudio.io/blog/release-5.7-folders-editable-results ; https://docs.beekeeperstudio.io/user_guide/query-magics/ ; https://docs.beekeeperstudio.io/user_guide/data-export/ ; https://outerbase.com/blog/outerbase-studio-open-source-database-management/
- DbGate: https://www.dbgate.io/features/ ; https://docs.dbgate.io/perspectives/index.html
- Others: https://www.heidisql.com/ ; https://www.navicat.com/en/company/aboutus/blog/2425-data-profiling-in-navicat-17 ; https://www.quest.com/products/toad-for-sql-server ; https://dbschema.com/features.html ; https://sequel-ace.com/favorites.html ; https://github.com/sqlectron/sqlectron
- Plan Explorer: https://documentation.solarwinds.com/en/success_center/sqlsentry/content/planexplorer/results.htm ; https://documentation.solarwinds.com/en/success_center/sqlsentry/content/planexplorer/integrated-planexplorer-overview.htm ; https://www.solarwinds.com/free-tools/plan-explorer
- SQL Prompt: https://documentation.red-gate.com/sp/ssms-tab-management/sql-history ; https://www.red-gate.com/products/sql-prompt/
- Fabric: https://learn.microsoft.com/en-us/fabric/data-warehouse/sql-query-editor ; https://blog.fabric.microsoft.com/en-US/blog/copilot-and-query-editor-in-sql-database-in-fabric-ga-update/ ; https://blog.fabric.microsoft.com/en-us/blog/ssms-22-meets-fabric-data-warehouse-evolving-the-developer-experiences/ ; https://learn.microsoft.com/en-us/fabric/data-engineering/explore-lakehouse-with-vs-code
- DuckDB UI: https://duckdb.org/2025/03/12/duckdb-ui ; https://devclass.com/2025/03/19/duckdb-project-releases-local-web-ui-but-not-as-open-source/
- Notebooks: https://learn.hex.tech/docs/explore-data/cells/sql-cells/sql-cells-introduction ; https://learn.hex.tech/docs/explore-data/cells/visualization-cells/table-display-cells ; https://deepnote.com/docs/sql-cells ; https://observablehq.com/blog/introducing-data-table-cell ; https://github.com/pinterest/querybook
- Semantic/pipeline: https://docs.malloydata.dev/documentation/setup/extension ; https://getbruin.com/docs/bruin/vscode-extension/overview.html ; https://github.com/AltimateAI/vscode-dbt-power-user ; https://plugins.jetbrains.com/plugin/15884-bigquery-query-size-estimator
- TUIs: https://harlequin.sh/ ; https://github.com/jorgerojas26/lazysql ; https://github.com/achristmascarl/rainfrog
- AI/BYO: https://www.bytebase.com/blog/top-text-to-sql-query-tools/
