//! Flat-file import: sniff a file's format, infer a schema and preview it, then stream it as
//! Arrow batches. The driver turns batches into a bulk insert; this crate never talks to SQL.
//!
//! Formats: CSV/TSV (delimiter and header sniffed, overridable), Parquet, Arrow IPC (file or
//! stream). Type suggestions map the Arrow schema to T-SQL types; the user can override them and
//! the driver casts each batch to the chosen types before sending.

use arrow::array::RecordBatch;
use arrow::datatypes::{DataType, Schema, SchemaRef, TimeUnit};
use arrow::util::display::{ArrayFormatter, FormatOptions};
use cobalt_core::SqlType;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek};
use std::path::Path;
use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("arrow error: {0}")]
    Arrow(#[from] arrow::error::ArrowError),
    #[error("parquet error: {0}")]
    Parquet(#[from] parquet::errors::ParquetError),
    #[error("{0}")]
    Invalid(String),
}

pub type Result<T> = std::result::Result<T, ImportError>;

/// Rows per batch handed to the driver.
pub const BATCH_ROWS: usize = 8192;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileFormat {
    Csv { delimiter: u8, has_header: bool },
    Parquet,
    ArrowIpc,
}

impl FileFormat {
    /// Guess from the extension; for delimited text also sniff the delimiter from the first line.
    pub fn sniff(path: &Path) -> FileFormat {
        let ext = path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).unwrap_or_default();
        match ext.as_str() {
            "parquet" | "pq" => FileFormat::Parquet,
            "arrow" | "feather" | "ipc" | "arrows" => FileFormat::ArrowIpc,
            "tsv" | "tab" => FileFormat::Csv { delimiter: b'\t', has_header: true },
            _ => {
                let delimiter = sniff_delimiter(path).unwrap_or(b',');
                FileFormat::Csv { delimiter, has_header: true }
            }
        }
    }

    pub fn label(&self) -> String {
        match self {
            FileFormat::Csv { delimiter, .. } => match delimiter {
                b'\t' => "Tab-separated text".into(),
                b';' => "Semicolon-separated text".into(),
                b'|' => "Pipe-separated text".into(),
                b',' => "CSV".into(),
                d => format!("Delimited text ({:?})", *d as char),
            },
            FileFormat::Parquet => "Parquet".into(),
            FileFormat::ArrowIpc => "Arrow IPC".into(),
        }
    }
}

/// The delimiter that splits the first non-empty line into the most fields (`,` `;` `|` tab).
fn sniff_delimiter(path: &Path) -> Option<u8> {
    let f = File::open(path).ok()?;
    let mut r = BufReader::new(f);
    let mut line = String::new();
    for _ in 0..5 {
        line.clear();
        if r.read_line(&mut line).ok()? == 0 {
            return None;
        }
        if !line.trim().is_empty() {
            break;
        }
    }
    let best = [b',', b'\t', b';', b'|'].into_iter().map(|d| (line.bytes().filter(|b| *b == d).count(), d)).max_by_key(|(n, _)| *n)?;
    if best.0 == 0 { None } else { Some(best.1) }
}

/// One column as we will offer it: the file's type and a suggested T-SQL type.
#[derive(Debug, Clone)]
pub struct ImportColumn {
    pub name: String,
    pub arrow: DataType,
    pub sql_type: SqlType,
    pub nullable: bool,
    /// Longest text value seen in the sample (for sizing nvarchar).
    pub max_len: usize,
}

/// What we learned from the file before importing.
#[derive(Debug, Clone)]
pub struct Inspection {
    pub format: FileFormat,
    pub schema: SchemaRef,
    pub columns: Vec<ImportColumn>,
    /// First rows, formatted as text (at most `preview_rows` × columns).
    pub preview: Vec<Vec<String>>,
    /// Exact (Parquet) or estimated (CSV) row count when we can tell.
    pub row_estimate: Option<u64>,
    pub estimate_is_exact: bool,
    pub file_bytes: u64,
}

/// Read enough of the file to know its shape: schema, suggested SQL types, a preview.
pub fn inspect(path: &Path, format: &FileFormat, sample_rows: usize, preview_rows: usize) -> Result<Inspection> {
    let file_bytes = std::fs::metadata(path)?.len();
    let (schema, sample, row_estimate, exact): (SchemaRef, Vec<RecordBatch>, Option<u64>, bool) = match format {
        FileFormat::Csv { delimiter, has_header } => {
            let fmt = arrow::csv::reader::Format::default().with_header(*has_header).with_delimiter(*delimiter);
            let (schema, _n) = fmt.infer_schema(File::open(path)?, Some(sample_rows.max(1)))?;
            let schema = Arc::new(schema);
            let mut reader = arrow::csv::ReaderBuilder::new(schema.clone()).with_header(*has_header).with_delimiter(*delimiter).with_batch_size(sample_rows.max(1)).build(File::open(path)?)?;
            let sample = match reader.next() {
                Some(b) => vec![b?],
                None => Vec::new(),
            };
            let estimate = estimate_csv_rows(path, file_bytes, *has_header);
            (schema, sample, estimate, false)
        }
        FileFormat::Parquet => {
            let builder = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(File::open(path)?)?;
            let schema = builder.schema().clone();
            let rows = builder.metadata().file_metadata().num_rows().max(0) as u64;
            let mut reader = builder.with_batch_size(sample_rows.max(1)).build()?;
            let sample = match reader.next() {
                Some(b) => vec![b?],
                None => Vec::new(),
            };
            (schema, sample, Some(rows), true)
        }
        FileFormat::ArrowIpc => {
            let mut reader = open_ipc(path)?;
            let schema = reader.schema();
            let sample = match reader.next() {
                Some(b) => vec![b?],
                None => Vec::new(),
            };
            (schema, sample, None, false)
        }
    };
    let sample_batch = sample.first();
    let mut columns = Vec::with_capacity(schema.fields().len());
    for (i, f) in schema.fields().iter().enumerate() {
        let max_len = sample_batch.map(|b| text_max_len(b.column(i))).unwrap_or(0);
        let sql_type = suggest_sql_type(f.data_type(), max_len);
        columns.push(ImportColumn { name: f.name().clone(), arrow: f.data_type().clone(), sql_type, nullable: true, max_len });
    }
    let preview = sample_batch.map(|b| preview_rows_of(b, preview_rows)).unwrap_or_default();
    Ok(Inspection { format: format.clone(), schema, columns, preview, row_estimate, estimate_is_exact: exact, file_bytes })
}

/// Stream every batch of the file (the inferred schema for CSV so types match `inspect`).
pub fn open_batches(path: &Path, format: &FileFormat, schema: SchemaRef, batch_rows: usize) -> Result<Box<dyn Iterator<Item = Result<RecordBatch>> + Send>> {
    match format {
        FileFormat::Csv { delimiter, has_header } => {
            let reader = arrow::csv::ReaderBuilder::new(schema).with_header(*has_header).with_delimiter(*delimiter).with_batch_size(batch_rows.max(1)).build(File::open(path)?)?;
            Ok(Box::new(reader.map(|r| r.map_err(ImportError::from))))
        }
        FileFormat::Parquet => {
            let reader = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(File::open(path)?)?.with_batch_size(batch_rows.max(1)).build()?;
            Ok(Box::new(reader.map(|r| r.map_err(ImportError::from))))
        }
        FileFormat::ArrowIpc => Ok(Box::new(open_ipc(path)?)),
    }
}

/// Arrow IPC file, or stream format as a fallback.
struct IpcReader {
    inner: IpcInner,
}
enum IpcInner {
    File(arrow::ipc::reader::FileReader<BufReader<File>>),
    Stream(arrow::ipc::reader::StreamReader<BufReader<File>>),
}
impl IpcReader {
    fn schema(&self) -> SchemaRef {
        match &self.inner {
            IpcInner::File(r) => r.schema(),
            IpcInner::Stream(r) => r.schema(),
        }
    }
}
impl Iterator for IpcReader {
    type Item = Result<RecordBatch>;
    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.inner {
            IpcInner::File(r) => r.next().map(|b| b.map_err(ImportError::from)),
            IpcInner::Stream(r) => r.next().map(|b| b.map_err(ImportError::from)),
        }
    }
}
fn open_ipc(path: &Path) -> Result<IpcReader> {
    let mut f = File::open(path)?;
    let mut magic = [0u8; 6];
    let n = f.read(&mut magic)?;
    f.seek(std::io::SeekFrom::Start(0))?;
    if n >= 6 && &magic == b"ARROW1" {
        Ok(IpcReader { inner: IpcInner::File(arrow::ipc::reader::FileReader::try_new(BufReader::new(f), None)?) })
    } else {
        Ok(IpcReader { inner: IpcInner::Stream(arrow::ipc::reader::StreamReader::try_new(BufReader::new(f), None)?) })
    }
}

/// Rough CSV row count from the average length of the first 200 lines.
fn estimate_csv_rows(path: &Path, file_bytes: u64, has_header: bool) -> Option<u64> {
    let f = File::open(path).ok()?;
    let mut r = BufReader::new(f);
    let mut line = String::new();
    let mut lines = 0u64;
    let mut bytes = 0u64;
    while lines < 200 {
        line.clear();
        let n = r.read_line(&mut line).ok()?;
        if n == 0 {
            break;
        }
        lines += 1;
        bytes += n as u64;
    }
    if lines == 0 || bytes == 0 {
        return Some(0);
    }
    let avg = bytes as f64 / lines as f64;
    let mut est = (file_bytes as f64 / avg).round() as u64;
    if has_header {
        est = est.saturating_sub(1);
    }
    Some(est)
}

fn text_max_len(col: &arrow::array::ArrayRef) -> usize {
    use arrow::array::{Array, LargeStringArray, StringArray};
    if let Some(a) = col.as_any().downcast_ref::<StringArray>() {
        return (0..a.len()).filter(|i| !a.is_null(*i)).map(|i| a.value(i).chars().count()).max().unwrap_or(0);
    }
    if let Some(a) = col.as_any().downcast_ref::<LargeStringArray>() {
        return (0..a.len()).filter(|i| !a.is_null(*i)).map(|i| a.value(i).chars().count()).max().unwrap_or(0);
    }
    0
}

fn preview_rows_of(b: &RecordBatch, n: usize) -> Vec<Vec<String>> {
    let opts = FormatOptions::default().with_null("NULL");
    let fmts: Vec<Option<ArrayFormatter<'_>>> = b.columns().iter().map(|c| ArrayFormatter::try_new(c.as_ref(), &opts).ok()).collect();
    (0..b.num_rows().min(n))
        .map(|r| fmts.iter().map(|f| f.as_ref().map(|f| f.value(r).to_string()).unwrap_or_else(|| "?".into())).collect())
        .collect()
}

/// A T-SQL type for an Arrow type. Text gets an nvarchar sized a step above the longest sample.
pub fn suggest_sql_type(dt: &DataType, max_len: usize) -> SqlType {
    match dt {
        DataType::Boolean => SqlType::Bit,
        DataType::Int8 | DataType::Int16 => SqlType::SmallInt,
        DataType::UInt8 => SqlType::TinyInt,
        DataType::Int32 | DataType::UInt16 => SqlType::Int,
        DataType::Int64 | DataType::UInt32 | DataType::UInt64 => SqlType::BigInt,
        DataType::Float16 | DataType::Float32 => SqlType::Real,
        DataType::Float64 => SqlType::Float,
        DataType::Decimal128(p, s) | DataType::Decimal256(p, s) => SqlType::Decimal { precision: (*p).min(38), scale: (*s).max(0) as u8 },
        DataType::Date32 | DataType::Date64 => SqlType::Date,
        DataType::Time32(_) | DataType::Time64(_) => SqlType::Time { scale: 7 },
        DataType::Timestamp(_, None) => SqlType::DateTime2 { scale: 7 },
        DataType::Timestamp(_, Some(_)) => SqlType::DateTimeOffset { scale: 7 },
        DataType::Binary | DataType::LargeBinary | DataType::FixedSizeBinary(_) | DataType::BinaryView => SqlType::VarBinary { len: None },
        DataType::Utf8 | DataType::LargeUtf8 | DataType::Utf8View => SqlType::NVarChar { len: text_bucket(max_len) },
        _ => SqlType::NVarChar { len: None },
    }
}

/// 50 / 100 / 255 / 1000 / 4000 / max, one step above what we saw (never below 50).
fn text_bucket(max_len: usize) -> Option<u32> {
    let buckets = [50u32, 100, 255, 1000, 4000];
    for b in buckets {
        if (max_len as u32) * 2 <= b {
            return Some(b);
        }
    }
    None
}

/// Parse a T-SQL type as a user types it: `int`, `nvarchar(100)`, `nvarchar(max)`, `decimal(18,2)`,
/// `datetime2(3)`… Returns `None` when it is not something we know how to bulk-load.
pub fn parse_sql_type(s: &str) -> Option<SqlType> {
    let s = s.trim().to_ascii_lowercase();
    let (name, args) = match s.split_once('(') {
        Some((n, rest)) => (n.trim().to_string(), rest.trim_end_matches(')').split(',').map(|a| a.trim().to_string()).filter(|a| !a.is_empty()).collect::<Vec<_>>()),
        None => (s.clone(), Vec::new()),
    };
    let len = |i: usize| -> Option<u32> { args.get(i).and_then(|a| if a == "max" { None } else { a.parse::<u32>().ok() }) };
    let is_max = args.first().map(|a| a == "max").unwrap_or(false);
    let num = |i: usize, default: u8| -> u8 { args.get(i).and_then(|a| a.parse::<u8>().ok()).unwrap_or(default) };
    Some(match name.as_str() {
        "bit" => SqlType::Bit,
        "tinyint" => SqlType::TinyInt,
        "smallint" => SqlType::SmallInt,
        "int" | "integer" => SqlType::Int,
        "bigint" => SqlType::BigInt,
        "decimal" | "dec" => SqlType::Decimal { precision: num(0, 18), scale: num(1, 0) },
        "numeric" => SqlType::Numeric { precision: num(0, 18), scale: num(1, 0) },
        "money" => SqlType::Money,
        "smallmoney" => SqlType::SmallMoney,
        "float" => SqlType::Float,
        "real" => SqlType::Real,
        "date" => SqlType::Date,
        "time" => SqlType::Time { scale: num(0, 7) },
        "datetime" => SqlType::DateTime,
        "smalldatetime" => SqlType::SmallDateTime,
        "datetime2" => SqlType::DateTime2 { scale: num(0, 7) },
        "datetimeoffset" => SqlType::DateTimeOffset { scale: num(0, 7) },
        "char" => SqlType::Char { len: Some(len(0).unwrap_or(1)) },
        "nchar" => SqlType::NChar { len: Some(len(0).unwrap_or(1)) },
        "varchar" => SqlType::VarChar { len: if is_max { None } else { Some(len(0).unwrap_or(255)) } },
        "nvarchar" => SqlType::NVarChar { len: if is_max { None } else { Some(len(0).unwrap_or(255)) } },
        "text" => SqlType::Text,
        "ntext" => SqlType::NText,
        "binary" => SqlType::Binary { len: Some(len(0).unwrap_or(1)) },
        "varbinary" => SqlType::VarBinary { len: if is_max { None } else { Some(len(0).unwrap_or(255)) } },
        "image" => SqlType::Image,
        "uniqueidentifier" => SqlType::UniqueIdentifier,
        "xml" => SqlType::Xml,
        "json" => SqlType::Json,
        _ => return None,
    })
}

/// `CREATE TABLE [schema].[table] (...)` for the included columns.
pub fn create_table_sql(schema: &str, table: &str, columns: &[(String, SqlType, bool)]) -> String {
    let b = |s: &str| format!("[{}]", s.replace(']', "]]"));
    let cols: Vec<String> = columns.iter().map(|(n, t, nullable)| format!("    {} {} {}", b(n), t, if *nullable { "NULL" } else { "NOT NULL" })).collect();
    format!("CREATE TABLE {}.{} (\n{}\n);", b(schema), b(table), cols.join(",\n"))
}

/// A safe default table name from a file name: letters, digits and underscores.
pub fn table_name_from(path: &Path) -> String {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("imported");
    let mut out: String = stem.chars().map(|c| if c.is_alphanumeric() || c == '_' { c } else { '_' }).collect();
    if out.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(true) {
        out.insert(0, '_');
    }
    out
}

/// Keep only the given column indexes, in that order.
pub fn project(batch: &RecordBatch, indexes: &[usize]) -> Result<RecordBatch> {
    Ok(batch.project(indexes)?)
}

#[allow(dead_code)]
fn _schema_type_check(_: &Schema, _: TimeUnit) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn csv_inspect_and_batches() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("people.csv");
        let mut f = File::create(&p).unwrap();
        writeln!(f, "id,name,joined,score,active").unwrap();
        for i in 0..250 {
            writeln!(f, "{i},Person {i},2024-01-{:02},{}.5,{}", (i % 28) + 1, i, i % 2 == 0).unwrap();
        }
        drop(f);
        let fmt = FileFormat::sniff(&p);
        assert_eq!(fmt, FileFormat::Csv { delimiter: b',', has_header: true });
        let ins = inspect(&p, &fmt, 100, 5).unwrap();
        assert_eq!(ins.columns.len(), 5);
        assert_eq!(ins.columns[0].sql_type, SqlType::BigInt);
        assert!(matches!(ins.columns[1].sql_type, SqlType::NVarChar { len: Some(50) }));
        assert_eq!(ins.columns[2].sql_type, SqlType::Date);
        assert_eq!(ins.columns[3].sql_type, SqlType::Float);
        assert_eq!(ins.columns[4].sql_type, SqlType::Bit);
        assert_eq!(ins.preview.len(), 5);
        assert_eq!(ins.preview[1][1], "Person 1");
        let est = ins.row_estimate.unwrap();
        assert!((200..=300).contains(&est), "estimate {est}");
        let total: usize = open_batches(&p, &fmt, ins.schema.clone(), 100).unwrap().map(|b| b.unwrap().num_rows()).sum();
        assert_eq!(total, 250);
    }

    #[test]
    fn delimiter_sniff_and_types() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.txt");
        std::fs::write(&p, "a;b;c\n1;2;3\n").unwrap();
        assert_eq!(FileFormat::sniff(&p), FileFormat::Csv { delimiter: b';', has_header: true });
        assert_eq!(parse_sql_type("nvarchar(100)"), Some(SqlType::NVarChar { len: Some(100) }));
        assert_eq!(parse_sql_type("NVARCHAR(MAX)"), Some(SqlType::NVarChar { len: None }));
        assert_eq!(parse_sql_type("decimal(18, 2)"), Some(SqlType::Decimal { precision: 18, scale: 2 }));
        assert_eq!(parse_sql_type("datetime2"), Some(SqlType::DateTime2 { scale: 7 }));
        assert_eq!(parse_sql_type("geography"), None);
        let ddl = create_table_sql("dbo", "t", &[("id".into(), SqlType::Int, false), ("n]ame".into(), SqlType::NVarChar { len: Some(50) }, true)]);
        assert!(ddl.starts_with("CREATE TABLE [dbo].[t] ("));
        assert!(ddl.contains("[n]]ame] nvarchar(50) NULL"));
        assert_eq!(table_name_from(Path::new("C:/data/2024 sales.csv")), "_2024_sales");
    }
}
