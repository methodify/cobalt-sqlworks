# Azure Data Studio (ADS) — Core Feature Inventory

Scope: connections, Object Explorer, query editor, results grid, Edit Data, settings, status bar/tabs, shortcuts, admin wizards, shell. (Notebooks, dashboards/insight widgets, execution-plan viewer and the extension ecosystem in depth are covered by the other agent; they are only touched on here where they intersect with core behaviour.)

Retirement context: ADS was announced EOL on Feb 6 2025 and retired **Feb 28 2026**; last GA build 1.52 (June 2025); Microsoft's migration target is VS Code + MSSQL extension. ([what-is-azure-data-studio](https://learn.microsoft.com/en-us/azure-data-studio/what-is-azure-data-studio), [release notes](https://learn.microsoft.com/en-us/azure-data-studio/release-notes-azure-data-studio))

Sources: Microsoft Learn (now archived under `/previous-versions/azure-data-studio/`), the `microsoft/azuredatastudio` and `microsoft/sqltoolsservice` GitHub sources (fetched raw from `main`), Microsoft SQL Server blog release posts, and community write-ups. Items marked **[memory]** were not confirmed by a fetched source.

---

## 0. Architecture in one paragraph

ADS is a VS Code fork (Electron; last merged VS Code ~1.82 in ADS 1.47) with a data-tools shell layered on top. All SQL work (connection, IntelliSense, query execution, scripting, Object Explorer, backup/restore, DacFx, profiler) is delegated over JSON-RPC to **SQL Tools Service** (.NET, `microsoft/sqltoolsservice`, MIT) which uses `Microsoft.Data.SqlClient` as the driver and SMO/DacFx for metadata and scripting. Other engines (PostgreSQL, MySQL, Kusto, etc.) plug in as *connection providers* via the `azdata` extension API and contribute their own tools service. ([what-is-azure-data-studio](https://learn.microsoft.com/en-us/azure-data-studio/what-is-azure-data-studio))

---

## 1. Connections & connection management

### 1.1 Connection dialog
Sources: [connect](https://learn.microsoft.com/en-us/azure-data-studio/connect), [quickstart-sql-server](https://learn.microsoft.com/en-us/azure-data-studio/quickstart-sql-server), [quickstart-sql-database](https://learn.microsoft.com/en-us/azure-data-studio/quickstart-sql-database), [quickstart-postgres](https://learn.microsoft.com/en-us/azure-data-studio/quickstart-postgres), `src/sql/workbench/services/connection/browser/connectionDialogWidget.ts`, `connectionWidget.ts`, `connectionBrowseTab.ts`, `advancedPropertiesController.ts`, `passwordChangeDialog.ts`, `extensions/mssql/package.json`.

The dialog is a right-hand flyout ("Connection" pane) with a **tabbed top half** and a **Connection Details form** in the bottom half:

* Tabs: **Recent** (tree of recent connections, "No recent connection" empty state, action bar with *Clear Recent Connections*, Delete key removes an entry, context menu via `RecentConnectionActionsProvider`) and **Browse** (added 1.24). The Browse tab contains a filter box ("Type here to filter the list", regex, case-insensitive, auto-expands matches) over two sources: **Saved Connections** (server groups + profiles) and provider-contributed trees — principally **Azure** (accounts > tenants > subscriptions > resource types: SQL server, SQL database, SQL Managed Instance, Synapse workspace / dedicated SQL pool, Azure Database for PostgreSQL Flexible Server, Cosmos DB for PostgreSQL, etc.; 1.45 added tenant hierarchy and only shows resource types that exist). Clicking a resource fills the form (source tagged `azure` or `savedconnections`).
* **Connection type** dropdown (provider: MSSQL, PGSQL, MySQL, Kusto, LogAnalytics, MongoDB, Oracle ... whichever providers are installed; filtered to *query providers* when opened from an editor).
* **Input type** radio: *Parameters* / *Connection String* (connection-string mode shows a single "Connection string" box that the provider parses; 1.43 added optional name/group when creating from a connection string).
* Fields, in order: **Server** (required; placeholder "server name or IP"), **Authentication type**, **User name**, **Password**, **Remember password** checkbox, **Account** (Azure auth only; includes "Add an account..." and a "Refresh account credentials" link when stale), **Microsoft Entra tenant** (Azure auth; hidden since 1.44 when the SQL Authentication Provider is on), **Encrypt** and **Trust server certificate** (promoted from Advanced to the front page in 1.40 with info-icon tooltips), **Database** (editable dropdown, `<Default>`, loads DB list on focus, "Loading..."), **Server group** (`<Default>`, existing groups, "Add new group..." which opens the group dialog), **Name (optional)** (friendly profile name), then an **Advanced...** button and **Connect / Cancel**. Enter connects; Esc cancels.
* Authentication types (MSSQL): **SQL Login**, **Windows Authentication** (`Integrated`; default on Windows; supported on macOS/Linux via Kerberos since 2019), **Microsoft Entra ID – Universal with MFA support** (`AzureMFA`, default for Azure), **Azure AD – Password** (`AzureMFAAndUser`), plus internal `DSTS` and `None` entries in the enum. There is no dedicated service-principal / managed-identity auth type in the dialog; those only worked via connection string / advanced properties [memory]. `sql.defaultAuthenticationType` picks the default. ([settings-list](https://learn.microsoft.com/en-us/azure-data-studio/settings-list))
* Firewall: connecting to Azure SQL from a non-allowed IP pops a **Create new firewall rule** form (account/tenant pre-selected; rule name customizable since 1.41). ([quickstart-sql-database](https://learn.microsoft.com/en-us/azure-data-studio/quickstart-sql-database))
* **Change Password** dialog (1.41/1.42): "Password must be changed for '{user}' to continue logging into '{server}'", New password / Confirm password, mismatch validation.
* Duplicate handling: 1.44/1.45 allow multiple profiles to the same server with different advanced properties or profile names; dragging a duplicate prompts a notification.
* Tab/tooltip visibility of advanced options improved in 1.47.

### 1.2 Advanced Properties dialog (MSSQL provider)
Title "Advanced Properties", OK / Discard, options grouped by `groupName`, each with description hover. The MSSQL provider's `connectionOptions` (from `extensions/mssql/package.json`):

| Group | Options (default) |
|---|---|
| Source | connectionName, server (required), database, attachDbFilename, failoverPartner, contextConnection |
| Security | authenticationType (SqlLogin/Integrated/AzureMFA), user, password, columnEncryptionSetting (disabled/enabled — Always Encrypted), secureEnclaves (disabled/enabled), attestationProtocol (hgs/aas/none), enclaveAttestationUrl, **encrypt** (`true`; values false/true/strict — Strict/Mandatory/Optional UI in 1.42), persistSecurityInfo, hostNameInCertificate (1.42), **trustServerCertificate** (`false`) |
| Initialization | applicationName (`azdata`), applicationIntent (ReadWrite/ReadOnly), connectTimeout (30), commandTimeout (30; added 1.42), currentLanguage |
| Connection Resiliency | connectRetryCount (1), connectRetryInterval (10) |
| Context | workstationId |
| Pooling | pooling, maxPoolSize, minPoolSize, loadBalanceTimeout |
| Replication | replication |
| Advanced | multipleActiveResultSets (MARS), packetSize, typeSystemVersion, multiSubnetFailover, port |

The same options can be passed on the command line (`-Z/--connectionProperties '{"key":"value"}'`) or in an `azuredatastudio://connect?...` URI. ([command-line](https://learn.microsoft.com/en-us/azure-data-studio/command-line))

### 1.3 Server groups, saved/recent connections, credential storage
Sources: [server-groups](https://learn.microsoft.com/en-us/azure-data-studio/server-groups), `connectionTreeAction.ts`, `connection.contribution.ts`.

* **SERVERS** view (Connections activity-bar icon, **Ctrl+G**). Toolbar: *New Connection*, *New Server Group*, *Show Active Connections / Show All Connections* toggle, *Collapse All Connections* (`registeredServers.collapseAll`), and (mssql) *Enable/Disable Group by Schema*. Tree supports drag-and-drop of servers between groups and drag of table/column names into the editor (1.20).
* **Server group** dialog: name, colour (fixed palette editable via the *Server Groups* colour list in settings), description. Context menu on a group: *New Connection*, *Edit Server Group*, *Delete* (deleting a group with no connections was fixed in 1.42).
* Connection profile context menu (built-ins in order): *Disconnect* (if connected), *Edit Connection*, *Delete Connection*, *Refresh*, plus contributed items (*Manage*, *New Query*, *Launch Profiler*, *Data-tier Application Wizard*, *Schema Compare*, *Import wizard*, *New Database*, *New Login*, *Server Properties*, ...).
* Persistence: groups and profiles are stored in user `settings.json` under `datasource.connectionGroups` / `datasource.connections` [memory, consistent with docs saying "saved into User Settings"]; passwords go to the OS credential store (Windows Credential Manager / macOS Keychain / libsecret) via the credentials provider; `azure.noSystemKeychain` forces a flat file on macOS. Azure tokens are cached; commands *Azure Accounts: Clear Azure Account Token Cache* and *Clear all saved accounts*. ([azure-connectivity](https://learn.microsoft.com/en-us/azure-data-studio/azure-connectivity))
* **Recent connections** list, capped by `sql.maxRecentConnections` (25); 1.45.1 fixed temporary ("Do not save") connections in the recent list.
* `connection.showUnsupportedServerVersionWarning` (true), `sql.defaultEngine` (`MSSQL` — drives the language flavor of new `.sql` files and the default provider in the dialog).
* Connection pooling for MSSQL introduced 1.45 and defaulted on in 1.46 (`mssql.enableConnectionPooling`), with command **SQL Server: Clear Pooled Connections**.
* Command line: `azuredatastudio -S server -D db -U user -T AzureMFA|SqlLogin|Integrated -P MSSQL -A appName -c connect|openConnectionDialog --showDashboard file.sql`; URI handler `azuredatastudio://connect?server=&database=&user=&authenticationType=&applicationName=&connectionProperties={...}`.

### 1.4 Azure accounts
Sources: [azure-connectivity](https://learn.microsoft.com/en-us/azure-data-studio/azure-connectivity).

* **Azure** activity-bar view ("Linked accounts") lists accounts and the resource tree (subscriptions > resource types); *Add an account* uses **Code Grant** (browser, default, `accounts.azure.auth.codeGrant`) or **Device Code** (`accounts.azure.auth.deviceCode`). MSAL only since 1.46 (ADAL fallback existed 1.41–1.45 via *Azure: Authentication Library*).
* National clouds: `accounts.azure.cloud.enablePublicCloud|enableChinaCloud|enableUsGovCloud`; **custom cloud endpoints** via `azure.customProviderSettings` (1.46).
* Filters: `azure.resource.config.filter`, `azure.tenant.config.filter`; proxy: `http.proxy`, `http.proxyStrictSSL`, `http.proxyAuthorization`, `http.proxySupport`, `http.systemCertificates`; diagnostics: `azure.loggingLevel`, `azure.piiLogging` (warning toast when enabled, 1.44).
* **Sql Authentication Provider** (`mssql.enableSqlAuthenticationProvider`, default on since 1.44) hands token acquisition to the driver, removing the tenant picker and improving token refresh.
* Azure portal deep-link from an Azure SQL connection (1.19); sleeping serverless Azure SQL is not woken by Object Explorer (1.41).

### 1.5 Engines reachable
* Built-in MSSQL provider: SQL Server 2012+ (on-prem/VM/containers, Linux, Arc-enabled SQL MI), Azure SQL Database, Azure SQL MI, Azure Synapse dedicated SQL pool (`SqlDw`) and serverless SQL pool (`SqlOnDemand`), SQL Server Big Data Clusters, Azure SQL Edge, Microsoft Dataverse TDS endpoint (1.42), Power BI Datamart (1.36.2), and **Fabric Data Warehouse / SQL analytics endpoint** — connected through the same dialog using the Fabric SQL connection string and *Microsoft Entra ID – Universal with MFA* auth; the SQL Database Projects extension gained "Synapse Data Warehouse in Microsoft Fabric" (1.46) and "Fabric mirrored SQL database" (1.49) target platforms. ([release notes](https://learn.microsoft.com/en-us/azure-data-studio/release-notes-azure-data-studio); Fabric connectivity note [memory] beyond the project-target facts)
* Via extensions: PostgreSQL (preview; adds "PostgreSQL" connection type with Server name / User name / Password / Remember / Database / Server group / Name fields), MySQL (GA 1.45; MySQL native or Entra auth), Kusto/Azure Data Explorer, Azure Monitor Logs (KQL), MongoDB / Cosmos DB for Mongo, MongoDB Atlas (3rd party), Oracle (preview), Cosmos DB NoSQL. ([quickstart-postgres](https://learn.microsoft.com/en-us/azure-data-studio/quickstart-postgres), [mysql-extension](https://learn.microsoft.com/en-us/azure-data-studio/extensions/mysql-extension))

---

## 2. Object Explorer

Sources: `sqltoolsservice/src/Microsoft.SqlTools.SqlCore/ObjectExplorer/SmoModel/SmoTreeNodesDefinition.xml`, `src/sql/workbench/contrib/scripting/browser/scripting.contribution.ts`, `serverTreeActionProvider.ts`, `connectionTreeAction.ts`, `filterDialog/filterDialog.ts`, `extensions/mssql/package.json`, [release notes](https://learn.microsoft.com/en-us/azure-data-studio/release-notes-azure-data-studio).

### 2.1 Tree structure (MSSQL provider, from the SMO tree definition)
`ValidFor` gates are shown in brackets: AllOnPrem, AzureV12 (Azure SQL DB), SqlDw (Synapse dedicated), SqlOnDemand (Synapse serverless), SqlYYYY+ version gates.

```
Server (label = profile name or server; icon differs for cloud/on-prem/Arc)
├─ Databases                      (user DBs; filter props: Name, Owner, CreateDate; Status/IsLedger shown)
│  ├─ System Databases
│  └─ <Database>  (folders after nodes)
│     ├─ Tables                    [filters Name, Schema, CreateDate, Owner]
│     │  ├─ System Tables
│     │  ├─ FileTables             [on-prem]
│     │  ├─ External Tables        [2016+/AzureV12/SqlOnDemand]
│     │  ├─ Dropped Ledger Tables  [2022+/AzureV12]
│     │  └─ <Table> (subtypes: temporal SystemVersioned + History table child, Ledger append-only/updatable, External)
│     │     ├─ Columns (filters Name, InPrimaryKey) ├─ Dropped Ledger Columns
│     │     ├─ Keys (PK/unique) [not SqlDw] ├─ Constraints (default/check) ├─ Triggers (DML) [on-prem/AzureV12]
│     │     ├─ Indexes (filters Name, IsMemoryOptimized) [not SqlOnDemand] └─ Statistics [not SqlOnDemand]
│     ├─ Views (System Views, Dropped Ledger Views; <View> > Columns, Triggers, Indexes, Statistics)
│     ├─ Synonyms                  [on-prem/AzureV12]
│     ├─ Programmability
│     │  ├─ Stored Procedures (System Stored Procedures; <Proc> > Parameters) [filters + IsNativelyCompiled]
│     │  ├─ Functions > Table-valued Functions, Scalar-valued Functions, Aggregate Functions, System Functions
│     │  ├─ Database Triggers (DDL) ├─ Assemblies ├─ Rules ├─ Defaults
│     │  ├─ Types > System Data Types (Exact/Approximate Numerics, Date and Times, Character Strings, Unicode, Binary, Other, CLR, Spatial), User-Defined Data Types, User-Defined Table Types (> Columns, Keys, Constraints), User-Defined Types, XML Schema Collections
│     │  └─ Sequences               [2012+/AzureV12]
│     ├─ External Resources > External Data Sources, External File Formats   [2016+/AzureV12/SqlOnDemand]
│     ├─ Service Broker > Message Types, Contracts, Queues, Services, Routes, Remote Service Bindings, Broker Priorities, Event Notifications (+System_* folders) [on-prem]
│     ├─ Storage > Filegroups (> files), Full Text Catalogs, Full Text Stoplists, Log Files, Partition Functions, Partition Schemes, Search Property Lists [on-prem/AzureV12]
│     └─ Security > Users, Roles (Database Roles, Application Roles), Schemas, Asymmetric Keys, Certificates, Symmetric Keys, Database Scoped Credentials, Database Encryption Keys, Master Keys, Signatures, Database Audit Specifications, Security Policies, Always Encrypted Keys (Column Master Keys, Column Encryption Keys)
├─ Security > Logins, Server Roles, Credentials, Cryptographic Providers, Server Audits, Server Audit Specifications
└─ Server Objects > Endpoints, Linked Servers, Triggers, Error Messages   [on-prem]
```
(Also Event Sessions / Event Notifications nodes exist at server level.) **Group by Schema** mode (`mssql.objectExplorer.groupBySchema`, 1.42) re-roots the database as `<schema>` nodes each containing Tables/Views/Synonyms/Programmability, with a *Built-in schemas* folder for db_owner etc.

### 2.2 Node actions / context menus
Core scripting menu (`0_query` group, in order, with node-type gating):
1. **Select Top 1000** — Table, View, History table (label "Take 10" for Kusto/Log Analytics)
2. **Edit Data** — Table only; hidden for ledger append-only/dropped, Synapse serverless (`SqlOnDemand`) and dedicated (`SqlDataWarehouse`)
3. **Script as Create** — Table, View, Schema, User, UDTT, Stored Procedure, Aggregate/Partition/Scalar/Table-valued Function, Trigger, Database Trigger, Index, Key, Database Role, Application Role
4. **Script as Drop** — same set
5. **Script as Alter** — Stored Procedure, View, functions (MSSQL/Kusto)
6. **Script as Execute** — Stored Procedure, Function
7. **Refresh** (also inline)
Scripts open in a new query editor connected to the same database.

Built-in tree actions: *Filter* (`objectExplorer.filterChildren`, inline funnel icon; on folders with filter properties), *Remove Filter*, *Refresh*, *Disconnect*, *Edit Connection*, *Delete Connection*, *New Query* (`objectExplorer.newQuery` — opens an editor pre-connected to that node's DB), *Manage* (opens the server/database dashboard; 1.47 fixed it opening the logical server instead of the DB), *Collapse all*, arrow-key expand/collapse (1.45), inline edit/delete/refresh icons (1.45).

MSSQL-extension items (all Object Explorer + Data Explorer contexts, `when` on `connectionProvider == MSSQL`, `nodeType`, `mssql:engineedition`, `mssql:iscloud`): **New Database** (Ledger option 1.48), **Drop Database**, **Attach Database / Detach Database** (1.46 preview), **Backup Database / Restore Database** (also on the database dashboard toolbar), **Database Properties / Server Properties** (1.46 preview; "Object Properties" dialog, improved 1.47/1.49), **New Table** (Tables folder), **Design Table** (default double-click action; Table Designer GA 1.40), **Drop Object**, **Rename Object**, **New Login / New User / New Database Role / New Application Role / New Server Role** (User Management, 1.44+ preview, with MUST_CHANGE, role membership), **Enable/Disable Group by Schema**, **Select Top 1000** on temporal history tables (1.47). Extensions add: **Data-tier Application Wizard** (Databases folder/db/server), **Schema Compare** (db/server), **Import wizard** (Ctrl+I; server/db), **Launch Profiler** (Alt+P; server), **Generate Scripts** [memory: only via 3rd-party ExtraSqlScriptAs; no built-in Generate Scripts wizard — listed as SSMS-only in the feature matrix].

### 2.3 Filtering & search
* **Filter dialog** (preview 1.44, GA for MSSQL 1.45): subtitle "Path: <node path>", table with **Property / Operator / Value / Clear** columns. Operators — string: Contains, Not Contains, Equals, Not Equals, Starts With, Not Starts With, Ends With, Not Ends With; number/date: Equals, Not Equals, Greater Than, Greater Than Or Equals, Less Than, Less Than Or Equals, Between, Not Between (Between adds an "And" row); boolean/choice: Equals, Not Equals. Buttons OK / Clear All / Cancel; validation on Between ranges. Filter properties per folder: Tables/Views/Sequences (Name, Schema, CreateDate, Owner), Stored Procedures & functions (+ IsNativelyCompiled), Databases (Name, Owner, CreateDate), Columns (Name, InPrimaryKey), Indexes (Name, IsMemoryOptimized), Tables also expose Durability (Schema+Data / Schema Only) and IsMemoryOptimized.
* **Search Servers** command (`mssql.searchServers`, from the Servers view title) plus the dashboard **search widget** (`t:`, `v:`, `sp:`, `f:` prefixes) for object search; results list carries the same context menu (Edit Data, Script as ..., Select Top 1000). ([tutorial-sql-editor](https://learn.microsoft.com/en-us/azure-data-studio/tutorial-sql-editor), [andrewvillazon](https://www.andrewvillazon.com/auzre-data-studio-tips-tricks/))
* `mssql.objectExplorer.expandTimeout` (45 s) — configurable expand timeout (1.42). Async server tree default since 1.45.
* Double-click on a Table opens Table Designer; on a User opens the user dialog (1.43).

---

## 3. Query editor

Sources: `src/sql/workbench/contrib/query/browser/{queryActions,keyboardQueryActions,query.contribution,flavorStatus}.ts`, `extensions/mssql/package.json`, [code-snippets](https://learn.microsoft.com/en-us/azure-data-studio/code-snippets), [tutorial-sql-editor](https://learn.microsoft.com/en-us/azure-data-studio/tutorial-sql-editor), [release notes](https://learn.microsoft.com/en-us/azure-data-studio/release-notes-azure-data-studio).

### 3.1 Editor toolbar (left to right)
**Run** (F5; runs selection if any, else whole document; disabled when disconnected/empty) · **Cancel** (enabled while executing; 1.46 made it stop immediately) · **Disconnect** / **Connect** / **Change Connection** (label "Change"; shortened 1.44) · **Database dropdown** (`ListDatabasesActionItem`; lists DBs on focus, accepts typed names, strips Synapse Gen3 pool suffix, contained-user fix 1.42) · **Estimated Plan** (Ctrl+L) · **Enable/Disable Actual Plan** toggle (Ctrl+M; toolbar toggle since 1.37) · **Enable/Disable SQLCMD** (preview features only; switches language flavor to `sqlcmd`) · **Parse** (Shift+Alt+P; added to toolbar 1.43) · **To Notebook / Export as Notebook** (`mssql.exportSqlAsNotebook`). Right side of the tab bar hosts VS Code's split-editor etc.

### 3.2 Language features (MSSQL via SQL Tools Service)
* IntelliSense: keyword/object/column completion, signature help, quick info hovers, error diagnostics (red squiggles) — settings `mssql.intelliSense.enableIntelliSense`, `.enableSuggestions`, `.enableErrorChecking`, `.enableQuickInfo`, `.lowerCaseSuggestions`; **Refresh IntelliSense Cache** command (no default key); Ctrl+Space triggers suggestions; IntelliCode and GitHub Copilot available as extensions (Copilot "query contextualization" preview via `queryEditor.githubCopilotContextualizationEnabled`).
* **Peek Definition** (Alt+F12) and **Go to Definition** (F12) on tables/views/procs/functions — opens the object's CREATE script inline or in a new tab.
* **Snippets**: built-in prefixes (from `extensions/mssql/snippets/mssql.json`): `sqlCreateDatabase, sqlDropDatabase, sqlCreateTable, sqlDropTable, sqlAddColumn, sqlDropColumn, sqlSelect, sqlInsertRows, sqlDeleteRows, sqlUpdateRows, sqlCreateStoredProc, sqlListTablesAndViews, sqlListDatabases, sqlListColumns, sqlCursor, sqlGetSpaceUsed, sqlCreateIndex, sqlCreateTempTable` (docs also reference view/login/user/role snippets from earlier versions). Type `sql` to list them; tab-stop placeholders (`${1:TableName}`), *Change All Occurrences* (Ctrl+F2) for renaming; user snippets via *Preferences: Open User Snippets > SQL* (`sql.json`); `editor.snippetSuggestions` top/bottom/inline/none.
* **Formatting**: *Format Document* (Shift+Alt+F) / *Format Selection* backed by the tools service; settings `mssql.format.alignColumnDefinitionsInColumns`, `.datatypeCasing`, `.keywordCasing` (none/uppercase/lowercase), `.placeCommasBeforeNextStatement`, `.placeSelectStatementReferencesOnNewLine`. Third-party Poor SQL Formatter / Redgate SQL Prompt extensions offered richer formatting.
* Syntax highlighting via the T-SQL TextMate grammar (fixes for nested comments, functions, `DESC` in 1.45/1.50); bracket matching, code folding (Ctrl+Shift+[ / ]), minimap, multi-cursor (Alt+Click, Ctrl+Alt+Up/Down, Ctrl+D), box selection (Shift+Alt+drag), line move/duplicate (Alt+Up/Down, Shift+Alt+Up/Down), comment toggles (Ctrl+/, Shift+Alt+A), find/replace with regex (Ctrl+F / Ctrl+H), join lines, case transforms, Zen mode, split/side-by-side editors, diff of two files — all inherited from VS Code. ([andrewvillazon](https://www.andrewvillazon.com/auzre-data-studio-tips-tricks/), [sqlservercentral](https://www.sqlservercentral.com/articles/editor-tips-and-tricks-for-azure-data-studio))
* Drag a table/column from the Servers tree into the editor to insert its name (1.20).
* **Language flavor**: `.sql` files not yet connected get the default flavor (`sql.defaultEngine`); the status-bar item ("MSSQL"/"PGSQL"/...) opens *Change SQL Engine Provider*; cannot change while connected.

### 3.3 Execution semantics
* **Run** (F5) executes the selection or the whole editor; batches split on `GO` (with `GO n` repeat) by the tools service; results stream in per result set; the **Run Current Query** (Ctrl+F5, 1.18) executes only the statement under the cursor; **Focus on Current Query** (Ctrl+Shift+O) selects it.
* **Query shortcuts** `sql.query.shortcut1..9`: run the setting's text as a proc with the selected text as argument — defaults `sp_help` = Alt+F2, `sp_who` = Ctrl+Shift+1, `sp_lock` = Ctrl+Shift+2 [defaults from source; exact key map partially memory].
* Per-connection execution options (`mssql.query.*`, applied via SET before each run): `rowCount` (0 = unlimited), `textSize` (2 147 483 647), `executionTimeout` (0 = none), `noCount`, `noExec`, `parseOnly`, `arithAbort` (true), `statisticsTime`, `statisticsIO`, `xactAbortOn`, `transactionIsolationLevel` (READ COMMITTED / READ UNCOMMITTED / REPEATABLE READ / SERIALIZABLE), `deadlockPriority` (Normal/Low), `lockTimeout` (-1), `queryGovernorCostLimit` (-1), `ansiDefaults`, `quotedIdentifier` (true), `ansiNullDefaultOn`, `implicitTransactions`, `cursorCloseOnCommit`, `ansiPadding`, `ansiWarnings`, `ansiNulls`, `alwaysEncryptedParameterization` (Always Encrypted parameterization, 1.18), plus `maxCharsToStore` (65 535), `maxXmlCharsToStore` (2 097 152), `displayBitAsNumber` (true).
* Transactions: each editor owns one connection (SPID shown on the tab since 1.48); `BEGIN TRAN` stays open across runs until committed/rolled back; disconnecting rolls back [memory]; 1.49 fixed the editor overriding a T-SQL-set isolation level.
* **SQLCMD mode** (Sept 2019, preview-gated): toolbar toggle; supports the common `:setvar`, `:connect`, `:r`, `$(var)` subset but "not all commands" and no SQLCMD syntax highlighting; integrates with Schema Compare's SQLCMD-script output. ([discussion #24216](https://github.com/microsoft/azuredatastudio/discussions/24216), [Sept 2019 blog](https://www.microsoft.com/en-us/sql-server/blog/2019/09/10/the-september-release-of-azure-data-studio-is-now-available/))
* **Parallel message processing** (`mssql.parallelMessageProcessing`, default on since 1.47; limit 100) speeds result/message throughput.
* Query text > 65 535 characters displays fully (1.40); results pane blank/#-prefixed fixes (1.48).
* **Query History** (extension, GA 1.40): panel in the bottom Panel area listing each executed query with text, connection and timestamp; context menu **Open Query**, **Run Query**, **Delete**, **Clear All History**; header buttons *Clear*, *Pause/Start Query History Capture*; double-click opens or runs (`queryHistory.doubleClickAction`), search/filter in the view; settings `queryHistory.captureEnabled` (true), `queryHistory.persistHistory` (true), `queryHistory.maxEntries` (100); command *Open Storage Folder*. ([Aug/Nov 2022 blogs](https://www.microsoft.com/en-us/sql-server/blog/2022/11/16/azure-data-studio-november-release/), `extensions/query-history/package.json`)
* **Copy Query With Results** (Ctrl+K Ctrl+V) copies text + HTML-formatted results.

---

## 4. Results grid & messages

Sources: `gridPanel.ts`, `actions.ts`, `headerFilter.plugin.ts`, `messagePanel.ts`, `queryResultsView.ts`, `statusBarItems.ts`, `chartOptions.ts`, `query.contribution.ts`, `resultsGrid.contribution.ts`, [sqlshack charts](https://www.sqlshack.com/create-charts-from-sql-server-data-using-azure-data-studio/), [release notes](https://learn.microsoft.com/en-us/azure-data-studio/release-notes-azure-data-studio).

### 4.1 Layout
* Results pane sits under the editor behind a draggable sash; **Toggle Query Results** (Ctrl+Shift+R) hides/shows it; **Toggle Focus Between Query And Results** (Ctrl+Shift+F). Tabs: **Results**, **Messages**, and dynamically **Chart**, **Query Plan** (Execution Plan), **Top Operations**, **Plan Tree**, plus extension-registered model-view tabs; visible-tab set persists per editor. Messages tab auto-activates on error.
* Each result set is its own SlickGrid stacked vertically inside a scrollable view; minimum 8 visible rows; a per-grid **Maximize / Restore** action expands one set to fill the pane. Results **stream** as rows arrive (`queryEditor.results.streaming`, true) and are backed by the tools service's disk-based result buffer, so very large result sets don't live in renderer memory; only rows below `queryEditor.results.inMemoryDataProcessingThreshold` (5 000) are sortable/filterable in-memory, with `promptForLargeRowSelection` confirming large selections.
* Grid appearance: row-number column, header row 26 px, rows 29 px (`resultsGrid.rowHeight`), `resultsGrid.fontFamily|fontSize|fontWeight|letterSpacing|cellPadding`, `resultsGrid.autoSizeColumns` (true) with `resultsGrid.maxColumnWidth` (400 px; raised 1.44), NULL cells rendered as `NULL` with the `queryEditorNullBackground` colour, bit shown as 0/1 (`mssql.query.displayBitAsNumber`), datetimeoffset precision fix 1.42.
* **Cell viewers**: JSON strings render as links (`resultsGrid.showJsonAsLink`; 1.42 added an option to disable special JSON handling) — clicking opens a formatted JSON editor tab; XML cells (including `FOR XML` and formatted XML stored in varchar, 1.47) open in an XML editor; double-click a cell selects the row (1.43); Shift+click / Shift+arrow multi-cell selection (1.40), keyboard navigation and focus fixes 1.41.
* **Selection summary** on the status bar for multi-cell selections: numeric → `Average / Count / Sum` (tooltip adds Distinct Count, Max, Min, Null Count); non-numeric → `Count / Distinct Count / Null Count` (1.30, extended 1.41).

### 4.2 Sorting, filtering, resizing
Column header funnel (preview 1.30, later default): **Sort Ascending / Sort Descending**, search box, **Select All** checkbox list of distinct values with `(NULL)` and `(Blanks)` entries, count badges, **OK / Clear / Cancel**; keyboard shortcut **Ctrl/Cmd+Shift+O** toggles column sort (1.45). Columns are resizable by drag; widths persist in editor state.

### 4.3 Context menu (in order) and action bar
Right-click: **Select All** · — · **Save as CSV / Excel / JSON / Markdown / XML** · (contributed items e.g. SandDance) · — · **Copy** (Ctrl+C) · **Copy With Headers** (Ctrl+Shift+C) · **Copy Headers** (Ctrl+Shift+H) · **Maximize / Restore**. Chord shortcuts: Save as CSV Ctrl+K Ctrl+C, JSON Ctrl+K Ctrl+J, Markdown Ctrl+K Ctrl+M, Excel Ctrl+K Ctrl+E, XML Ctrl+K Ctrl+X. The vertical **action bar** on the right of each grid (36 px; hide with `queryEditor.results.showActionBar`, 1.41) shows Save as CSV / Excel / JSON / Markdown / XML, **Chart**, **Visualizer** (when a visualizer extension such as SandDance is installed) and Maximize. Saving a *selection* exports only the selected cells; `queryEditor.results.openAfterSave` (true) opens the file; Excel export is validated against the 1 048 576-row / 16 384-column limits with a notification and a prompt to open the file location (1.46).

Copy behaviour settings: `queryEditor.results.copyIncludeHeaders` (false), `copyRemoveNewLine` (true), `skipNewLineAfterTrailingLineBreak` (1.45), `showCopyCompletedNotification` (auto-closing after 3 s), `preferProvidersCopyHandler` (large-copy performance moved to the tools service, 1.45/1.48), progress notification for big copies.

Save-format settings: `queryEditor.results.saveAsCsv.includeHeaders|delimiter|lineSeperator|textIdentifier|encoding`; `saveAsExcel.includeHeaders|freezeHeaderRow|autoFilterHeaderRow|autoSizeColumns|boldHeaderRow` (1.47); `saveAsMarkdown.encoding|includeHeaders|lineSeparator`; `saveAsXml.formatted|encoding`; JSON has no options.

### 4.4 Chart viewer
Chart tab (from the grid's Chart icon): **Chart Type** — Bar, Horizontal Bar, Line, Pie, Doughnut, Scatter, Time Series, Table, Count, Image (`queryEditor.chart.defaultChartType`, default horizontalBar); **Data Direction** (Vertical/Horizontal), **Use column names as labels**, **Use first column as row label**, **Legend Position** (top/bottom/left/right/none), **Y/X Axis Label**, **Y/X Axis Min/Max** (dates for time series), **Data Type** (Number/Point) for line, **Encoding** / **Image Format** for image. Buttons **Create Insight** (emits the JSON for a dashboard widget), **Copy as image**, **Save as image** (.png). Row-count limit before charting is validated.

### 4.5 Messages pane
Tree of batch entries: "Started executing query at Line N" with timestamp (clickable to jump to the editor line range), rows-affected messages, PRINT/RAISERROR output, errors in red with clickable line numbers, "Total execution time: hh:mm:ss.fff". Context menu **Copy / Copy All**. Settings `queryEditor.messages.wordwrap` (true), `queryEditor.messages.showBatchTime` (false).

---

## 5. Edit Data

Sources: `editDataActions.ts`, `editDataGridPanel.ts`, [tutorial-sql-editor](https://learn.microsoft.com/en-us/azure-data-studio/tutorial-sql-editor), [sqlshack dashboards](https://www.sqlshack.com/server-and-database-dashboards-in-azure-data-studio/).

* Launched from a table's **Edit Data** (Object Explorer, dashboard search results, or the Table Designer's edit-data tab). Opens an "Edit Data" tab (title shortened in 1.47) with a toolbar: **Run** (refresh), **Stop**, **Show SQL Pane / Hide SQL Pane** (reveals the generated `SELECT` which you can edit to add a WHERE/ORDER BY and re-run), and **Max Rows** dropdown (`200` default, `1000`, `10000`).
* Grid semantics: inline cell editors on editable columns (computed/identity/timestamp columns are read-only); a change is submitted when you press **Enter** or leave the row; **Esc** reverts the current row; **Ctrl+0** sets a cell to NULL; the last row is a NULL placeholder — typing in it and pressing Enter INSERTs; right-click **Delete Row** deletes and commits; per-row errors surface as notifications ("An error occurred while submitting data to new row"); cells containing the Unicode null char are not editable; HTML in cells is rendered safely (1.45); invalid values for incompatible column types are rejected (1.46). Edits are executed through the tools service's edit session (keyed on primary key/unique index — tables without a key are not editable [memory]).
* Not available for views (open GitHub request), Synapse, or ledger append-only/dropped tables.

---

## 6. Settings relevant to SQL work

Sources: [settings](https://learn.microsoft.com/en-us/azure-data-studio/settings), [settings-list](https://learn.microsoft.com/en-us/azure-data-studio/settings-list), `extensions/mssql/package.json`, `query.contribution.ts`, `resultsGrid.contribution.ts`, `connection.contribution.ts`.

* Mechanics: Settings UI (Ctrl+,) with User/Workspace scopes, `@modified` filter, gear > Reset Setting; JSON at `%APPDATA%\azuredatastudio\User\settings.json` (macOS `~/Library/Application Support/azuredatastudio/User/`, Linux `~/.config/azuredatastudio/User/`); keybindings.json and snippets live beside it. **No built-in Settings Sync** (the VS Code feature was not carried over; users resorted to the third-party Settings Sync extension with mixed results). ([code-settings-sync issues](https://github.com/shanalikhan/code-settings-sync/issues/892))
* Shell: `workbench.enablePreviewFeatures` (gates SQLCMD, backup/restore, attach/detach, create/delete database, database/server properties, user management, Central Management Servers, Server Reports, SQL Assessment, Agent, Profiler, etc. — [preview-features](https://learn.microsoft.com/en-us/azure-data-studio/preview-features)), `workbench.colorTheme` (Default Dark Azure Data Studio is the default since 1.20.1; Light, High Contrast, and marketplace themes), `workbench.startupEditor` / "Show welcome page on startup", `window.restoreWindows`, `files.autoSave` (off/afterDelay/onFocusChange/onWindowChange) + `files.autoSaveDelay`, `files.hotExit`, `editor.fontFamily|fontSize|tabSize|insertSpaces|detectIndentation|snippetSuggestions`, `editor.suggest.showSnippets`, `terminal.integrated.*`, `http.proxy*`.
* SQL/core: `sql.defaultEngine`, `sql.defaultAuthenticationType`, `sql.maxRecentConnections`, `sql.query.shortcut1-9`, `connection.showUnsupportedServerVersionWarning`, `datasource.connections` / `datasource.connectionGroups` [memory], `queryEditor.tabColorMode` (off/border/fill), `queryEditor.showConnectionInfoInTitle` (true), `queryEditor.promptToSaveGeneratedFiles` (false), `queryEditor.results.*` (see §4), `queryEditor.messages.*`, `queryEditor.chart.defaultChartType`, `resultsGrid.*` (see §4.1), `executionPlan.tooltips.enableOnHoverTooltips`, `queryHistory.*`, `dashboard.server.widgets` / `dashboard.database.widgets` (dashboard agent).
* MSSQL provider: `mssql.intelliSense.*`, `mssql.format.*`, `mssql.query.*` (see §3.3), `mssql.objectExplorer.groupBySchema|expandTimeout`, `mssql.enableSqlAuthenticationProvider`, `mssql.enableConnectionPooling`, `mssql.parallelMessageProcessing|parallelMessageProcessingLimit`, `mssql.executionPlan.expensiveOperationMetric` (off/actualElapsedTime/actualElapsedCpuTime/cost/subtreeCost/actualNumberOfRowsForAllExecutions/numberOfRowsRead), `mssql.tableDesigner.preloadDatabaseModel|allowDisableAndReenableDdlTriggers`, `mssql.tracingLevel` (All/Off/Critical/Error/Warning/Information/Verbose), `mssql.trace.server`, `mssql.logDebugInfo`, `mssql.piiLogging`, `mssql.logRetentionMinutes` (10 080), `mssql.logFilesRemovalLimit` (100), `mssql.ignorePlatformWarning`.
* Azure: `accounts.azure.auth.codeGrant|deviceCode`, `accounts.azure.cloud.*`, `azure.customProviderSettings`, `azure.resource.config.filter`, `azure.tenant.config.filter`, `azure.loggingLevel`, `azure.piiLogging`, `azure.noSystemKeychain`.
* Extension examples: `dacFx.defaultSaveLocation`, `flatFileImport.logDebugInfo`, `profiler.viewTemplates` / `profiler.sessionTemplates` / `profiler.filters`, `bigdatacluster.ignoreSslVerification`.

---

## 7. Status bar, tab naming, tab colouring

Sources: `statusBarItems.ts`, `flavorStatus.ts`, `connectionGlobalStatus.ts` [memory for exact text], `query.contribution.ts`, [settings-list](https://learn.microsoft.com/en-us/azure-data-studio/settings-list), [release notes](https://learn.microsoft.com/en-us/azure-data-studio/release-notes-azure-data-studio).

* **Status bar (right side, priority 100)**: connection status item (`server : database` for the active editor, click to change/connect; updates when `USE db` runs — fixed 1.45), **language flavor** ("MSSQL"; "Choose SQL Language" when unset; click to change provider), **Executing query...** while running, **time elapsed** timer (ticks every second, stops at completion; 1.42 fixed it running on), **N rows** total across result sets (also rows-affected for DML), and the **selection summary** aggregates (§4.1). Left side is VS Code's (branch, problems, notifications bell, feedback smiley).
* **Tab titles**: untitled editors are `SQLQuery_1`, `SQLQuery_2`, …; with `queryEditor.showConnectionInfoInTitle` the tab reads `SQLQuery_1 - server.database (user (SPID))` — SPID/session ID appended since 1.48; Edit Data tabs are `Edit Data - schema.table` (shortened 1.47); scripted objects open as `SQLQuery_n` with the connection suffix; `queryEditor.promptToSaveGeneratedFiles` controls save prompts for generated scripts.
* **Tab colouring**: `queryEditor.tabColorMode` = `off` (default) / `border` (top border in the server-group colour) / `fill` (whole tab background); realigned with group colours in 1.44–1.46, and the Servers tree shows a reduced colour block beside each connection (1.45). Status-bar colouring by group was a long-standing open request, never shipped.

---

## 8. Command palette & keyboard shortcuts (defaults, Windows/Linux; Cmd on macOS)

Sources: [keyboard-shortcuts](https://learn.microsoft.com/en-us/azure-data-studio/keyboard-shortcuts), `keyboardQueryActions.ts`, `query.contribution.ts`, extension manifests, [integrated-terminal](https://learn.microsoft.com/en-us/azure-data-studio/integrated-terminal), [tutorial-backup-restore](https://learn.microsoft.com/en-us/azure-data-studio/tutorial-backup-restore-sql-server).

| Action | Key |
|---|---|
| Command Palette | Ctrl+Shift+P (F1) |
| New Query (new SQL editor; connects to current tree selection) | Ctrl+N |
| Open Servers/Connections view | Ctrl+G |
| New Connection dialog | Ctrl+Shift+N? — not fixed; via view toolbar / welcome page [memory] |
| Run Query | F5 (Ctrl+E only via SSMS Keymap extension) |
| Run Current Query (statement under cursor) | Ctrl+F5 |
| Cancel Query | Alt+Pause/Break |
| Display Estimated Execution Plan | Ctrl+L |
| Enable/Disable Actual Execution Plan | Ctrl+M |
| Parse Query | Shift+Alt+P |
| Focus on Current Query | Ctrl+Shift+O (editor) / column sort toggle (grid) |
| Toggle Query Results | Ctrl+Shift+R |
| Toggle focus editor ⇄ results | Ctrl+Shift+F |
| Copy with headers / Copy headers | Ctrl+Shift+C / Ctrl+Shift+H |
| Save results as CSV / JSON / Markdown / Excel / XML | Ctrl+K Ctrl+C / Ctrl+K Ctrl+J / Ctrl+K Ctrl+M / Ctrl+K Ctrl+E / Ctrl+K Ctrl+X |
| Copy Query With Results | Ctrl+K Ctrl+V |
| sp_help / sp_who / sp_lock on selection | Alt+F2 / Ctrl+Shift+1 / Ctrl+Shift+2 |
| IntelliSense trigger | Ctrl+Space |
| Peek Definition / Go to Definition | Alt+F12 / F12 |
| Format Document | Shift+Alt+F |
| Change All Occurrences | Ctrl+F2 |
| Toggle line / block comment | Ctrl+/ , Shift+Alt+A |
| Fold / Unfold | Ctrl+Shift+[ / ] |
| Task History panel | Ctrl+T |
| Import wizard | Ctrl+I |
| Launch Profiler / Start-Stop Profiler | Alt+P / Alt+S |
| Integrated terminal / new terminal | Ctrl+` / Ctrl+Shift+` |
| Settings / Keyboard Shortcuts editor | Ctrl+, / Ctrl+K Ctrl+S |
| Zoom in/out (whole UI) | Ctrl+= / Ctrl+- |
| Full screen / Zen mode | F11 (re-enabled 1.46) / Ctrl+K Z |

Keybindings are editable in the Keyboard Shortcuts editor (search, right-click *Change/Add/Remove Key binding*, *Reset Keybinding to Default*) or `keybindings.json`; keymap *extensions* are unsupported except the SSMS Keymap. Notable palette commands: *SQL Server: Clear Pooled Connections*, *Azure Accounts: Clear Azure Account Token Cache*, *Clear all saved accounts*, *Profiler: Open XEL File*, *Preferences: Open User Snippets*, *Terminal: Run Selected Text in Active Terminal*, *Execution Plan: …* (1.38), *Connections: Collapse All Connections*, *Query History: Enable/Disable Capture*, *Feature Tour* (1.20).

---

## 9. Admin wizards & database tasks

### 9.1 Backup / Restore (built-in, preview-gated, MSSQL on-prem/VM/MI only)
Sources: [tutorial-backup-restore-sql-server](https://learn.microsoft.com/en-us/azure-data-studio/tutorial-backup-restore-sql-server), [release notes](https://learn.microsoft.com/en-us/azure-data-studio/release-notes-azure-data-studio).
* Entry: database dashboard **Tasks** widget (Backup / Restore), database context menu (*Backup Database*, *Restore Database* — added to the DB menu in 1.48).
* **Backup database** dialog: backup name, recovery model (read-only), backup type (Full / Differential / Transaction Log), copy-only checkbox, backup files list (add/remove paths; disk or URL — URL/Azure blob support 1.37 preview), Media options (append/overwrite, new media set + name/description), reliability (verify, checksum, continue on error), compression (default/compress/no compress), encryption (algorithm + certificate/asymmetric key), transaction-log options (truncate / back up tail), retain days [field list partly memory]. Buttons **Backup**, **Script** (generate T-SQL into a new editor), **Cancel**.
* **Restore database** dialog: *Restore from* = Backup file / Database / URL (1.48 also **S3-compatible storage**); backup file path picker (server-side file browser); source database; **Target database** (new name or existing); Backup sets table with checkboxes; Files tab (relocate data/log files, target folders); Options tab (WITH REPLACE, KEEP_REPLICATION, restricted user, recovery state RECOVERY / NORECOVERY / STANDBY with standby file, tail-log backup, close existing connections, prompt before restore) [tab contents partly memory]. Buttons Restore / Script / Cancel.
* Both run as background **Tasks** shown in the **Task History** panel (Ctrl+T; "No task history to display"; badge "{n} in progress tasks"; right-click a finished task > **Script** to see the generated statement; failed tasks show an error dialog).

### 9.2 Import wizard (SQL Server Import extension, GA 1.22)
[sql-server-import-extension](https://learn.microsoft.com/en-us/azure-data-studio/extensions/sql-server-import-extension). Right-click database > **Import wizard** (Ctrl+I). Step 1: server/database dropdowns (pre-filled), **Browse** for `.txt`/`.csv` (later `.json`), table name (auto from file), schema (`dbo`). Step 2: preview of first 50 rows parsed by PROSE (auto delimiter/type detection) with **Create derived column** (example-driven column synthesis with *Preview Transformation*). Step 3: modify columns — name, data type, primary key, allow nulls. Step 4: summary and result; **Import new file** to loop. Uses the PROSE framework (same as SSMS Import Flat File).

### 9.3 Data-tier Application Wizard (SQL Server dacpac extension, GA 1.13)
[sql-server-dacpac-extension](https://learn.microsoft.com/en-us/azure-data-studio/extensions/sql-server-dacpac-extension). Databases folder / database / server > **Data-tier Application Wizard**. Four operations: **Deploy** a `.dacpac` (to new or existing DB, with upgrade), **Extract** a database to `.dacpac` (name/version/location), **Create** a database from a `.bacpac` (import), **Export** schema + data to `.bacpac`. Default save folder `dacFx.defaultSaveLocation`. Uses DacFx via the tools service; runs as a Task (script/monitor in Task History).

### 9.4 Schema Compare (extension, GA 1.13)
[schema-compare-extension](https://learn.microsoft.com/en-us/azure-data-studio/extensions/schema-compare-extension). Database context menu > **Schema Compare** (source pre-filled). Source/Target pickers (…): live database connection, `.dacpac`, or **SQL project**. Toolbar: **Compare**, **Cancel**, **Generate script** (T-SQL / SQLCMD deployment script to editor), **Apply** (push changes to target with confirmation), **Options** (DacFx deployment options + object-type include/exclude list; "Yes" re-compare prompt after changing), **Switch direction**, **Open .scmp / Save .scmp** (comparison file; also openable from the File Explorer context menu, 1.40), include/exclude checkboxes per difference, diff view of selected object's script. Also *Update Project from Database* from the dashboard toolbar with "View changes in Schema Compare".

### 9.5 SQL Server Profiler (extension, preview)
[sql-server-profiler-extension](https://learn.microsoft.com/en-us/azure-data-studio/extensions/sql-server-profiler-extension), `profiler.contribution.ts`. Server > **Launch Profiler** (Alt+P). Built on Extended Events: choose a **session template** — `Standard_OnPrem`, `TSQL_OnPrem` (standalone), `Standard_Azure` (Azure SQL DB; only template there) — or an existing session; name it; **Start/Stop** (Alt+S toggle), **Pause**, **Clear**, autoscroll, **Filter…** (per-column filters, `profiler.filters`), text search (Ctrl+F, 1.47), **view templates** selecting columns: *Standard View* (EventClass, TextData, ApplicationName, NTUserName, LoginName, ClientProcessID, SPID, StartTime, CPU, Reads, Writes, Duration, DatabaseID, DatabaseName, HostName), *TSQL View*, *Tuning View* (adds ObjectType), *TSQL_Locks View*, *TSQL_Duration View*; details pane shows the selected event's T-SQL. **Profiler: Open XEL File** (files up to 1 GB since 1.45, with progress dialog 1.46). Columns resizable (1.47).

### 9.6 SQL Server Agent (extension, preview)
[sql-server-agent-extension](https://learn.microsoft.com/en-us/azure-data-studio/extensions/sql-server-agent-extension). Appears as a **SQL Agent** tab on the server dashboard (Manage) for non-cloud, non-Express MSSQL. Tabs: **Jobs** (grid with name, last run, next run, enabled, status, category, runnable, schedule, last run outcome; per-job history chart; actions *New Job*, *Edit Job*, *Run*, *Stop*, *Delete*, *Refresh*; job dialog with General/Steps/Schedules/Alerts/Notifications pages and a step editor incl. T-SQL/PowerShell/notebook step types), **Notebooks** (schedule a notebook as an agent job — `agent.openNotebookDialog` "Schedule Notebook", template re-upload), **Alerts**, **Operators**, **Proxies** with New/Edit/Delete dialogs [dialog page detail partly memory].

### 9.7 Other database admin surfaces (built-in mssql extension, preview)
* **New Database** dialog (name, owner, collation, recovery model, compatibility level, containment, ledger toggle 1.48).
* **Database Properties** / **Server Properties** read-mostly dialogs (1.46/1.47; General, Files, Filegroups, Options pages [memory]).
* **Attach / Detach Database** dialogs (1.46).
* **User Management** (1.44–1.45): New/Edit **Login** (auth type, password policy/expiration/must-change, default database/language, server roles, user mapping, securables, status), **User** (type: login-mapped / contained password / Entra / no login, default schema, owned schemas, memberships), **Database Role**, **Application Role**, **Server Role**; **Object Properties** for these; **Drop Object**, **Rename Object**.
* **Table Designer** (GA 1.40; DacFx-based; columns, PK/FK/check constraints, indexes incl. columnstore/filtered/included columns, temporal/system-versioning, memory-optimized, graph, computed columns, "Preview Database Updates" + generated script, publish).
* **Central Management Servers** extension (CMS groups from a central server; `cmsConnectionController.ts`), **Server Reports**, **SQL Assessment** (best-practice rules), **Managed Instance Dashboard**, **whoisactive**, **Database Administration Tool Extensions for Windows** (launches SSMS dialogs such as Properties / Generate Scripts from ADS on Windows) — see the [extension list](https://github.com/microsoft/azuredatastudio/wiki/List-of-Extensions).

---

## 10. Shell & everything else

Sources: [what-is-azure-data-studio](https://learn.microsoft.com/en-us/azure-data-studio/what-is-azure-data-studio), [integrated-terminal](https://learn.microsoft.com/en-us/azure-data-studio/integrated-terminal), [sqlshack starting](https://www.sqlshack.com/starting-your-journey-with-azure-data-studio/), [sqlshack dashboards](https://www.sqlshack.com/server-and-database-dashboards-in-azure-data-studio/), [preview-features](https://learn.microsoft.com/en-us/azure-data-studio/preview-features), GitHub README.

* **Welcome page** (Help > Welcome; "Show welcome page on startup"): Start — *New connection*, *New query*, *New notebook*, *Open file*, *New deployment* (Deploy SQL Server 2017/2019/2022 to Windows/container/Azure VM/Azure SQL DB, Big Data Cluster, Arc), *Help*; *Recent* files/folders; *Useful links*; *Extensions* (top picks); "Feature Tour" walkthrough (1.20); first-launch toast to enable preview features ("Yes (recommended)"), telemetry notice; 1.51 added retirement banner and notification.
* **Activity bar**: Connections (Servers tree + the Azure resource tree), Explorer (files/folders/workspaces, open editors), Search (Ctrl+Shift+F across files), Source Control (Git built in), Notebooks/Jupyter Books, Extensions (curated ADS gallery + VSIX install; marketplace filtered to ADS-compatible extensions), Azure accounts; bottom-left **Manage** gear (Settings, Keyboard Shortcuts, Color Theme, Check for Updates, Extensions, Command Palette).
* **Panel** (bottom): Terminal, Problems, Output (channels per extension/tools service log), Debug Console, Query History, Tasks (Task History), Notebook variables.
* **Integrated terminal**: Ctrl+`; multiple instances (+ / trash / rename), shell configurable (`terminal.integrated.shell.*`, `shellArgs`, font settings, `commandsToSkipShell`), *Run Selected Text / Run Active File*, find (Ctrl+F), external "Open in Terminal" fallback; commonly used to run sqlcmd, bcp, az/azdata, PowerShell (PowerShell extension bundled with kernel support). Nov 2022 hotfix patched an .exe execution vulnerability.
* **Files & workspaces**: standard VS Code file explorer; ADS 1.25 introduced *Workspaces* for SQL projects (`.code-workspace`), "Open Folder", multi-root, Local History timeline (VS Code 1.66+), `files.hotExit` restores unsaved query tabs across restarts (backups under `%APPDATA%\azuredatastudio\Backups`), auto-save modes, `.sql` files default to the MSSQL flavor, generated scripts are untitled unless `promptToSaveGeneratedFiles`.
* **Themes**: Default Dark Azure Data Studio (default), Default Light Azure Data Studio, High Contrast, plus VS Code/third-party themes (Atom One Dark, Palenight, HCQ); `workbench.colorCustomizations` supports ADS-specific tokens (e.g. `queryEditor.nullBackground`, `resultsErrorColor`); dashboard colour detection accepts hex (1.39).
* **Manage dashboard** (server/database "home"): **Tasks** widget (server: New Query, New Notebook, Restore, Configure/Deploy..., extension tasks such as Import wizard, Data-tier wizard, Schema Compare; database: Backup, Restore, New Query, Import wizard, Update Project from Database, …), server **properties** block (version, edition, computer name, OS), **Backup status** and **Database size** insight widgets, **Search** widget listing tables/views/procs with the Edit Data / Script as / Select Top 1000 context menu, extension tabs (SQL Agent, Server Reports, Assessment, Managed Instance, Profiler). Widgets configurable via `dashboard.server.widgets` / `dashboard.database.widgets` and "Create Insight" from the chart viewer (detail left to the dashboard agent).
* **Notebooks** (SQL kernel using the same query runner, results as grids/charts, PowerShell/Python/Kusto/.NET Interactive kernels), **SQL Database Projects**, **Query Plan Viewer** (built in since 1.35, GA 1.40) — covered by the other report.
* **Localization**: 10 language packs; **Accessibility**: screen-reader labels, keyboard navigation and colour-contrast fixes in nearly every release.
* **Updates/diagnostics**: in-app update notifications (moved to Download Center 1.48.1), `Help > Toggle Developer Tools`, *Show Log File* (`mssql.showLogFile`), log retention settings, telemetry opt-out via `telemetry.telemetryLevel`.
* **Command-line/URI** launch (§1.3), XCopy-able zip installs alongside MSI/DMG/DEB/RPM/TAR; native ARM64 builds for Windows and macOS (1.41/1.42); RHEL 8 glibc known issue from 1.48 onward.
* **Source code**: GitHub `microsoft/azuredatastudio` under a source-available EULA (no redistribution); tools service MIT; last release 1.52 (Jun 2025).

---

### Quick gap list a replacement should note (things ADS never had or shipped late)
* No Results-to-Text / Results-to-File, no client statistics or live query stats, no spatial viewer, no Query Options dialog (only settings), no Registered Servers/multi-server query, no Generate Scripts wizard, no Activity Monitor, no error-log viewer, no XEvent management UI beyond Profiler, no Always On/replication/Service Broker/Policy tooling, no database diagrams. ([what-is-azure-data-studio feature matrix](https://learn.microsoft.com/en-us/azure-data-studio/what-is-azure-data-studio))
* Object Explorer filtering, results-grid filter/sort, SPID on tabs, Excel export options, attach/detach and user management arrived only in 1.30–1.48 and mostly behind the preview flag.
* No Settings Sync; tab colouring by server group is opt-in and status bar never coloured; SQLCMD stayed preview with partial command support and no highlighting.
