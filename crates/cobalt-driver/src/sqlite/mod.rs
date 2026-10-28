//! SQLite over rusqlite, behind the same `Driver` / `Connection` traits as SQL Server.
//!
//! rusqlite is synchronous, so every connection owns a thread that holds the `rusqlite::Connection`
//! and runs jobs sent over a channel; `execute` streams Arrow batches back through a bounded
//! channel, so the session runtime never blocks on the database. Cancellation is
//! `sqlite3_interrupt` through rusqlite's `InterruptHandle`.
//!
//! Result columns are typed by the declared affinity of the column when the statement names
//! one (`INTEGER` → Int64, `REAL` → Float64, `BLOB` → Binary, anything else → Utf8) and by the
//! first batch's storage classes for expressions; a value of another storage class is coerced
//! when it parses and otherwise becomes NULL, and the Messages pane says how many.

use crate::*;
use arrow::record_batch::RecordBatch;
use async_trait::async_trait;
use cobalt_core::*;
use rusqlite::fallible_iterator::FallibleIterator;
use rusqlite::{Batch, InterruptHandle, OpenFlags};
use std::path::Path;
use std::time::Duration;

pub struct SqliteDriver;

#[async_trait]
impl Driver for SqliteDriver {
    fn name(&self) -> &'static str {
        "SQLite"
    }

    async fn connect(&self, profile: &ConnectionProfile, _creds: &ResolvedCredentials, role: ConnectionRole) -> Result<Box<dyn Connection>> {
        let c = SqliteConnection::open(profile, role)?;
        Ok(Box::new(c))
    }
}

type Job = Box<dyn FnOnce(&mut rusqlite::Connection) + Send>;

pub struct SqliteConnection {
    jobs: std::sync::mpsc::Sender<Job>,
    engine: EngineInfo,
    profile: ProfileId,
    role: ConnectionRole,
    database: String,
    cancel: CancelHandle,
    usable: bool,
}

/// `true` when the first 16 bytes are SQLite's header.
pub fn is_sqlite_file(path: &Path) -> bool {
    cobalt_sqlite::is_sqlite_file(path)
}

fn other(e: impl std::fmt::Display) -> DriverError {
    DriverError::Other(e.to_string())
}

fn server_error(e: &rusqlite::Error) -> ServerMessage {
    let (number, text, line) = match e {
        rusqlite::Error::SqliteFailure(code, msg) => (code.extended_code, msg.clone().unwrap_or_else(|| code.to_string()), 0),
        // a parse / prepare error names the offending token: the message alone, with its line
        rusqlite::Error::SqlInputError { error, msg, sql, offset } => (error.extended_code, msg.clone(), sql.get(..(*offset as usize).min(sql.len())).map(|s| s.matches('\n').count() as u32 + 1).unwrap_or(0)),
        other => (0, other.to_string(), 0),
    };
    ServerMessage { number, state: 0, class: 0, message: text, server: None, procedure: None, line, is_error: true }
}

fn info(text: impl Into<String>) -> ServerMessage {
    ServerMessage { number: 0, state: 0, class: 0, message: text.into(), server: None, procedure: None, line: 0, is_error: false }
}

pub use cobalt_sqlite::read::{affinity, arrow_schema, column_types, declared_types, read_row, storage_class_type, to_batch};

/// `EXPLAIN QUERY PLAN` rows (id, parent, notused, detail) as an indented tree.
fn plan_tree(rows: &[(i64, i64, String)]) -> String {
    fn depth(id: i64, rows: &[(i64, i64, String)]) -> usize {
        let mut d = 0;
        let mut cur = id;
        while let Some((_, parent, _)) = rows.iter().find(|(i, _, _)| *i == cur) {
            if *parent == 0 {
                break;
            }
            d += 1;
            cur = *parent;
            if d > 64 {
                break;
            }
        }
        d
    }
    let mut out = String::from("QUERY PLAN\n");
    for (id, _, detail) in rows {
        out.push_str(&"   ".repeat(depth(*id, rows)));
        out.push_str("|--");
        out.push_str(detail);
        out.push('\n');
    }
    out
}

/// Run every statement of `sql` on the worker thread, streaming items into `tx`.
fn run_batch(conn: &rusqlite::Connection, sql: &str, batch_rows: usize, plan: bool, tx: &tokio::sync::mpsc::Sender<StreamItem>, cancel: &CancelHandle) {
    let send = |item: StreamItem| tx.blocking_send(item).is_ok();
    let mut first_error: Option<ServerMessage> = None;
    let autocommit_at_start = conn.is_autocommit();
    let mut batch = Batch::new(conn, sql);
    loop {
        let stmt = match batch.next() {
            Ok(Some(s)) => s,
            Ok(None) => break,
            Err(e) => {
                let m = server_error(&e);
                send(StreamItem::Message(m.clone()));
                first_error = Some(m);
                break;
            }
        };
        let mut stmt = stmt;
        if plan {
            // the plan of this statement instead of its rows
            let text = stmt.expanded_sql().unwrap_or_default();
            drop(stmt);
            match conn.prepare(&format!("EXPLAIN QUERY PLAN {text}")) {
                Ok(mut p) => {
                    let rows: Vec<(i64, i64, String)> = p
                        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(3)?)))
                        .and_then(|it| it.collect::<rusqlite::Result<Vec<_>>>())
                        .unwrap_or_default();
                    send(StreamItem::Message(info(plan_tree(&rows))));
                    let cols = vec![ColumnInfo::new("id", SqlType::BigInt, false, 0), ColumnInfo::new("parent", SqlType::BigInt, false, 1), ColumnInfo::new("detail", SqlType::NVarChar { len: None }, false, 2)];
                    send(StreamItem::ResultSetStart { columns: cols.clone() });
                    let values: Vec<Vec<rusqlite::types::Value>> = rows.iter().map(|(a, b, d)| vec![rusqlite::types::Value::Integer(*a), rusqlite::types::Value::Integer(*b), rusqlite::types::Value::Text(d.clone())]).collect();
                    let mut misfits = vec![0u64; 3];
                    if let Ok(b) = to_batch(&values, &cols, &mut misfits) {
                        send(StreamItem::Rows(b));
                    }
                    send(StreamItem::ResultSetEnd { rows: values.len() as u64 });
                }
                Err(e) => {
                    let m = server_error(&e);
                    send(StreamItem::Message(m.clone()));
                    first_error = Some(m);
                    break;
                }
            }
            continue;
        }
        let ncols = stmt.column_count();
        if ncols == 0 {
            match stmt.execute([]) {
                Ok(n) => {
                    send(StreamItem::RowsAffected(n as u64));
                }
                Err(e) => {
                    let m = server_error(&e);
                    send(StreamItem::Message(m.clone()));
                    first_error = Some(m);
                    break;
                }
            }
            continue;
        }
        let names: Vec<String> = (0..ncols).map(|i| stmt.column_name(i).map(|s| s.to_string()).unwrap_or_else(|_| format!("column{}", i + 1))).collect();
        let decls = declared_types(&stmt);
        let mut rows_iter = match stmt.query([]) {
            Ok(r) => r,
            Err(e) => {
                let m = server_error(&e);
                send(StreamItem::Message(m.clone()));
                first_error = Some(m);
                break;
            }
        };
        // the first chunk decides the types of untyped columns; every chunk becomes one batch
        let mut chunk: Vec<Vec<rusqlite::types::Value>> = Vec::with_capacity(batch_rows);
        let mut cols: Option<Vec<ColumnInfo>> = None;
        let mut misfits: Vec<u64> = vec![0; ncols];
        let mut total: u64 = 0;
        let mut failed = false;
        let stop;
        loop {
            let step = rows_iter.next();
            let flush_and_end = match step {
                Ok(Some(row)) => {
                    chunk.push(read_row(row, ncols));
                    if chunk.len() < batch_rows {
                        continue;
                    }
                    false
                }
                Ok(None) => true,
                Err(e) => {
                    let m = server_error(&e);
                    send(StreamItem::Message(m.clone()));
                    first_error = Some(m);
                    failed = true;
                    true
                }
            };
            if cols.is_none() {
                let types = column_types(&decls, &chunk);
                let c: Vec<ColumnInfo> = names.iter().zip(types).enumerate().map(|(i, (n, t))| ColumnInfo::new(n, t, true, i)).collect();
                send(StreamItem::ResultSetStart { columns: c.clone() });
                cols = Some(c);
            }
            if !chunk.is_empty() {
                match to_batch(&chunk, cols.as_ref().unwrap(), &mut misfits) {
                    Ok(b) => {
                        total += b.num_rows() as u64;
                        if !send(StreamItem::Rows(b)) {
                            return;
                        }
                    }
                    Err(e) => {
                        let m = ServerMessage { number: 0, state: 0, class: 0, message: e, server: None, procedure: None, line: 0, is_error: true };
                        send(StreamItem::Message(m.clone()));
                        first_error = Some(m);
                        failed = true;
                    }
                }
                chunk.clear();
            }
            if flush_and_end || failed || cancel.is_cancelled() {
                stop = failed || cancel.is_cancelled();
                break;
            }
        }
        let cols = cols.unwrap_or_default();
        for (i, n) in misfits.iter().enumerate() {
            if *n > 0 {
                send(StreamItem::Message(info(format!("{n} value{} in column {} did not fit its declared type {} and show as NULL", if *n == 1 { "" } else { "s" }, cols[i].name, cols[i].sql_type))));
            }
        }
        send(StreamItem::ResultSetEnd { rows: total });
        if stop {
            break;
        }
    }
    // a transaction the batch itself opened (BEGIN … error) is rolled back, so the next batch
    // starts clean and nothing half-done stays behind
    if first_error.is_some() && autocommit_at_start && !conn.is_autocommit() && conn.execute_batch("ROLLBACK").is_ok() {
        send(StreamItem::Message(info("The transaction was rolled back.")));
    }
    send(StreamItem::Done { error: first_error, cancelled: cancel.is_cancelled() });
}

impl SqliteConnection {
    fn open(profile: &ConnectionProfile, role: ConnectionRole) -> Result<Self> {
        let path = profile.server.trim().to_string();
        if path.is_empty() {
            return Err(DriverError::Connect("no database file given".into()));
        }
        let exists = Path::new(&path).exists();
        if !exists && !profile.options.create_if_missing {
            return Err(DriverError::Connect(format!("{path} does not exist")));
        }
        let read_only = profile.options.read_only || (exists && std::fs::metadata(&path).map(|m| m.permissions().readonly()).unwrap_or(false));
        let flags = if read_only { OpenFlags::SQLITE_OPEN_READ_ONLY } else { OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE } | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let conn = rusqlite::Connection::open_with_flags(&path, flags).map_err(|e| DriverError::Connect(e.to_string()))?;
        conn.busy_timeout(Duration::from_secs(5)).map_err(other)?;
        let version = rusqlite::version().to_string();
        let interrupt: InterruptHandle = conn.get_interrupt_handle();
        let (tx, rx) = std::sync::mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("sqlite-conn".into())
            .spawn(move || {
                let mut conn = conn;
                while let Ok(job) = rx.recv() {
                    job(&mut conn);
                }
            })
            .map_err(other)?;
        let name = Path::new(&path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.clone());
        let mut capabilities = EngineKind::Sqlite.capabilities();
        capabilities.read_only = read_only;
        let engine = EngineInfo { kind: EngineKind::Sqlite, version: version.clone(), product_level: String::new(), edition: if read_only { "read-only".into() } else { "read-write".into() }, server_name: name, version_text: format!("SQLite {version}"), capabilities };
        let cancel = CancelHandle::new(move || interrupt.interrupt());
        Ok(Self { jobs: tx, engine, profile: profile.id, role, database: "main".into(), cancel, usable: true })
    }

    async fn call<R: Send + 'static>(&self, f: impl FnOnce(&mut rusqlite::Connection) -> R + Send + 'static) -> Result<R> {
        let (rtx, rrx) = tokio::sync::oneshot::channel();
        self.jobs
            .send(Box::new(move |c| {
                let _ = rtx.send(f(c));
            }))
            .map_err(|_| DriverError::Disconnected("the SQLite worker is gone".into()))?;
        rrx.await.map_err(|_| DriverError::Disconnected("the SQLite worker is gone".into()))
    }

    async fn query_rows<T: Send + 'static>(&self, sql: String, f: impl Fn(&rusqlite::Row<'_>) -> rusqlite::Result<T> + Send + 'static) -> Result<Vec<T>> {
        self.call(move |c| -> rusqlite::Result<Vec<T>> {
            let mut st = c.prepare(&sql)?;
            let it = st.query_map([], |r| f(r))?;
            it.collect()
        })
        .await?
        .map_err(|e| DriverError::Server(server_error(&e)))
    }
}

fn q(name: &str) -> String {
    cobalt_sqlite::quote(name)
}

#[async_trait]
impl Connection for SqliteConnection {
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
        let batch_rows = opts.batch_rows.clamp(256, 65_536);
        let plan = matches!(opts.plan, PlanMode::Estimated);
        self.cancel.reset();
        let cancel = self.cancel.clone();
        self.jobs.send(Box::new(move |c| run_batch(&*c, &sql, batch_rows, plan, &tx, &cancel))).map_err(|_| DriverError::Disconnected("the SQLite worker is gone".into()))?;
        Ok(Box::pin(futures_util::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|i| (i, rx)) })))
    }

    fn cancel_handle(&self) -> CancelHandle {
        self.cancel.clone()
    }

    async fn bulk_insert(&mut self, table: &str, columns: &[ColumnInfo], rx: std::sync::mpsc::Receiver<std::result::Result<RecordBatch, String>>, progress: &mut (dyn FnMut(u64) -> bool + Send)) -> Result<u64> {
        // `table` arrives quoted by the caller (`"name"`); the insert names the columns
        let insert = format!("INSERT INTO {table} ({}) VALUES ({})", columns.iter().map(|c| q(&c.name)).collect::<Vec<_>>().join(", "), columns.iter().map(|_| "?").collect::<Vec<_>>().join(", "));
        let mut rows = 0u64;
        if !progress(0) {
            return Err(DriverError::Cancelled);
        }
        loop {
            let batch = match rx.recv() {
                Ok(Ok(b)) => b,
                Ok(Err(e)) => return Err(DriverError::Other(e)),
                Err(_) => break,
            };
            let sql = insert.clone();
            let n = self.call(move |c| cobalt_sqlite::insert_batch(c, &sql, &batch)).await?.map_err(|e| DriverError::Server(server_error(&e)))?;
            rows += n;
            if !progress(rows) {
                return Err(DriverError::Cancelled);
            }
        }
        Ok(rows)
    }

    async fn change_database(&mut self, database: &str) -> Result<()> {
        // attached databases are addressed by name in SQL; "main" is the file
        self.database = database.to_string();
        Ok(())
    }

    async fn ping(&mut self) -> Result<()> {
        self.call(|c| c.query_row("SELECT 1", [], |_| Ok(())).map_err(|e| e.to_string())).await?.map_err(DriverError::Disconnected)
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
        self.query_rows("PRAGMA database_list".into(), move |r| Ok(DatabaseInfo { name: r.get::<_, String>(1)?, is_system: false, state: "online".into(), is_read_only: ro })).await
    }

    async fn list_schemas(&mut self, _database: &str) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    async fn list_objects(&mut self, database: &str) -> Result<Vec<ObjectRef>> {
        let db = database.to_string();
        let schema_table = if db.is_empty() || db == "main" { "sqlite_schema".to_string() } else { format!("{}.sqlite_schema", q(&db)) };
        let db2 = db.clone();
        self.query_rows(format!("SELECT rowid, type, name FROM {schema_table} WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite_%' ORDER BY type, name"), move |r| {
            let kind = if r.get::<_, String>(1)? == "view" { ObjectKind::View } else { ObjectKind::Table };
            Ok(ObjectRef { database: db2.clone(), schema: String::new(), name: r.get(2)?, kind, object_id: Some(r.get::<_, i64>(0)? as i32) })
        })
        .await
    }

    async fn list_columns(&mut self, obj: &ObjectRef) -> Result<Vec<ColumnInfo>> {
        let name = obj.name.clone();
        self.query_rows(format!("PRAGMA table_xinfo({})", q(&name)), |r| {
            let cid: i64 = r.get(0)?;
            let decl: Option<String> = r.get(2)?;
            let notnull: i64 = r.get(3)?;
            let dflt: Option<String> = r.get(4)?;
            let pk: i64 = r.get(5)?;
            let hidden: i64 = r.get(6)?;
            let mut c = ColumnInfo::new(r.get::<_, String>(1)?, affinity(decl.as_deref()), notnull == 0, cid.max(0) as usize);
            c.in_primary_key = pk > 0;
            c.default_definition = dflt;
            c.is_computed = hidden == 2 || hidden == 3;
            Ok(c)
        })
        .await
    }

    async fn list_parameters(&mut self, _obj: &ObjectRef) -> Result<Vec<ParameterInfo>> {
        Ok(Vec::new())
    }

    async fn list_indexes(&mut self, obj: &ObjectRef) -> Result<Vec<IndexInfo>> {
        let name = obj.name.clone();
        let heads: Vec<(String, bool, String)> = self.query_rows(format!("PRAGMA index_list({})", q(&name)), |r| Ok((r.get::<_, String>(1)?, r.get::<_, i64>(2)? != 0, r.get::<_, String>(3)?))).await?;
        let mut out = Vec::new();
        for (iname, unique, origin) in heads {
            let cols: Vec<(String, bool)> = self.query_rows(format!("PRAGMA index_xinfo({})", q(&iname)), |r| Ok((r.get::<_, Option<String>>(2)?.unwrap_or_else(|| "rowid".into()), r.get::<_, i64>(3)? != 0))).await?;
            out.push(IndexInfo { name: iname, is_unique: unique, is_clustered: false, is_primary_key: origin == "pk", is_unique_constraint: origin == "u", index_type: "btree".into(), key_columns: cols, included_columns: Vec::new(), filter: None });
        }
        Ok(out)
    }

    async fn list_keys(&mut self, obj: &ObjectRef) -> Result<Vec<KeyInfo>> {
        let cols = self.list_columns(obj).await?;
        let mut out = Vec::new();
        let pk: Vec<String> = cols.iter().filter(|c| c.in_primary_key).map(|c| c.name.clone()).collect();
        if !pk.is_empty() {
            out.push(KeyInfo { name: "PRIMARY KEY".into(), kind: KeyKind::PrimaryKey, columns: pk, references: None, definition: None });
        }
        let name = obj.name.clone();
        let fks: Vec<(i64, String, String, String)> = self.query_rows(format!("PRAGMA foreign_key_list({})", q(&name)), |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?, r.get::<_, Option<String>>(4)?.unwrap_or_default()))).await?;
        let mut by_id: std::collections::BTreeMap<i64, (String, Vec<String>, Vec<String>)> = Default::default();
        for (id, table, from, to) in fks {
            let e = by_id.entry(id).or_insert_with(|| (table, Vec::new(), Vec::new()));
            e.1.push(from);
            e.2.push(to);
        }
        for (id, (table, from, to)) in by_id {
            let target = ObjectRef { database: obj.database.clone(), schema: String::new(), name: table, kind: ObjectKind::Table, object_id: None };
            out.push(KeyInfo { name: format!("FK_{id}"), kind: KeyKind::ForeignKey, columns: from, references: Some((target, to)), definition: None });
        }
        Ok(out)
    }

    async fn load_catalog(&mut self, database: &str) -> Result<DatabaseCatalog> {
        let objects = self.list_objects(database).await?;
        let mut cat = DatabaseCatalog { database: database.to_string(), schemas: Vec::new(), objects: objects.clone(), columns: Default::default(), refreshed_at: Some(chrono::Utc::now()) };
        for o in &objects {
            if let Some(id) = o.object_id {
                if let Ok(cols) = self.list_columns(o).await {
                    cat.columns.insert(id, cols);
                }
            }
        }
        Ok(cat)
    }

    async fn script(&mut self, obj: &ObjectRef, kind: ScriptKind) -> Result<String> {
        let name = obj.name.clone();
        match kind {
            ScriptKind::Create => {
                let rows: Vec<String> = self.query_rows(format!("SELECT sql FROM sqlite_schema WHERE sql IS NOT NULL AND (name = {n} OR tbl_name = {n}) ORDER BY CASE type WHEN 'table' THEN 0 WHEN 'view' THEN 0 WHEN 'index' THEN 1 ELSE 2 END, rowid", n = format!("'{}'", name.replace('\'', "''"))), |r| r.get::<_, String>(0)).await?;
                Ok(rows.into_iter().map(|s| format!("{s};")).collect::<Vec<_>>().join("\n\n") + "\n")
            }
            ScriptKind::Drop => Ok(format!("DROP {} {};\n", if obj.kind == ObjectKind::View { "VIEW" } else { "TABLE" }, q(&name))),
            ScriptKind::Select => {
                let cols = self.list_columns(obj).await?;
                let list = if cols.is_empty() { "*".to_string() } else { cols.iter().map(|c| format!("    {}", q(&c.name))).collect::<Vec<_>>().join(",\n") };
                Ok(format!("SELECT\n{list}\nFROM {}\nLIMIT 1000;\n", q(&name)))
            }
            _ => Err(DriverError::Unsupported("script kind")),
        }
    }

    async fn table_stats(&mut self, obj: &ObjectRef) -> Result<TableStats> {
        let name = obj.name.clone();
        let rows: Vec<i64> = self.query_rows(format!("SELECT count(*) FROM {}", q(&name)), |r| r.get(0)).await?;
        let bytes: Vec<i64> = self.query_rows("SELECT page_count * page_size FROM pragma_page_count(), pragma_page_size()".into(), |r| r.get(0)).await.unwrap_or_default();
        Ok(TableStats { rows: rows.first().copied().unwrap_or(0).max(0) as u64, reserved_bytes: bytes.first().copied().unwrap_or(0).max(0) as u64 })
    }

    async fn distinct_values(&mut self, obj: &ObjectRef, column: &str, limit: usize) -> Result<Vec<String>> {
        let (t, c) = (obj.name.clone(), column.to_string());
        self.query_rows(format!("SELECT DISTINCT CAST({c} AS TEXT) FROM {t} WHERE {c} IS NOT NULL ORDER BY 1 LIMIT {}", limit.max(1), c = q(&c), t = q(&t)), |r| r.get::<_, String>(0)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dir(std::path::PathBuf);
    impl Dir {
        fn new() -> Self {
            let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0);
            let p = std::env::temp_dir().join(format!("cobalt-sqlite-test-{}-{n}", std::process::id()));
            std::fs::create_dir_all(&p).unwrap();
            Dir(p)
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn fixture() -> (Dir, rusqlite::Connection) {
        let dir = Dir::new();
        let conn = rusqlite::Connection::open(dir.0.join("t.sqlite")).unwrap();
        conn.execute_batch("CREATE TABLE t (id INTEGER PRIMARY KEY, amount REAL, name TEXT, payload BLOB, untyped); INSERT INTO t VALUES (1, 1.5, 'a', x'00ff', 7), (2, 'twelve', 'b', NULL, 'x');").unwrap();
        (dir, conn)
    }

    #[test]
    fn declared_types_drive_the_columns() {
        let (_d, conn) = fixture();
        let mut st = conn.prepare("SELECT id, amount, name, payload, untyped, id + 1 AS expr FROM t").unwrap();
        let decl: Vec<Option<String>> = st.columns().iter().map(|c| c.decl_type().map(str::to_string)).collect();
        assert_eq!(decl[0].as_deref(), Some("INTEGER"), "{decl:?}");
        let rows: Vec<Vec<rusqlite::types::Value>> = st.query_map([], |r| Ok(read_row(r, 6))).unwrap().collect::<rusqlite::Result<_>>().unwrap();
        let types = column_types(&declared_types(&st), &rows);
        assert_eq!(types[0], SqlType::BigInt);
        assert_eq!(types[1], SqlType::Float);
        assert_eq!(types[2], SqlType::NVarChar { len: None });
        assert_eq!(types[3], SqlType::VarBinary { len: None });
        assert_eq!(types[4], SqlType::BigInt, "an untyped column takes the first storage class seen");
        assert_eq!(types[5], SqlType::BigInt, "an expression takes the first storage class");
        let cols: Vec<ColumnInfo> = ["id", "amount", "name", "payload", "untyped", "expr"].iter().zip(types).enumerate().map(|(i, (n, t))| ColumnInfo::new(*n, t, true, i)).collect();
        let mut misfits = vec![0u64; 6];
        let b = to_batch(&rows, &cols, &mut misfits).unwrap();
        assert_eq!(b.num_rows(), 2);
        assert_eq!(misfits[1], 1, "the text does not fit REAL");
        assert!(b.column(1).is_null(1));
        assert_eq!(misfits[4], 1, "the text does not fit the integer the column started with");
    }

    #[test]
    fn run_batch_streams_sets_messages_and_done() {
        let (_d, conn) = fixture();
        let (tx, mut rx) = tokio::sync::mpsc::channel::<StreamItem>(64);
        let cancel = CancelHandle::noop();
        run_batch(&conn, "SELECT id, name FROM t ORDER BY id; UPDATE t SET name = 'z' WHERE id = 1; SELECT * FROM nope;", 1000, false, &tx, &cancel);
        drop(tx);
        let mut items = Vec::new();
        while let Some(i) = rx.blocking_recv() {
            items.push(i);
        }
        assert!(matches!(items[0], StreamItem::ResultSetStart { ref columns } if columns.len() == 2 && columns[0].sql_type == SqlType::BigInt), "{:?}", items.first().map(|i| format!("{i:?}").chars().take(120).collect::<String>()));
        assert!(matches!(items[1], StreamItem::Rows(ref b) if b.num_rows() == 2));
        assert!(matches!(items[2], StreamItem::ResultSetEnd { rows: 2 }));
        assert!(matches!(items[3], StreamItem::RowsAffected(1)));
        assert!(matches!(items[4], StreamItem::Message(ref m) if m.is_error && m.message.contains("nope")));
        assert!(matches!(items.last(), Some(StreamItem::Done { error: Some(_), cancelled: false })));
    }

    #[test]
    fn plan_is_a_tree() {
        let (_d, conn) = fixture();
        let (tx, mut rx) = tokio::sync::mpsc::channel::<StreamItem>(64);
        run_batch(&conn, "SELECT * FROM t WHERE id = 1", 1000, true, &tx, &CancelHandle::noop());
        drop(tx);
        let mut text = String::new();
        while let Some(i) = rx.blocking_recv() {
            if let StreamItem::Message(m) = i {
                text = m.message;
            }
        }
        assert!(text.starts_with("QUERY PLAN"), "{text}");
        assert!(text.contains("SEARCH t USING INTEGER PRIMARY KEY"), "{text}");
    }
}
