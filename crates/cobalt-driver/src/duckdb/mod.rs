//! DuckDB behind the same `Driver` / `Connection` traits as SQL Server and SQLite, over the
//! `cobalt-duckdb` binding (the official shared library, downloaded on first use). One thread
//! per connection owns the `duckdb_connection`; `execute` streams DuckDB's Arrow chunks, cast
//! to the grid's types (nested values as text), through a bounded channel. Cancel is
//! `duckdb_interrupt`. The catalog comes from DuckDB's own `duckdb_*()` table functions.

use crate::*;
use arrow::record_batch::RecordBatch;
use async_trait::async_trait;
use cobalt_core::*;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The DuckDB driver: `root` is where the shared library lives (downloaded on first connect).
pub struct DuckDbDriver {
    pub root: PathBuf,
}

#[async_trait]
impl Driver for DuckDbDriver {
    fn name(&self) -> &'static str {
        "DuckDB"
    }

    async fn connect(&self, profile: &ConnectionProfile, _creds: &ResolvedCredentials, role: ConnectionRole) -> Result<Box<dyn Connection>> {
        let root = self.root.clone();
        let profile = profile.clone();
        let c = tokio::task::spawn_blocking(move || DuckDbConnection::open(&root, &profile, role)).await.map_err(|e| DriverError::Other(e.to_string()))??;
        Ok(Box::new(c))
    }
}

/// Make sure the library is on disk, downloading it when it is not (the first DuckDB
/// connection on a machine; about 20 MB).
pub fn ensure_library(root: &Path) -> std::result::Result<(), String> {
    if cobalt_duckdb::is_installed(root) {
        return Ok(());
    }
    tracing::info!(version = cobalt_duckdb::VERSION, "downloading the DuckDB library");
    let mut last = 0u64;
    cobalt_duckdb::install(root, &mut |done, total| {
        if done - last > 4_000_000 || Some(done) == total {
            tracing::info!(done, ?total, "DuckDB download");
            last = done;
        }
    })
    .map(|_| ())
}

type Job = Box<dyn FnOnce(&mut cobalt_duckdb::Connection) + Send>;

pub struct DuckDbConnection {
    jobs: std::sync::mpsc::Sender<Job>,
    engine: EngineInfo,
    profile: ProfileId,
    role: ConnectionRole,
    database: String,
    cancel: CancelHandle,
    usable: bool,
}

pub fn is_duckdb_file(path: &Path) -> bool {
    cobalt_duckdb::is_duckdb_file(path)
}

fn q(name: &str) -> String {
    cobalt_duckdb::quote(name)
}

fn lit(s: &str) -> String {
    cobalt_duckdb::lit(s)
}

/// `"schema"."name"`, or just `"name"` when the schema is empty / main.
fn qualified(obj: &ObjectRef) -> String {
    if obj.schema.is_empty() || obj.schema == "main" {
        q(&obj.name)
    } else {
        format!("{}.{}", q(&obj.schema), q(&obj.name))
    }
}

fn server_error(message: &str, interrupted: bool) -> ServerMessage {
    // DuckDB names the line in "LINE n:" after the message
    let line = message.split("LINE ").nth(1).and_then(|r| r.split(':').next()).and_then(|n| n.trim().parse::<u32>().ok()).unwrap_or(0);
    let text = if interrupted { "Query was interrupted.".to_string() } else { message.to_string() };
    ServerMessage { number: 0, state: 0, class: 0, message: text, server: None, procedure: None, line, is_error: true }
}

fn info(text: impl Into<String>) -> ServerMessage {
    ServerMessage { number: 0, state: 0, class: 0, message: text.into(), server: None, procedure: None, line: 0, is_error: false }
}

impl DuckDbConnection {
    fn open(root: &Path, profile: &ConnectionProfile, role: ConnectionRole) -> Result<Self> {
        let path = profile.server.trim().to_string();
        if path.is_empty() {
            return Err(DriverError::Connect("no database file given".into()));
        }
        let p = Path::new(&path);
        let exists = p.exists();
        if !exists && !profile.options.create_if_missing {
            return Err(DriverError::Connect(format!("{path} does not exist")));
        }
        ensure_library(root).map_err(|e| DriverError::Connect(format!("the DuckDB engine could not be installed: {e}")))?;
        let read_only = profile.options.read_only || (exists && std::fs::metadata(p).map(|m| m.permissions().readonly()).unwrap_or(false));
        let db = cobalt_duckdb::open(root, p, read_only).map_err(DriverError::Connect)?;
        let conn = cobalt_duckdb::Connection::new(&db).map_err(DriverError::Connect)?;
        let version = conn.version().trim_start_matches('v').to_string();
        let interrupter = conn.interrupter();
        let (tx, rx) = std::sync::mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("duckdb-conn".into())
            .spawn(move || {
                let mut conn = conn;
                while let Ok(job) = rx.recv() {
                    job(&mut conn);
                }
            })
            .map_err(|e| DriverError::Other(e.to_string()))?;
        let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.clone());
        let mut capabilities = EngineKind::DuckDb.capabilities();
        capabilities.read_only = read_only;
        let engine = EngineInfo { kind: EngineKind::DuckDb, version: version.clone(), product_level: String::new(), edition: if read_only { "read-only".into() } else { "read-write".into() }, server_name: name, version_text: format!("DuckDB {version}"), capabilities };
        let cancel = CancelHandle::new(move || interrupter.interrupt());
        // the file's own database name is the catalog DuckDB reports for it
        let mut this = Self { jobs: tx, engine, profile: profile.id, role, database: String::new(), cancel, usable: true };
        this.database = this.current_database_blocking().unwrap_or_else(|| "main".into());
        Ok(this)
    }

    fn current_database_blocking(&self) -> Option<String> {
        let (rtx, rrx) = std::sync::mpsc::channel();
        self.jobs
            .send(Box::new(move |c| {
                let v = c.query("SELECT current_database()").ok().and_then(|(_, b)| b.first().and_then(|b| b.column(0).as_any().downcast_ref::<arrow::array::StringArray>().map(|a| a.value(0).to_string())));
                let _ = rtx.send(v);
            }))
            .ok()?;
        rrx.recv().ok().flatten()
    }

    async fn call<R: Send + 'static>(&self, f: impl FnOnce(&mut cobalt_duckdb::Connection) -> R + Send + 'static) -> Result<R> {
        let (rtx, rrx) = tokio::sync::oneshot::channel();
        self.jobs
            .send(Box::new(move |c| {
                let _ = rtx.send(f(c));
            }))
            .map_err(|_| DriverError::Disconnected("the DuckDB worker is gone".into()))?;
        rrx.await.map_err(|_| DriverError::Disconnected("the DuckDB worker is gone".into()))
    }

    /// Run a query and read its rows as text columns (the catalog queries).
    async fn rows(&self, sql: String) -> Result<Vec<Vec<Option<String>>>> {
        self.call(move |c| c.query(&sql).map(|(_, batches)| text_rows(&batches))).await?.map_err(|e| DriverError::Server(server_error(&e, false)))
    }
}

/// Every cell as text (None for NULL), batch by batch.
fn text_rows(batches: &[RecordBatch]) -> Vec<Vec<Option<String>>> {
    use arrow::util::display::{ArrayFormatter, FormatOptions};
    let opts = FormatOptions::default().with_display_error(true);
    let mut out = Vec::new();
    for b in batches {
        let fs: Vec<Option<ArrayFormatter<'_>>> = b.columns().iter().map(|a| ArrayFormatter::try_new(a.as_ref(), &opts).ok()).collect();
        for r in 0..b.num_rows() {
            out.push(b.columns().iter().zip(&fs).map(|(a, f)| if a.is_null(r) { None } else { f.as_ref().map(|f| f.value(r).to_string()) }).collect());
        }
    }
    out
}

fn cell(row: &[Option<String>], i: usize) -> String {
    row.get(i).cloned().flatten().unwrap_or_default()
}

/// Run a script on the connection thread, streaming to the session.
fn run_script(c: &mut cobalt_duckdb::Connection, sql: &str, plan: PlanMode, tx: &tokio::sync::mpsc::Sender<StreamItem>, cancel: &CancelHandle) {
    let send = |item: StreamItem| tx.blocking_send(item).is_ok();
    // EXPLAIN [ANALYZE] in front of every statement for the plan modes
    let script = match plan {
        PlanMode::None => sql.to_string(),
        other => {
            let prefix = if matches!(other, PlanMode::Actual) { "EXPLAIN ANALYZE " } else { "EXPLAIN " };
            let spans = cobalt_sql::statements::statements(sql);
            if spans.is_empty() {
                format!("{prefix}{sql}")
            } else {
                spans.iter().map(|s| format!("{prefix}{};", sql[s.start..s.end].trim().trim_end_matches(';'))).collect::<Vec<_>>().join("\n")
            }
        }
    };
    let mut cols: Vec<ColumnInfo> = Vec::new();
    let mut is_plan_set = false;
    let mut first_error: Option<ServerMessage> = None;
    let mut cancelled = false;
    c.run(&script, &mut |item| match item {
        cobalt_duckdb::Item::Start { schema } => {
            cols = cobalt_duckdb::columns_for(&schema);
            is_plan_set = plan != PlanMode::None && cols.len() == 2 && cols[0].name == "explain_key";
            send(StreamItem::ResultSetStart { columns: cols.clone() })
        }
        cobalt_duckdb::Item::Batch(b) => {
            if is_plan_set {
                // the plan text as a message too, where the Messages pane shows it whole
                for r in text_rows(std::slice::from_ref(&b)) {
                    let text = format!("{}\n{}", cell(&r, 0), cell(&r, 1));
                    if !send(StreamItem::Message(ServerMessage { procedure: Some("plan".into()), ..info(text) })) {
                        return false;
                    }
                }
            }
            match cobalt_duckdb::conform_to_columns(b, &cols) {
                Ok(b) => send(StreamItem::Rows(b)),
                Err(e) => {
                    let m = server_error(&format!("could not convert the rows: {e}"), false);
                    send(StreamItem::Message(m.clone()));
                    first_error = Some(m);
                    false
                }
            }
        }
        cobalt_duckdb::Item::End { rows } => send(StreamItem::ResultSetEnd { rows }),
        cobalt_duckdb::Item::Changed(n) => send(StreamItem::RowsAffected(n)),
        cobalt_duckdb::Item::Nothing => true,
        cobalt_duckdb::Item::Error { message, interrupted } => {
            let m = server_error(&message, interrupted);
            cancelled |= interrupted;
            send(StreamItem::Message(m.clone()));
            first_error = Some(m);
            false
        }
    });
    send(StreamItem::Done { error: first_error, cancelled: cancelled || cancel.is_cancelled() });
}

#[async_trait]
impl Connection for DuckDbConnection {
    fn engine(&self) -> &EngineInfo {
        &self.engine
    }
    fn spid(&self) -> Option<i32> {
        None
    }
    fn current_database(&self) -> &str {
        &self.database
    }
    fn profile_id(&self) -> ProfileId {
        self.profile
    }
    fn role(&self) -> ConnectionRole {
        self.role
    }

    async fn execute<'a>(&'a mut self, sql: &str, opts: &ExecOptions) -> Result<QueryStream<'a>> {
        let (tx, rx) = tokio::sync::mpsc::channel::<StreamItem>(4);
        let sql = sql.to_string();
        let plan = opts.plan;
        self.cancel.reset();
        let cancel = self.cancel.clone();
        self.jobs.send(Box::new(move |c| run_script(c, &sql, plan, &tx, &cancel))).map_err(|_| DriverError::Disconnected("the DuckDB worker is gone".into()))?;
        Ok(Box::pin(futures_util::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|i| (i, rx)) })))
    }

    fn cancel_handle(&self) -> CancelHandle {
        self.cancel.clone()
    }

    async fn bulk_insert(&mut self, table: &str, columns: &[ColumnInfo], rx: std::sync::mpsc::Receiver<std::result::Result<RecordBatch, String>>, progress: &mut (dyn FnMut(u64) -> bool + Send)) -> Result<u64> {
        // the batches become an Arrow view; one INSERT … SELECT moves them, DuckDB casting to the table's types
        if !progress(0) {
            return Err(DriverError::Cancelled);
        }
        let table = table.to_string();
        let names: Vec<String> = columns.iter().map(|c| c.name.clone()).collect();
        let fields: Vec<arrow::datatypes::Field> = columns.iter().map(|c| arrow::datatypes::Field::new(&c.name, c.sql_type.arrow_type(), true)).collect();
        let schema = Arc::new(arrow::datatypes::Schema::new(fields));
        let counted = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let c2 = Arc::clone(&counted);
        let s2 = Arc::clone(&schema);
        let reader = arrow::array::RecordBatchIterator::new(
            std::iter::from_fn(move || rx.recv().ok()).map(move |b| {
                let b = b.map_err(|e| arrow::error::ArrowError::ExternalError(e.into()))?;
                c2.fetch_add(b.num_rows() as u64, std::sync::atomic::Ordering::Relaxed);
                cobalt_duckdb::conform(b, &s2).map_err(|e| arrow::error::ArrowError::ExternalError(e.into()))
            }),
            schema,
        );
        let r = self
            .call(move |c| -> std::result::Result<(), String> {
                let view = "__cobalt_import";
                c.register_arrow_view(view, Box::new(reader))?;
                let cols = names.iter().map(|n| q(n)).collect::<Vec<_>>().join(", ");
                let r = c.execute(&format!("INSERT INTO {table} ({cols}) SELECT {cols} FROM {}", q(view))).map(|_| ());
                c.drop_arrow_view(view);
                r
            })
            .await?;
        r.map_err(|e| if e.contains("INTERRUPT") { DriverError::Cancelled } else { DriverError::Server(server_error(&e, false)) })?;
        let rows = counted.load(std::sync::atomic::Ordering::Relaxed);
        if !progress(rows) {
            return Err(DriverError::Cancelled);
        }
        Ok(rows)
    }

    async fn change_database(&mut self, database: &str) -> Result<()> {
        let db = database.to_string();
        let sql = format!("USE {}", q(&db));
        self.call(move |c| c.execute(&sql)).await?.map_err(|e| DriverError::Server(server_error(&e, false)))?;
        self.database = db;
        Ok(())
    }

    async fn ping(&mut self) -> Result<()> {
        self.call(|c| c.execute("SELECT 1").map(|_| ())).await?.map_err(DriverError::Disconnected)
    }

    fn is_usable(&self) -> bool {
        self.usable
    }

    async fn close(&mut self) -> Result<()> {
        self.usable = false;
        Ok(())
    }

    async fn list_databases(&mut self) -> Result<Vec<DatabaseInfo>> {
        let ro = self.engine.capabilities.read_only;
        let rows = self.rows("SELECT database_name, readonly FROM duckdb_databases() WHERE NOT internal ORDER BY database_name".into()).await?;
        Ok(rows.into_iter().map(|r| DatabaseInfo { name: cell(&r, 0), is_system: false, state: "online".into(), is_read_only: ro || cell(&r, 1) == "true" }).collect())
    }

    async fn list_schemas(&mut self, database: &str) -> Result<Vec<String>> {
        let rows = self.rows(format!("SELECT schema_name FROM duckdb_schemas() WHERE database_name = {} AND NOT internal ORDER BY schema_name", lit(database))).await?;
        Ok(rows.into_iter().map(|r| cell(&r, 0)).collect())
    }

    async fn list_objects(&mut self, database: &str) -> Result<Vec<ObjectRef>> {
        let sql = format!(
            "SELECT schema_name, table_name, 'table', table_oid FROM duckdb_tables() WHERE database_name = {d} AND NOT internal UNION ALL SELECT schema_name, view_name, 'view', view_oid FROM duckdb_views() WHERE database_name = {d} AND NOT internal ORDER BY 1, 3, 2",
            d = lit(database)
        );
        let rows = self.rows(sql).await?;
        Ok(rows
            .into_iter()
            .map(|r| {
                let kind = if cell(&r, 2) == "view" { ObjectKind::View } else { ObjectKind::Table };
                let schema = cell(&r, 0);
                ObjectRef { database: database.to_string(), schema: if schema == "main" { String::new() } else { schema }, name: cell(&r, 1), kind, object_id: cell(&r, 3).parse::<i64>().ok().map(|v| v as i32) }
            })
            .collect())
    }

    async fn list_columns(&mut self, obj: &ObjectRef) -> Result<Vec<ColumnInfo>> {
        let schema = if obj.schema.is_empty() { "main".to_string() } else { obj.schema.clone() };
        let sql = format!(
            "SELECT column_name, data_type, is_nullable, column_default, column_index FROM duckdb_columns() WHERE schema_name = {} AND table_name = {} ORDER BY column_index",
            lit(&schema),
            lit(&obj.name)
        );
        let rows = self.rows(sql).await?;
        let mut cols: Vec<ColumnInfo> = rows
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let mut c = ColumnInfo::new(cell(r, 0), cobalt_duckdb::sql_type_from_duckdb(&cell(r, 1)), cell(r, 2) != "false", i);
                c.default_definition = r.get(3).cloned().flatten();
                c
            })
            .collect();
        let pk = self.rows(format!("SELECT unnest(constraint_column_names) FROM duckdb_constraints() WHERE schema_name = {} AND table_name = {} AND constraint_type = 'PRIMARY KEY'", lit(&schema), lit(&obj.name))).await.unwrap_or_default();
        for r in pk {
            let n = cell(&r, 0);
            if let Some(c) = cols.iter_mut().find(|c| c.name.eq_ignore_ascii_case(&n)) {
                c.in_primary_key = true;
            }
        }
        Ok(cols)
    }

    async fn list_parameters(&mut self, _obj: &ObjectRef) -> Result<Vec<ParameterInfo>> {
        Ok(Vec::new())
    }

    async fn list_indexes(&mut self, obj: &ObjectRef) -> Result<Vec<IndexInfo>> {
        let schema = if obj.schema.is_empty() { "main".to_string() } else { obj.schema.clone() };
        let rows = self.rows(format!("SELECT index_name, is_unique, is_primary, expressions, sql FROM duckdb_indexes() WHERE schema_name = {} AND table_name = {} ORDER BY index_name", lit(&schema), lit(&obj.name))).await?;
        Ok(rows
            .into_iter()
            .map(|r| {
                let exprs = cell(&r, 3);
                let cols: Vec<(String, bool)> = exprs.trim_matches(['[', ']']).split(',').map(|s| (s.trim().trim_matches(['\'', '"']).to_string(), false)).filter(|(s, _)| !s.is_empty()).collect();
                IndexInfo { name: cell(&r, 0), is_unique: cell(&r, 1) == "true", is_clustered: false, is_primary_key: cell(&r, 2) == "true", is_unique_constraint: false, index_type: "art".into(), key_columns: cols, included_columns: Vec::new(), filter: None }
            })
            .collect())
    }

    async fn list_keys(&mut self, obj: &ObjectRef) -> Result<Vec<KeyInfo>> {
        let schema = if obj.schema.is_empty() { "main".to_string() } else { obj.schema.clone() };
        let rows = self.rows(format!("SELECT constraint_type, constraint_column_names, referenced_table, referenced_column_names, constraint_text FROM duckdb_constraints() WHERE schema_name = {} AND table_name = {} AND constraint_type IN ('PRIMARY KEY', 'FOREIGN KEY', 'UNIQUE')", lit(&schema), lit(&obj.name))).await?;
        let list = |s: String| -> Vec<String> { s.trim_matches(['[', ']']).split(',').map(|x| x.trim().trim_matches(['\'', '"']).to_string()).filter(|x| !x.is_empty()).collect() };
        let mut out = Vec::new();
        for (i, r) in rows.into_iter().enumerate() {
            let kind = match cell(&r, 0).as_str() {
                "PRIMARY KEY" => KeyKind::PrimaryKey,
                "FOREIGN KEY" => KeyKind::ForeignKey,
                _ => KeyKind::Unique,
            };
            let columns = list(cell(&r, 1));
            let references = if kind == KeyKind::ForeignKey {
                Some((ObjectRef { database: obj.database.clone(), schema: obj.schema.clone(), name: cell(&r, 2), kind: ObjectKind::Table, object_id: None }, list(cell(&r, 3))))
            } else {
                None
            };
            out.push(KeyInfo { name: format!("{}_{}", cell(&r, 0).replace(' ', "_"), i + 1), kind, columns, references, definition: Some(cell(&r, 4)) });
        }
        Ok(out)
    }

    async fn load_catalog(&mut self, database: &str) -> Result<DatabaseCatalog> {
        let objects = self.list_objects(database).await?;
        let schemas = self.list_schemas(database).await.unwrap_or_default();
        let mut cat = DatabaseCatalog { database: database.to_string(), schemas: schemas.into_iter().filter(|s| s != "main").collect(), objects: objects.clone(), columns: Default::default(), refreshed_at: Some(chrono::Utc::now()) };
        // one pass over duckdb_columns for every object
        let rows = self.rows(format!("SELECT schema_name, table_name, column_name, data_type, is_nullable FROM duckdb_columns() WHERE database_name = {} AND NOT internal ORDER BY schema_name, table_name, column_index", lit(database))).await.unwrap_or_default();
        for r in rows {
            let schema = cell(&r, 0);
            let schema_key = if schema == "main" { String::new() } else { schema };
            let name = cell(&r, 1);
            if let Some(o) = objects.iter().find(|o| o.name == name && o.schema == schema_key) {
                if let Some(id) = o.object_id {
                    let cols = cat.columns.entry(id).or_default();
                    let ord = cols.len();
                    cols.push(ColumnInfo::new(cell(&r, 2), cobalt_duckdb::sql_type_from_duckdb(&cell(&r, 3)), cell(&r, 4) != "false", ord));
                }
            }
        }
        Ok(cat)
    }

    async fn script(&mut self, obj: &ObjectRef, kind: ScriptKind) -> Result<String> {
        let schema = if obj.schema.is_empty() { "main".to_string() } else { obj.schema.clone() };
        match kind {
            ScriptKind::Create => {
                let sql = if obj.kind == ObjectKind::View {
                    format!("SELECT sql FROM duckdb_views() WHERE schema_name = {} AND view_name = {}", lit(&schema), lit(&obj.name))
                } else {
                    format!("SELECT sql FROM duckdb_tables() WHERE schema_name = {} AND table_name = {}", lit(&schema), lit(&obj.name))
                };
                let rows = self.rows(sql).await?;
                let text = rows.first().map(|r| cell(r, 0)).unwrap_or_default();
                Ok(if text.is_empty() { format!("-- no definition for {}\n", qualified(obj)) } else { format!("{};\n", text.trim_end_matches(';')) })
            }
            ScriptKind::Drop => Ok(format!("DROP {} {};\n", if obj.kind == ObjectKind::View { "VIEW" } else { "TABLE" }, qualified(obj))),
            ScriptKind::Select => {
                let cols = self.list_columns(obj).await?;
                let list = if cols.is_empty() { "*".to_string() } else { cols.iter().map(|c| format!("    {}", q(&c.name))).collect::<Vec<_>>().join(",\n") };
                Ok(format!("SELECT\n{list}\nFROM {}\nLIMIT 1000;\n", qualified(obj)))
            }
            _ => Err(DriverError::Unsupported("script kind")),
        }
    }

    async fn table_stats(&mut self, obj: &ObjectRef) -> Result<TableStats> {
        let rows = self.rows(format!("SELECT count(*) FROM {}", qualified(obj))).await?;
        let schema = if obj.schema.is_empty() { "main".to_string() } else { obj.schema.clone() };
        let size = self.rows(format!("SELECT estimated_size FROM duckdb_tables() WHERE schema_name = {} AND table_name = {}", lit(&schema), lit(&obj.name))).await.unwrap_or_default();
        let _ = size;
        Ok(TableStats { rows: rows.first().map(|r| cell(r, 0)).and_then(|s| s.parse().ok()).unwrap_or(0), reserved_bytes: 0 })
    }

    async fn distinct_values(&mut self, obj: &ObjectRef, column: &str, limit: usize) -> Result<Vec<String>> {
        let rows = self.rows(format!("SELECT DISTINCT CAST({c} AS VARCHAR) FROM {t} WHERE {c} IS NOT NULL ORDER BY 1 LIMIT {}", limit.max(1), c = q(column), t = qualified(obj))).await?;
        Ok(rows.into_iter().map(|r| cell(&r, 0)).collect())
    }
}
