//! Write a Delta table to a temp dir and read it back with delta-rs + parquet.

use arrow::array::*;
use arrow::datatypes::*;
use cobalt_core::{ColumnInfo, SqlType};
use cobalt_export_delta::*;
use cobalt_results::{CellValue, MemoryBudget, ResultSet, RunState};
use deltalake::kernel::DataType as KDataType;
use std::path::Path;
use std::sync::Arc;

const ROWS: usize = 3000;

fn columns() -> Vec<ColumnInfo> {
    let c = |n: &str, t: SqlType, o: usize| ColumnInfo::new(n, t, true, o);
    vec![
        c("id", SqlType::Int, 0),
        c("category", SqlType::VarChar { len: Some(10) }, 1),
        c("ti", SqlType::TinyInt, 2),
        c("amount", SqlType::Decimal { precision: 18, scale: 4 }, 3),
        c("dt2", SqlType::DateTime2 { scale: 7 }, 4),
        c("dto", SqlType::DateTimeOffset { scale: 7 }, 5),
        c("uid", SqlType::UniqueIdentifier, 6),
        c("bin", SqlType::VarBinary { len: None }, 7),
        c("t", SqlType::Time { scale: 7 }, 8),
        c("note", SqlType::NVarChar { len: None }, 9),
        c("flag", SqlType::Bit, 10),
        c("dt", SqlType::Date, 11),
    ]
}

fn opt<T>(idx: &[usize], f: impl Fn(usize) -> T) -> Vec<Option<T>> {
    idx.iter().map(|&i| if is_null(i) { None } else { Some(f(i)) }).collect()
}

fn is_null(i: usize) -> bool {
    i % 5 == 2
}

fn batch(schema: &SchemaRef, start: usize, len: usize) -> RecordBatch {
    let idx: Vec<usize> = (start..start + len).collect();
    let cols: Vec<ArrayRef> = vec![
        Arc::new(Int32Array::from(idx.iter().map(|&i| i as i32).collect::<Vec<_>>())),
        Arc::new(StringArray::from(idx.iter().map(|&i| format!("CAT{}", i % 3)).collect::<Vec<_>>())),
        Arc::new(UInt8Array::from(opt(&idx, |i| (i % 256) as u8))),
        Arc::new(Decimal128Array::from(opt(&idx, |i| i as i128 * 12345)).with_precision_and_scale(18, 4).unwrap()),
        Arc::new(TimestampMicrosecondArray::from(opt(&idx, |i| 1_704_164_645_123_456 + i as i64 * 1_000_000))),
        Arc::new(TimestampMicrosecondArray::from(opt(&idx, |i| 1_704_164_645_123_456 + i as i64 * 1_000_000)).with_timezone("UTC")),
        Arc::new(StringArray::from(opt(&idx, |i| format!("6F9619FF-8B86-D011-B42D-00C04FC9{:04X}", i)))),
        Arc::new(LargeBinaryArray::from(opt(&idx, |i| (i as u32).to_le_bytes().to_vec()).iter().map(|o| o.as_deref()).collect::<Vec<_>>())),
        Arc::new(Time64NanosecondArray::from(opt(&idx, |i| (i % 86400) as i64 * 1_000_000_000 + 123_456_700))),
        Arc::new(LargeStringArray::from(opt(&idx, |i| format!("note {i}")))),
        Arc::new(BooleanArray::from(opt(&idx, |i| i % 2 == 0))),
        Arc::new(Date32Array::from(opt(&idx, |i| 19000 + (i % 100) as i32))),
    ];
    RecordBatch::try_new(schema.clone(), cols).unwrap()
}

fn fixture() -> Arc<ResultSet> {
    let rs = ResultSet::new(0, columns(), Arc::new(MemoryBudget::unlimited()), std::env::temp_dir());
    for (start, len) in [(0, 1000), (1000, 1000), (2000, 1000)] {
        rs.append(batch(&rs.schema, start, len)).unwrap();
    }
    rs.set_state(RunState::Complete);
    rs
}

fn ok(_: Progress) -> bool {
    true
}

async fn open(path: &Path) -> deltalake::DeltaTable {
    let url = deltalake::table::builder::ensure_table_uri(path.to_string_lossy().as_ref()).unwrap();
    deltalake::open_table(url).await.unwrap()
}

/// Read every data file of the table back through parquet; returns (row count, one concatenated batch).
async fn read_back(table: &deltalake::DeltaTable) -> (usize, Vec<RecordBatch>) {
    let mut batches = Vec::new();
    for uri in table.get_file_uris().unwrap() {
        // delta-rs hands back `file:///…` on Unix and a bare `C:/…` path on Windows.
        let file_path = if uri.starts_with("file:") { url::Url::parse(&uri).unwrap().to_file_path().unwrap() } else { std::path::PathBuf::from(&uri) };
        let file = std::fs::File::open(&file_path).unwrap();
        let reader = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(file).unwrap().build().unwrap();
        for b in reader {
            batches.push(b.unwrap());
        }
    }
    (batches.iter().map(|b| b.num_rows()).sum(), batches)
}

#[tokio::test]
async fn create_then_append_then_overwrite() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sales");
    let rs = fixture();

    // Create.
    let opts = DeltaOptions { mode: DeltaMode::Create, partition_columns: vec![], table_name: Some("sales".into()), description: Some("test table".into()) };
    let mut seen = Vec::new();
    let stats = write_delta(&rs, &path, &opts, &mut |p| {
        seen.push(p);
        true
    })
    .await
    .unwrap();
    assert_eq!(stats.rows, ROWS);
    assert!(stats.bytes > 0);
    assert_eq!(seen.first().unwrap().rows_done, 0);
    assert_eq!(seen.last().unwrap().rows_done, ROWS);
    assert!(path.join("_delta_log").join("00000000000000000000.json").is_file());
    assert!(path.join("_delta_log").join("00000000000000000001.json").is_file());

    let table = open(&path).await;
    assert_eq!(table.version(), Some(1));
    let snapshot = table.snapshot().unwrap();
    assert_eq!(snapshot.metadata().name(), Some("sales"));
    assert_eq!(snapshot.metadata().description(), Some("test table"));
    let schema = snapshot.schema();
    let ty = |n: &str| schema.field(n).unwrap().data_type().clone();
    assert_eq!(ty("id"), KDataType::INTEGER);
    assert_eq!(ty("ti"), KDataType::SHORT);
    assert_eq!(ty("amount"), KDataType::decimal(18, 4).unwrap());
    assert_eq!(ty("dt2"), KDataType::TIMESTAMP_NTZ);
    assert_eq!(ty("dto"), KDataType::TIMESTAMP);
    assert_eq!(ty("uid"), KDataType::STRING);
    assert_eq!(ty("bin"), KDataType::BINARY);
    assert_eq!(ty("t"), KDataType::STRING);
    assert_eq!(ty("note"), KDataType::STRING);
    assert_eq!(ty("flag"), KDataType::BOOLEAN);
    assert_eq!(ty("dt"), KDataType::DATE);

    let (n, batches) = read_back(&table).await;
    assert_eq!(n, ROWS);
    let all = arrow::compute::concat_batches(&batches[0].schema(), &batches).unwrap();
    let ids = arrow::compute::sort(all.column(0), None).unwrap();
    assert_eq!(CellValue::from_array(&ids, 0), CellValue::Int(0));
    assert_eq!(CellValue::from_array(&ids, ROWS - 1), CellValue::Int(ROWS as i64 - 1));
    // Parquet types on disk.
    let s = all.schema();
    assert_eq!(s.field_with_name("dt2").unwrap().data_type(), &DataType::Timestamp(TimeUnit::Microsecond, None));
    assert_eq!(s.field_with_name("dto").unwrap().data_type(), &DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())));
    assert_eq!(s.field_with_name("amount").unwrap().data_type(), &DataType::Decimal128(18, 4));
    assert_eq!(s.field_with_name("t").unwrap().data_type(), &DataType::Utf8);
    // Find row id=1 and check values.
    let id_col = all.column(0).as_primitive::<Int32Type>();
    let r = (0..all.num_rows()).find(|&r| id_col.value(r) == 1).unwrap();
    assert_eq!(CellValue::from_array(all.column(3), r), CellValue::Decimal(12345, 4));
    assert_eq!(CellValue::from_array(all.column(8), r), CellValue::Text("00:00:01.1234567".into()));
    assert_eq!(CellValue::from_array(all.column(4), r).to_json(), serde_json::json!("2024-01-02T03:04:06.123456"));
    assert_eq!(CellValue::from_array(all.column(7), r), CellValue::Bytes(vec![1, 0, 0, 0]));
    let r2 = (0..all.num_rows()).find(|&r| id_col.value(r) == 2).unwrap();
    for c in 2..12 {
        assert!(all.column(c).is_null(r2), "col {c} should be null");
    }

    // Create on existing errors.
    let err = write_delta(&rs, &path, &opts, &mut ok).await.unwrap_err();
    assert!(matches!(err, DeltaError::Exists(_)), "{err}");
    assert_eq!(open(&path).await.version(), Some(1));

    // Append doubles.
    let opts = DeltaOptions { mode: DeltaMode::Append, ..Default::default() };
    let stats = write_delta(&rs, &path, &opts, &mut ok).await.unwrap();
    assert_eq!(stats.rows, ROWS);
    let table = open(&path).await;
    assert_eq!(table.version(), Some(2));
    assert_eq!(read_back(&table).await.0, ROWS * 2);

    // Overwrite resets.
    let opts = DeltaOptions { mode: DeltaMode::Overwrite, ..Default::default() };
    write_delta(&rs, &path, &opts, &mut ok).await.unwrap();
    let table = open(&path).await;
    assert_eq!(read_back(&table).await.0, ROWS);
    assert!(table.version().unwrap() >= 3);
}

#[tokio::test]
async fn partitioned_by_category() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("parts");
    let rs = fixture();
    let opts = DeltaOptions { mode: DeltaMode::Create, partition_columns: vec!["category".into()], ..Default::default() };
    write_delta(&rs, &path, &opts, &mut ok).await.unwrap();
    for c in 0..3 {
        let d = path.join(format!("category=CAT{c}"));
        assert!(d.is_dir(), "{d:?}");
        assert!(std::fs::read_dir(&d).unwrap().any(|e| e.unwrap().path().extension().map(|x| x == "parquet").unwrap_or(false)));
    }
    let table = open(&path).await;
    assert_eq!(table.snapshot().unwrap().metadata().partition_columns(), &["category".to_string()]);
    let (n, _) = read_back(&table).await;
    assert_eq!(n, ROWS);
    assert_eq!(table.get_file_uris().unwrap().count(), 3);

    // Appending to a partitioned table keeps the layout.
    let opts = DeltaOptions { mode: DeltaMode::Append, ..Default::default() };
    write_delta(&rs, &path, &opts, &mut ok).await.unwrap();
    let table = open(&path).await;
    assert_eq!(read_back(&table).await.0, ROWS * 2);

    let bad = DeltaOptions { mode: DeltaMode::Overwrite, partition_columns: vec!["nope".into()], ..Default::default() };
    assert!(matches!(write_delta(&rs, &path, &bad, &mut ok).await.unwrap_err(), DeltaError::UnknownPartitionColumn(_)));
}

#[test]
fn blocking_wrapper_and_cancel() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("blocking");
    let rs = fixture();
    let opts = DeltaOptions::default();
    // Cancel on the first poll: nothing is left behind.
    let err = write_delta_blocking(&rs, &path, &opts, &mut |_| false).unwrap_err();
    assert!(matches!(err, DeltaError::Cancelled));
    assert!(!path.exists());
    // Then a real write through the blocking wrapper.
    let stats = write_delta_blocking(&rs, &path, &opts, &mut ok).unwrap();
    assert_eq!(stats.rows, ROWS);
    assert!(path.join("_delta_log").join("00000000000000000000.json").is_file());
    let rt = tokio::runtime::Runtime::new().unwrap();
    let n = rt.block_on(async { read_back(&open(&path).await).await.0 });
    assert_eq!(n, ROWS);
}

#[test]
fn append_to_missing_creates() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fresh");
    let rs = fixture();
    let opts = DeltaOptions { mode: DeltaMode::Append, ..Default::default() };
    write_delta_blocking(&rs, &path, &opts, &mut ok).unwrap();
    assert!(path.join("_delta_log").join("00000000000000000000.json").is_file());
}
