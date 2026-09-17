//! Result-set exporters for Cobalt SQL Works.
//!
//! Every exporter streams from the Arrow batches of a [`ResultSet`] (visible rows, in view
//! order) so the UI stays responsive and memory stays flat. File exporters write to a temp file
//! next to the target and rename on success; a cancelled or failed export leaves nothing behind.
//!
//! The [`text`] module holds the clipboard builders (TSV / CSV / Markdown / JSON / INSERT / IN-list)
//! that operate on a rectangular [`text::Selection`].

pub mod text;

mod arrow_file;
mod delimited;
mod excel;
mod json_file;
mod markdown;
mod parquet_file;
mod xml_file;

use cobalt_core::ExportSettings;
use cobalt_results::{CellFormatter, ResultSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Rows per output batch when gathering visible rows through a sort/filter view.
pub const EXPORT_BATCH_ROWS: usize = 8192;

/// Excel's hard row limit (1,048,576 rows including the header).
pub const EXCEL_MAX_ROWS: usize = 1_048_576;

/// The ISO-ish datetime format every exporter uses regardless of the grid's display setting.
pub const EXPORT_DATETIME_FORMAT: &str = "%Y-%m-%d %H:%M:%S%.f";

// ---------------------------------------------------------------------------------------------
// Format
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Format {
    Csv,
    Tsv,
    Json,
    JsonLines,
    Xml,
    Markdown,
    Excel,
    Parquet,
    ArrowIpc,
}

impl Format {
    pub fn extension(&self) -> &'static str {
        match self {
            Format::Csv => "csv",
            Format::Tsv => "tsv",
            Format::Json => "json",
            Format::JsonLines => "jsonl",
            Format::Xml => "xml",
            Format::Markdown => "md",
            Format::Excel => "xlsx",
            Format::Parquet => "parquet",
            Format::ArrowIpc => "arrow",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Format::Csv => "CSV",
            Format::Tsv => "TSV",
            Format::Json => "JSON",
            Format::JsonLines => "JSON Lines",
            Format::Xml => "XML",
            Format::Markdown => "Markdown table",
            Format::Excel => "Excel workbook",
            Format::Parquet => "Parquet",
            Format::ArrowIpc => "Arrow IPC (Feather v2)",
        }
    }

    pub fn all() -> &'static [Format] {
        &[
            Format::Csv,
            Format::Tsv,
            Format::Excel,
            Format::Json,
            Format::JsonLines,
            Format::Xml,
            Format::Markdown,
            Format::Parquet,
            Format::ArrowIpc,
        ]
    }

    /// Case-insensitive; accepts the common aliases (`tab`, `ndjson`, `markdown`, `feather`, `ipc`).
    pub fn from_extension(ext: &str) -> Option<Format> {
        let ext = ext.trim().trim_start_matches('.').to_ascii_lowercase();
        Some(match ext.as_str() {
            "csv" => Format::Csv,
            "tsv" | "tab" => Format::Tsv,
            "json" => Format::Json,
            "jsonl" | "ndjson" => Format::JsonLines,
            "xml" => Format::Xml,
            "md" | "markdown" => Format::Markdown,
            "xlsx" => Format::Excel,
            "parquet" | "pq" => Format::Parquet,
            "arrow" | "feather" | "ipc" => Format::ArrowIpc,
            _ => return None,
        })
    }

    /// Text formats can be streamed to any `Write` (see [`export_to_writer`]).
    pub fn is_text(&self) -> bool {
        matches!(self, Format::Csv | Format::Tsv | Format::Json | Format::JsonLines | Format::Xml | Format::Markdown)
    }
}

impl std::fmt::Display for Format {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

// ---------------------------------------------------------------------------------------------
// Options
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum LineEnding {
    #[default]
    Crlf,
    Lf,
}

impl LineEnding {
    pub fn as_str(&self) -> &'static str {
        match self {
            LineEnding::Crlf => "\r\n",
            LineEnding::Lf => "\n",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Encoding {
    #[default]
    Utf8,
    Utf16Le,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CsvOptions {
    pub delimiter: u8,
    pub quote_all: bool,
    pub include_headers: bool,
    pub line_ending: LineEnding,
    /// Write a byte-order mark. Always written for UTF-16.
    pub bom: bool,
    pub null_as: String,
    pub encoding: Encoding,
}

impl Default for CsvOptions {
    fn default() -> Self {
        Self { delimiter: b',', quote_all: false, include_headers: true, line_ending: LineEnding::Crlf, bom: false, null_as: String::new(), encoding: Encoding::Utf8 }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct JsonOptions {
    pub pretty: bool,
    /// Newline-delimited objects instead of an array (also selected by [`Format::JsonLines`]).
    pub lines: bool,
    /// `true`: nulls as JSON `null`; `false`: the key is omitted.
    pub null_as_null: bool,
    /// `true`: temporal values as ISO 8601 (`2024-01-02T03:04:05.123`); `false`: the grid's display format.
    pub dates_as_iso: bool,
}

impl Default for JsonOptions {
    fn default() -> Self {
        Self { pretty: true, lines: false, null_as_null: true, dates_as_iso: true }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct XmlOptions {
    pub root_element: String,
    pub row_element: String,
    /// `<row col="v"/>` instead of `<row><col>v</col></row>`.
    pub attribute_style: bool,
    /// Indent with two spaces; otherwise a single line.
    pub formatted: bool,
    /// Emit `<!-- columns: name type, ... -->` after the declaration.
    pub include_schema_comment: bool,
}

impl Default for XmlOptions {
    fn default() -> Self {
        Self { root_element: "rows".into(), row_element: "row".into(), attribute_style: false, formatted: true, include_schema_comment: false }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct MarkdownOptions {
    pub include_headers: bool,
    pub align_numbers_right: bool,
    pub escape_pipes: bool,
    pub null_as: String,
}

impl Default for MarkdownOptions {
    fn default() -> Self {
        Self { include_headers: true, align_numbers_right: true, escape_pipes: true, null_as: "NULL".into() }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExcelOptions {
    pub sheet_name: String,
    pub freeze_header: bool,
    pub autofilter: bool,
    pub bold_header: bool,
    pub autofit: bool,
    /// Numbers/dates/booleans as native Excel types with number formats; otherwise everything is text.
    pub native_types: bool,
    /// Row count above which the export is refused with [`ExportError::TooManyRows`].
    pub max_rows_warn: usize,
}

impl Default for ExcelOptions {
    fn default() -> Self {
        Self { sheet_name: "Results".into(), freeze_header: true, autofilter: true, bold_header: true, autofit: true, native_types: true, max_rows_warn: EXCEL_MAX_ROWS }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Compression {
    None,
    Snappy,
    #[default]
    Zstd,
    Lz4,
}

impl Compression {
    pub fn parse(s: &str) -> Option<Compression> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "none" | "uncompressed" | "" => Compression::None,
            "snappy" => Compression::Snappy,
            "zstd" => Compression::Zstd,
            "lz4" | "lz4_raw" => Compression::Lz4,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParquetOptions {
    pub compression: Compression,
    pub row_group_rows: usize,
    pub statistics: bool,
}

impl Default for ParquetOptions {
    fn default() -> Self {
        Self { compression: Compression::Zstd, row_group_rows: 131_072, statistics: true }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IpcCompression {
    Lz4,
    Zstd,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct ArrowOptions {
    pub compression: Option<IpcCompression>,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct ExportOptions {
    pub csv: CsvOptions,
    pub json: JsonOptions,
    pub xml: XmlOptions,
    pub markdown: MarkdownOptions,
    pub excel: ExcelOptions,
    pub parquet: ParquetOptions,
    pub arrow: ArrowOptions,
}

impl ExportOptions {
    pub fn from_settings(s: &ExportSettings) -> Self {
        let delimiter = match s.csv_delimiter.as_str() {
            "\\t" | "tab" | "\t" => b'\t',
            other => other.bytes().next().unwrap_or(b','),
        };
        let line_ending = match s.csv_line_ending.to_ascii_lowercase().as_str() {
            "\n" | "lf" | "\\n" => LineEnding::Lf,
            _ => LineEnding::Crlf,
        };
        Self {
            csv: CsvOptions {
                delimiter,
                quote_all: s.csv_quote_all,
                include_headers: s.csv_include_headers,
                line_ending,
                bom: s.csv_bom,
                null_as: s.csv_null_as.clone(),
                encoding: Encoding::Utf8,
            },
            json: JsonOptions { pretty: s.json_pretty, lines: s.json_lines, ..Default::default() },
            xml: XmlOptions {
                row_element: if s.xml_row_element.trim().is_empty() { "row".into() } else { s.xml_row_element.clone() },
                attribute_style: s.xml_attribute_style,
                ..Default::default()
            },
            markdown: MarkdownOptions::default(),
            excel: ExcelOptions { freeze_header: s.excel_freeze_header, autofilter: s.excel_autofilter, bold_header: s.excel_bold_header, ..Default::default() },
            parquet: ParquetOptions {
                compression: Compression::parse(&s.parquet_compression).unwrap_or_default(),
                row_group_rows: if s.parquet_row_group_rows == 0 { 131_072 } else { s.parquet_row_group_rows },
                statistics: true,
            },
            arrow: ArrowOptions::default(),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Progress / stats / errors
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Progress {
    pub rows_done: usize,
    pub rows_total: usize,
    pub bytes_written: u64,
}

#[derive(Clone, Debug, Default)]
pub struct ExportStats {
    pub rows: usize,
    pub bytes: u64,
    pub elapsed: Duration,
    /// Empty for [`export_to_writer`].
    pub path: PathBuf,
    /// Non-fatal notes (e.g. strings truncated for Excel).
    pub warnings: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("arrow error: {0}")]
    Arrow(#[from] arrow::error::ArrowError),
    #[error("parquet error: {0}")]
    Parquet(#[from] parquet::errors::ParquetError),
    #[error("excel error: {0}")]
    Excel(#[from] rust_xlsxwriter::XlsxError),
    #[error("csv error: {0}")]
    Csv(#[from] csv::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("result set error: {0}")]
    Results(#[from] cobalt_results::ResultError),
    #[error("{rows} rows exceed the limit of {limit} rows for this format")]
    TooManyRows { rows: usize, limit: usize },
    #[error("export cancelled")]
    Cancelled,
    #[error("{0}")]
    Unsupported(String),
}

pub type Result<T> = std::result::Result<T, ExportError>;

// ---------------------------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------------------------

/// Export the visible rows of `rs` to `path`. Writes to a temp file in the same directory and
/// renames on success; the partial file is deleted on error or cancel.
///
/// `progress` is polled before the first batch and after every batch; returning `false` cancels.
pub fn export_to_file(
    rs: &ResultSet,
    format: Format,
    path: &Path,
    opts: &ExportOptions,
    fmt: &CellFormatter,
    progress: &mut dyn FnMut(Progress) -> bool,
) -> Result<ExportStats> {
    let started = Instant::now();
    if format == Format::Excel {
        let rows = rs.visible_count();
        let limit = opts.excel.max_rows_warn.min(EXCEL_MAX_ROWS - 1);
        if rows > limit {
            return Err(ExportError::TooManyRows { rows, limit });
        }
    }
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
    std::fs::create_dir_all(&dir)?;
    let tmp = temp_path(path);
    let result = (|| -> Result<(usize, u64, Vec<String>)> {
        let counter = Arc::new(AtomicU64::new(0));
        let ctx = Ctx { rs, opts, fmt: export_formatter(fmt), bytes: counter.clone() };
        if format == Format::Excel {
            // rust_xlsxwriter needs a seekable target; it writes the file itself.
            let (rows, warnings) = excel::write_file(&ctx, &tmp, progress)?;
            let bytes = std::fs::metadata(&tmp).map(|m| m.len()).unwrap_or(0);
            return Ok((rows, bytes, warnings));
        }
        let file = std::fs::File::create(&tmp)?;
        let mut sink = CountingWriter::new(std::io::BufWriter::with_capacity(1 << 16, file), counter);
        let (rows, warnings) = write_any(&ctx, format, &mut sink, progress)?;
        sink.flush()?;
        let bytes = ctx.bytes.load(Ordering::Relaxed);
        drop(sink);
        Ok((rows, bytes, warnings))
    })();
    match result {
        Ok((rows, bytes, warnings)) => {
            if path.exists() {
                std::fs::remove_file(path)?;
            }
            std::fs::rename(&tmp, path)?;
            tracing::info!(?path, rows, bytes, format = %format, "export complete");
            Ok(ExportStats { rows, bytes, elapsed: started.elapsed(), path: path.to_path_buf(), warnings })
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Stream the visible rows to any writer. Supports every format except Excel (which needs `Seek`).
/// `stats.path` is empty.
pub fn export_to_writer<W: Write + Send>(
    rs: &ResultSet,
    format: Format,
    writer: W,
    opts: &ExportOptions,
    fmt: &CellFormatter,
    progress: &mut dyn FnMut(Progress) -> bool,
) -> Result<ExportStats> {
    if format == Format::Excel {
        return Err(ExportError::Unsupported("Excel export needs a seekable file; use export_to_file".into()));
    }
    let started = Instant::now();
    let counter = Arc::new(AtomicU64::new(0));
    let mut sink = CountingWriter::new(writer, counter.clone());
    let ctx = Ctx { rs, opts, fmt: export_formatter(fmt), bytes: counter };
    let (rows, warnings) = write_any(&ctx, format, &mut sink, progress)?;
    sink.flush()?;
    Ok(ExportStats { rows, bytes: ctx.bytes.load(Ordering::Relaxed), elapsed: started.elapsed(), path: PathBuf::new(), warnings })
}

/// The formatter every exporter uses: the caller's settings (NULL text, bit style) but never
/// truncated and with ISO datetimes.
pub fn export_formatter(fmt: &CellFormatter) -> CellFormatter {
    fmt.clone().with_max_chars(0).with_datetime_format(EXPORT_DATETIME_FORMAT)
}

fn write_any<W: Write + Send>(ctx: &Ctx<'_>, format: Format, sink: &mut CountingWriter<W>, progress: &mut dyn FnMut(Progress) -> bool) -> Result<(usize, Vec<String>)> {
    match format {
        Format::Csv => delimited::write(ctx, sink, progress, ctx.opts.csv.delimiter),
        Format::Tsv => delimited::write(ctx, sink, progress, b'\t'),
        Format::Json => json_file::write(ctx, sink, progress, ctx.opts.json.lines),
        Format::JsonLines => json_file::write(ctx, sink, progress, true),
        Format::Xml => xml_file::write(ctx, sink, progress),
        Format::Markdown => markdown::write(ctx, sink, progress),
        Format::Parquet => parquet_file::write(ctx, sink, progress),
        Format::ArrowIpc => arrow_file::write(ctx, sink, progress),
        Format::Excel => Err(ExportError::Unsupported("Excel export needs a seekable file; use export_to_file".into())),
    }
}

fn temp_path(path: &Path) -> PathBuf {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "export".into());
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
    path.with_file_name(format!(".{name}.{}.{nanos}.partial", std::process::id()))
}

// ---------------------------------------------------------------------------------------------
// Shared plumbing for the format modules
// ---------------------------------------------------------------------------------------------

/// Everything a format writer needs.
pub(crate) struct Ctx<'a> {
    pub rs: &'a ResultSet,
    pub opts: &'a ExportOptions,
    pub fmt: CellFormatter,
    pub bytes: Arc<AtomicU64>,
}

impl Ctx<'_> {
    /// Unique column names as used for JSON keys / XML names (`rs.schema` already de-duplicates).
    pub fn names(&self) -> Vec<String> {
        self.rs.schema.fields().iter().map(|f| f.name().clone()).collect()
    }

    /// Drive `f` over every visible batch, reporting progress after each; `Cancelled` when the
    /// callback returns false.
    pub fn for_each_batch(&self, progress: &mut dyn FnMut(Progress) -> bool, mut f: impl FnMut(&arrow::array::RecordBatch) -> Result<()>) -> Result<usize> {
        let total = self.rs.visible_count();
        let mut done = 0usize;
        if !progress(Progress { rows_done: 0, rows_total: total, bytes_written: 0 }) {
            return Err(ExportError::Cancelled);
        }
        for batch in self.rs.view_batches(EXPORT_BATCH_ROWS) {
            let batch = batch?;
            f(&batch)?;
            done += batch.num_rows();
            if !progress(Progress { rows_done: done, rows_total: total, bytes_written: self.bytes.load(Ordering::Relaxed) }) {
                return Err(ExportError::Cancelled);
            }
        }
        Ok(done)
    }

    /// Formatted text for every column of a batch.
    pub fn format_batch(&self, batch: &arrow::array::RecordBatch, fmt: &CellFormatter) -> Vec<Arc<Vec<Arc<str>>>> {
        (0..batch.num_columns()).map(|c| fmt.format_column(batch.column(c), &self.rs.columns[c])).collect()
    }

    /// SQL type names, one per column, as JSON (`["int","nvarchar(50)"]`) for file metadata.
    pub fn sql_types_json(&self) -> String {
        serde_json::to_string(&self.rs.columns.iter().map(|c| c.sql_type.to_string()).collect::<Vec<_>>()).unwrap_or_default()
    }
}

/// A `Write` adapter that counts bytes into a shared counter (so progress can report bytes even
/// while the underlying writer is owned by a csv/parquet writer).
pub(crate) struct CountingWriter<W> {
    inner: W,
    bytes: Arc<AtomicU64>,
}

impl<W: Write> CountingWriter<W> {
    fn new(inner: W, bytes: Arc<AtomicU64>) -> Self {
        Self { inner, bytes }
    }
}

impl<W: Write> Write for CountingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.bytes.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Deduplicate a list of names by appending `_2`, `_3`, … (used where a schema is built from
/// user-visible column names rather than `rs.schema`).
pub(crate) fn dedupe_names<'a>(names: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut seen: std::collections::HashMap<String, usize> = Default::default();
    let mut out = Vec::new();
    for n in names {
        let count = seen.entry(n.to_string()).or_insert(0);
        *count += 1;
        out.push(if *count == 1 { n.to_string() } else { format!("{n}_{count}") });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_roundtrip() {
        for f in Format::all() {
            assert_eq!(Format::from_extension(f.extension()), Some(*f));
            assert_eq!(Format::from_extension(&f.extension().to_uppercase()), Some(*f));
        }
        assert_eq!(Format::from_extension(".ndjson"), Some(Format::JsonLines));
        assert_eq!(Format::from_extension("feather"), Some(Format::ArrowIpc));
        assert_eq!(Format::from_extension("docx"), None);
    }

    #[test]
    fn options_from_settings() {
        let mut s = ExportSettings::default();
        s.csv_delimiter = "\\t".into();
        s.csv_line_ending = "\n".into();
        s.parquet_compression = "snappy".into();
        s.parquet_row_group_rows = 0;
        let o = ExportOptions::from_settings(&s);
        assert_eq!(o.csv.delimiter, b'\t');
        assert_eq!(o.csv.line_ending, LineEnding::Lf);
        assert_eq!(o.parquet.compression, Compression::Snappy);
        assert_eq!(o.parquet.row_group_rows, 131_072);
        let d = ExportOptions::from_settings(&ExportSettings::default());
        assert_eq!(d.csv.delimiter, b',');
        assert_eq!(d.csv.line_ending, LineEnding::Crlf);
        assert_eq!(d.parquet.compression, Compression::Zstd);
        assert!(d.json.pretty && !d.json.lines);
    }

    #[test]
    fn dedupe() {
        assert_eq!(dedupe_names(["a", "a", "b", "a"]), vec!["a", "a_2", "b", "a_3"]);
    }
}
