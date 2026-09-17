# Azure Data Studio (ADS) — Feature Inventory: Plans, Notebooks, Dashboards, Extensions, Table Designer, Export, Charts, Release History

Scope: the "advanced" half of ADS (execution plans, notebooks, dashboards/insights, the extension ecosystem, Table Designer, import/export formats, charting, and release history through retirement). Connection dialog / Object Explorer / query editor / results-grid basics are covered by a sibling report and are only referenced here where they intersect.

Sourcing convention: every section ends with the URLs actually fetched. Items marked **[memory]** come from my own recollection and were not confirmed by a fetched page. Items marked **[unverified]** are things the brief assumed but I could not substantiate.

Retirement status (fetched): ADS was announced for retirement on **February 6, 2025** and **retired February 28, 2026**; it receives no updates or security fixes. The GitHub repo is read-only. Last functional release was **1.52 (June 18, 2025)**; the last feature release with meaningful new capabilities was **1.48 (Feb 2024)**.

---

## 1. Execution plans (Query Plan Viewer)

### 1.1 History
- **2018–early 2022 (legacy viewer)**: an "Explain" button on the query-editor toolbar produced an estimated plan rendered by a web-based viewer with non-SSMS icons, no properties window, and heavy vertical spacing. Hugo Kornelis criticised it for "different icons from SSMS", "generic or missing icons for common operators", excessive screen space, and "absent properties window access". Only the first statement in a batch rendered graphically.
- **1.35 (Feb 24, 2022)**: new **Query Plan Viewer** (preview) — graphic estimated *and* actual plans, SSMS-style icons.
- **1.36 (Apr 2022)**: operator icons + search, parallelism indicators, tooltips, save as `.sqlplan`.
- **1.37 (Jun 2022)**: **Plan comparison** with visual indicators; toolbar button to toggle actual-plan capture; extra cost precision on large plans.
- **1.38 (Jul 2022)**: Top Operations pane links jump to operators; `Ctrl+M` rebound to toggle actual plan (no longer executes); plan labels update on orientation toggle; command-palette commands prefixed "Execution Plan"; collapse/expand at operator level.
- **1.39 (Aug 2022)**: copy text from Properties-pane cells; "find node" inside plan comparison.
- **1.40 (Nov 16, 2022)**: **GA**. "Highlight most expensive operator" by user-selected metric; full-text display and copying in Properties; filter in Properties pane; collapse/expand sub-categories in plan comparison.
- **1.41 (Jan 2023)**: default save folder changed to user home.
- After that, only bug fixes; this is the plan viewer as it existed at retirement.

### 1.2 How plans were produced
- **Estimated plan**: select the query text and press the toolbar **Estimated Plan** button (`EstimatedQueryPlanAction`, label "Estimated Plan"; historically the button was labelled **Explain** and older docs/tutorials still say "Click Explain"). With nothing selected it shows estimated plans for *every* statement in the window.
- **Actual plan**: **Enable Actual Plan** toggle button (`ToggleActualExecutionPlanModeAction`, labels "Enable Actual Plan"/"Disable Actual Plan") or **Ctrl+M**; thereafter every Run/F5 adds a **Query Plan** tab beside Results and Messages. There was also an `ActualQueryPlanAction` labelled "Actual" ("Run Current Query with Actual Plan" in the command palette).
- Erik Darling notes early on the feature required the `workbench.enablePreviewFeatures` setting.
- Opening a saved **`.sqlplan`** file in ADS rendered it in the same viewer; the viewer read standard **showplan XML**.

### 1.3 Output tabs
When a plan is present the results area shows **Results | Messages | Query Plan | Plan Tree | Top Operations**:
- **Query Plan** — the graphical plan(s), one per statement, with a right-side vertical toolbar and a right-click context menu.
- **Plan Tree** — the plan in text/tabular form with sortable columns.
- **Top Operations** — every operator as a grid row, sortable by attribute (Est Rows, Est Cost, Subtree Cost, Actual Rows, Actual Elapsed Time, etc.), plus a **Filter** box on the right to find operators by common field value (e.g. all Nested Loops touching one object). Clicking an operator name jumps to it in the graph and draws a green box around it.

### 1.4 Toolbar / context-menu options (verbatim from docs)
| Option | Behaviour |
|---|---|
| Save Plan File | save as `.sqlplan` |
| Show Query Plan XML | open the showplan XML in a new editor |
| Open Query | open the plan's query text in a new editor |
| Zoom In / Zoom Out / Zoom to fit / Custom Zoom | custom zoom takes a numeric level |
| Find Node | search nodes by attribute values |
| Properties | toggle the Properties pane |
| Compare execution plan | open the comparison window |
| Highlight expensive operators | pick a metric (Actual Elapsed Time, Actual Elapsed CPU Time, Cost, Subtree Cost, Actual Number of Rows for All Executions, Number of Rows Read) and the costliest operator is highlighted |
| Tooltips | toggle hover tooltips on operators |
| Top Operations | switch to the Top Operations pane |
| Toggle Orientation | (comparison window) horizontal vs vertical layout |

### 1.5 Operator visuals and the Properties pane
- Operators use the same icon set as SSMS; each node shows the operator name, object (truncated, e.g. "Clustered Index S…"), and **cost percentage**; edges are drawn as thick "accusatory, Snag-It style arrows" whose thickness scales with row count; hovering an arrow shows row properties like SSMS. Parallelism icons were added in 1.36 (their direction fixed in 1.36.2).
- Operator-level **collapse/expand** (1.38).
- **Properties pane** shows the full operator property set (Grant Fritchey: "parameter values, query hash, ANSI settings, wait stats"); properties sortable **by importance or alphabetically**; **filter** box; cell text selectable/copyable, long values shown in full (1.40).
- Warnings: standard showplan warnings surfaced as operator icon badges and in Properties **[memory]**; Erik Darling noted in 2022 that missing-index requests for Eager Index Spools were not surfaced.

### 1.6 Plan comparison
Right-click → **Compare execution plan**. The current plan opens in the top half of a new window with an **Add execution plan** button in the bottom half; browse to a saved `.sqlplan`. The comparison toolbar has the same zoom/find/properties icons plus **Toggle Orientation**. Selecting an operator in each plan and opening **Properties** shows attributes side by side: attributes with *different* values listed at the top, identical ones collapsed under **Equivalent Properties**; sort icons, expand/collapse, and a filter menu. Similar regions between plans are highlighted **[memory]**.

### 1.7 Comparison with SSMS and SentryOne Plan Explorer (2022 reviews)
- **Present and praised**: higher-DPI assets than SSMS; SSMS-consistent icons; full properties; XML access; Top Operations grid; plan comparison (after 1.37); "find node"; expensive-operator highlighting by chosen metric; multi-statement batches rendered.
- **Missing vs SSMS**: no **Live Query Statistics**; no way to hide the results grid while viewing a plan; no batch-mode vs row-mode visual distinction; truncated operator labels; no I/O rollup on the root node; no execution-count badge; no "Analyze actual execution plan" / plan-scenario analysis; no Query Store UI to open plans from.
- **vs SentryOne Plan Explorer**: Plan Explorer's own free ADS extension existed (community list: "SentryOne Plan Explorer – rich graphical execution plans") giving Plan Explorer-style zoom/highlighting/properties inside ADS; ADS native viewer lacked Plan Explorer's costed-by-I/O/CPU views, statement-level grid, index analysis, join diagram, and wait tracking **[memory]**. Other community options: **Paste The Plan** (send XML plan to Brent Ozar's service) and **queryplan.show** (alternative chart layouts, 2019).

Sources: https://learn.microsoft.com/en-us/previous-versions/azure-data-studio/query-plan-viewer · https://learn.microsoft.com/en-us/previous-versions/azure-data-studio/release-notes-azure-data-studio · https://raw.githubusercontent.com/microsoft/azuredatastudio/main/src/sql/workbench/contrib/query/browser/queryActions.ts · https://www.scarydba.com/2022/03/07/query-plans-in-azure-data-studio/ · https://erikdarling.com/trying-out-azure-data-studio-query-plans/ · https://sqlserverfast.com/blog/hugo/2022/03/execution-plans-in-azure-data-studio/ · https://www.sqlshack.com/view-execution-plans-in-azure-data-studio/ · https://learn.microsoft.com/en-us/previous-versions/azure-data-studio/tutorial-qds-sql-server

---

## 2. Notebooks

### 2.1 Foundations
- Notebooks were introduced in the **October 2018** SQL Server 2019 preview extension, moved into core in **March 2019** ("first public preview of SQL Notebooks"), and stayed labelled *Preview* in the SSMS comparison table to the end.
- Files are standard **`.ipynb`**; the Jupyter server is started in-process by ADS with a bundled or user-specified Python. Notebooks created in ADS open as **Trusted**; notebooks from elsewhere open **Not Trusted** and can be trusted via the toolbar toggle (`TrustedAction`, labels "Trusted"/"Not Trusted").
- Creation entry points: **File > New Notebook**; right-click a server/database → **New Notebook**; command palette **New Notebook**; **Notebooks** viewlet in the activity bar; default file name `Notebook-1.ipynb`.

### 2.2 Kernels (the **Kernel** dropdown) and **Attach to**
| Kernel | Notes |
|---|---|
| **SQL** | T-SQL cells against any MSSQL connection (also PostgreSQL via the PostgreSQL extension; MySQL via MySQL extension). Full query-editor experience in the cell: IntelliSense, snippets, results grid with Save As CSV/Excel/JSON/XML and **Show Chart**. Results **streaming** for SQL notebooks arrived in 1.24. |
| **Python 3** | Local Python. First use shows **Configure Python for Notebooks** (New Python installation vs Use existing Python installation); a **Manage Packages** dialog and **Python Dependencies Wizard** (1.18–1.19) install/upgrade packages; **Reinstall Notebook Dependencies** command. Python bundled at 3.8.10 (1.30), Jupyter/notebook pinned 6.5.6 (1.47). |
| **PowerShell** | Added 1.13 (Nov 2019) via the PowerShell extension; results streaming added 1.23. |
| **Kusto** | Added 1.22 (Sept 2020) with the Kusto (KQL) extension; attach to an ADX cluster+database; results with Save As + Show Chart. |
| **PySpark3 / PySpark / Spark (Scala, R)** | For SQL Server 2019 Big Data Clusters; Attach to = cluster endpoint. |
| **.NET Interactive** | Added via the .NET Interactive Notebooks extension in 1.36 (multi-language C#/F#/PowerShell cells). |
| **Kqlmagic** | Not a kernel; `%kql` / `%%kql` magics inside the Python kernel to query Azure Data Explorer, Application Insights and Log Analytics with Plotly `render` charts. |
| **Azure Monitor Logs** | Log Analytics workspace queries via the Azure Monitor Logs extension (1.32). |

**Attach to** selects the connection context; the dropdown had **Change Connection** and **Add New Connection**.

### 2.3 Notebook toolbar and cell features
- Toolbar (from source `notebookActions.ts`): **+ Cell** (Code cell / Text cell), **Run all**, **Clear Results**, **Trusted/Not Trusted**, **Collapse Cells / Expand Cells**, **Run with Parameters**, **Kernel** dropdown, **Attach to** dropdown, **Notebook Views** (Editor / Create New View — 1.33 "Notebook Views" let you lay out cells as a dashboard), Find (Ctrl+F, 1.15).
- Cell actions: run cell (F5), move cell up/down, convert between Code and Text (1.21), split cell (1.33), collapse code cells (1.13), undo/redo (1.34), keyboard navigation between cells (1.35), add cell before/after via "More Actions" (Nov 2018).
- **Text (Markdown) cells**: **WYSIWYG** editing (1.22) with a **Markdown toolbar** (1.17: bold/italic/underline/code/link/list/image/**headers**), **side-by-side Markdown preview** (1.20), option to render HTML `<table>` as a table or not (1.41).
- **Charts in SQL notebooks**: 1.16 added charting for query results in notebook cells; **chart persistence** in the `.ipynb` (1.17) so the chart image is saved as an output (though Steve Jones observed the chart state was not always retained on reopen).
- **Results caching**: query results and rendered charts are stored as standard cell outputs in the `.ipynb`, so a saved notebook re-opens with its last results (this is the "runbook with evidence" scenario Microsoft pushed: run diagnostics, save with results, share).
- **Performance**: large-notebook performance work in 1.32/1.33; Jupyter server startup improved ~50% on Windows (1.26).

### 2.4 Parameterization (Python, PySpark, PowerShell, .NET Interactive kernels)
1. **Run with Parameters** (1.29): tag a code cell as a **Parameters** cell (one parameter per line); the toolbar icon prompts for each value in a series of input boxes, then opens a new notebook with an `# Injected-Parameters` cell.
2. **Papermill**: `papermill Input.ipynb Output.ipynb -p x 10 -p y 20` or `pm.execute_notebook(...)` from Python.
3. **URI parameterization** (1.24/1.26): `azuredatastudio://microsoft.notebook/open?url=<https|http|file URL>?x=10&y=20` opens a downloaded copy with injected parameters (URI handler also supports plain opening; file support added 1.30).

### 2.5 Jupyter Books
- A book is a folder with `_toc.yml` / `_config.yml` organising notebooks and markdown into chapters/sections, shown as a tree in the **Notebooks** viewlet.
- Commands/dialogs: **Jupyter Books: Create Book (Preview)** (1.16; a notebook-driven experience that installs Python deps and prompts for the notebook/markdown folder), **Create Book dialog** (1.27), **Add Notebook / Remove Notebook** (1.28), add section and drag-and-drop reordering (1.33), edit books via right-click (1.26), **pinned notebooks** (1.22), **search across notebooks and books** in the viewlet (1.21), **Jupyter Books picker** to open remote books from GitHub releases (1.21; a **Remote Jupyter Book Publish** GitHub Action packages `.zip`/`.tar.gz` releases).
- Microsoft shipped content books, e.g. the SQL Server 2019 Big Data Cluster troubleshooting book (1.13 "troubleshooting Jupyter Book") and "SQL Server 2019 Guide" **[memory]**. Extension authors could ship a **Jupyter Book extension** or **Notebook extension** (documented generator templates).

### 2.6 Export / conversion
- Query editor → **To Notebook** (`ExportAsNotebookAction`, 1.24 "SQL to notebook support"): converts the open `.sql` into a SQL-kernel notebook (each batch → code cell).
- Notebook → **Export notebook as SQL file** (toolbar; fixed in 1.31).
- Native **HTML/PDF export was not built in** — GitHub issue #13910 (Dec 2020) requested HTML export with ADS styling; the documented workaround was `jupyter nbconvert`. (The VS Code MSSQL successor later added "SQL notebook export options" in 1.43, June 2026.) Third-party summaries claiming a "Share → PDF/HTML" button in ADS appear to be inaccurate **[unverified]**.
- Notebook results per cell: Save As CSV / Excel / JSON / XML, Show Chart, Visualizer (SandDance).

### 2.7 Notebook Jobs (SQL Agent extension)
Schedule a notebook as an Agent job from the Agent extension's **Notebooks** tab: **New Notebook Job** dialog (notebook file, **storage database**, **execution database**, schedule). ADS creates `nb_template` and `nb_materialized` tables in the storage DB; each run stores a fully materialised notebook with results; the Notebooks tab shows last/next run, a 5-run status bar graph, and **Past Runs** — double-click opens the executed notebook. Deleting the job drops the tables.

Sources: https://learn.microsoft.com/en-us/previous-versions/azure-data-studio/notebooks/notebooks-guidance · .../notebooks/notebooks-sql-kernel · .../notebooks/notebooks-python-kernel · .../notebooks/notebooks-kusto-kernel · .../notebooks/notebooks-kqlmagic · .../notebooks/run-with-parameters · .../notebooks/parameterize-papermill · .../notebooks/parameterize-uri · https://www.microsoft.com/en-us/sql-server/blog/2020/11/16/building-and-sharing-jupyter-books-in-azure-data-studio/ · https://raw.githubusercontent.com/microsoft/azuredatastudio/main/src/sql/workbench/contrib/notebook/browser/notebookActions.ts · https://github.com/microsoft/azuredatastudio/issues/13910 · https://www.sqlservercentral.com/articles/notebook-jobs-in-azure-data-studio · release notes (above)

---

## 3. Dashboards, the Manage page, Insights, Properties

### 3.1 The Manage page
Right-click a server or database → **Manage** opens the **server dashboard** or **database dashboard** as an editor tab. Layout:
- **Header/properties strip** — server: version, edition, computer name, OS version; database: recovery model, last database backup, last log backup, compatibility level, owner (configurable via `dashboard.server.properties` / `dashboard.database.properties`).
- **Left navigation** (added when extensions contribute tabs): **Home**, then sections such as **General** (e.g. SQL Assessment, Machine Learning), **Administration** (SQL Agent), **Monitoring** (Server Reports, Tempdb), plus extension tabs (Azure SQL Migration, Managed Instance, SQL Server 2019/BDC, Azure Arc…).
- **Home widgets** (grid of `gridItemConfig` cells):
  - **Tasks widget** (`tasks-widget`) — buttons for common actions: New Query, New Notebook, Backup, Restore, Configure/Properties, Import Wizard, Schema Compare, Data-tier Application Wizard, Launch Profiler, etc. (extensions contribute tasks).
  - **Search / Explorer widget** (`explorer-widget`) — searchable list of databases (server) or objects (database) with inline actions (Edit Data, Script as…).
  - **Backup status** (`backup-history-server-insight`, a count widget colour-coded red/orange/green by backup age) and **Database Size (MB)** (`all-database-size-server-insight`, horizontal bar) on the server home (hidden on cloud/Express).
- Dashboard colours picked up hex values in 1.39.

### 3.2 Built-in insight widgets (from `extensions/insights-default/package.json`)
| Widget id | Name | Chart |
|---|---|---|
| `query-data-store-db-insight` | Top 5 Slowest Queries (requires Query Store) | timeSeries, legend top |
| `table-space-db-insight` | Space used per table | horizontalBar, vertical data, columns as labels |
| `all-database-size-server-insight` | Database Size (MB) | horizontalBar |
| `backup-history-server-insight` | Backup Status | count |
Also contributed by extensions: **Server Reports** (DB Space Usage, DB Buffer Usage, CPU Utilization, Backup Growth Trend, Wait Counts), **Managed Instance** dashboard, **Azure SQL Data Warehouse Insights**, **WhoIsActive** (sp_whoisactive as graphs and tasks), community MSSQL Db/Instance Insights, AlwaysOn Insights, Idera SQL DM.

### 3.3 Custom insight widgets (JSON + T-SQL)
- Run any query → **Chart** → configure → **Create Insight** copies a JSON block to the clipboard; paste into `dashboard.database.widgets` or `dashboard.server.widgets` in `settings.json`.
- Schema: `name`, `gridItemConfig { sizex, sizey }`, `widget.insights-widget { type: { <chartType>: { dataDirection, dataType, legendPosition, labelFirstColumn, columnsAsLabels, … } }, queryFile | query, autoRefreshInterval, details: { queryFile, label, value } }`.
- Widget **… menu**: **Show Details** (runs the details query and shows a grid in an Insights flyout), **Run Query** (opens the query in an editor), **Refresh**.
- Insight charts support legend toggling of series and hover values.
- Extension contribution points: `dashboard.tabs` (id, title, description, `when`, `alwaysShow`, `container`), `dashboard.insights`, `dashboard.containers` (widgets-container, grid-container, webview-container, controlhost-container, nav-section), **homepage actions** (toolbar buttons on the Manage page).
- Known limitation (SQLServerCentral): editing `dashboard.database.widgets` replaces the defaults, and custom widgets apply to all databases on the instance.

### 3.4 Properties dialogs (late additions, all Preview)
- **1.46 (Sept 2023)**: **Server Properties (Preview)** and **Database Properties (Preview)** from the right-click menu; not every SSMS field present by design; **Script** button writes ALTER statements to an editor; **Help** link. **Attach/Detach Database (Preview)**.
- **1.47**: usability improvements to object properties dialogs. Also **User Management**: Create/edit Login, User, Database Role, Server Role (Preview, 1.44) with Securables dialog.
- **1.48 (Feb 2024)**: Restore dialog gained **S3-compatible storage** and **Restore from URL**; **Restore Database** on the database context menu; **Ledger** checkbox in Create Database.
- Windows-only **Database Administration Tool Extensions for Windows** launched genuine SSMS property dialogs and the **Generate Scripts** wizard.

Sources: https://learn.microsoft.com/en-us/previous-versions/azure-data-studio/insight-widgets · .../tutorial-build-custom-insight-sql-server · .../tutorial-qds-sql-server · .../tutorial-table-space-sql-server · .../extensions/dashboard-extension · https://raw.githubusercontent.com/microsoft/azuredatastudio/main/extensions/insights-default/package.json · https://www.sqlservercentral.com/articles/server-dashboards-in-azure-data-studio · https://www.sqlservercentral.com/articles/database-dashboards-in-azure-data-studio · release notes

---

## 4. Built-in and Microsoft extensions — catalogue

Extension mechanics: Extensions view (`Ctrl+Shift+X`), Microsoft-curated gallery (`extensionsGallery.json`, no full marketplace), **Install from VSIX**, auto-update settings, filters `@builtin/@installed/@recommended/@category`, extensions stored in `~/.azuredatastudio/extensions`. Bundled-in-repo extensions (from `/extensions`): admin-pack, admin-tool-ext-win, agent, arc, asde-deployment, azcli, azurecore, azuremonitor, cms, dacpac, data-workspace, datavirtualization, import, insights-default, kusto, machine-learning, mssql, notebook, profiler, query-history, resource-deployment, schema-compare, server-report, sql-assessment, sql-bindings, sql-database-projects, sql-migration, plus VS Code language/theme extensions (git, github, markdown, json, xml, yaml, powershell, python, r, ipynb, notebook-renderers, tunnel-forwarding, etc.).

**Admin Pack for SQL Server** — a meta-extension bundling SQL Server Agent, SQL Server Profiler, SQL Server Import and SQL Server dacpac in one install.

**SQL Server Agent (Preview forever)** — a **SQL Agent** tab on the server Manage page with five sub-tabs: **Jobs** (grid: Name, Last Run, Next Run, Enabled, Status, Category, Runnable, Scheduled, Last Run Outcome, Previous Runs bar-chart of recent run durations, red = failed), **Notebooks** (notebook jobs — see 2.7), **Alerts**, **Operators**, **Proxies** (proxy list only; edit/create unreliable). Job detail view: **Run, Stop, Refresh, Edit, Delete**, job history with step outcomes. **New Job** dialog tabs: **General** (name, owner, category, description), **Steps** (add/reorder; step dialog with type, database, command, **Parse**, advanced retry/output options), **Schedules** (pick existing schedules only), **Alerts**, **Notifications**. **New Alert** dialog: General, Response, Options. Microsoft's own retirement guidance sent Agent users back to SSMS.

**SQL Server Profiler (Preview)** — XEvent-based trace viewer "similar to SSMS Profiler". Launch via **Alt+P**, right-click server → **Launch Profiler**, or the **Profiler** button in query editor. **New Session** dialog: template dropdown (**Standard_OnPrem**, **Standard_Azure** — only one available for Azure SQL DB —, **TSQL_OnPrem**), session name. Toolbar: **Select Session**, **New Session**, **Start/Stop** (Alt+S toggle), **Pause Capture**, **Clear**, **Auto Scroll**, **View** dropdown (**Standard View, TSQL View, Tuning View, TSQL_Locks View, TSQL_Duration View**), **Filter** (per-column filters). Event grid + detail pane with the statement text; open the highlighted statement in an editor and press Explain. **Profiler: Open XEL File** command opens saved `.xel` traces (up to 1 GB with a progress dialog, 1.45–1.46).

**SQL Server Import** — "Import Flat File" wizard (right-click DB → **Import Wizard**; Tasks widget). Pages: (1) Specify input file (server/database, **Browse** file `.txt/.csv`, table name auto-filled, schema default `dbo`), (2) **Preview data** (first 50 rows) with **Create derived column** (PROSE-driven by-example transformation with **Preview Transformation**), (3) **Modify columns** (column name, data type, primary key, allow nulls) — types inferred by Microsoft Research **PROSE**, (4) Summary with **Import Data**, then **Done / Previous / Import new file**. GA in 1.22.

**SQL Server dacpac** — the **Data-tier Application Wizard** (right-click Databases folder or database): **Deploy** dacpac → database (new or upgrade existing), **Extract** database → dacpac, **Import** bacpac → new database, **Export** database → bacpac (schema + data). GA in 1.13.

**Schema Compare** — right-click DB → **Schema Compare**. Source/Target pickers (each a **database, .dacpac, or SQL project**), **Compare**, **Switch direction**, **Options** dialog (DacFx deployment options + object-type include/exclude), results grid (type, source name, target name, action Add/Change/Delete, include checkboxes) with side-by-side script diff, **Generate script**, **Apply**, **Save** as `.scmp` and open `.scmp` from the file context menu (1.40); SQLCMD-mode support; "Update Project from Database" flow with the Projects extension. GA 1.13.

**SQL Database Projects** — project-based development (preview 1.22, GA 1.43). **Database Projects** view: create blank project, **create from connected database** (import), open SSDT `.sqlproj`; **SDK-style Microsoft.Build.Sql** format (1.36); target platforms incl. SQL Server 2022, Azure SQL DB/MI, Synapse Serverless/Dedicated, **Fabric Data Warehouse**, **Fabric mirrored SQL database** (1.49); add/rename/move files; **Build** (.NET 6+), **Publish** (to database, **to a new container**, from **publish profiles** — create from Add Item menu, save publish settings), **SQLCMD variables** editor, database references (system DBs, dacpac, project), Schema Compare integration, **Table Designer inside projects** (1.37), Code Analysis. **SQL Bindings** extension generated Azure Functions SQL bindings from a project **[memory]**.

**Query History** — a **Query History** panel (View: Toggle Panel) listing each executed query with a ✔/❌ status, query text, server/database, timestamp; right-click **Open Query / Run Query / Delete / Clear All History**; double-click to open or run (1.39); **pause/resume capture**; settings `queryHistory.maxEntries`, persistence to an encrypted on-disk store (1.40; "Query History: Open storage folder"). Preview Oct 2019, GA 1.40.

**Central Management Servers (Preview)** — CMS node in the Connections viewlet (Ctrl/Cmd+G): register a CMS, create server groups, register servers, run against the group. No multi-server query grid.

**Server Reports** — dashboard tab with "Monitor" and "Performance" categories: **DB Space Usage** (top 10), **DB Buffer Usage** (top 10), **CPU Utilization**, **Backup Growth Trend**, **Wait Counts** (Paul Randal's query), plus a **Tempdb** page.

**SQL Server Assessment (Preview)** — Manage → General → **SQL Assessment**: **Invoke Assessment** (server or database, incl. tempdb), **View applicable rules**, **Export as script** (INSERT INTO table format), **Create HTML Report**, **History** tab, **Info**; result grid with severity/message/rule filters; MI supported. Introduced 1.19.

**Managed Instance Dashboard** — **Managed Instance** tab: Properties (vCores, memory, storage, tier, hardware gen, log throughput), Local SSD and Premium disk storage usage, 2-hour CPU/storage graphs; **Recommendations** (storage limit, throughput limit, memory pressure, log/data file limits, VLF count) with remediation scripts; **Replicas** sync state; **Logs** (filtered error log with logical names).

**Azure SQL Migration** — Manage → **Azure SQL Migration** landing page + wizard: assessment (readiness for SQL MI / SQL VM / Azure SQL DB, incl. precomputed Arc assessments), **SKU recommendations** from collected perf data (elastic model, premium-series MI SKUs), online/offline migration orchestrated by Azure DMS with self-hosted integration runtime, TDE and login migration, save/resume wizard, **New support request** button. GA 1.36; retired alongside ADS with Arc/DMS portal alternatives.

**Database Migration Assessment for Oracle / Database Schema Conversion Toolkit / Extension for Oracle / Azure PostgreSQL Migration / Azure Cosmos DB Migration for MongoDB** — migration-assessment and conversion tooling (Oracle→PostgreSQL/Azure SQL; Postgres; Mongo→Cosmos), 2022–2023.

**PostgreSQL (Preview)** — connection manager, Object Explorer, query editor with IntelliSense, dashboards/insights, snippets, notebooks (SQL kernel), Entra ID auth (1.16), Azure Postgres Flexible Server / Cosmos DB for PostgreSQL in the Azure tree (1.45). **MySQL** — preview 1.40, GA 1.45: MySQL native and Entra auth, object explorer, IntelliSense, results export CSV/JSON/XML/Excel, dashboards, notebooks. **Azure Cosmos DB / MongoDB Atlas** — Mongo API browsing and querying (preview 2022–2023).

**Kusto (KQL) (Preview)** — connection type **Kusto** (cluster URL, Entra auth), KQL query editor with IntelliSense, results grid → charts/SandDance, **Kusto notebook kernel**. **Azure Monitor Logs (Preview)** — connection type **Azure Monitor Logs** (Workspace ID), KQL editor and notebooks against Log Analytics.

**Machine Learning (Preview)** — Manage → General → **Machine Learning**: **Manage packages** (Python and R packages in the database), **Import models** (ONNX into a model table), **Make predictions** wizard (PREDICT T-SQL generation), create notebooks; settings for Python/R paths; supports SQL MI.

**Azure Arc** — **Azure Arc Controllers** view: deploy a data controller (direct/indirect), deploy **Arc-enabled SQL Managed Instance** and **PostgreSQL** via wizards, per-resource dashboards. **ASDE-Deployment / Resource Deployment ("New Deployment")** — notebook-based wizards (File > New Deployment) for SQL Server on Windows, SQL Server in a Docker container (2017/2019/2022), Big Data Cluster, **Azure SQL Database**, **SQL Server on Azure VM**, **Azure SQL Edge**. **Data Virtualization** — PolyBase **Virtualize Data / Create External Table** wizard (SQL Server, Oracle, MongoDB, Teradata, CSV on HDFS). **Big Data Clusters (SQL Server 2019 extension)** — HDFS browsing, Spark job submission, cluster dashboards (GA 1.14).

**SandDance for Azure Data Studio** — the **Visualizer** button on any results grid (or *View in SandDance* on `.csv/.tsv` files) opens Microsoft Research's SandDance unit-visualisation explorer (grid/column/scatter/stacks/treemap/density layouts with animated transitions, colour/size/facet encodings) — recommended ≤100k rows.

**GitHub Copilot** (gallery from 1.44, May 2023) — inline completions and comment-driven suggestions in any editor, `Tab` accept / `Esc` reject, `Ctrl+Enter` completions panel, change GitHub account (1.48). **No Copilot Chat** in ADS. **Visual Studio IntelliCode** (Nov 2019) — AI-ranked completions. **Redgate SQL Prompt** formatting styles (1.18), **Redgate SQL Search** (community list).

**Query Store** — **no Microsoft Query Store extension or UI ever shipped in ADS**; the CHANGELOG has zero "Query Store" mentions and the SSMS comparison table leaves Query Store blank for ADS. The only Query-Store-related surfaces were the built-in "Top 5 Slowest Queries" insight (requires Query Store on) and community extensions (Qpi/Query Performance Insights). **[unverified: the brief's "Query Store (added late)"]**

**Database Diagrams** — never shipped (issue #94 stayed open); community "Schema Visualization" extension filled the gap. The successor is VS Code MSSQL **Schema Designer** (2025).

**Table Designer, Object Explorer filtering, Query plan viewer** — core features, not extensions (see §5, sibling report, §1). **Language Packs** (9), **PowerShell**, **Azure CLI**, **.NET 6 Runtime**, **Azure SQL Data Warehouse Insights**, **WhoIsActive** round out the first-party list.

Sources: https://github.com/microsoft/azuredatastudio/wiki/List-of-Extensions · https://github.com/microsoft/azuredatastudio/tree/main/extensions · Learn pages under https://learn.microsoft.com/en-us/previous-versions/azure-data-studio/extensions/ (sql-server-agent-extension, sql-server-profiler-extension, sql-server-import-extension, sql-server-dacpac-extension, schema-compare-extension, kusto-extension, azure-monitor-logs-extension, machine-learning-extension, azure-sql-migration-extension, postgres-extension, mysql-extension, github-copilot-extension-overview, sanddance-extension, azure-sql-managed-instance-extension, add-extensions) · READMEs at https://raw.githubusercontent.com/microsoft/azuredatastudio/main/extensions/{query-history,server-report,sql-assessment,cms,admin-tool-ext-win,admin-pack,agent,notebook,datavirtualization,arc}/README.md · https://www.sqlservercentral.com/articles/managing-sql-agent-in-azure-data-studio · https://www.sqlshack.com/sql-server-profiler-in-azure-data-studio/ · https://raw.githubusercontent.com/microsoft/azuredatastudio/main/CHANGELOG.md · https://learn.microsoft.com/en-us/sql/tools/visual-studio-code-extensions/sql-database-projects/sql-database-projects-extension

---

## 5. Table Designer (Feb 2022 preview → Nov 2022 GA)

- **Entry points**: right-click **Tables** → **New Table**; right-click a table → **Design** (double-click opens it from 1.43); available in SQL Database Projects (1.37).
- **Three panes**: (1) main pane with tabs **Columns | Primary Key | Foreign Keys | Check Constraints | Indexes | General**; (2) **Properties** pane for the selected object (table, column, key, index); (3) read-only **Script** pane showing the T-SQL generated in real time plus success/error messages. Panes resizable.
- **Table properties**: Name, Schema, Description; **System Versioning Enabled** (history table name; period columns ValidFrom/ValidTo added by default from 1.40), **Memory Optimized** (durability), **Graph table** (node/edge) (1.36).
- **Columns grid**: Name, Data Type, Length/Precision/Scale, Allow Nulls, Primary Key checkbox, Default Value, **Identity** (seed/increment), **Computed** column expression/persisted (1.37), Description; **Move** handle for drag reordering and choose insertion position (1.37); row-actions column (1.38).
- **Primary Key** tab: name, clustered, columns with **Move Up / Move Down** ordering (1.40).
- **Foreign Keys** tab: **+ New Foreign Key**, name, **Foreign table**, column mappings (**+ New Column Mapping**: column → foreign column), on-delete/on-update actions, enabled/not-for-replication.
- **Check Constraints** tab: **+ New Check Constraint**, Name, Expression, enabled.
- **Indexes** tab: name, clustered/unique, key columns with sort order, **included columns** (1.39), **filter predicate** (1.39), **columnstore** and **hash indexes for memory-optimized tables** (1.40).
- **Publish** (toolbar icon or Ctrl+S): preview dialog listing all changes and warnings (data loss / downtime), then **Publish** directly (DacFx-based) or **Generate Script** into a query editor (run with SQLCMD mode). Checkbox **Preview Database Updates** (1.40). Setting to keep DDL triggers enabled during publish (1.45). Unsaved state shown by a dot on the tab.
- Successor: VS Code MSSQL Table Designer ("Modify Table Structure"), GA May 2025 with the same tabs plus Script As Create panel.

Sources: https://learn.microsoft.com/en-us/previous-versions/azure-data-studio/overview-of-the-table-designer-in-azure-data-studio · .../create-temporal-tables-in-azure-data-studio · release notes 1.35–1.45

---

## 6. Data export / import formats

**Results grid (query editor & notebooks)** — right-side toolbar/context menu (from `actions.ts`): **Save As CSV**, **Save As JSON**, **Save As Markdown**, **Save As Excel** (`.xlsx`; extra Excel options in 1.47; open-file-location prompt 1.46), **Save As XML**; **Copy / Copy With Headers / Copy Headers**, **Select All**, **Maximize/Restore**, **Chart**, **Visualizer** (SandDance). Settings for CSV delimiter/quoting/encoding, `saveAsCsv.includeHeaders`, JSON string handling toggle (1.42), line-break handling on copy (1.45), formatted XML display (1.47), full text >65,535 chars (1.40). Results to Text/Results to File (SSMS) never existed.
**Charts** — **Copy as image**, **Save as image** (PNG) **[memory: PNG]**.
**Notebooks** — `.ipynb` (with cached outputs); per-cell CSV/Excel/JSON/XML; SQL → notebook (**To Notebook**) and notebook → `.sql`; no native HTML/PDF (nbconvert workaround).
**Import** — Import Flat File wizard (`.csv`, `.txt`, PROSE type inference, derived columns); **bacpac import**; **dacpac deploy**; Schema Compare apply; SQL Project publish; Restore from disk/URL/S3 (1.48); Backup dialog. Copy Data Wizard / BCP GUI never existed (integrated terminal + bcp/sqlcmd instead).
**Export** — bacpac export (schema+data), dacpac extract, Generate Scripts (Windows-only SSMS dialog), object **Script as Create/Alter/Drop/Select/Execute**, Schema Compare **Generate script**, Query History export **[memory: none]**, `.sqlplan`, showplan XML, `.scmp`, `.xel` (open only), SQL Assessment HTML report / INSERT script.

Sources: https://raw.githubusercontent.com/microsoft/azuredatastudio/main/src/sql/workbench/contrib/query/browser/actions.ts · https://learn.microsoft.com/en-us/previous-versions/azure-data-studio/what-is-azure-data-studio (feature table) · extension pages above · release notes

---

## 7. Charting and visualisation

**Chart viewer** (results grid → **Chart** icon → **Chart Viewer** tab; also in notebook cells → Show Chart).
- **Chart Type** combo (from source `interfaces.ts`/`chartOptions.ts` and docs): **bar**, **horizontalBar**, **line**, **pie**, **doughnut**, **scatter**, **timeSeries**, **image**, **count**, **table** (the last three are "insight types" rather than graphs; `table` renders the grid in a widget, `count` renders big numbers with colour thresholds, `image` renders a column containing an encoded image).
- **Options per type** (Chart.js-based; default type `bar`):
  - bar / horizontalBar / pie / doughnut / line: **Data Direction** (Horizontal | Vertical), **Use column names as labels** (default on), **Use first column as row label**, **Legend Position** (Top | Bottom | Left | Right | None); line also **Data Type** (Number | Point); bar/horizontalBar/line add **X/Y Axis Label**, **Y Axis Min/Max** (bar) or **X Axis Min/Max** (horizontalBar).
  - scatter: Legend Position, X/Y Axis Label.
  - timeSeries: Legend Position, Y Axis Label/Min/Max, X Axis Label, **X Axis Minimum/Maximum Date**.
  - image: **Encoding** (default `hex`), **Image Format** (default `jpeg`).
  - count / table: no options.
- Actions: **Create Insight** (copies widget JSON), **Configure Chart**, **Copy as image**, **Save as image**. Legend series toggle and hover tooltips.
- 1.47 "Improved charting capability" for extensions (extension API for charts).
- **SandDance Visualizer** (see §4) for exploratory multi-dimensional views. **Kqlmagic** Plotly renders (timechart, columnchart, piechart, etc.) inside Python notebooks. Kusto `render` in KQL results **[memory]**.

Sources: https://raw.githubusercontent.com/microsoft/azuredatastudio/main/src/sql/workbench/contrib/charts/browser/{interfaces.ts,chartOptions.ts,actions.ts} · https://www.sqlshack.com/create-charts-from-sql-server-data-using-azure-data-studio/ · https://voiceofthedba.com/2020/03/30/charting-in-azure-data-studio/ · https://learn.microsoft.com/en-us/previous-versions/azure-data-studio/extensions/sanddance-extension

---

## 8. Release history highlights and retirement

### 8.1 Timeline (feature highlights only)
| Version (date) | Highlights |
|---|---|
| SQL Operations Studio previews (Nov 2017–Aug 2018) | Cross-platform editor, Object Explorer, results grid, dashboards/insights, integrated terminal, Git. **[memory]** |
| **1.0 GA (Sep 25, 2018)** | Renamed Azure Data Studio; IntelliSense/snippets, charting, dashboards; preview extensions: Notebooks (SQL 2019 ext), Azure Resource Explorer, Profiler, Agent, Import, PolyBase Create External Table wizard; Redgate SQL Search. |
| Oct 2018 | SQL Server 2019/BDC preview (HDFS/Spark), notebooks, Azure Resource Explorer into core, Agent dialogs (steps/schedules/alerts/notifications), custom connection names, auto-reconnect, VS Code 1.26. |
| Nov 2018 | Notebook cell add before/after, Attach-to "Add New Connection", Reinstall Notebook Dependencies; Paste the Plan & High Color Queries extensions. |
| Mar 2019 (1.5) | **PostgreSQL** preview, **SQL Notebooks** public preview in core, PowerShell extension, **dacpac** split from Import, queryplan.show, VS Code 1.30. |
| Nov 2019 (1.13) | SQL Server 2019 GA features (BDC deploy, HDFS tiering, data virtualization), **PowerShell notebooks**, collapsible code cells, Jupyter Books, Deploy wizard (2017/2019 Windows & containers), **Schema Compare GA**, **dacpac GA**, IntelliCode. |
| Oct 2019 (1.12) | **Query History** extension (preview), cell-level copy/paste. |
| Dec 2019 (1.14) | BDC/SQL 2019 GA. |
| Feb–Mar 2020 (1.15–1.16) | Azure sign-in overhaul, Find in Notebook, **charting in SQL notebooks**, Create Jupyter Book, Postgres Entra ID. |
| Apr–Jun 2020 (1.17–1.19) | Markdown toolbar, chart persistence, KQL magic, Always Encrypted (+ enclaves), Cloud Shell, new welcome page; **Machine Learning** ext, Redgate SQL Prompt, Python deps wizard; Azure portal integration, **SQL Assessment** ext, Data Virtualization MongoDB/Teradata. |
| Jul–Sep 2020 (1.20–1.22) | Feature Tour, drag-drop columns/tables to editor, Azure account icon, side-by-side markdown; move/convert cells, Jupyter Books picker, notebook search; **WYSIWYG** text cells, **Kusto kernel**, pinned notebooks, **SQL Database Projects** (preview), **Kusto (KQL)** ext, **Azure Arc** public preview, **SQL Server Import GA**, dark theme default. |
| Oct–Dec 2020 (1.23–1.25) | Azure SQL Edge, Azure SQL DB/VM deployments, PowerShell streaming; **SQL → notebook**, notebook parameterization, SQL results streaming, browse tab in connection dialog; project workspaces. |
| 2021 (1.26–1.34) | Create book dialog, Jupyter server startup 2× faster, URI parameterization; **Run with Parameters**; **results grid filter/sort** + numeric summary status bar (1.30); WYSIWYG links; **Azure Monitor Logs** ext; Notebook Views, split cells, book sections; undo/redo; SQL Projects .NET 6 build + **publish to container**. |
| **Feb 2022 (1.35)** | **Table Designer** and **Query Plan Viewer** (preview). |
| 2022 (1.36–1.40) | Temporal/memory-optimized/graph tables, SDK-style SQL projects, **Azure SQL Migration GA**, .NET Interactive; plan comparison, Backup/Restore to URL (MI); computed columns, drag columns; Top Operations links; included/filtered indexes, Ledger icons; **Nov 2022 (1.40)**: **Table Designer GA, Query Plan Viewer GA, Query History GA**, Encrypt=True default, ARM64 macOS, MySQL preview, Azure SQL DB offline migrations, `.scmp` open, SQL Server 2022 syntax. |
| 2023 (1.41–1.47) | MSAL auth, Synapse nodes, SPID/aggregates in results; ARM64 STS, strict encryption, Dataverse TDS, schema grouping in OE; **SQL Projects GA**, **Parse** button; **Object Explorer filtering (preview)**, **GitHub Copilot**, user/role management (preview); MySQL GA, connection pooling, OE inline actions, XEL up to 1 GB; **Server/Database Properties (preview)**, Attach/Detach, custom clouds; Entra rename, Excel save options, parallel message processing. |
| **Feb 2024 (1.48)** | Last feature release: Restore from URL / S3, Ledger in Create Database, SPID in editor tabs, command-line Connect. |
| Jun–Nov 2024 (1.48.1–1.50) | Hotfixes, Fabric mirrored DB target, Electron 30. |
| Jan–Feb 2025 (1.51, 1.51.1) | Kernel dependency fixes; **retirement banner and notification**. |
| **Jun 2025 (1.52)** | Final release: security fixes; SQL Projects and Migration extension updates. |

### 8.2 Retirement facts
- Announced **Feb 6, 2025** (Azure SQL blog / techcommunity), retired **Feb 28, 2026**; no updates, security patches or maintenance thereafter; repo read-only; Learn docs moved to `previous-versions`.
- Stated reason: consolidate SQL tooling into **VS Code + MSSQL extension** (and SSMS) "to focus investment on fewer, more capable tools".
- Official mapping table: SQL Server Agent → **SSMS**; Profiler → **MSSQL Query Profiler** / SSMS XEvent Profiler; Database administration → MSSQL Database operations + SSMS; Schema management → MSSQL **Schema Compare**, **Schema Designer**, Schema Designer + Copilot, SQL Database Projects/SSDT; Flat-file import → MSSQL Import flat file; DACPAC → MSSQL Data-tier Application dialog / SqlPackage; SQL assessment → **Azure Arc migration assessment**; Azure SQL migration → DMS portal/CLI, Arc, Striim; SQL projects → SQL Database Projects extension; PostgreSQL → **PostgreSQL extension for VS Code**; Cosmos DB → Azure Databases extension; MySQL → "pending announcement". Community-noted gaps at announcement time: no connection import/export, no Agent, Linux users lose SSMS.
- 1.39 of MSSQL (Jan 2026) shipped an **"Azure Data Studio Migration Toolkit"** to move connections/settings.

### 8.3 What the VS Code MSSQL extension gained 2024–2026 (the successor feature set)
- **1.25 (Oct 2024)**: modern UI preview — **Connection dialog** (Parameters / Connection String / Browse Azure, saved & recent), **Table Designer**, **Query results pane** (bottom panel or tab), **Query plan visualizer**, **Object Explorer filtering**; multiple result sets; Azure subscription browsing.
- **1.26–1.29 (Nov 2024–Feb 2025)**: results in tabs, **actual plans**, theming; row filtering, maximize; results font settings, grid auto-resize, edge tables in designer; Always Encrypted enclaves, copy/filter/sort improvements.
- **1.30–1.32 (Mar–May 2025)**: new UI on by default; **Schema Compare (preview)**, execution timing; **1.32 GA** of connection dialog, OE filtering, table designer, results, plan visualizer; **GitHub Copilot integration (preview)**; **Schema Designer (preview)** (drag-and-drop ER modelling, auto-layout); vector datatype.
- **1.33–1.35 (Jun–Aug 2025)**: **Local SQL Server containers** (create/manage Docker containers), **Copilot Agent Mode**, **connection groups**; Copilot agent expansion; **GA** of Schema Designer, Schema Compare, Local containers; text-view results; password memory; grid performance.
- **1.36–1.38 (Sep–Dec 2025)**: **Fabric connectivity** and SQL-in-Fabric provisioning (preview), Copilot slash commands; **Copilot GA** (Ask, Agent, slash), **Edit Data (preview)**, **Data-tier Application dialog (preview)**, **Publish SQL Project dialog**, What's New panel; multi-tenant sign-in.
- **1.39–1.40 (Jan–Feb 2026)**: ADS migration toolkit, table explorer, workspace connections, data copy/export; **Global Object Search**, **Backup/Restore dialogs**, **Flat File Import**, **Create/Rename/Drop Database**, **Query Profiler (preview)**, SQL Server 2025 ARM.
- **1.41–1.45 (Mar–Aug 2026)**: **Data API builder**, Code Analysis settings, **SQL Notebooks (preview)**; database ops GA, Entra MFA via VS Code accounts, View table diagram, background task panel; **SQL Notebooks GA**, Azure SQL provisioning (preview→GA), Schema Designer ORM migrations, notebook export; **Shortcuts Configuration** (Quick Queries), new Results Grid (freeze/hide columns), SQL Formatter (preview), IntelliSense for large DBs, missing-index recommendations in execution plans (1.45.1).
- Current MSSQL feature table (Aug 2026): Connection dialog, Object Explorer, Query results pane, Query plan visualizer, Table designer, Schema designer, Schema compare, GitHub Copilot, Local SQL Server containers, View and edit data, DACPAC/BACPAC, Fabric integration, Database management, Backup and restore, Database object search, Import flat file, Query profiler, Schema designer with Copilot, Data API builder, SQL notebooks, SQL Formatter (preview), Azure integration, Shortcuts configuration. Still absent vs ADS: SQL Agent, dashboards/insight widgets, Server Reports, CMS, Kusto/Azure Monitor/MySQL providers, SandDance, Python/PowerShell/Kusto notebook kernels (SQL only), migration wizard, Machine Learning, Arc deployment.

Sources: https://learn.microsoft.com/en-us/previous-versions/azure-data-studio/release-notes-azure-data-studio · https://learn.microsoft.com/en-us/sql/tools/whats-happening-azure-data-studio · https://devblogs.microsoft.com/azure-sql/azure-data-studio-retirement/ · https://www.microsoft.com/en-us/sql-server/blog/2018/09/25/azure-data-studio-for-sql-server/ · .../2018/10/18/the-october-release-of-azure-data-studio-is-now-available/ · .../2018/11/06/the-november-release-of-azure-data-studio-is-now-available/ · .../2019/03/18/the-march-release-of-azure-data-studio-is-now-available/ · .../2019/10/02/the-october-2019-release-of-azure-data-studio-is-now-available/ · https://github.com/microsoft/vscode-mssql/blob/main/CHANGELOG.md · https://learn.microsoft.com/en-us/sql/tools/visual-studio-code-extensions/mssql/mssql-extension-visual-studio-code · https://github.com/microsoft/azuredatastudio/blob/main/README.md

---

## 9. Takeaways for a replacement product

1. **Plan viewer parity bar** = ADS 1.40: graphical estimated/actual plans, `.sqlplan` open/save, showplan XML, Properties pane with sort/filter/copy, Top Operations grid with filter and jump-to-node, Find Node, expensive-operator highlight by selectable metric, plan comparison with side-by-side properties, zoom/fit/orientation. Users still wanted Live Query Stats, hide-results, batch/row-mode cues, I/O rollups, missing-index surfacing, and Plan-Explorer-style cost breakdowns.
2. **Notebooks as runbooks**: SQL kernel with cached results, Jupyter Books, parameterization (Run with Parameters / URI / Papermill), Agent notebook jobs with materialised outputs, To-Notebook conversion. HTML/PDF export was the standing gap.
3. **Dashboards**: the JSON+T-SQL insight widget model (count/chart/table types, details query, tasks/explorer widgets, per-server and per-database, extension-contributed tabs) was cheap and loved; its weakness was settings-file editing and lost defaults.
4. **Extension catalogue** shows what "DBA-lite" meant: Agent (jobs/alerts/operators), XEvent Profiler with templates/views, flat-file import with type inference, dacpac/bacpac wizard, Schema Compare, SQL projects, Query History, Server Reports, Assessment, CMS, Copilot completions — all without ever adding Query Store, database diagrams, live query stats, or maintenance plans.
5. **Table Designer** feature list (§5) and **export formats** (§6: CSV/JSON/Markdown/Excel/XML + chart images) are the minimum users now expect; VS Code MSSQL has re-implemented most of these and added Schema Designer, local containers, Copilot chat/agent, and Data API builder.
