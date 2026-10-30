//! The engine: one DataFusion `SessionContext` for the app, its memory pool sized by Cobalt's
//! budget and its spill directory, and the registry of local tables (results of open tabs,
//! later files, scratch and lakehouse tables).

use crate::provider::ResultSetProvider;
use arrow::array::RecordBatch;
use arrow::datatypes::{DataType, SchemaRef, TimeUnit};
use cobalt_core::{ColumnInfo, SqlType};
use cobalt_results::{MemoryBudget, ResultSet};
use datafusion::dataframe::DataFrame;
use datafusion::execution::memory_pool::FairSpillPool;
use datafusion::execution::runtime_env::RuntimeEnvBuilder;
use datafusion::prelude::{SessionConfig, SessionContext};
use parking_lot::Mutex;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The DataFusion version in the binary (for the status bar).
pub const ENGINE_VERSION: &str = "53";

/// Where a local table's data comes from.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TableSource {
    /// A result set of an open tab (`tab` = the tab id, `set` = the set index in its run).
    Result { tab: String, set: usize, title: String },
    /// A file (Parquet, CSV, JSON lines, Arrow) or a Delta folder, read on every scan.
    File { path: PathBuf },
    /// A table a statement made (`CREATE TABLE … AS`, `CREATE EXTERNAL TABLE`).
    Statement,
    /// A scratch table: persisted as Parquet in the app data folder, back next session
    /// (`scratch.<name>`).
    Scratch { path: PathBuf },
    /// A Delta table in a Fabric lakehouse, read from OneLake with the signed-in token.
    OneLake { url: String },
    /// A table or view in a SQLite database file, read from the file on every scan.
    Sqlite { path: PathBuf, table: String },
    /// A table or view in a DuckDB database file, read through DuckDB on every scan.
    DuckDb { path: PathBuf, table: String },
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct TableInfo {
    pub name: String,
    pub source: TableSource,
    pub columns: Vec<ColumnInfo>,
    pub rows: usize,
    /// `false` when the count is not known without a scan (CSV, JSON, Arrow files).
    pub rows_known: bool,
    /// `true` while the producing run is still streaming rows into it.
    pub live: bool,
    /// Bytes the result set holds in memory (result tables; `None` for files and scratch).
    pub memory_bytes: Option<usize>,
    /// Size of the file on disk (Parquet, CSV, scratch tables; `None` for folders and results).
    pub file_bytes: Option<u64>,
}

struct Entry {
    info: TableInfo,
    rs: Option<Arc<ResultSet>>,
}

#[derive(Default)]
struct Registry {
    tables: BTreeMap<String, Entry>,
    /// The name `_` points at (the most recent result).
    latest: Option<String>,
}

/// The schema scratch tables live in.
pub const SCRATCH_SCHEMA: &str = "scratch";
/// Registry names of scratch tables start with this.
pub const SCRATCH_PREFIX: &str = "scratch.";

pub struct Engine {
    ctx: SessionContext,
    registry: Mutex<Registry>,
    budget: Arc<MemoryBudget>,
    spill_dir: PathBuf,
    /// Where scratch tables persist (`<name>.parquet`), once `load_scratch` ran.
    scratch_dir: Mutex<Option<PathBuf>>,
    /// Bumped whenever the registry changes (callers rebuild their catalogs).
    generation: std::sync::atomic::AtomicU64,
}

/// The name `_` resolves to the latest result.
pub const LATEST: &str = "_";

impl Engine {
    /// `budget_bytes` sizes the engine's memory pool (operators spill past it, under `spill_dir`).
    pub fn new(budget: Arc<MemoryBudget>, budget_bytes: usize, spill_dir: PathBuf) -> Result<Self, String> {
        let _ = std::fs::create_dir_all(&spill_dir);
        let rt = RuntimeEnvBuilder::new().with_memory_pool(Arc::new(FairSpillPool::new(budget_bytes.max(64 << 20)))).with_temp_file_path(spill_dir.clone()).build_arc().map_err(|e| e.to_string())?;
        let config = SessionConfig::new().with_batch_size(8192).with_information_schema(true);
        let ctx = SessionContext::new_with_config_rt(config, rt);
        if let Some(cat) = ctx.catalog("datafusion") {
            let _ = cat.register_schema(SCRATCH_SCHEMA, Arc::new(datafusion::catalog::memory::MemorySchemaProvider::new()));
        }
        // delta-rs learns abfss:// (OneLake) once
        deltalake::azure::register_handlers(None);
        Ok(Self { ctx, registry: Mutex::new(Registry::default()), budget, spill_dir, scratch_dir: Mutex::new(None), generation: std::sync::atomic::AtomicU64::new(1) })
    }

    /// Where scratch tables persist; the `.parquet` files there register as `scratch.<name>`.
    pub async fn load_scratch(&self, dir: PathBuf) -> usize {
        let _ = std::fs::create_dir_all(&dir);
        let mut n = 0;
        if let Ok(rd) = std::fs::read_dir(&dir) {
            let mut paths: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().map(|e| e.eq_ignore_ascii_case("parquet")).unwrap_or(false)).collect();
            paths.sort();
            for p in paths {
                let name = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                if name.is_empty() {
                    continue;
                }
                match self.register_scratch_file(&name, &p).await {
                    Ok(_) => n += 1,
                    Err(e) => tracing::warn!(path = %p.display(), "scratch table not loaded: {e}"),
                }
            }
        }
        *self.scratch_dir.lock() = Some(dir);
        self.bump();
        n
    }

    pub fn scratch_dir(&self) -> Option<PathBuf> {
        self.scratch_dir.lock().clone()
    }

    async fn register_scratch_file(&self, name: &str, path: &Path) -> Result<TableInfo, String> {
        let file_bytes = std::fs::metadata(path).ok().map(|m| m.len());
        let qualified = format!("{SCRATCH_SCHEMA}.{}", quote_ident(name));
        let _ = self.ctx.deregister_table(qualified.as_str());
        self.ctx.register_parquet(qualified.as_str(), path.to_string_lossy().replace('\\', "/"), datafusion::prelude::ParquetReadOptions::default()).await.map_err(|e| friendly(e.to_string()))?;
        let provider = self.ctx.table_provider(qualified.as_str()).await.map_err(|e| e.to_string())?;
        let info = TableInfo { name: format!("{SCRATCH_SCHEMA}.{name}"), source: TableSource::Scratch { path: path.to_path_buf() }, columns: columns_for(&provider.schema()), rows: parquet_rows(path).unwrap_or(0), rows_known: true, live: false, memory_bytes: None, file_bytes };
        self.registry.lock().tables.insert(info.name.clone(), Entry { info: info.clone(), rs: None });
        Ok(info)
    }

    /// Persist a result set as `scratch.<name>` (Parquet in the scratch folder) and register it.
    pub async fn save_scratch(&self, name: &str, rs: Arc<ResultSet>) -> Result<TableInfo, String> {
        let name = sanitize_name(name);
        if name.is_empty() {
            return Err("a name needs at least one letter".into());
        }
        let Some(dir) = self.scratch_dir() else { return Err("no scratch folder".into()) };
        let path = dir.join(format!("{name}.parquet"));
        let tmp = dir.join(format!("{name}.parquet.part"));
        {
            let f = std::fs::File::create(&tmp).map_err(|e| format!("could not write {}: {e}", tmp.display()))?;
            let mut w = datafusion::parquet::arrow::ArrowWriter::try_new(f, Arc::clone(&rs.schema), None).map_err(|e| e.to_string())?;
            for b in rs.raw_batches() {
                w.write(&b).map_err(|e| e.to_string())?;
            }
            w.close().map_err(|e| e.to_string())?;
        }
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
        let info = self.register_scratch_file(&name, &path).await?;
        self.bump();
        Ok(info)
    }

    /// Delete a scratch table (its file goes).
    pub fn remove_scratch(&self, name: &str) -> Result<(), String> {
        let bare = name.strip_prefix("scratch.").unwrap_or(name);
        let qualified = format!("{SCRATCH_SCHEMA}.{}", quote_ident(bare));
        let _ = self.ctx.deregister_table(qualified.as_str());
        let path = self.registry.lock().tables.remove(&format!("{SCRATCH_SCHEMA}.{bare}")).and_then(|e| match e.info.source {
            TableSource::Scratch { path } => Some(path),
            _ => None,
        });
        if let Some(p) = path {
            std::fs::remove_file(&p).map_err(|e| format!("could not delete {}: {e}", p.display()))?;
        }
        self.bump();
        Ok(())
    }

    /// A Delta table in OneLake (`abfss://<workspace>@onelake.dfs.fabric.microsoft.com/<lakehouse>/Tables/…`)
    /// with a bearer token, as a local table. Read-only.
    pub async fn register_onelake(&self, name: &str, url: &str, bearer_token: &str) -> Result<TableInfo, String> {
        let u = url::Url::parse(url).map_err(|e| format!("bad OneLake URL: {e}"))?;
        let mut opts = std::collections::HashMap::new();
        opts.insert("bearer_token".to_string(), bearer_token.to_string());
        let table = deltalake::open_table_with_storage_options(u, opts).await.map_err(|e| format!("could not open the Delta table: {e}"))?;
        let provider = table.table_provider().await.map_err(|e| format!("Delta table provider: {e}"))?;
        let _ = self.ctx.deregister_table(name);
        self.ctx.register_table(name, Arc::clone(&provider)).map_err(|e| e.to_string())?;
        let _ = self.ctx.deregister_table(LATEST);
        let _ = self.ctx.register_table(LATEST, provider);
        let info = TableInfo { name: name.to_string(), source: TableSource::OneLake { url: url.to_string() }, columns: columns_for(&self.ctx.table_provider(name).await.map_err(|e| e.to_string())?.schema()), rows: 0, rows_known: false, live: false, memory_bytes: None, file_bytes: None };
        let mut r = self.registry.lock();
        r.tables.insert(name.to_string(), Entry { info: info.clone(), rs: None });
        r.latest = Some(name.to_string());
        drop(r);
        self.bump();
        Ok(info)
    }

    pub fn budget(&self) -> &Arc<MemoryBudget> {
        &self.budget
    }
    pub fn spill_dir(&self) -> &PathBuf {
        &self.spill_dir
    }
    pub fn context(&self) -> &SessionContext {
        &self.ctx
    }
    pub fn generation(&self) -> u64 {
        self.generation.load(std::sync::atomic::Ordering::Relaxed)
    }
    fn bump(&self) {
        self.generation.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Register (or replace) a result set under `name`; it also becomes `_`.
    pub fn register_result(&self, name: &str, rs: Arc<ResultSet>, source: TableSource) {
        let provider = Arc::new(ResultSetProvider::new(Arc::clone(&rs)));
        let _ = self.ctx.deregister_table(name);
        if let Err(e) = self.ctx.register_table(name, provider.clone()) {
            tracing::warn!(name, "could not register the local table: {e}");
            return;
        }
        let _ = self.ctx.deregister_table(LATEST);
        let _ = self.ctx.register_table(LATEST, provider);
        let mut r = self.registry.lock();
        let live = rs.state().is_live();
        r.tables.insert(name.to_string(), Entry { info: TableInfo { name: name.to_string(), source, columns: rs.columns.clone(), rows: rs.row_count(), rows_known: true, live, memory_bytes: Some(rs.bytes_in_memory()), file_bytes: None }, rs: Some(rs) });
        r.latest = Some(name.to_string());
        drop(r);
        self.bump();
    }

    /// Forget a table.
    pub fn deregister(&self, name: &str) {
        let _ = self.ctx.deregister_table(name);
        let mut r = self.registry.lock();
        r.tables.remove(name);
        if r.latest.as_deref() == Some(name) {
            r.latest = None;
            let _ = self.ctx.deregister_table(LATEST);
        }
        drop(r);
        self.bump();
    }

    /// `CREATE TABLE scratch.<name> AS SELECT * FROM <from>`: any local table becomes a scratch
    /// table (persisted). Returns the scratch table.
    pub async fn copy_to_scratch(&self, name: &str, from: &str) -> Result<TableInfo, String> {
        let Some(_) = self.scratch_dir() else { return Err("no scratch folder".into()) };
        let name = sanitize_name(name);
        if name.is_empty() {
            return Err("give the table a name".into());
        }
        if !self.has_table(from) {
            return Err(format!("no local table named {from}"));
        }
        let qualified = format!("{SCRATCH_SCHEMA}.{}", quote_ident(&name));
        if self.has_table(&format!("{SCRATCH_PREFIX}{name}")) {
            self.remove_scratch(&format!("{SCRATCH_PREFIX}{name}"))?;
        }
        let sql = format!("CREATE TABLE {qualified} AS SELECT * FROM {}", quote_table(from));
        let df = self.ctx.sql(&sql).await.map_err(|e| friendly(e.to_string()))?;
        df.collect().await.map_err(|e| friendly(e.to_string()))?;
        self.sync_from_context().await;
        self.tables().into_iter().find(|t| t.name == format!("{SCRATCH_PREFIX}{name}")).ok_or_else(|| "the scratch table was not written".to_string())
    }

    /// Drop a table whatever its source: a result or file table is forgotten (the tab keeps its
    /// results; the file stays), a statement table is dropped, a scratch table's file is deleted.
    pub fn forget(&self, name: &str) -> Result<(), String> {
        let source = self.registry.lock().tables.get(name).map(|e| e.info.source.clone());
        match source {
            None => Err(format!("no local table named {name}")),
            Some(TableSource::Scratch { .. }) => self.remove_scratch(name),
            Some(TableSource::Statement) => {
                let _ = self.ctx.deregister_table(quote_ident(name).as_str());
                self.registry.lock().tables.remove(name);
                self.bump();
                Ok(())
            }
            Some(_) => {
                self.deregister(name);
                Ok(())
            }
        }
    }

    /// Forget the tables of a tab whose names start with `prefix` (a notebook cell's run).
    pub fn deregister_tab_prefix(&self, tab: &str, prefix: &str) {
        let names: Vec<String> = self.registry.lock().tables.values().filter(|e| e.info.name.starts_with(prefix) && matches!(&e.info.source, TableSource::Result { tab: t, .. } if t == tab)).map(|e| e.info.name.clone()).collect();
        for n in names {
            self.deregister(&n);
        }
    }

    /// Forget every table a tab produced (the tab closed or started a new run).
    pub fn deregister_tab(&self, tab: &str) {
        let names: Vec<String> = self.registry.lock().tables.values().filter(|e| matches!(&e.info.source, TableSource::Result { tab: t, .. } if t == tab)).map(|e| e.info.name.clone()).collect();
        for n in names {
            self.deregister(&n);
        }
    }

    /// Rename a table (the new name must be free); returns the error text otherwise.
    pub fn rename(&self, from: &str, to: &str) -> Result<(), String> {
        let to = sanitize_name(to);
        if to.is_empty() {
            return Err("a name needs at least one letter".into());
        }
        let (rs, source) = {
            let r = self.registry.lock();
            if r.tables.contains_key(&to) {
                return Err(format!("a local table named {to} exists"));
            }
            let Some(e) = r.tables.get(from) else { return Err(format!("no local table named {from}")) };
            (e.rs.clone(), e.info.source.clone())
        };
        let was_latest = self.registry.lock().latest.as_deref() == Some(from);
        self.deregister(from);
        if let Some(rs) = rs {
            self.register_result(&to, rs, source);
            if !was_latest {
                // register_result makes the new name the latest; restore the previous one
                let prev = self.registry.lock().tables.values().filter(|e| e.info.name != to).map(|e| e.info.name.clone()).next_back();
                if let Some(p) = prev {
                    let prs = self.registry.lock().tables.get(&p).and_then(|e| e.rs.clone());
                    if let Some(prs) = prs {
                        let _ = self.ctx.deregister_table(LATEST);
                        let _ = self.ctx.register_table(LATEST, Arc::new(ResultSetProvider::new(prs)));
                        self.registry.lock().latest = Some(p);
                    }
                }
            }
        }
        Ok(())
    }

    /// Register a table (or view) of a SQLite database file as a local table, read from the
    /// file on every scan (projected columns only). It also becomes `_`.
    pub async fn register_sqlite(&self, name: &str, path: &Path, table: &str) -> Result<TableInfo, String> {
        let p = path.to_path_buf();
        let t = table.to_string();
        let (columns, rows) = tokio::task::spawn_blocking(move || -> Result<(Vec<ColumnInfo>, u64), String> {
            let conn = cobalt_sqlite::open_read_only(&p)?;
            let cols = cobalt_sqlite::table_columns(&conn, &t).map_err(|e| e.to_string())?;
            if cols.is_empty() {
                return Err(format!("{t}: no such table in {}", p.display()));
            }
            let rows = cobalt_sqlite::table_row_count(&conn, &t).map_err(|e| e.to_string())?;
            Ok((cols, rows))
        })
        .await
        .map_err(|e| e.to_string())??;
        let _ = self.ctx.deregister_table(name);
        let provider = Arc::new(crate::sqlite_provider::SqliteTableProvider::new(path.to_path_buf(), table.to_string(), columns.clone()));
        self.ctx.register_table(name, provider.clone()).map_err(|e| e.to_string())?;
        let _ = self.ctx.deregister_table(LATEST);
        let _ = self.ctx.register_table(LATEST, provider);
        let info = TableInfo { name: name.to_string(), source: TableSource::Sqlite { path: path.to_path_buf(), table: table.to_string() }, columns, rows: rows as usize, rows_known: true, live: false, memory_bytes: None, file_bytes: std::fs::metadata(path).ok().map(|m| m.len()) };
        let mut r = self.registry.lock();
        r.tables.insert(name.to_string(), Entry { info: info.clone(), rs: None });
        r.latest = Some(name.to_string());
        drop(r);
        self.bump();
        Ok(info)
    }

    /// Register a table (or view) of a DuckDB database file as a local table, read through
    /// DuckDB on every scan (projected columns only); `lib_root` is where the DuckDB library lives.
    pub async fn register_duckdb(&self, name: &str, lib_root: &Path, path: &Path, table: &str) -> Result<TableInfo, String> {
        let (root, p, t) = (lib_root.to_path_buf(), path.to_path_buf(), table.to_string());
        let (schema, rows) = tokio::task::spawn_blocking(move || -> Result<(SchemaRef, u64), String> {
            let schema = cobalt_duckdb::table_schema(&root, &p, &t)?;
            let (_, b) = cobalt_duckdb::read_table(&root, &p, &t, Some(&[]), None).unwrap_or((Arc::new(arrow::datatypes::Schema::empty()), Vec::new()));
            let rows = b.iter().map(|b| b.num_rows() as u64).sum();
            Ok((schema, rows))
        })
        .await
        .map_err(|e| e.to_string())??;
        let _ = self.ctx.deregister_table(name);
        let provider = Arc::new(crate::sqlite_provider::DuckDbTableProvider::new(lib_root.to_path_buf(), path.to_path_buf(), table.to_string(), schema.clone()));
        self.ctx.register_table(name, provider.clone()).map_err(|e| e.to_string())?;
        let _ = self.ctx.deregister_table(LATEST);
        let _ = self.ctx.register_table(LATEST, provider);
        let info = TableInfo { name: name.to_string(), source: TableSource::DuckDb { path: path.to_path_buf(), table: table.to_string() }, columns: columns_for(&schema), rows: rows as usize, rows_known: true, live: false, memory_bytes: None, file_bytes: std::fs::metadata(path).ok().map(|m| m.len()) };
        let mut r = self.registry.lock();
        r.tables.insert(name.to_string(), Entry { info: info.clone(), rs: None });
        r.latest = Some(name.to_string());
        drop(r);
        self.bump();
        Ok(info)
    }

    /// Register a file as a table: Parquet, CSV / TSV / delimited text (header and delimiter
    /// sniffed), JSON lines, Arrow IPC, or a Delta folder (through delta-rs). It also becomes `_`.
    pub async fn register_file(&self, name: &str, path: &Path) -> Result<TableInfo, String> {
        let path_str = path.to_string_lossy().replace('\\', "/");
        let ext = path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).unwrap_or_default();
        let dot_ext = format!(".{ext}");
        let _ = self.ctx.deregister_table(name);
        let mut rows: Option<usize> = None;
        if path.is_dir() {
            if !path.join("_delta_log").is_dir() {
                return Err("a folder is a table only when it is a Delta table (it has a _delta_log)".into());
            }
            let url = url::Url::from_directory_path(path).map_err(|_| "the folder's path is not absolute".to_string())?;
            let table = deltalake::open_table(url).await.map_err(|e| format!("could not open the Delta table: {e}"))?;
            let provider = table.table_provider().await.map_err(|e| format!("Delta table provider: {e}"))?;
            self.ctx.register_table(name, provider).map_err(|e| e.to_string())?;
        } else {
            match ext.as_str() {
                "parquet" | "pq" => {
                    self.ctx.register_parquet(name, &path_str, datafusion::prelude::ParquetReadOptions::default().file_extension(&dot_ext)).await.map_err(|e| friendly(e.to_string()))?;
                    rows = parquet_rows(path);
                }
                "csv" | "tsv" | "tab" | "psv" | "txt" => {
                    let delimiter = match ext.as_str() {
                        "tsv" | "tab" => b'\t',
                        "psv" => b'|',
                        _ => sniff_delimiter(path).unwrap_or(b','),
                    };
                    self.ctx.register_csv(name, &path_str, datafusion::prelude::CsvReadOptions::new().has_header(true).delimiter(delimiter).file_extension(&dot_ext).schema_infer_max_records(5000)).await.map_err(|e| friendly(e.to_string()))?;
                }
                "jsonl" | "ndjson" | "json" => {
                    self.ctx.register_json(name, &path_str, datafusion::prelude::JsonReadOptions::default().file_extension(&dot_ext).schema_infer_max_records(5000)).await.map_err(|e| friendly(e.to_string()))?;
                }
                "arrow" | "feather" | "ipc" | "arrows" => {
                    self.ctx.register_arrow(name, &path_str, datafusion::datasource::file_format::options::ArrowReadOptions { file_extension: &dot_ext, ..Default::default() }).await.map_err(|e| friendly(e.to_string()))?;
                }
                _ => return Err(format!("{}: not a file the local engine reads (Parquet, CSV / TSV / delimited text, JSON lines, Arrow IPC, or a Delta folder)", path.display())),
            }
        }
        let provider = self.ctx.table_provider(name).await.map_err(|e| e.to_string())?;
        let columns = columns_for(&provider.schema());
        let _ = self.ctx.deregister_table(LATEST);
        let _ = self.ctx.register_table(LATEST, provider);
        let info = TableInfo { name: name.to_string(), source: TableSource::File { path: path.to_path_buf() }, columns, rows: rows.unwrap_or(0), rows_known: rows.is_some(), live: false, memory_bytes: None, file_bytes: std::fs::metadata(path).ok().filter(|m| m.is_file()).map(|m| m.len()) };
        let mut r = self.registry.lock();
        r.tables.insert(name.to_string(), Entry { info: info.clone(), rs: None });
        r.latest = Some(name.to_string());
        drop(r);
        self.bump();
        Ok(info)
    }

    /// Tables a statement created in the engine (`CREATE TABLE … AS`, `CREATE EXTERNAL TABLE`)
    /// that the registry does not know yet; tables a statement dropped leave the registry.
    pub async fn sync_from_context(&self) {
        self.sync_scratch().await;
        let Some(schema) = self.ctx.catalog("datafusion").and_then(|c| c.schema("public")) else { return };
        let names = schema.table_names();
        // scratch tables live in their own schema (synced above)
        let known: Vec<String> = self.registry.lock().tables.keys().filter(|k| !k.starts_with(SCRATCH_PREFIX)).cloned().collect();
        let mut changed = false;
        for n in names.iter().filter(|n| n.as_str() != LATEST && !known.contains(n)) {
            if let Ok(Some(p)) = schema.table(n).await {
                let info = TableInfo { name: n.clone(), source: TableSource::Statement, columns: columns_for(&p.schema()), rows: 0, rows_known: false, live: false, memory_bytes: None, file_bytes: None };
                self.registry.lock().tables.insert(n.clone(), Entry { info, rs: None });
                changed = true;
            }
        }
        for k in known.iter().filter(|k| !names.contains(k)) {
            self.registry.lock().tables.remove(k);
            changed = true;
        }
        if changed {
            self.bump();
        }
    }

    /// `CREATE TABLE scratch.x AS …` made an in-memory table: write it to the scratch folder
    /// and re-register it from the file; `DROP TABLE scratch.x` dropped one: its file goes.
    async fn sync_scratch(&self) {
        let Some(schema) = self.ctx.catalog("datafusion").and_then(|c| c.schema(SCRATCH_SCHEMA)) else { return };
        let Some(dir) = self.scratch_dir() else { return };
        let names = schema.table_names();
        let known: Vec<String> = self.registry.lock().tables.keys().filter(|k| k.starts_with(SCRATCH_PREFIX)).map(|k| k[SCRATCH_PREFIX.len()..].to_string()).collect();
        let mut changed = false;
        for n in names.iter().filter(|n| !known.contains(n)) {
            // an in-memory table: materialise it
            let qualified = format!("{SCRATCH_SCHEMA}.{}", quote_ident(n));
            let Ok(df) = self.ctx.sql(&format!("SELECT * FROM {qualified}")).await else { continue };
            let Ok(batches) = df.collect().await else { continue };
            let Some(first) = batches.first() else { continue };
            let path = dir.join(format!("{n}.parquet"));
            let tmp = dir.join(format!("{n}.parquet.part"));
            let ok = (|| -> Result<(), String> {
                let f = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
                let mut w = datafusion::parquet::arrow::ArrowWriter::try_new(f, first.schema(), None).map_err(|e| e.to_string())?;
                for b in &batches {
                    w.write(b).map_err(|e| e.to_string())?;
                }
                w.close().map_err(|e| e.to_string())?;
                std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
            })();
            if let Err(e) = ok {
                tracing::warn!(name = n, "scratch table not persisted: {e}");
                continue;
            }
            let _ = self.ctx.deregister_table(qualified.as_str());
            if self.register_scratch_file(n, &path).await.is_ok() {
                changed = true;
            }
        }
        for k in known.iter().filter(|k| !names.contains(k)) {
            let path = self.registry.lock().tables.remove(&format!("scratch.{k}")).and_then(|e| match e.info.source {
                TableSource::Scratch { path } => Some(path),
                _ => None,
            });
            if let Some(p) = path {
                let _ = std::fs::remove_file(p);
            }
            changed = true;
        }
        if changed {
            self.bump();
        }
    }

    /// Every local table, with live row counts.
    pub fn tables(&self) -> Vec<TableInfo> {
        let r = self.registry.lock();
        r.tables
            .values()
            .map(|e| {
                let mut i = e.info.clone();
                if let Some(rs) = &e.rs {
                    i.rows = rs.row_count();
                    i.live = rs.state().is_live();
                    i.memory_bytes = Some(rs.bytes_in_memory());
                }
                i
            })
            .collect()
    }

    pub fn latest(&self) -> Option<String> {
        self.registry.lock().latest.clone()
    }

    pub fn has_table(&self, name: &str) -> bool {
        self.registry.lock().tables.contains_key(name)
    }

    /// The names of the engine's functions (scalar, aggregate, window), for completion.
    pub fn functions(&self) -> Vec<String> {
        let state = self.ctx.state();
        let mut v: Vec<String> = state.scalar_functions().keys().cloned().collect();
        v.extend(state.aggregate_functions().keys().cloned());
        v.extend(state.window_functions().keys().cloned());
        v.sort();
        v.dedup();
        v
    }

    /// Plan a statement.
    pub async fn sql(&self, sql: &str) -> Result<DataFrame, String> {
        self.ctx.sql(sql).await.map_err(|e| friendly(e.to_string()))
    }

    /// The logical and physical plan of a statement, as text.
    pub async fn explain(&self, sql: &str) -> Result<String, String> {
        let df = self.ctx.sql(&format!("EXPLAIN {sql}")).await.map_err(|e| friendly(e.to_string()))?;
        let batches = df.collect().await.map_err(|e| friendly(e.to_string()))?;
        let mut out = String::new();
        for b in &batches {
            let kinds = b.column(0).as_any().downcast_ref::<arrow::array::StringArray>();
            let plans = b.column(1).as_any().downcast_ref::<arrow::array::StringArray>();
            if let (Some(k), Some(p)) = (kinds, plans) {
                for i in 0..b.num_rows() {
                    out.push_str(&format!("== {} ==\n{}\n\n", k.value(i), p.value(i)));
                }
            }
        }
        Ok(out)
    }
}

/// A quoted identifier for the engine.
pub fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// A registry name as a table reference in SQL: `"name"`, or `scratch."name"` for a scratch
/// table (the schema and the name are separate identifiers; `"scratch.name"` is a different,
/// non-existent table). Always quoted: for generated SQL.
pub fn quote_table(name: &str) -> String {
    match name.strip_prefix(SCRATCH_PREFIX) {
        Some(rest) => format!("{SCRATCH_SCHEMA}.{}", quote_ident(rest)),
        None => quote_ident(name),
    }
}

/// `quote_table` for text a person reads and edits: each part is left bare when the engine
/// takes it as is (lower-case letters, digits, underscores), quoted otherwise.
pub fn table_ref(name: &str) -> String {
    let part = |p: &str| {
        let simple = !p.is_empty() && p.chars().next().map(|c| c.is_ascii_lowercase() || c == '_').unwrap_or(false) && p.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
        if simple { p.to_string() } else { quote_ident(p) }
    };
    match name.strip_prefix(SCRATCH_PREFIX) {
        Some(rest) => format!("{SCRATCH_SCHEMA}.{}", part(rest)),
        None => part(name),
    }
}

/// The row count in a Parquet file's footer.
fn parquet_rows(path: &Path) -> Option<usize> {
    use datafusion::parquet::file::reader::{FileReader, SerializedFileReader};
    let f = std::fs::File::open(path).ok()?;
    let r = SerializedFileReader::new(f).ok()?;
    Some(r.metadata().file_metadata().num_rows().max(0) as usize)
}

/// The delimiter of a delimited text file, from its first line: the most frequent of `,` `;`
/// `\t` `|` outside quotes.
fn sniff_delimiter(path: &Path) -> Option<u8> {
    use std::io::{BufRead, BufReader};
    let f = std::fs::File::open(path).ok()?;
    let mut line = String::new();
    BufReader::new(f).read_line(&mut line).ok()?;
    let mut counts = [(b',', 0usize), (b';', 0), (b'\t', 0), (b'|', 0)];
    let mut quoted = false;
    for b in line.bytes() {
        if b == b'"' {
            quoted = !quoted;
        } else if !quoted {
            for c in counts.iter_mut() {
                if c.0 == b {
                    c.1 += 1;
                }
            }
        }
    }
    counts.iter().max_by_key(|c| c.1).filter(|c| c.1 > 0).map(|c| c.0)
}

/// DataFusion's error text without the enum wrapper.
fn friendly(e: String) -> String {
    let e = e.trim();
    for prefix in ["Error during planning: ", "SQL error: ", "Schema error: ", "Execution error: ", "Arrow error: ", "External error: "] {
        if let Some(rest) = e.strip_prefix(prefix) {
            return rest.to_string();
        }
    }
    e.to_string()
}

/// A local table name: lower-case, letters / digits / underscores, starting with a letter.
pub fn sanitize_name(s: &str) -> String {
    let mut out = String::new();
    for c in s.trim().chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if (c == '_' || c == ' ' || c == '-' || c == '.' || c == '·') && !out.ends_with('_') && !out.is_empty() {
            out.push('_');
        }
    }
    while out.ends_with('_') {
        out.pop();
    }
    if out.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) {
        out.insert(0, 't');
    }
    out
}

/// The SQL type a result column takes for an Arrow type the engine produced.
pub fn sql_type_for(dt: &DataType) -> SqlType {
    SqlType::from_arrow(dt)
}


/// Columns for a result set from the engine's output schema.
pub fn columns_for(schema: &SchemaRef) -> Vec<ColumnInfo> {
    schema.fields().iter().enumerate().map(|(i, f)| ColumnInfo::new(f.name().clone(), sql_type_for(f.data_type()), f.is_nullable(), i)).collect()
}

/// Cast a batch to the result set's schema where the engine's types differ (`Utf8View`,
/// timestamps in another unit, `Int8`…), so every chunk matches the grid's expectations.
pub fn conform(batch: RecordBatch, schema: &SchemaRef) -> Result<RecordBatch, String> {
    if batch.schema().fields() == schema.fields() {
        return Ok(batch);
    }
    let cols: Result<Vec<_>, String> = batch
        .columns()
        .iter()
        .zip(schema.fields())
        .map(|(c, f)| {
            if c.data_type() == f.data_type() {
                Ok(Arc::clone(c))
            } else {
                let target = match (c.data_type(), f.data_type()) {
                    // timestamps keep their zone when the target has one
                    (DataType::Timestamp(_, Some(z)), DataType::Timestamp(u, _)) => DataType::Timestamp(*u, Some(z.clone())),
                    (_, t) => t.clone(),
                };
                arrow::compute::cast(c, &target).map_err(|e| format!("{}: {e}", f.name()))
            }
        })
        .collect();
    let cols = cols?;
    // the batch's own schema (nullability, zones) with the target types
    let fields: Vec<arrow::datatypes::Field> = schema.fields().iter().zip(cols.iter()).map(|(f, c)| arrow::datatypes::Field::new(f.name(), c.data_type().clone(), true)).collect();
    RecordBatch::try_new(Arc::new(arrow::datatypes::Schema::new(fields)), cols).map_err(|e| e.to_string())
}

/// `TimeUnit` is re-exported for callers that build schemas.
pub type Unit = TimeUnit;
