# Cobalt SQL Works — Architecture (V1)

*2026-09-16 · derived from `product_design.md` and `decisions/`*

## 1. Workspace layout

```
sqlworks/
├─ Cargo.toml                 workspace; shared [workspace.dependencies]; profiles
├─ crates/
│  ├─ cobalt-core/            plain types shared by everything (no egui, no tokio)
│  ├─ cobalt-driver/          Driver/Connection/QueryStream traits + TDS→Arrow mapping; feature "mssql" = tiberius-ng impl
│  ├─ cobalt-auth/            Entra PKCE + device code + az CLI; keyring storage
│  ├─ cobalt-store/           rusqlite: profiles, groups, history, tab snapshots, catalog cache, settings migration
│  ├─ cobalt-sql/             T-SQL lexer, batch/statement splitter, completion engine, formatter wrapper
│  ├─ cobalt-results/         ResultSet: Arrow batches + spill + sort/filter index + row access API
│  ├─ cobalt-export/          csv/tsv/json/jsonl/xml/markdown/xlsx/parquet/arrow-ipc writers over ResultSet
│  ├─ cobalt-export-delta/    deltalake writer (isolated: heavy deps)
│  ├─ cobalt-plan/            showplan XML → PlanModel; layered layout (positions), summaries
│  ├─ cobalt-app/             the binary: egui/eframe UI, session manager, egui_agent wiring
│  └─ cobalt-cli/             (V1.x) headless runner for "export whole query" and agent-less scripting
├─ assets/                    fonts, icons (SVG → egui textures), themes
├─ tests/                     integration tests against Docker SQL Server (env-gated)
└─ docs/
```

Dependency direction: `core ← {driver, auth, store, sql, results, plan} ← {export, export-delta}
← app`. Nothing below `app` depends on egui. Nothing below `driver` depends on tokio.

## 2. Key crates and versions (pinned at workspace level)

| Concern | Crate | Version |
|---|---|---|
| UI | egui, eframe (wgpu default; `glow` feature), egui_extras | 0.36 |
| Grid | egui_table | 0.10 |
| Docking/tabs | egui_dock | 0.21 |
| Tree | egui_ltreeview | 0.9 (or hand-rolled if it fights lazy loading) |
| Toasts | egui-notify | 0.23 |
| Icons | egui-phosphor | 0.14 |
| Agent | egui_agent (git tag v0.3.0; features pipe, accesskit-introspection, screenshot) | 0.3.0 |
| TDS | tiberius-ng (lib name `tiberius`; rustls, tds80, chrono, rust_decimal) | 0.13 |
| Async | tokio (rt-multi-thread, net, io-util, time, sync) | 1.53 |
| Auth | oauth2 (PKCE), azure_identity (az CLI), reqwest (rustls), keyring | latest / 1.0 / 4.x |
| Arrow | arrow, arrow-ipc, parquet — **pinned to the version deltalake 0.32 requires** (arrow 58 at time of writing; verify at build) | see build |
| Delta | deltalake (features: rustls, datafusion off) | 0.32 |
| Excel | rust_xlsxwriter | 0.9x |
| SQL parse | sqlparser (MsSqlDialect), sqlformat | 0.63 / 0.5 |
| XML | quick-xml | 0.41 |
| Local DB | rusqlite (bundled) | 0.40 |
| Paths | directories | 6 |
| Logging | tracing, tracing-subscriber, tracing-appender | 0.1 |
| Dialogs | rfd | 0.17 |
| Clipboard | arboard (via egui) | — |
| Misc | serde, serde_json, toml, thiserror, anyhow, chrono, rust_decimal, uuid, parking_lot, crossbeam-channel, memmap2, tempfile, open | — |

## 3. Threading model

```
┌──────────────┐  Command enum   ┌────────────────────┐  tokio tasks   ┌───────────┐
│ UI thread    │ ──────────────▶ │ SessionManager     │ ─────────────▶ │ tiberius  │
│ (egui frame) │ ◀────────────── │ (tokio runtime,    │ ◀───────────── │ conn/stream│
│              │  Event enum via │  background thread)│                └───────────┘
│              │  crossbeam +    └────────────────────┘
│              │  ctx.request_repaint()
└──────────────┘
```

- One multi-thread tokio runtime owned by `SessionManager`, created at startup on its own thread.
- The UI sends `Command`s (`Connect`, `RunBatch`, `Cancel`, `ExpandNode`, `Export`, …) over an
  unbounded channel. Every command carries a `RequestId`.
- The runtime sends `Event`s back (`Connected`, `ResultSetStarted{schema}`, `RowsAppended{batch}`,
  `Message`, `BatchDone`, `RunDone`, `Error`, `NodeChildren`, `ExportProgress`, …) over a
  crossbeam channel and calls `ctx.request_repaint()` after each push. The UI drains the channel
  at the top of each frame.
- Rows are converted to Arrow **on the runtime thread** in chunks (default 4,096 rows) and sent
  as `RecordBatch` (`Arc`, cheap to move). The UI never touches driver types.
- Cancellation: `Cancel{tab}` → `SessionManager` calls the driver's cancel (attention packet) and
  drops the stream; the UI marks the run "cancelling" until `RunDone{cancelled: true}`.
- Exports run as tokio blocking tasks over a cloned `Arc<ResultSet>`; progress events stream
  back; cancellation via a `CancellationToken`.
- Metadata (object explorer) uses a separate connection per server (`ConnectionRole::Metadata`)
  so it never queues behind a user query.

## 4. Data model

### 4.1 cobalt-core
```rust
pub struct ProfileId(Uuid); pub struct GroupId(Uuid);
pub struct ConnectionProfile { id, name: Option<String>, server: String, port: Option<u16>,
    database: Option<String>, auth: AuthMethod, group: Option<GroupId>, color: Option<Color>,
    options: ConnectionOptions, read_only_guard: bool }
pub enum AuthMethod { SqlLogin{user, password: SecretRef}, EntraInteractive{tenant: Option<String>, account_hint},
    EntraDeviceCode{..}, AzureCli, WindowsIntegrated, ServicePrincipal{..} }
pub struct ConnectionOptions { encrypt: Encrypt, trust_server_certificate: bool, host_name_in_certificate,
    application_name, connect_timeout, command_timeout, application_intent, mars, packet_size }
pub struct ServerGroup { id, name, color, parent: Option<GroupId>, order }
pub enum EngineKind { SqlServer{major}, AzureSqlDb, AzureSqlMi, Synapse, FabricWarehouse, FabricSqlEndpoint, FabricSqlDb, Unknown }
pub struct EngineInfo { kind, version: String, edition: String, capabilities: Capabilities }
pub struct Capabilities { actual_plans: bool, procedures: bool, sequences: bool, transactions: bool, read_only: bool, … }
// catalog
pub struct ObjectRef { database, schema, name, kind: ObjectKind }
pub enum ObjectKind { Table, View, Procedure, ScalarFunction, TableFunction, Synonym, Sequence, TableType, Schema }
pub struct ColumnInfo { name, sql_type: SqlType, nullable, ordinal, is_identity, is_computed, in_pk }
pub enum SqlType { Bit, TinyInt, SmallInt, Int, BigInt, Decimal{p,s}, Numeric{p,s}, Money, SmallMoney, Float, Real,
    Date, Time{s}, DateTime, DateTime2{s}, SmallDateTime, DateTimeOffset{s}, Char{n}, VarChar{n}, NChar{n}, NVarChar{n},
    Text, NText, Binary{n}, VarBinary{n}, Image, UniqueIdentifier, Xml, SqlVariant, Geography, Geometry, HierarchyId, Json, Vector, Other(String) }
```

### 4.2 cobalt-driver
```rust
#[async_trait] pub trait Driver: Send + Sync {
    async fn connect(&self, profile: &ConnectionProfile, creds: &ResolvedCredentials, role: ConnectionRole) -> Result<Box<dyn Connection>>;
}
#[async_trait] pub trait Connection: Send {
    fn engine(&self) -> &EngineInfo;
    fn spid(&self) -> Option<i32>;
    async fn execute(&mut self, sql: &str, opts: &ExecOptions) -> Result<QueryStream>;   // one batch
    async fn cancel_handle(&self) -> CancelHandle;                                        // clonable, usable from another task
    async fn change_database(&mut self, db: &str) -> Result<()>;
    async fn ping(&mut self) -> Result<()>;
    fn catalog(&mut self) -> Box<dyn CatalogReader + '_>;                                 // list databases/schemas/objects/columns/params/definition
}
pub enum StreamItem { ResultSetStart{ columns: Vec<ColumnInfo> }, Rows(RecordBatch), Message(ServerMessage),
    RowsAffected(u64), ResultSetEnd, Error(ServerError) }
pub struct QueryStream { /* impl Stream<Item = StreamItem> */ }
```
`mssql` module: tiberius-ng implementation, `tds_to_arrow` (per-column builders keyed by `SqlType`),
`catalog` queries against `sys.*` / `INFORMATION_SCHEMA` (with edition-specific variants for Fabric),
`scripting` (Script as Create for tables/views/procs/functions using `OBJECT_DEFINITION` and
sys.columns/indexes/keys for tables).

### 4.3 cobalt-results
```rust
pub struct ResultSet { schema: SchemaRef, columns: Vec<ColumnInfo>, chunks: RwLock<Vec<Chunk>>, total_rows: AtomicUsize,
    state: RunState, spill: SpillPolicy, view: RwLock<ViewIndex> }
enum Chunk { Mem(RecordBatch), Spilled{ file: PathBuf, rows: usize, mmap: OnceCell<Mmap> } }
pub struct ViewIndex { order: Option<Arc<Vec<u32>>>, filter: Option<Arc<Vec<u32>>>, sort: Vec<SortKey>, filters: Vec<ColumnFilter> }
impl ResultSet { fn row_count(&self) -> usize; fn visible_count(&self) -> usize;
    fn cell(&self, visible_row: usize, col: usize) -> CellRef<'_>;   // borrows into the batch; formats lazily
    fn slice_for_export(&self) -> impl Iterator<Item = RecordBatch>; fn sort(&self, keys); fn filter(&self, f); fn distinct_values(&self, col, limit) }
```
Cell formatting is done by `CellFormatter` (settings-aware: null text, date format, bit style).
Sorting/filtering build a `u32` index over global row ids in a blocking task; the grid reads
through the index.

### 4.4 cobalt-store (SQLite schema, v1)
```
groups(id, name, color, parent_id, sort)             profiles(id, group_id, name, server, port, database, auth_json, options_json, color, read_only, created, last_used)
history(id, profile_id, server, database, sql, started, ended, duration_ms, rows, status, error, tab_id, starred)   + FTS5 virtual table on sql
tab_snapshots(tab_id, profile_id, title, text, cursor, updated)   -- hot exit + "restore closed"
catalog_cache(profile_id, database, json, refreshed)
kv(key, value)                                        -- window geometry, layout, misc
```
Passwords/refresh tokens never go in SQLite; `SecretRef` points at a `keyring` entry
(`service = "cobalt-sqlworks"`, `user = "<profile id>:<kind>"`).

### 4.5 cobalt-sql
- `lexer`: hand-written T-SQL tokenizer → `Token{kind, span}`; kinds: Keyword, Function, Identifier,
  BracketIdent, QuotedIdent, String, Number, Comment, Variable(@x), TempTable(#t), Operator,
  Punct, Whitespace. Incremental per-line cache keyed by line hash for the highlighter.
- `batches`: split on `GO` lines (with count); `statements`: split within a batch on `;` and
  statement-start keywords with a paren/string/comment-aware scanner; `statement_at(offset)`.
- `completion`: context detection (after FROM/JOIN → objects; after `alias.` → columns; after
  SELECT/WHERE → columns of statement tables + functions + keywords); alias map from a light
  regex/paren scan, `sqlparser` when it parses; ranking by prefix + kind.
- `format`: `sqlformat` with Cobalt defaults (uppercase keywords, 4-space indent).

### 4.6 cobalt-plan
- `parse(xml) -> Plan { statements: Vec<Statement { text, cost, nodes: Vec<Node>, root, warnings, missing_indexes } > }`
  with `Node { id, physical_op, logical_op, object, est_rows, est_cost, subtree_cost, actual_rows, actual_time_ms,
  actual_executions, rows_read, parallel, warnings, properties: Vec<(String, String)>, children }`.
- `layout(&Statement) -> Layout { positions: HashMap<NodeId, Rect>, edges }` — right-to-left layered
  tree (Reingold–Tilford-style), spacing constants from the theme.
- `icons`: `PhysicalOp → IconId` mapping.

## 5. App structure (cobalt-app)

```
src/
  main.rs               eframe setup, tokio thread, egui_agent wrap, tracing init, panic hook
  app.rs                CobaltApp { state, sessions, store, settings, theme, layout, toasts }; eframe::App impl; event drain
  state/                AppState: connections tree model, tabs (Vec<EditorTab>), active tab, dialogs, palette
  session/              SessionManager (tokio side): connections map, run loop, catalog fetch, export tasks
  ui/
    shell.rs            menu bar, sidebar strip, dock layout, status bar
    servers.rs          tree view, context menus, connection dialog trigger
    connection_dialog.rs
    editor/             text editor widget (TextEdit + layouter), toolbar, completion popup, find/replace
    results/            grid (egui_table), header filter popup, cell viewer, messages pane, result tabs, fetch-more bar
    plan/               canvas painter, properties pane, top ops
    history.rs
    settings.rs
    palette.rs
    theme.rs            Cobalt Light / Dark visuals + tokens; plan/grid colors
  agent.rs              AgentApp impl: dispatch verbs (connect, open_query, run, results, export, …)
  keymap.rs             default ADS/SSMS bindings; palette registry
```

Editor: `egui::TextEdit::multiline` with a custom `layouter` fed by `cobalt-sql::lexer` and a
per-tab highlight cache; gutter drawn separately (line numbers, statement-timing later). This
is the egui demo's "code editor" pattern, hardened. Completion popup is an `egui::Area` anchored
at the cursor galley position.

Grid: `egui_table::Table` with a `TableDelegate` reading `ResultSet::cell`. Column widths kept
in tab state. Selection state in tab state; painting via `ui.painter().rect_filled` under cells.

## 6. Agent wiring

```rust
#[cfg(feature = "agent")] {
  let control = egui_agent::Control::spawn(egui_agent::Config {
      transport: cfg!(windows).then(|| Transport::Pipe("cobalt.agent".into())).unwrap_or(Transport::UnixSocket(runtime_dir.join("cobalt.agent.sock"))),
      enabled: std::env::var("COBALT_AGENT").is_ok(), ..Default::default() });
  control.bind_context(cc.egui_ctx.clone());
  Box::new(egui_agent::wrap(app, control))
}
```
`impl AgentApp for CobaltApp { fn dispatch(..) }` routes the verbs in `product_design.md` §4.10 to
the same functions the UI calls; results are returned as JSON (`ActionResult::with`). Important
widgets use `egui_agent::widgets::*` helpers with stable ids (`servers.new_connection`,
`editor.run`, `grid.copy_headers`, …). `#[derive(Snapshot)]` on `AppState` summary structs.

## 7. Build, test, run

- `cargo build` (dev, opt-level 1 for deps) · `cargo run -p cobalt-app` · `COBALT_AGENT=1 cargo run`.
- Unit tests per crate; `cobalt-sql` and `cobalt-plan` have fixture corpora (`tests/fixtures/*.sql`, `*.sqlplan`).
- Integration: `tests/` gated on `COBALT_TEST_MSSQL=server;user;password` — runs against the
  Docker container (`cobalt-mssql`, sa) with a seeded database (`tests/seed.sql`: types table,
  1M-row table generated via cross join, procs with PRINT/RAISERROR/multiple result sets, JSON/XML columns).
- UI: `egui_agent::TestHarness` tests for connect → run → grid flows (in-process, no display).
- Release: `cargo build --release --no-default-features` (agent compiled out), `cargo-dist` for
  Windows MSI + Linux tar/AppImage; macOS later.

## 8. Milestones for the build

| # | Milestone | Proves |
|---|---|---|
| M0 | Workspace skeleton, core types, eframe window with theme + egui_agent + palette; CI build | Toolchain and egui 0.36 + egui_agent 0.3.0 compile together |
| M1 | `cobalt-driver` mssql: connect (SQL auth), execute, stream, cancel, messages, TDS→Arrow for all types; integration tests green | Driver works |
| M2 | `cobalt-store` + connection library UI + servers tree with lazy catalog | Connect/browse |
| M3 | Editor (highlight, run keys, DB dropdown) + results grid (stream, cap, select, copy, sort, filter, viewers) + messages | The core loop |
| M4 | Exports (all formats incl. Parquet/Arrow/Delta) + history | Take the data somewhere |
| M5 | Plan viewer (parse, layout, canvas, properties, top ops, .sqlplan) | Plans |
| M6 | Entra auth (PKCE, device code, az CLI) + Fabric capability detection + Windows auth | Fabric-ready |
| M7 | Completion, format, snippets, settings UI, keymap, hot exit, ADS import, polish, Linux build, installers | Alpha |
