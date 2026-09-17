//! Write every format to a temp dir and read it back with an independent reader.

use arrow::array::*;
use arrow::datatypes::*;
use cobalt_core::{ColumnInfo, SqlType};
use cobalt_export::text::Selection;
use cobalt_export::*;
use cobalt_results::{CellFormatter, CellValue, ColumnFilter, FilterOp, MemoryBudget, ResultSet, RunState, SortKey, ViewSpec};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const ROWS: usize = 10_000;
const BATCHES: [usize; 3] = [4000, 4000, 2000];

fn columns() -> Vec<ColumnInfo> {
    let c = |n: &str, t: SqlType, o: usize| ColumnInfo::new(n, t, true, o);
    vec![
        c("b", SqlType::Bit, 0),
        c("ti", SqlType::TinyInt, 1),
        c("si", SqlType::SmallInt, 2),
        c("i", SqlType::Int, 3),
        c("bi", SqlType::BigInt, 4),
        c("r", SqlType::Real, 5),
        c("f", SqlType::Float, 6),
        c("d", SqlType::Decimal { precision: 18, scale: 4 }, 7),
        c("s", SqlType::NVarChar { len: Some(50) }, 8),
        c("long text", SqlType::NVarChar { len: None }, 9),
        c("bin", SqlType::VarBinary { len: Some(8) }, 10),
        c("dt", SqlType::Date, 11),
        c("t", SqlType::Time { scale: 7 }, 12),
        c("dtm", SqlType::DateTime, 13),
        c("dt2", SqlType::DateTime2 { scale: 7 }, 14),
        c("dto", SqlType::DateTimeOffset { scale: 7 }, 15),
        c("s", SqlType::VarChar { len: Some(10) }, 16),
    ]
}

fn opt<T>(idx: &[usize], f: impl Fn(usize) -> T) -> Vec<Option<T>> {
    idx.iter().map(|&i| if is_null(i) { None } else { Some(f(i)) }).collect()
}

fn is_null(i: usize) -> bool {
    i % 7 == 3
}

fn text_for(i: usize) -> &'static str {
    match i % 5 {
        0 => "plain",
        1 => "has \"quotes\"",
        2 => "com,ma",
        3 => "line1\nline2",
        _ => "ünïcödé | pipe",
    }
}

fn batch(schema: &SchemaRef, start: usize, len: usize) -> RecordBatch {
    let idx: Vec<usize> = (start..start + len).collect();
    let cols: Vec<ArrayRef> = vec![
        Arc::new(BooleanArray::from(opt(&idx, |i| i % 2 == 0))),
        Arc::new(UInt8Array::from(opt(&idx, |i| (i % 256) as u8))),
        Arc::new(Int16Array::from(opt(&idx, |i| (i % 30000) as i16 - 15000))),
        Arc::new(Int32Array::from(opt(&idx, |i| i as i32))),
        Arc::new(Int64Array::from(opt(&idx, |i| i as i64 * 1_000_000_007))),
        Arc::new(Float32Array::from(opt(&idx, |i| i as f32 * 0.5))),
        Arc::new(Float64Array::from(opt(&idx, |i| i as f64 / 4.0))),
        Arc::new(Decimal128Array::from(opt(&idx, |i| i as i128 * 12345)).with_precision_and_scale(18, 4).unwrap()),
        Arc::new(StringArray::from(idx.iter().map(|&i| if is_null(i) || i % 11 == 0 { None } else { Some(text_for(i)) }).collect::<Vec<_>>())),
        Arc::new(LargeStringArray::from(opt(&idx, |i| format!("long text {i} ").repeat(3)))),
        Arc::new(BinaryArray::from(opt(&idx, |i| (i as u32).to_le_bytes().to_vec()).iter().map(|o| o.as_deref()).collect::<Vec<_>>())),
        Arc::new(Date32Array::from(opt(&idx, |i| 19000 + (i % 1000) as i32))),
        Arc::new(Time64NanosecondArray::from(opt(&idx, |i| (i % 86400) as i64 * 1_000_000_000 + 123_456_700))),
        Arc::new(TimestampMillisecondArray::from(opt(&idx, |i| 1_704_164_645_000 + i as i64 * 1000))),
        Arc::new(TimestampNanosecondArray::from(opt(&idx, |i| 1_704_164_645_123_456_700 + i as i64 * 1_000_000_000))),
        Arc::new(TimestampNanosecondArray::from(opt(&idx, |i| 1_704_164_645_123_456_700 + i as i64 * 1_000_000_000)).with_timezone("UTC")),
        Arc::new(StringArray::from(opt(&idx, |i| format!("x{i}")))),
    ];
    RecordBatch::try_new(schema.clone(), cols).unwrap()
}

fn fixture() -> Arc<ResultSet> {
    let rs = ResultSet::new(0, columns(), Arc::new(MemoryBudget::unlimited()), std::env::temp_dir());
    let mut start = 0;
    for len in BATCHES {
        rs.append(batch(&rs.schema, start, len)).unwrap();
        start += len;
    }
    rs.set_state(RunState::Complete);
    rs
}

fn small() -> Arc<ResultSet> {
    let rs = ResultSet::new(0, columns(), Arc::new(MemoryBudget::unlimited()), std::env::temp_dir());
    rs.append(batch(&rs.schema, 0, 12)).unwrap();
    rs.set_state(RunState::Complete);
    rs
}

fn empty() -> Arc<ResultSet> {
    ResultSet::from_batches(0, columns(), vec![])
}

fn ok(_: Progress) -> bool {
    true
}

fn export(rs: &ResultSet, format: Format, dir: &Path, opts: &ExportOptions) -> (PathBuf, ExportStats) {
    let path = dir.join(format!("out.{}", format.extension()));
    let stats = export_to_file(rs, format, &path, opts, &CellFormatter::default(), &mut ok).unwrap();
    assert!(path.exists());
    assert_eq!(stats.path, path);
    assert!(no_partials(dir));
    (path, stats)
}

fn no_partials(dir: &Path) -> bool {
    !std::fs::read_dir(dir).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().ends_with(".partial"))
}

// ---------------------------------------------------------------------------------------------
// CSV / TSV
// ---------------------------------------------------------------------------------------------

#[test]
fn csv_roundtrip_default() {
    let dir = tempfile::tempdir().unwrap();
    let rs = fixture();
    let (path, stats) = export(&rs, Format::Csv, dir.path(), &ExportOptions::default());
    assert_eq!(stats.rows, ROWS);
    assert!(stats.bytes > 0);
    assert_eq!(stats.bytes, std::fs::metadata(&path).unwrap().len());
    let raw = std::fs::read(&path).unwrap();
    assert!(!raw.starts_with(&[0xEF, 0xBB, 0xBF]));
    assert!(raw.windows(2).filter(|w| w == b"\r\n").count() >= ROWS);
    let mut rdr = csv::ReaderBuilder::new().has_headers(true).from_path(&path).unwrap();
    let headers: Vec<String> = rdr.headers().unwrap().iter().map(String::from).collect();
    assert_eq!(headers.len(), 17);
    assert_eq!(headers[8], "s");
    assert_eq!(headers[16], "s");
    assert_eq!(headers[9], "long text");
    let records: Vec<csv::StringRecord> = rdr.records().map(|r| r.unwrap()).collect();
    assert_eq!(records.len(), ROWS);
    let r1 = &records[1];
    assert_eq!(&r1[0], "0"); // bit as number (default setting)
    assert_eq!(&r1[1], "1");
    assert_eq!(&r1[3], "1");
    assert_eq!(&r1[7], "1.2345");
    assert_eq!(&r1[8], "has \"quotes\"");
    assert_eq!(&r1[11], "2022-01-09");
    assert_eq!(&r1[12], "00:00:01.1234567");
    assert_eq!(&r1[13], "2024-01-02 03:04:06");
    assert_eq!(&r1[14], "2024-01-02 03:04:06.1234567");
    assert_eq!(&r1[15], "2024-01-02 03:04:06.1234567");
    assert_eq!(&r1[16], "x1");
    assert_eq!(&records[2][8], "com,ma");
    assert_eq!(&records[8][8], "line1\nline2");
    assert_eq!(&records[4][8], "ünïcödé | pipe");
    assert_eq!(&records[0][12], "00:00:00.1234567");
    assert_eq!(&records[0][10], "0x00000000");
    // NULL row: every field empty by default.
    let r3 = &records[3];
    assert!(r3.iter().all(|f| f.is_empty()), "{r3:?}");
    // Last row of the last batch is a NULL row (9999 % 7 == 3); the one before carries data.
    assert_eq!(&records[ROWS - 1][3], "");
    assert_eq!(&records[ROWS - 2][3], "9998");
}

#[test]
fn csv_options_delimiter_quoteall_lf_bom_null() {
    let dir = tempfile::tempdir().unwrap();
    let rs = small();
    let opts = ExportOptions {
        csv: CsvOptions { delimiter: b';', quote_all: true, include_headers: false, line_ending: LineEnding::Lf, bom: true, null_as: "NULL".into(), encoding: Encoding::Utf8 },
        ..Default::default()
    };
    let (path, _) = export(&rs, Format::Csv, dir.path(), &opts);
    let raw = std::fs::read(&path).unwrap();
    assert!(raw.starts_with(&[0xEF, 0xBB, 0xBF]));
    let text = String::from_utf8(raw[3..].to_vec()).unwrap();
    assert!(!text.contains("\r\n"));
    let first = text.lines().next().unwrap();
    assert!(first.starts_with("\"1\";\"0\";\"-15000\";\"0\";"), "{first}");
    assert_eq!(text.lines().count(), 13); // 12 rows, one of which has an embedded newline
    assert!(text.contains("\"NULL\";\"NULL\";"));
    let mut rdr = csv::ReaderBuilder::new().delimiter(b';').has_headers(false).from_reader(text.as_bytes());
    let n = rdr.records().collect::<std::result::Result<Vec<_>, csv::Error>>().unwrap().len();
    assert_eq!(n, 12);
}

#[test]
fn csv_utf16le() {
    let dir = tempfile::tempdir().unwrap();
    let rs = small();
    let mut opts = ExportOptions::default();
    opts.csv.encoding = Encoding::Utf16Le;
    let (path, _) = export(&rs, Format::Csv, dir.path(), &opts);
    let raw = std::fs::read(&path).unwrap();
    assert!(raw.starts_with(&[0xFF, 0xFE]));
    let units: Vec<u16> = raw[2..].chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    let text = String::from_utf16(&units).unwrap();
    assert!(text.starts_with("b,ti,si,i,"));
    assert!(text.contains("ünïcödé | pipe"));
}

#[test]
fn tsv_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let rs = small();
    let (path, _) = export(&rs, Format::Tsv, dir.path(), &ExportOptions::default());
    let mut rdr = csv::ReaderBuilder::new().delimiter(b'\t').has_headers(true).from_path(&path).unwrap();
    let records: Vec<csv::StringRecord> = rdr.records().map(|r| r.unwrap()).collect();
    assert_eq!(records.len(), 12);
    assert_eq!(&records[2][8], "com,ma");
    assert_eq!(&records[8][8], "line1\nline2");
}

// ---------------------------------------------------------------------------------------------
// JSON
// ---------------------------------------------------------------------------------------------

#[test]
fn json_pretty_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let rs = fixture();
    let (path, stats) = export(&rs, Format::Json, dir.path(), &ExportOptions::default());
    assert_eq!(stats.rows, ROWS);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.starts_with("[\n  {\n"));
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    let arr = v.as_array().unwrap();
    assert_eq!(arr.len(), ROWS);
    let r1 = &arr[1];
    assert_eq!(r1["b"], false);
    assert_eq!(r1["ti"], 1);
    assert_eq!(r1["bi"], 1_000_000_007i64);
    assert_eq!(r1["f"], 0.25);
    assert_eq!(r1["d"], 1.2345);
    assert_eq!(r1["s"], "has \"quotes\"");
    assert_eq!(r1["s_2"], "x1");
    assert_eq!(r1["bin"], "0x01000000");
    assert_eq!(r1["dt"], "2022-01-09");
    assert_eq!(r1["t"], "00:00:01.123456700");
    assert_eq!(r1["dtm"], "2024-01-02T03:04:06");
    assert_eq!(r1["dt2"], "2024-01-02T03:04:06.123456700");
    let dto = r1["dto"].as_str().unwrap();
    assert!(dto.starts_with("2024-01-02T03:04:06.1234567") && dto.ends_with('Z'), "{dto}");
    assert!(arr[3]["i"].is_null());
    assert!(arr[3].as_object().unwrap().contains_key("i"));
    assert_eq!(arr[8]["s"], "line1\nline2");
}

#[test]
fn json_compact_omit_nulls_display_dates() {
    let dir = tempfile::tempdir().unwrap();
    let rs = small();
    let opts = ExportOptions { json: JsonOptions { pretty: false, lines: false, null_as_null: false, dates_as_iso: false }, ..Default::default() };
    let (path, _) = export(&rs, Format::Json, dir.path(), &opts);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.starts_with("[{\"b\":"));
    assert!(!text.contains('\n') || text.matches('\n').count() <= 2);
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    let arr = v.as_array().unwrap();
    assert_eq!(arr.len(), 12);
    assert!(!arr[3].as_object().unwrap().contains_key("i"));
    assert_eq!(arr[1]["dtm"], "2024-01-02 03:04:06");
    assert_eq!(arr[1]["dt2"], "2024-01-02 03:04:06.1234567");
}

#[test]
fn jsonl_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let rs = fixture();
    let (path, _) = export(&rs, Format::JsonLines, dir.path(), &ExportOptions::default());
    let text = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), ROWS);
    for (i, l) in lines.iter().enumerate().step_by(997) {
        let v: serde_json::Value = serde_json::from_str(l).unwrap();
        if is_null(i) {
            assert!(v["i"].is_null());
        } else {
            assert_eq!(v["i"], i as u64);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// XML
// ---------------------------------------------------------------------------------------------

fn count_xml(path: &Path, name: &str) -> (usize, usize, usize) {
    use quick_xml::events::Event;
    let text = std::fs::read_to_string(path).unwrap();
    let mut reader = quick_xml::Reader::from_str(&text);
    let (mut starts, mut empties, mut nils) = (0, 0, 0);
    loop {
        match reader.read_event().unwrap() {
            Event::Eof => break,
            Event::Start(e) if e.name().as_ref() == name => starts += 1,
            Event::Empty(e) => {
                if e.name().as_ref() == name {
                    empties += 1;
                }
                if e.attributes().any(|a| a.unwrap().key.as_ref() == "xsi:nil") {
                    nils += 1;
                }
            }
            _ => {}
        }
    }
    (starts, empties, nils)
}

#[test]
fn xml_element_style() {
    let dir = tempfile::tempdir().unwrap();
    let rs = fixture();
    let (path, stats) = export(&rs, Format::Xml, dir.path(), &ExportOptions::default());
    assert_eq!(stats.rows, ROWS);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.starts_with("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<rows xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\">\n  <row>\n    <b>1</b>"), "{}", &text[..200]);
    assert!(text.contains("<long_text>"));
    assert!(text.contains("<s_2>x1</s_2>"));
    assert!(text.contains("<s>has &quot;quotes&quot;</s>") || text.contains("<s>has \"quotes\"</s>"));
    assert!(text.contains("<s>ünïcödé | pipe</s>"));
    assert!(text.ends_with("</rows>\n"));
    let (rows, _, nils) = count_xml(&path, "row");
    assert_eq!(rows, ROWS);
    let null_rows = (0..ROWS).filter(|&i| is_null(i)).count();
    let extra_s_nulls = (0..ROWS).filter(|&i| !is_null(i) && i % 11 == 0).count();
    assert_eq!(nils, null_rows * 17 + extra_s_nulls);
}

#[test]
fn xml_attribute_style_single_line_with_comment() {
    let dir = tempfile::tempdir().unwrap();
    let rs = small();
    let opts = ExportOptions {
        xml: XmlOptions { root_element: "data set".into(), row_element: "r".into(), attribute_style: true, formatted: false, include_schema_comment: true },
        ..Default::default()
    };
    let (path, _) = export(&rs, Format::Xml, dir.path(), &opts);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.starts_with("<?xml version=\"1.0\" encoding=\"utf-8\"?><!-- columns: b bit, ti tinyint,"), "{}", &text[..120]);
    assert!(text.contains("<data_set><r b=\"1\" ti=\"0\" si=\"-15000\" i=\"0\""));
    assert!(text.contains("s=\"has &quot;quotes&quot;\"") );
    assert!(!text.contains('\n') || text.matches('\n').count() <= 1, "should be single line");
    assert!(text.ends_with("</data_set>"));
    let (_, empties, nils) = count_xml(&path, "r");
    assert_eq!(empties, 12);
    assert_eq!(nils, 0);
    // The all-NULL row has no attributes at all.
    assert!(text.contains("<r/>"));
}

// ---------------------------------------------------------------------------------------------
// Markdown
// ---------------------------------------------------------------------------------------------

#[test]
fn markdown_table() {
    let dir = tempfile::tempdir().unwrap();
    let rs = small();
    let (path, _) = export(&rs, Format::Markdown, dir.path(), &ExportOptions::default());
    let text = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 14);
    assert_eq!(lines[0], "| b | ti | si | i | bi | r | f | d | s | long text | bin | dt | t | dtm | dt2 | dto | s |");
    assert_eq!(lines[1], "|:---|---:|---:|---:|---:|---:|---:|---:|:---|:---|:---|:---|:---|:---|:---|:---|:---|");
    assert!(lines[2].starts_with("| 1 | 0 | -15000 | 0 | 0 | 0.0 | 0.0 | 0.0000 | NULL | long text 0 long text 0 long text 0  | 0x00000000 | 2022-01-08 | 00:00:00.1234567 | 2024-01-02 03:04:05 | 2024-01-02 03:04:05.1234567 | 2024-01-02 03:04:05.1234567 | x0 |"), "{}", lines[2]);
    assert!(lines[5].contains("| NULL | NULL | NULL |"));
    assert!(lines[6].contains("| ünïcödé \\| pipe |"));
    assert!(lines[10].contains("| line1<br>line2 |"));
}

// ---------------------------------------------------------------------------------------------
// Excel
// ---------------------------------------------------------------------------------------------

fn open_sheet(path: &Path, sheet: &str) -> calamine::Range<calamine::Data> {
    use calamine::Reader;
    let mut wb: calamine::Xlsx<_> = calamine::open_workbook(path).unwrap();
    assert_eq!(wb.sheet_names(), vec![sheet.to_string()]);
    wb.worksheet_range(sheet).unwrap()
}

#[test]
fn excel_native_types() {
    use calamine::Data;
    let dir = tempfile::tempdir().unwrap();
    let rs = fixture();
    let (path, stats) = export(&rs, Format::Excel, dir.path(), &ExportOptions::default());
    assert_eq!(stats.rows, ROWS);
    assert!(stats.bytes > 0 && stats.warnings.is_empty());
    let range = open_sheet(&path, "Results");
    // The last data row (i = 9999) is all-NULL, so calamine's used range ends one row early.
    assert_eq!(range.height(), ROWS);
    assert_eq!(range.width(), 17);
    assert_eq!(range.get((0, 8)), Some(&Data::String("s".into())));
    assert_eq!(range.get((0, 9)), Some(&Data::String("long text".into())));
    let row = |r: usize, c: usize| range.get((r, c)).cloned().unwrap_or(Data::Empty);
    assert_eq!(row(2, 0), Data::Bool(false));
    assert!(matches!(row(2, 3), Data::Float(f) if f == 1.0) || matches!(row(2, 3), Data::Int(1)), "{:?}", row(2, 3));
    assert!(matches!(row(2, 7), Data::Float(f) if (f - 1.2345).abs() < 1e-9), "{:?}", row(2, 7));
    assert_eq!(row(2, 8), Data::String("has \"quotes\"".into()));
    assert_eq!(row(9, 8), Data::String("line1\nline2".into()));
    assert_eq!(row(2, 10), Data::String("0x01000000".into()));
    match row(2, 11) {
        Data::DateTime(d) => assert_eq!(d.as_datetime().unwrap().date().to_string(), "2022-01-09"),
        other => panic!("date cell: {other:?}"),
    }
    match row(2, 12) {
        Data::DateTime(d) => assert_eq!(d.as_datetime().unwrap().time().format("%H:%M:%S%.3f").to_string(), "00:00:01.123"),
        other => panic!("time cell: {other:?}"),
    }
    match row(2, 14) {
        Data::DateTime(d) => assert_eq!(d.as_datetime().unwrap().format("%Y-%m-%d %H:%M:%S%.3f").to_string(), "2024-01-02 03:04:06.123"),
        other => panic!("datetime cell: {other:?}"),
    }
    assert_eq!(row(2, 16), Data::String("x1".into()));
    // NULL row is blank.
    for c in 0..17 {
        assert_eq!(row(4, c), Data::Empty, "col {c}");
    }
    assert_eq!(row(ROWS - 1, 16), Data::String(format!("x{}", ROWS - 2)));
    assert_eq!(row(ROWS, 16), Data::Empty);
}

#[test]
fn excel_text_only_and_sheet_name() {
    use calamine::Data;
    let dir = tempfile::tempdir().unwrap();
    let rs = small();
    let opts = ExportOptions {
        excel: ExcelOptions { sheet_name: "My:Query/Results".into(), native_types: false, bold_header: false, autofilter: false, freeze_header: false, autofit: false, ..Default::default() },
        ..Default::default()
    };
    let (path, _) = export(&rs, Format::Excel, dir.path(), &opts);
    let range = open_sheet(&path, "MyQueryResults");
    assert_eq!(range.height(), 13);
    assert_eq!(range.get((1, 3)), Some(&Data::String("0".into())));
    assert_eq!(range.get((2, 7)), Some(&Data::String("1.2345".into())));
    assert_eq!(range.get((2, 11)), Some(&Data::String("2022-01-09".into())));
}

#[test]
fn excel_too_many_rows_refused_before_writing() {
    let dir = tempfile::tempdir().unwrap();
    let rs = fixture();
    let mut opts = ExportOptions::default();
    opts.excel.max_rows_warn = 100;
    let path = dir.path().join("big.xlsx");
    let err = export_to_file(&rs, Format::Excel, &path, &opts, &CellFormatter::default(), &mut ok).unwrap_err();
    assert!(matches!(err, ExportError::TooManyRows { rows: ROWS, limit: 100 }), "{err}");
    assert!(!path.exists());
    assert!(no_partials(dir.path()));
}

#[test]
fn excel_long_string_truncated_with_warning() {
    use calamine::Data;
    let cols = vec![ColumnInfo::new("s", SqlType::NVarChar { len: None }, true, 0)];
    let schema = Arc::new(Schema::new(vec![Field::new("s", DataType::LargeUtf8, true)]));
    let long = "y".repeat(40_000);
    let b = RecordBatch::try_new(schema, vec![Arc::new(LargeStringArray::from(vec![Some(long.as_str()), Some("short")]))]).unwrap();
    let rs = ResultSet::from_batches(0, cols, vec![b]);
    let dir = tempfile::tempdir().unwrap();
    let (path, stats) = export(&rs, Format::Excel, dir.path(), &ExportOptions::default());
    assert_eq!(stats.warnings.len(), 1);
    assert!(stats.warnings[0].contains("32767"));
    let range = open_sheet(&path, "Results");
    match range.get((1, 0)) {
        Some(Data::String(s)) => assert_eq!(s.chars().count(), 32_767),
        other => panic!("{other:?}"),
    }
}

// ---------------------------------------------------------------------------------------------
// Parquet
// ---------------------------------------------------------------------------------------------

fn read_parquet(path: &Path) -> (SchemaRef, Vec<RecordBatch>, parquet::file::metadata::ParquetMetaData) {
    let file = std::fs::File::open(path).unwrap();
    let builder = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(file).unwrap();
    let schema = builder.schema().clone();
    let md = builder.metadata().as_ref().clone();
    let batches: Vec<RecordBatch> = builder.build().unwrap().map(|b| b.unwrap()).collect();
    (schema, batches, md)
}

fn assert_same_fields(a: &Schema, b: &Schema) {
    assert_eq!(a.fields().len(), b.fields().len());
    for (x, y) in a.fields().iter().zip(b.fields().iter()) {
        assert_eq!(x.name(), y.name());
        assert_eq!(x.data_type(), y.data_type(), "column {}", x.name());
    }
}

#[test]
fn parquet_roundtrip_schema_and_values() {
    let dir = tempfile::tempdir().unwrap();
    let rs = fixture();
    let mut opts = ExportOptions::default();
    opts.parquet.row_group_rows = 3000;
    let (path, stats) = export(&rs, Format::Parquet, dir.path(), &opts);
    assert_eq!(stats.rows, ROWS);
    let (schema, batches, md) = read_parquet(&path);
    assert_same_fields(&schema, &rs.schema);
    assert_eq!(schema.metadata().get("cobalt.sql_types").map(String::as_str), Some(r#"["bit","tinyint","smallint","int","bigint","real","float","decimal(18,4)","nvarchar(50)","nvarchar(max)","varbinary(8)","date","time(7)","datetime","datetime2(7)","datetimeoffset(7)","varchar(10)"]"#));
    let fm = md.file_metadata();
    assert!(fm.created_by().unwrap().contains("Cobalt SQL Works"));
    assert!(fm.key_value_metadata().unwrap().iter().any(|kv| kv.key == "cobalt.sql_types"));
    assert_eq!(md.num_row_groups(), 4);
    let total: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(total, ROWS);
    let all = arrow::compute::concat_batches(&schema, &batches).unwrap();
    let v = |r: usize, c: usize| CellValue::from_array(all.column(c), r);
    assert_eq!(v(1, 3), CellValue::Int(1));
    assert_eq!(v(1, 7), CellValue::Decimal(12345, 4));
    assert_eq!(v(1, 8), CellValue::Text("has \"quotes\"".into()));
    assert_eq!(v(4001, 16), CellValue::Text("x4001".into()));
    assert!(v(3, 0).is_null() && v(3, 15).is_null());
    assert_eq!(v(1, 12).to_json(), serde_json::json!("00:00:01.123456700"));
    assert_eq!(v(1, 14).to_json(), serde_json::json!("2024-01-02T03:04:06.123456700"));
    // And every batch/column read back equals the source.
    let src = rs.view_to_single_batch().unwrap();
    for c in 0..src.num_columns() {
        assert_eq!(src.column(c).as_ref(), all.column(c).as_ref(), "column {c}");
    }
}

#[test]
fn parquet_compressions() {
    let dir = tempfile::tempdir().unwrap();
    let rs = small();
    for (i, comp) in [Compression::None, Compression::Snappy, Compression::Zstd, Compression::Lz4].iter().enumerate() {
        let mut opts = ExportOptions::default();
        opts.parquet.compression = *comp;
        opts.parquet.statistics = i % 2 == 0;
        let path = dir.path().join(format!("c{i}.parquet"));
        export_to_file(&rs, Format::Parquet, &path, &opts, &CellFormatter::default(), &mut ok).unwrap();
        let (_, batches, md) = read_parquet(&path);
        assert_eq!(batches.iter().map(|b| b.num_rows()).sum::<usize>(), 12);
        let col = md.row_group(0).column(3);
        let expected = match comp {
            Compression::None => parquet::basic::Compression::UNCOMPRESSED,
            Compression::Snappy => parquet::basic::Compression::SNAPPY,
            Compression::Zstd => parquet::basic::Compression::ZSTD(Default::default()),
            Compression::Lz4 => parquet::basic::Compression::LZ4_RAW,
        };
        assert_eq!(col.compression(), expected);
        assert_eq!(col.statistics().is_some(), i % 2 == 0);
    }
}

// ---------------------------------------------------------------------------------------------
// Arrow IPC
// ---------------------------------------------------------------------------------------------

#[test]
fn arrow_ipc_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let rs = fixture();
    for comp in [None, Some(IpcCompression::Lz4), Some(IpcCompression::Zstd)] {
        let mut opts = ExportOptions::default();
        opts.arrow.compression = comp;
        let path = dir.path().join(format!("{comp:?}.arrow"));
        let stats = export_to_file(&rs, Format::ArrowIpc, &path, &opts, &CellFormatter::default(), &mut ok).unwrap();
        assert_eq!(stats.rows, ROWS);
        let reader = arrow::ipc::reader::FileReader::try_new(std::fs::File::open(&path).unwrap(), None).unwrap();
        assert_same_fields(&reader.schema(), &rs.schema);
        assert!(reader.schema().metadata().contains_key("cobalt.sql_types"));
        assert_eq!(reader.custom_metadata().get("created_by").map(String::as_str), Some("Cobalt SQL Works"));
        let batches: Vec<RecordBatch> = reader.map(|b| b.unwrap()).collect();
        assert_eq!(batches.len(), 3);
        let all = arrow::compute::concat_batches(&reader_schema(&path), &batches).unwrap();
        assert_eq!(all.num_rows(), ROWS);
        let src = rs.view_to_single_batch().unwrap();
        for c in 0..src.num_columns() {
            assert_eq!(src.column(c).as_ref(), all.column(c).as_ref(), "column {c} ({comp:?})");
        }
    }
}

fn reader_schema(path: &Path) -> SchemaRef {
    arrow::ipc::reader::FileReader::try_new(std::fs::File::open(path).unwrap(), None).unwrap().schema()
}

// ---------------------------------------------------------------------------------------------
// Cross-cutting: cancel, progress, views, writer form, overwrite, empty
// ---------------------------------------------------------------------------------------------

#[test]
fn cancel_deletes_partial_file() {
    let dir = tempfile::tempdir().unwrap();
    let rs = fixture();
    for format in Format::all() {
        let path = dir.path().join(format!("cancel.{}", format.extension()));
        let mut calls = 0;
        let err = export_to_file(&rs, *format, &path, &ExportOptions::default(), &CellFormatter::default(), &mut |_| {
            calls += 1;
            calls < 2
        })
        .unwrap_err();
        assert!(matches!(err, ExportError::Cancelled), "{format:?}: {err}");
        assert!(!path.exists(), "{format:?} left the file");
        assert!(no_partials(dir.path()), "{format:?} left a partial");
    }
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn progress_reports_rows_and_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let rs = fixture();
    let path = dir.path().join("p.csv");
    let mut seen: Vec<Progress> = Vec::new();
    export_to_file(&rs, Format::Csv, &path, &ExportOptions::default(), &CellFormatter::default(), &mut |p| {
        seen.push(p);
        true
    })
    .unwrap();
    assert_eq!(seen.len(), 4); // initial + 3 batches
    assert_eq!(seen[0], Progress { rows_done: 0, rows_total: ROWS, bytes_written: 0 });
    assert_eq!(seen[1].rows_done, 4000);
    assert_eq!(seen[3].rows_done, ROWS);
    assert!(seen[3].bytes_written > seen[1].bytes_written && seen[1].bytes_written > 0);
    assert!(seen.iter().all(|p| p.rows_total == ROWS));
}

#[test]
fn export_follows_sort_and_filter_view() {
    let dir = tempfile::tempdir().unwrap();
    let rs = fixture();
    rs.apply_view(ViewSpec { filters: vec![ColumnFilter { column: 3, op: FilterOp::NotNull }], sort: vec![SortKey { column: 3, descending: true }] }).unwrap();
    let visible = rs.visible_count();
    assert!(visible < ROWS);
    let (path, stats) = export(&rs, Format::Csv, dir.path(), &ExportOptions::default());
    assert_eq!(stats.rows, visible);
    let mut rdr = csv::Reader::from_path(&path).unwrap();
    let records: Vec<csv::StringRecord> = rdr.records().map(|r| r.unwrap()).collect();
    assert_eq!(records.len(), visible);
    assert_eq!(&records[0][3], "9998"); // 9999 is a NULL row, filtered out
    assert_eq!(&records[1][3], "9997");
    assert!(records.iter().all(|r| !r[3].is_empty()));
    // Parquet through the same view.
    let (ppath, _) = export(&rs, Format::Parquet, dir.path(), &ExportOptions::default());
    let (_, batches, _) = read_parquet(&ppath);
    assert_eq!(batches.iter().map(|b| b.num_rows()).sum::<usize>(), visible);
    assert_eq!(CellValue::from_array(batches[0].column(3), 0), CellValue::Int(9998));
}

#[test]
fn writer_form_streams_text_and_binary() {
    let rs = small();
    let mut buf: Vec<u8> = Vec::new();
    let stats = export_to_writer(&rs, Format::Csv, &mut buf, &ExportOptions::default(), &CellFormatter::default(), &mut ok).unwrap();
    assert_eq!(stats.rows, 12);
    assert_eq!(stats.bytes as usize, buf.len());
    assert!(stats.path.as_os_str().is_empty());
    assert!(String::from_utf8(buf).unwrap().starts_with("b,ti,si,i,bi,r,f,d,s,long text,bin,dt,t,dtm,dt2,dto,s\r\n"));
    let mut pq: Vec<u8> = Vec::new();
    export_to_writer(&rs, Format::Parquet, &mut pq, &ExportOptions::default(), &CellFormatter::default(), &mut ok).unwrap();
    assert!(pq.starts_with(b"PAR1") && pq.ends_with(b"PAR1"));
    let mut md: Vec<u8> = Vec::new();
    export_to_writer(&rs, Format::Markdown, &mut md, &ExportOptions::default(), &CellFormatter::default(), &mut ok).unwrap();
    assert!(md.starts_with(b"| b | ti |"));
    let err = export_to_writer(&rs, Format::Excel, Vec::new(), &ExportOptions::default(), &CellFormatter::default(), &mut ok).unwrap_err();
    assert!(matches!(err, ExportError::Unsupported(_)));
}

#[test]
fn overwrites_existing_file() {
    let dir = tempfile::tempdir().unwrap();
    let rs = small();
    let path = dir.path().join("out.csv");
    std::fs::write(&path, "old contents that are longer than the new file? no").unwrap();
    export_to_file(&rs, Format::Csv, &path, &ExportOptions::default(), &CellFormatter::default(), &mut ok).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.starts_with("b,ti,"));
    assert!(!text.contains("old contents"));
}

#[test]
fn empty_result_set_every_format() {
    let dir = tempfile::tempdir().unwrap();
    let rs = empty();
    for format in Format::all() {
        let (path, stats) = export(&rs, *format, dir.path(), &ExportOptions::default());
        assert_eq!(stats.rows, 0, "{format:?}");
        // JSON Lines with no rows is legitimately an empty file.
        assert!(*format == Format::JsonLines || std::fs::metadata(&path).unwrap().len() > 0, "{format:?}");
    }
    let csv = std::fs::read_to_string(dir.path().join("out.csv")).unwrap();
    assert_eq!(csv, "b,ti,si,i,bi,r,f,d,s,long text,bin,dt,t,dtm,dt2,dto,s\r\n");
    let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.path().join("out.json")).unwrap()).unwrap();
    assert_eq!(json, serde_json::json!([]));
    let (_, batches, _) = read_parquet(&dir.path().join("out.parquet"));
    assert_eq!(batches.iter().map(|b| b.num_rows()).sum::<usize>(), 0);
    let range = open_sheet(&dir.path().join("out.xlsx"), "Results");
    assert_eq!(range.height(), 1);
}

#[test]
fn formatter_settings_flow_into_text_exports() {
    let dir = tempfile::tempdir().unwrap();
    let rs = small();
    let fmt = CellFormatter::default().with_bit_as_number(false).with_null_text("<n>").with_max_chars(3).with_datetime_format("%d/%m/%Y");
    let path = dir.path().join("fmt.csv");
    let mut opts = ExportOptions::default();
    opts.csv.null_as = "NULL".into();
    export_to_file(&rs, Format::Csv, &path, &opts, &fmt, &mut ok).unwrap();
    let mut rdr = csv::Reader::from_path(&path).unwrap();
    let records: Vec<csv::StringRecord> = rdr.records().map(|r| r.unwrap()).collect();
    assert_eq!(&records[0][0], "true"); // bit style honoured
    assert_eq!(&records[0][9], "long text 0 long text 0 long text 0 "); // never truncated
    assert_eq!(&records[0][13], "2024-01-02 03:04:05"); // ISO regardless of grid format
    assert_eq!(&records[3][0], "NULL"); // csv null_as wins over grid NULL text
}

#[test]
fn text_builders_against_fixture_selection() {
    let rs = fixture();
    let fmt = CellFormatter::default();
    let sel = Selection::rect(0, 2, 3, 4);
    let tsv = cobalt_export::text::to_tsv(&rs, &sel, true, &fmt, "").unwrap();
    assert_eq!(tsv, "i\tbi\r\n0\t0\r\n1\t1000000007\r\n2\t2000000014\r\n");
    let ins = cobalt_export::text::to_insert_statements(&rs, &Selection::rect(3, 4, 8, 9), "t", 10).unwrap();
    assert_eq!(ins, "INSERT INTO t (s, [long text]) VALUES\n(NULL, NULL),\n(N'ünïcödé | pipe', N'long text 4 long text 4 long text 4 ');\n");
    let inl = cobalt_export::text::to_in_list(&rs, &Selection::rect(0, 6, 8, 8)).unwrap();
    assert_eq!(inl, "(N'has \"quotes\"', N'com,ma', N'ünïcödé | pipe', N'plain')"); // rows 0 and 3 are NULL
    let inl = cobalt_export::text::to_in_list(&rs, &Selection::rect(0, 8, 8, 8)).unwrap();
    assert!(inl.ends_with(", N'line1\nline2')"), "{inl}");
}
