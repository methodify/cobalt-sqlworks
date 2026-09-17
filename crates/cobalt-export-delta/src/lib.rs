//! Delta Lake export via delta-rs (`deltalake`), isolated in its own crate so the heavy
//! dependency tree doesn't slow the rest of the build.
//!
//! The Delta protocol supports a narrower type set than Arrow, so batches are cast first
//! ([`delta_compatible_schema`] / [`cast_batch`]): `datetime2` → `timestamp_ntz` (µs),
//! `datetimeoffset` → `timestamp` (µs, UTC), `tinyint` → `short`, `time` → string.
//! The table is written with the non-DataFusion `RecordBatchWriter` (the workspace's `deltalake`
//! build has no `datafusion` feature) and committed as a single transaction.

use arrow::array::{Array, ArrayRef, RecordBatch, StringArray};
use arrow::datatypes::{DataType, Field, Schema, SchemaRef, TimeUnit};
use cobalt_results::ResultSet;
pub use cobalt_export::{ExportStats, Progress};
use deltalake::kernel::engine::arrow_conversion::TryIntoKernel;
use deltalake::kernel::transaction::CommitBuilder;
use deltalake::kernel::{Action, StructType};
use deltalake::operations::create::CreateBuilder;
use deltalake::protocol::{DeltaOperation, SaveMode};
use deltalake::writer::{DeltaWriter, RecordBatchWriter};
use deltalake::{DeltaTable, DeltaTableBuilder, DeltaTableError};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub mod remote;
pub use remote::{register_cloud_handlers, remote_table_exists, upload_file, upload_file_blocking, write_delta_remote, write_delta_remote_blocking, RemoteTarget};
use std::sync::Arc;
use std::time::Instant;

/// Rows per batch pulled from the result set.
pub const DELTA_BATCH_ROWS: usize = 65_536;
/// Flush buffered parquet data to files once this many bytes are buffered in the writer.
const FLUSH_BYTES: usize = 128 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum DeltaMode {
    /// Create a new table; error if a Delta table already exists at the path.
    #[default]
    Create,
    /// Replace the table's contents (and schema) in one transaction.
    Overwrite,
    /// Add the rows to an existing table (created if absent).
    Append,
}

impl DeltaMode {
    pub fn parse(s: &str) -> Option<DeltaMode> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "create" | "new" | "error" | "errorifexists" => DeltaMode::Create,
            "overwrite" | "replace" => DeltaMode::Overwrite,
            "append" | "add" => DeltaMode::Append,
            _ => return None,
        })
    }
    pub fn label(&self) -> &'static str {
        match self {
            DeltaMode::Create => "Create new",
            DeltaMode::Overwrite => "Overwrite",
            DeltaMode::Append => "Append",
        }
    }
    pub fn all() -> &'static [DeltaMode] {
        &[DeltaMode::Create, DeltaMode::Overwrite, DeltaMode::Append]
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeltaOptions {
    pub mode: DeltaMode,
    /// Column names (as shown in the grid) to partition by; become `col=value` directories.
    pub partition_columns: Vec<String>,
    pub table_name: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum DeltaError {
    #[error("delta error: {0}")]
    Delta(#[from] DeltaTableError),
    #[error("arrow error: {0}")]
    Arrow(#[from] arrow::error::ArrowError),
    #[error("result set error: {0}")]
    Results(#[from] cobalt_results::ResultError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("a Delta table already exists at {0}")]
    Exists(PathBuf),
    #[error("unknown partition column '{0}'")]
    UnknownPartitionColumn(String),
    #[error("invalid table location: {0}")]
    InvalidPath(String),
    #[error("export cancelled")]
    Cancelled,
}

pub type Result<T> = std::result::Result<T, DeltaError>;

// ---------------------------------------------------------------------------------------------
// Type mapping
// ---------------------------------------------------------------------------------------------

/// The Arrow type Delta will store a column as (`None` = unchanged).
fn delta_type(dt: &DataType) -> Option<DataType> {
    Some(match dt {
        DataType::UInt8 => DataType::Int16,
        DataType::UInt16 => DataType::Int32,
        DataType::UInt32 | DataType::UInt64 => DataType::Int64,
        DataType::Time32(_) | DataType::Time64(_) => DataType::Utf8,
        DataType::Timestamp(TimeUnit::Microsecond, _) => return None,
        DataType::Timestamp(_, tz) => DataType::Timestamp(TimeUnit::Microsecond, tz.clone()),
        DataType::Date64 => DataType::Date32,
        DataType::LargeUtf8 | DataType::Utf8View => DataType::Utf8,
        DataType::LargeBinary | DataType::BinaryView | DataType::FixedSizeBinary(_) => DataType::Binary,
        DataType::Decimal128(p, s) if *p > 38 || *s < 0 => DataType::Utf8,
        DataType::Decimal256(..) | DataType::Duration(_) | DataType::Interval(_) => DataType::Utf8,
        DataType::Float16 => DataType::Float32,
        DataType::Null => DataType::Utf8,
        _ => return None,
    })
}

/// Map every field to a Delta-compatible Arrow type (see the crate docs). Field names,
/// nullability and metadata are preserved.
pub fn delta_compatible_schema(schema: &Schema) -> Schema {
    let fields: Vec<Field> = schema
        .fields()
        .iter()
        .map(|f| match delta_type(f.data_type()) {
            Some(dt) => f.as_ref().clone().with_data_type(dt),
            None => f.as_ref().clone(),
        })
        .collect();
    Schema::new_with_metadata(fields, schema.metadata().clone())
}

/// Cast a batch to [`delta_compatible_schema`]. `Time64` becomes `HH:MM:SS.fffffff` text;
/// everything else goes through `arrow::compute::cast` (timestamps are truncated to µs).
pub fn cast_batch(batch: &RecordBatch) -> Result<RecordBatch> {
    let target = Arc::new(delta_compatible_schema(&batch.schema()));
    let mut cols: Vec<ArrayRef> = Vec::with_capacity(batch.num_columns());
    for (i, field) in target.fields().iter().enumerate() {
        let col = batch.column(i);
        if col.data_type() == field.data_type() {
            cols.push(col.clone());
            continue;
        }
        let out: ArrayRef = match col.data_type() {
            DataType::Time64(_) | DataType::Time32(_) => Arc::new(time_to_text(col)),
            _ => arrow::compute::cast(col, field.data_type())?,
        };
        cols.push(out);
    }
    Ok(RecordBatch::try_new(target, cols)?)
}

/// SQL Server style `HH:MM:SS.fffffff` (7 fractional digits, trailing zeros trimmed to none).
fn time_to_text(col: &ArrayRef) -> StringArray {
    let mut out: Vec<Option<String>> = Vec::with_capacity(col.len());
    for i in 0..col.len() {
        if col.is_null(i) {
            out.push(None);
            continue;
        }
        match cobalt_results::CellValue::from_array(col, i) {
            cobalt_results::CellValue::Time(t) => {
                use chrono::Timelike;
                let frac = t.nanosecond() / 100; // 7 digits
                out.push(Some(if frac == 0 { t.format("%H:%M:%S").to_string() } else { format!("{}.{:07}", t.format("%H:%M:%S"), frac) }));
            }
            other => out.push(Some(other.to_sql_literal().trim_matches('\'').to_string())),
        }
    }
    StringArray::from(out)
}

// ---------------------------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------------------------

/// Local directory → `file:///…` URL (created if missing).
fn table_url(path: &Path) -> Result<url::Url> {
    if let Ok(u) = url::Url::parse(&path.to_string_lossy()) {
        if u.scheme().len() > 1 && !u.cannot_be_a_base() && u.scheme() != "file" {
            return Ok(u); // s3://, abfss://, … — left for the cloud roadmap; delta-rs decides
        }
    }
    let s = path.to_string_lossy();
    deltalake::table::builder::ensure_table_uri(s.as_ref()).map_err(|e| DeltaError::InvalidPath(e.to_string()))
}

fn has_delta_log(path: &Path) -> bool {
    path.join("_delta_log").is_dir()
}

fn table_bytes(table: &DeltaTable) -> u64 {
    table.snapshot().map(|s| s.log_data().into_iter().map(|f| f.size().max(0) as u64).sum()).unwrap_or(0)
}

/// Write the visible rows of `rs` as a Delta table at `path` (a local directory).
///
/// `progress` is polled before the first batch and after every batch; returning `false` cancels.
/// A cancelled `Create` removes the directory it created; cancelled `Append`/`Overwrite` commit nothing.
pub async fn write_delta(rs: &ResultSet, path: &Path, opts: &DeltaOptions, progress: &mut (dyn FnMut(Progress) -> bool + Send)) -> Result<ExportStats> {
    let started = Instant::now();
    let existed_before = path.exists();
    let is_table = has_delta_log(path);
    if opts.mode == DeltaMode::Create && is_table {
        return Err(DeltaError::Exists(path.to_path_buf()));
    }
    for p in &opts.partition_columns {
        if !rs.schema.fields().iter().any(|f| f.name() == p) {
            return Err(DeltaError::UnknownPartitionColumn(p.clone()));
        }
    }
    let url = table_url(path)?;
    let schema: SchemaRef = Arc::new(delta_compatible_schema(&rs.schema));
    let kernel_schema: StructType = schema.as_ref().try_into_kernel()?;

    let result = write_inner(rs, &url, None, opts, is_table, schema, kernel_schema, progress).await;
    match result {
        Ok((rows, bytes)) => {
            tracing::info!(?path, rows, bytes, mode = ?opts.mode, "delta export complete");
            Ok(ExportStats { rows, bytes, elapsed: started.elapsed(), path: path.to_path_buf(), warnings: Vec::new() })
        }
        Err(e) => {
            if !existed_before {
                let _ = std::fs::remove_dir_all(path);
            } else if !is_table && opts.mode == DeltaMode::Create {
                let _ = std::fs::remove_dir_all(path.join("_delta_log"));
            }
            Err(e)
        }
    }
}

pub(crate) async fn write_inner(
    rs: &ResultSet,
    url: &url::Url,
    storage_options: Option<&HashMap<String, String>>,
    opts: &DeltaOptions,
    is_table: bool,
    schema: SchemaRef,
    kernel_schema: StructType,
    progress: &mut (dyn FnMut(Progress) -> bool + Send),
) -> Result<(usize, u64)> {
    let mut table: DeltaTable = match (opts.mode, is_table) {
        (DeltaMode::Append, true) => {
            let mut b = DeltaTableBuilder::from_url(url.clone())?;
            if let Some(so) = storage_options {
                b = b.with_storage_options(so.clone());
            }
            b.load().await?
        }
        (mode, _) => {
            let save_mode = if mode == DeltaMode::Overwrite { SaveMode::Overwrite } else { SaveMode::ErrorIfExists };
            let mut b = CreateBuilder::new()
                .with_location(url.as_str())
                .with_columns(kernel_schema.fields().cloned())
                .with_partition_columns(opts.partition_columns.iter().cloned())
                .with_save_mode(save_mode);
            if let Some(so) = storage_options {
                b = b.with_storage_options(so.clone());
            }
            if let Some(n) = &opts.table_name {
                b = b.with_table_name(n.clone());
            }
            if let Some(d) = &opts.description {
                b = b.with_comment(d.clone());
            }
            b.await?
        }
    };
    let bytes_before = table_bytes(&table);
    let partition_cols: Vec<String> = table.snapshot()?.metadata().partition_columns().to_vec();

    let mut writer = RecordBatchWriter::for_table(&table)?;
    let total = rs.visible_count();
    let mut done = 0usize;
    if !progress(Progress { rows_done: 0, rows_total: total, bytes_written: 0 }) {
        return Err(DeltaError::Cancelled);
    }
    let mut adds: Vec<Action> = Vec::new();
    let mut flushed_bytes: u64 = 0;
    for batch in rs.view_batches(DELTA_BATCH_ROWS) {
        let batch = batch?;
        let batch = cast_batch(&batch)?;
        let batch = RecordBatch::try_new(schema.clone(), batch.columns().to_vec())?;
        writer.write(batch.clone()).await?;
        done += batch.num_rows();
        if writer.buffer_len() >= FLUSH_BYTES {
            for add in writer.flush().await? {
                flushed_bytes += add.size.max(0) as u64;
                adds.push(Action::Add(add));
            }
        }
        if !progress(Progress { rows_done: done, rows_total: total, bytes_written: flushed_bytes }) {
            return Err(DeltaError::Cancelled);
        }
    }
    for add in writer.flush().await? {
        flushed_bytes += add.size.max(0) as u64;
        adds.push(Action::Add(add));
    }
    let _ = flushed_bytes;

    // One commit for the whole export (the table-creating commit is separate).
    if !adds.is_empty() {
        let operation = DeltaOperation::Write {
            mode: if opts.mode == DeltaMode::Append { SaveMode::Append } else { SaveMode::Overwrite },
            partition_by: if partition_cols.is_empty() { None } else { Some(partition_cols) },
            predicate: None,
        };
        let snapshot = table.snapshot()?;
        CommitBuilder::default().with_actions(adds).build(Some(snapshot), table.log_store(), operation).await?;
        table.update_state().await?;
    }
    let bytes = table_bytes(&table).saturating_sub(if opts.mode == DeltaMode::Append { bytes_before } else { 0 });
    Ok((done, bytes))
}

/// Blocking wrapper: spins a current-thread tokio runtime (for callers already on a blocking thread).
pub fn write_delta_blocking(rs: &ResultSet, path: &Path, opts: &DeltaOptions, progress: &mut (dyn FnMut(Progress) -> bool + Send)) -> Result<ExportStats> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    rt.block_on(write_delta(rs, path, opts, progress))
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::{Decimal128Array, Time64NanosecondArray, TimestampNanosecondArray, UInt8Array};

    #[test]
    fn schema_mapping() {
        let s = Schema::new(vec![
            Field::new("u8", DataType::UInt8, true),
            Field::new("t", DataType::Time64(TimeUnit::Nanosecond), true),
            Field::new("dt2", DataType::Timestamp(TimeUnit::Nanosecond, None), true),
            Field::new("dto", DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into())), true),
            Field::new("dtm", DataType::Timestamp(TimeUnit::Millisecond, None), true),
            Field::new("dec", DataType::Decimal128(18, 4), true),
            Field::new("ls", DataType::LargeUtf8, true),
            Field::new("lb", DataType::LargeBinary, true),
            Field::new("d", DataType::Date32, true),
            Field::new("i16", DataType::Int16, true),
            Field::new("f32", DataType::Float32, true),
            Field::new("bin", DataType::Binary, true),
            Field::new("b", DataType::Boolean, true),
        ]);
        let d = delta_compatible_schema(&s);
        let ty = |n: &str| d.field_with_name(n).unwrap().data_type().clone();
        assert_eq!(ty("u8"), DataType::Int16);
        assert_eq!(ty("t"), DataType::Utf8);
        assert_eq!(ty("dt2"), DataType::Timestamp(TimeUnit::Microsecond, None));
        assert_eq!(ty("dto"), DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())));
        assert_eq!(ty("dtm"), DataType::Timestamp(TimeUnit::Microsecond, None));
        assert_eq!(ty("dec"), DataType::Decimal128(18, 4));
        assert_eq!(ty("ls"), DataType::Utf8);
        assert_eq!(ty("lb"), DataType::Binary);
        assert_eq!(ty("d"), DataType::Date32);
        assert_eq!(ty("i16"), DataType::Int16);
        assert_eq!(ty("f32"), DataType::Float32);
        assert_eq!(ty("bin"), DataType::Binary);
        assert_eq!(ty("b"), DataType::Boolean);
        // Every mapped type must be accepted by the Delta kernel.
        let _: StructType = (&d).try_into_kernel().expect("kernel schema");
    }

    #[test]
    fn batch_cast() {
        let s = Arc::new(Schema::new(vec![
            Field::new("u8", DataType::UInt8, true),
            Field::new("t", DataType::Time64(TimeUnit::Nanosecond), true),
            Field::new("dt2", DataType::Timestamp(TimeUnit::Nanosecond, None), true),
            Field::new("dec", DataType::Decimal128(18, 4), true),
        ]));
        let b = RecordBatch::try_new(
            s,
            vec![
                Arc::new(UInt8Array::from(vec![Some(255), None])),
                Arc::new(Time64NanosecondArray::from(vec![Some(3_723_123_456_700), Some(0)])),
                Arc::new(TimestampNanosecondArray::from(vec![Some(1_704_164_645_123_456_789), None])),
                Arc::new(Decimal128Array::from(vec![Some(12345), None]).with_precision_and_scale(18, 4).unwrap()),
            ],
        )
        .unwrap();
        let c = cast_batch(&b).unwrap();
        assert_eq!(c.schema().field(0).data_type(), &DataType::Int16);
        let v = |r: usize, col: usize| cobalt_results::CellValue::from_array(c.column(col), r);
        assert_eq!(v(0, 0), cobalt_results::CellValue::Int(255));
        assert!(v(1, 0).is_null());
        assert_eq!(v(0, 1), cobalt_results::CellValue::Text("01:02:03.1234567".into()));
        assert_eq!(v(1, 1), cobalt_results::CellValue::Text("00:00:00".into()));
        assert_eq!(c.schema().field(2).data_type(), &DataType::Timestamp(TimeUnit::Microsecond, None));
        match v(0, 2) {
            cobalt_results::CellValue::DateTime(dt) => assert_eq!(dt.format("%Y-%m-%d %H:%M:%S%.6f").to_string(), "2024-01-02 03:04:05.123456"),
            other => panic!("{other:?}"),
        }
        assert_eq!(v(0, 3), cobalt_results::CellValue::Decimal(12345, 4));
    }

    #[test]
    fn mode_parse() {
        assert_eq!(DeltaMode::parse("create"), Some(DeltaMode::Create));
        assert_eq!(DeltaMode::parse("Overwrite"), Some(DeltaMode::Overwrite));
        assert_eq!(DeltaMode::parse("append"), Some(DeltaMode::Append));
        assert_eq!(DeltaMode::parse("?"), None);
    }
}
