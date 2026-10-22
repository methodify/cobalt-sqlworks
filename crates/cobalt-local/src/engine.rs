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
use std::path::PathBuf;
use std::sync::Arc;

/// The DataFusion version in the binary (for the status bar).
pub const ENGINE_VERSION: &str = "53";

/// Where a local table's data comes from.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TableSource {
    /// A result set of an open tab (`tab` = the tab id, `set` = the set index in its run).
    Result { tab: String, set: usize, title: String },
    File { path: PathBuf },
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct TableInfo {
    pub name: String,
    pub source: TableSource,
    pub columns: Vec<ColumnInfo>,
    pub rows: usize,
    /// `true` while the producing run is still streaming rows into it.
    pub live: bool,
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

pub struct Engine {
    ctx: SessionContext,
    registry: Mutex<Registry>,
    budget: Arc<MemoryBudget>,
    spill_dir: PathBuf,
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
        Ok(Self { ctx, registry: Mutex::new(Registry::default()), budget, spill_dir, generation: std::sync::atomic::AtomicU64::new(1) })
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
        r.tables.insert(name.to_string(), Entry { info: TableInfo { name: name.to_string(), source, columns: rs.columns.clone(), rows: rs.row_count(), live }, rs: Some(rs) });
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
    match dt {
        DataType::Boolean => SqlType::Bit,
        DataType::UInt8 => SqlType::TinyInt,
        DataType::Int8 | DataType::Int16 => SqlType::SmallInt,
        DataType::Int32 | DataType::UInt16 => SqlType::Int,
        DataType::Int64 | DataType::UInt32 => SqlType::BigInt,
        DataType::UInt64 => SqlType::Decimal { precision: 20, scale: 0 },
        DataType::Float16 | DataType::Float32 => SqlType::Real,
        DataType::Float64 => SqlType::Float,
        DataType::Decimal128(p, s) => SqlType::Decimal { precision: *p, scale: (*s).max(0) as u8 },
        DataType::Date32 | DataType::Date64 => SqlType::Date,
        DataType::Time32(_) | DataType::Time64(_) => SqlType::Time { scale: 7 },
        DataType::Timestamp(_, None) => SqlType::DateTime2 { scale: 7 },
        DataType::Timestamp(_, Some(_)) => SqlType::DateTimeOffset { scale: 7 },
        DataType::Utf8 | DataType::Utf8View => SqlType::NVarChar { len: Some(4000) },
        DataType::LargeUtf8 => SqlType::NVarChar { len: None },
        DataType::Binary | DataType::BinaryView | DataType::FixedSizeBinary(_) => SqlType::VarBinary { len: Some(8000) },
        DataType::LargeBinary => SqlType::VarBinary { len: None },
        DataType::Null => SqlType::NVarChar { len: Some(1) },
        other => SqlType::Other(other.to_string()),
    }
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
