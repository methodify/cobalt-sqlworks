//! Integration tests against a live SQL Server (see tests/seed.sql at the workspace root).
//!
//! Gated on `COBALT_TEST_MSSQL=1`; without it every test prints "skipped" and passes.
//! Overrides: `COBALT_TEST_MSSQL_SERVER` (default `localhost,1433`), `COBALT_TEST_MSSQL_USER`
//! (`sa`), `COBALT_TEST_MSSQL_PASSWORD`, `COBALT_TEST_MSSQL_DB` (`cobalt_test`).
//! Run with `--test-threads=1`: the tests share one server and some measure timing.

use arrow::array::{Array, AsArray};
use arrow::datatypes::{DataType, Decimal128Type, Float32Type, Float64Type, Int16Type, Int32Type, Int64Type, TimeUnit, UInt8Type};
use cobalt_core::*;
use cobalt_driver::mssql::MssqlDriver;
use cobalt_driver::{is_showplan_result, Connection, Driver, ScriptKind, StreamItem};
use futures_util::StreamExt;
use std::time::{Duration, Instant};

fn enabled() -> bool {
    std::env::var("COBALT_TEST_MSSQL").map(|v| !v.is_empty() && v != "0").unwrap_or(false)
}

macro_rules! require_server {
    () => {
        if !enabled() {
            eprintln!("skipped (set COBALT_TEST_MSSQL=1 to run against a live server)");
            return;
        }
    };
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| default.to_string())
}

fn profile() -> ConnectionProfile {
    let user = env_or("COBALT_TEST_MSSQL_USER", "sa");
    let mut p = ConnectionProfile::new(env_or("COBALT_TEST_MSSQL_SERVER", "localhost,1433"), AuthMethod::SqlLogin { user, password: None });
    p.database = Some(env_or("COBALT_TEST_MSSQL_DB", "cobalt_test"));
    p.options.trust_server_certificate = true;
    p.options.encrypt = Encrypt::Mandatory;
    p.options.connect_timeout_secs = 15;
    p
}

fn creds() -> ResolvedCredentials {
    ResolvedCredentials::SqlLogin { user: env_or("COBALT_TEST_MSSQL_USER", "sa"), password: Secret::new(env_or("COBALT_TEST_MSSQL_PASSWORD", "Cobalt!Dev2026pw")) }
}

async fn connect() -> Box<dyn Connection> {
    MssqlDriver::new().connect(&profile(), &creds(), ConnectionRole::Query).await.expect("connect")
}

async fn run(conn: &mut dyn Connection, sql: &str, opts: &ExecOptions) -> Vec<StreamItem> {
    let stream = conn.execute(sql, opts).await.expect("execute");
    stream.collect().await
}

/// Compact rendering for order assertions: `M:text`, `RS:<rows>`, `RA:<n>`, `DONE`, `ERR:<number>`.
fn shape(items: &[StreamItem]) -> Vec<String> {
    let mut out = Vec::new();
    let mut rows = 0u64;
    for it in items {
        match it {
            StreamItem::ResultSetStart { .. } => rows = 0,
            StreamItem::Rows(b) => rows += b.num_rows() as u64,
            StreamItem::ResultSetEnd { rows: n } => {
                assert_eq!(*n, rows, "ResultSetEnd count must equal streamed rows");
                out.push(format!("RS:{n}"));
            }
            StreamItem::RowsAffected(n) => out.push(format!("RA:{n}")),
            StreamItem::Message(m) => out.push(format!("M:{}", m.message)),
            StreamItem::Done { error: None, cancelled } => out.push(if *cancelled { "CANCELLED".into() } else { "DONE".into() }),
            StreamItem::Done { error: Some(e), .. } => out.push(format!("ERR:{}", e.number)),
        }
    }
    out
}

fn result_sets(items: &[StreamItem]) -> Vec<(Vec<ColumnInfo>, Vec<arrow::array::RecordBatch>, u64)> {
    let mut out = Vec::new();
    for it in items {
        match it {
            StreamItem::ResultSetStart { columns } => out.push((columns.clone(), Vec::new(), 0)),
            StreamItem::Rows(b) => out.last_mut().unwrap().1.push(b.clone()),
            StreamItem::ResultSetEnd { rows } => out.last_mut().unwrap().2 = *rows,
            _ => {}
        }
    }
    out
}

fn done(items: &[StreamItem]) -> (Option<ServerMessage>, bool) {
    match items.last() {
        Some(StreamItem::Done { error, cancelled }) => (error.clone(), *cancelled),
        other => panic!("stream must end with Done, got {other:?}"),
    }
}

fn ns(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32, nano: u32) -> i64 {
    chrono::NaiveDate::from_ymd_opt(y, mo, d).unwrap().and_hms_nano_opt(h, mi, s, nano).unwrap().and_utc().timestamp_nanos_opt().unwrap()
}

// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn t01_connect_engine_info() {
    require_server!();
    let mut conn = connect().await;
    let e = conn.engine();
    assert_eq!(e.kind, EngineKind::SqlServer, "{e:?}");
    assert_eq!(e.major_version(), 16, "expected SQL Server 2022, got {}", e.version);
    assert!(conn.spid().unwrap_or(0) > 0);
    assert_eq!(conn.current_database(), "cobalt_test");
    assert!(!e.server_name.is_empty() && e.version_text.contains("Microsoft SQL Server"));
    assert_eq!(conn.role(), ConnectionRole::Query);
    conn.close().await.unwrap();
}

#[tokio::test]
async fn t02_all_types_values() {
    require_server!();
    let mut conn = connect().await;
    let items = run(&mut *conn, "SELECT * FROM dbo.all_types ORDER BY id", &ExecOptions::default()).await;
    let (err, cancelled) = done(&items);
    assert!(err.is_none() && !cancelled, "{err:?}");
    let sets = result_sets(&items);
    assert_eq!(sets.len(), 1, "{:?}", shape(&items));
    let (columns, batches, rows) = &sets[0];
    assert_eq!(*rows, 3);
    assert_eq!(batches.len(), 1);
    let b = &batches[0];
    assert_eq!(b.num_rows(), 3);

    use SqlType::*;
    let expected: Vec<(&str, SqlType)> = vec![
        ("id", Int),
        ("c_bit", Bit),
        ("c_tinyint", TinyInt),
        ("c_smallint", SmallInt),
        ("c_int", Int),
        ("c_bigint", BigInt),
        ("c_decimal", Decimal { precision: 18, scale: 4 }),
        ("c_numeric", Numeric { precision: 10, scale: 2 }),
        ("c_money", Money),
        ("c_smallmoney", SmallMoney),
        ("c_float", Float),
        ("c_real", Real),
        ("c_date", Date),
        ("c_time", Time { scale: 7 }),
        ("c_datetime", DateTime),
        ("c_datetime2", DateTime2 { scale: 7 }),
        ("c_smalldatetime", SmallDateTime),
        ("c_datetimeoffset", DateTimeOffset { scale: 7 }),
        ("c_char", Char { len: Some(10) }),
        ("c_varchar", VarChar { len: Some(50) }),
        ("c_nchar", NChar { len: Some(10) }),
        ("c_nvarchar", NVarChar { len: Some(100) }),
        ("c_varcharmax", VarChar { len: None }),
        ("c_nvarcharmax", NVarChar { len: None }),
        ("c_binary", Binary { len: Some(8) }),
        ("c_varbinary", VarBinary { len: Some(50) }),
        ("c_varbinarymax", VarBinary { len: None }),
        ("c_uniqueidentifier", UniqueIdentifier),
        ("c_xml", Xml),
        ("c_json", NVarChar { len: None }),
        ("c_sqlvariant", SqlVariant),
    ];
    assert_eq!(columns.len(), 31);
    for (i, (name, ty)) in expected.iter().enumerate() {
        assert_eq!(columns[i].name, *name);
        assert_eq!(&columns[i].sql_type, ty, "column {name}");
        assert_eq!(columns[i].ordinal, i);
        assert_eq!(b.schema().field(i).data_type(), &ty.arrow_type(), "arrow type of {name}");
        assert_eq!(b.column(i).data_type(), &ty.arrow_type(), "array type of {name}");
    }
    assert!(columns[0].is_identity && !columns[0].nullable);
    assert!(columns[1].nullable);

    // row 1 values
    let c = |i: usize| b.column(i);
    assert_eq!(c(0).as_primitive::<Int32Type>().value(0), 1);
    assert!(c(1).as_boolean().value(0));
    assert_eq!(c(2).as_primitive::<UInt8Type>().value(0), 255);
    assert_eq!(c(3).as_primitive::<Int16Type>().value(0), -32768);
    assert_eq!(c(4).as_primitive::<Int32Type>().value(0), 2147483647);
    assert_eq!(c(5).as_primitive::<Int64Type>().value(0), 9223372036854775807);
    let dec = c(6).as_primitive::<Decimal128Type>();
    assert_eq!(dec.value(0), 12345678901);
    assert_eq!(dec.value_as_string(0), "1234567.8901");
    assert_eq!(c(7).as_primitive::<Decimal128Type>().value_as_string(0), "99999999.99");
    assert_eq!(c(8).as_primitive::<Decimal128Type>().value_as_string(0), "922337203685477.5807");
    assert_eq!(c(9).as_primitive::<Decimal128Type>().value_as_string(0), "214748.3647");
    assert_eq!(c(10).as_primitive::<Float64Type>().value(0), std::f64::consts::PI);
    assert_eq!(c(11).as_primitive::<Float32Type>().value(0), 2.5);
    let epoch = chrono::NaiveDate::from_ymd_opt(1970, 1, 1).unwrap();
    let date = chrono::NaiveDate::from_ymd_opt(2026, 9, 16).unwrap();
    assert_eq!(c(12).as_primitive::<arrow::datatypes::Date32Type>().value(0), (date - epoch).num_days() as i32);
    assert_eq!(c(13).as_primitive::<arrow::datatypes::Time64NanosecondType>().value(0), (13 * 3600 + 45 * 60 + 30) * 1_000_000_000 + 123_456_700);
    assert_eq!(c(14).as_primitive::<arrow::datatypes::TimestampMillisecondType>().value(0), ns(2026, 9, 16, 13, 45, 30, 123_000_000) / 1_000_000);
    assert_eq!(c(15).as_primitive::<arrow::datatypes::TimestampNanosecondType>().value(0), ns(2026, 9, 16, 13, 45, 30, 123_456_700));
    assert_eq!(c(16).as_primitive::<arrow::datatypes::TimestampMillisecondType>().value(0), ns(2026, 9, 16, 13, 45, 0, 0) / 1_000_000);
    let dto = c(17).as_primitive::<arrow::datatypes::TimestampNanosecondType>();
    assert_eq!(dto.data_type(), &DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into())));
    assert_eq!(dto.value(0), ns(2026, 9, 16, 20, 45, 30, 123_456_700), "datetimeoffset must be converted to UTC");
    assert_eq!(c(18).as_string::<i32>().value(0), "abc       ");
    assert_eq!(c(19).as_string::<i32>().value(0), "hello world");
    assert_eq!(c(20).as_string::<i32>().value(0), "ñandú     ");
    assert_eq!(c(21).as_string::<i32>().value(0), "こんにちは 世界");
    assert_eq!(c(22).as_string::<i64>().value(0), "long varchar max text");
    // the seed inserts this literal without an N prefix, so the server itself turned ✓ into ?
    assert_eq!(c(23).as_string::<i64>().value(0), "long nvarchar max ? text");
    assert_eq!(c(24).as_binary::<i32>().value(0), &[1, 2, 3, 4, 5, 6, 7, 8]);
    assert_eq!(c(25).as_binary::<i32>().value(0), &[0xDE, 0xAD, 0xBE, 0xEF]);
    assert_eq!(c(26).as_binary::<i64>().value(0), &[0xCA, 0xFE, 0xBA, 0xBE]);
    assert_eq!(c(27).as_string::<i32>().value(0), "6F9619FF-8B86-D011-B42D-00C04FC964FF");
    assert_eq!(c(28).as_string::<i64>().value(0), "<root><a x=\"1\">text</a></root>");
    assert_eq!(c(29).as_string::<i64>().value(0), "{\"k\":\"v\",\"n\":[1,2,3],\"o\":{\"deep\":true}}");
    assert_eq!(c(30).as_string::<i32>().value(0), "42");

    // row 2: sql_variant varchar, empty strings, zero guid
    assert_eq!(c(30).as_string::<i32>().value(1), "str");
    assert_eq!(c(27).as_string::<i32>().value(1), "00000000-0000-0000-0000-000000000000");
    assert_eq!(c(25).as_binary::<i32>().value(1), &[] as &[u8]);

    // row 3: all NULL except id
    for (i, col) in columns.iter().enumerate().skip(1) {
        assert!(c(i).is_null(2), "column {} should be NULL in row 3", col.name);
    }
}

#[tokio::test]
async fn t03_stream_two_million_rows() {
    require_server!();
    let mut conn = connect().await;
    let opts = ExecOptions { batch_rows: 4096, ..Default::default() };
    let started = Instant::now();
    let mut stream = conn.execute("SELECT * FROM dbo.big", &opts).await.unwrap();
    let mut total = 0u64;
    let mut batches = 0usize;
    let mut starts = 0;
    let mut end_rows = 0;
    let mut last: Option<StreamItem> = None;
    while let Some(item) = stream.next().await {
        match &item {
            StreamItem::ResultSetStart { columns } => {
                starts += 1;
                assert_eq!(columns.len(), 5);
            }
            StreamItem::Rows(b) => {
                assert!(b.num_rows() <= 4096 && b.num_rows() > 0);
                if !total.is_multiple_of(4096) {
                    panic!("only the last batch may be partial");
                }
                total += b.num_rows() as u64;
                batches += 1;
            }
            StreamItem::ResultSetEnd { rows } => end_rows = *rows,
            _ => {}
        }
        last = Some(item);
    }
    drop(stream);
    let elapsed = started.elapsed();
    assert_eq!(starts, 1);
    assert_eq!(total, 2_000_000);
    assert_eq!(end_rows, 2_000_000);
    assert_eq!(batches, 2_000_000usize.div_ceil(4096));
    assert!(matches!(last, Some(StreamItem::Done { error: None, cancelled: false })), "{last:?}");
    eprintln!("streamed 2,000,000 rows in {elapsed:?}");
    assert!(elapsed < Duration::from_secs(60), "too slow: {elapsed:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t04_cancel_from_another_task() {
    require_server!();
    let mut conn = connect().await;
    let handle = conn.cancel_handle();
    let mut stream = conn.execute("SELECT * FROM dbo.big", &ExecOptions::default()).await.unwrap();
    let mut rows = 0u64;
    let mut cancel_sent_at: Option<Instant> = None;
    let mut last = None;
    loop {
        let item = match tokio::time::timeout(Duration::from_secs(5), stream.next()).await {
            Ok(Some(item)) => item,
            Ok(None) => break,
            Err(_) => panic!("stream did not finish within 5 s of cancellation (rows={rows})"),
        };
        if let StreamItem::Rows(b) = &item {
            rows += b.num_rows() as u64;
            if rows >= 20_000 && cancel_sent_at.is_none() {
                let h = handle.clone();
                tokio::spawn(async move { h.cancel() }).await.unwrap();
                cancel_sent_at = Some(Instant::now());
            }
        }
        last = Some(item);
    }
    drop(stream);
    let sent = cancel_sent_at.expect("cancel was sent");
    assert!(sent.elapsed() < Duration::from_secs(5));
    assert!(matches!(last, Some(StreamItem::Done { error: None, cancelled: true })), "{last:?}");
    assert!((20_000..2_000_000).contains(&rows), "rows={rows}");
    assert!(handle.is_cancelled());

    // connection still usable
    conn.ping().await.expect("ping after cancel");
    let items = run(&mut *conn, "SELECT COUNT(*) AS n FROM dbo.big", &ExecOptions::default()).await;
    let sets = result_sets(&items);
    assert_eq!(sets[0].1[0].column(0).as_primitive::<Int32Type>().value(0), 2_000_000);
    assert_eq!(done(&items), (None, false));

    // a second cancel handle works for a second run
    let handle2 = conn.cancel_handle();
    let mut stream = conn.execute("SELECT * FROM dbo.big", &ExecOptions::default()).await.unwrap();
    let mut n = 0;
    while let Some(item) = stream.next().await {
        if let StreamItem::Rows(_) = item {
            n += 1;
            if n == 3 {
                handle2.cancel();
            }
        }
        if let StreamItem::Done { cancelled, .. } = item {
            assert!(cancelled);
        }
    }
    drop(stream);
    conn.ping().await.unwrap();
}

#[tokio::test]
async fn t05_messages_interleaved_with_result_sets() {
    require_server!();
    let mut conn = connect().await;
    let items = run(&mut *conn, "EXEC dbo.p_multi 5", &ExecOptions::default()).await;
    let s: Vec<String> = shape(&items).into_iter().filter(|s| !s.starts_with("RA:")).collect();
    assert_eq!(s, vec!["M:starting p_multi", "RS:5", "M:informational message 5", "RS:17", "M:done", "DONE"], "{:?}", shape(&items));
    let raise = items
        .iter()
        .find_map(|i| match i {
            StreamItem::Message(m) if m.message == "informational message 5" => Some(m.clone()),
            _ => None,
        })
        .unwrap();
    // SQL Server reports RAISERROR severity 10 as an INFO token with severity 0 (documented).
    assert!(raise.class <= 10, "{raise:?}");
    assert_eq!(raise.number, 50000);
    assert_eq!(raise.state, 1);
    assert!(!raise.is_error);
    assert!(raise.procedure.as_deref().unwrap_or_default().ends_with("p_multi"), "{raise:?}");
    let print = items
        .iter()
        .find_map(|i| match i {
            StreamItem::Message(m) if m.message == "starting p_multi" => Some(m.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(print.class, 0);
    assert!(print.headline().is_none());
}

#[tokio::test]
async fn t06_error_after_first_result_set() {
    require_server!();
    let mut conn = connect().await;
    let items = run(&mut *conn, "EXEC dbo.p_error", &ExecOptions::default()).await;
    let sets = result_sets(&items);
    assert!(!sets.is_empty());
    assert_eq!(sets[0].0[0].name, "before_error");
    assert_eq!(sets[0].2, 1);
    assert_eq!(sets[0].1[0].column(0).as_primitive::<Int32Type>().value(0), 1);
    let (err, cancelled) = done(&items);
    let err = err.expect("Done must carry the error");
    assert!(!cancelled);
    assert_eq!(err.number, 8134);
    assert_eq!(err.class, 16);
    assert!(err.is_error);
    assert!(err.procedure.as_deref().unwrap_or_default().ends_with("p_error"), "{err:?}");
    assert!(err.message.contains("Divide by zero"), "{}", err.message);
    // the error is also delivered in place as a message
    assert!(items.iter().any(|i| matches!(i, StreamItem::Message(m) if m.is_error && m.number == 8134)));
    conn.ping().await.unwrap();
}

#[tokio::test]
async fn t07_prints_and_rows_affected() {
    require_server!();
    let mut conn = connect().await;
    let items = run(&mut *conn, "PRINT 'a'; SELECT 1; PRINT 'b'; UPDATE dbo.child SET qty = qty WHERE 1=0;", &ExecOptions::default()).await;
    let s = shape(&items);
    assert_eq!(s, vec!["M:a", "RS:1", "M:b", "RA:0", "DONE"], "{s:?}");

    // NOCOUNT suppresses the count
    let items = run(&mut *conn, "UPDATE dbo.child SET qty = qty WHERE 1=0;", &ExecOptions { nocount: true, ..Default::default() }).await;
    assert_eq!(shape(&items), vec!["DONE"]);
    let items = run(&mut *conn, "UPDATE dbo.child SET qty = qty WHERE 1=0;", &ExecOptions::default()).await;
    assert_eq!(shape(&items), vec!["RA:0", "DONE"]);
}

#[tokio::test]
async fn t08_estimated_plan() {
    require_server!();
    let mut conn = connect().await;
    let opts = ExecOptions { plan: PlanMode::Estimated, ..Default::default() };
    let items = run(&mut *conn, "SELECT TOP 10 * FROM dbo.big", &opts).await;
    assert_eq!(done(&items), (None, false), "{:?}", shape(&items));
    let sets = result_sets(&items);
    assert_eq!(sets.len(), 1, "{:?}", shape(&items));
    assert!(is_showplan_result(&sets[0].0), "{:?}", sets[0].0);
    assert_eq!(sets[0].2, 1);
    let xml = sets[0].1[0].column(0).as_string::<i64>().value(0).to_string();
    assert!(xml.starts_with("<ShowPlanXML"), "{}", &xml[..xml.len().min(80)]);

    // showplan is switched off again: the next batch returns data
    let items = run(&mut *conn, "SELECT TOP 3 id FROM dbo.big", &ExecOptions::default()).await;
    let sets = result_sets(&items);
    assert_eq!(sets.len(), 1);
    assert!(!is_showplan_result(&sets[0].0));
    assert_eq!(sets[0].2, 3);
}

#[tokio::test]
async fn t09_actual_plan() {
    require_server!();
    let mut conn = connect().await;
    let opts = ExecOptions { plan: PlanMode::Actual, ..Default::default() };
    let items = run(&mut *conn, "SELECT TOP 10 * FROM dbo.big", &opts).await;
    assert_eq!(done(&items), (None, false), "{:?}", shape(&items));
    let sets = result_sets(&items);
    assert_eq!(sets.len(), 2, "{:?}", shape(&items));
    assert!(!is_showplan_result(&sets[0].0));
    assert_eq!(sets[0].2, 10);
    assert!(is_showplan_result(&sets[1].0));
    let xml = sets[1].1[0].column(0).as_string::<i64>().value(0).to_string();
    assert!(xml.starts_with("<ShowPlanXML"));
    assert!(xml.contains("ActualRows"), "actual plan should contain runtime counters");

    let items = run(&mut *conn, "SELECT TOP 3 id FROM dbo.big", &ExecOptions::default()).await;
    assert_eq!(result_sets(&items).len(), 1, "STATISTICS XML must be off again: {:?}", shape(&items));
}

#[tokio::test]
async fn t10_catalog() {
    require_server!();
    let mut conn = connect().await;

    let dbs = conn.list_databases().await.unwrap();
    let find = |n: &str| dbs.iter().find(|d| d.name.eq_ignore_ascii_case(n)).cloned();
    assert!(find("cobalt_test").is_some(), "{dbs:?}");
    let master = find("master").unwrap();
    assert!(master.is_system);
    assert!(!find("cobalt_test").unwrap().is_system);
    assert_eq!(master.state, "ONLINE");

    let schemas = conn.list_schemas("cobalt_test").await.unwrap();
    assert!(schemas.iter().any(|s| s == "dbo") && schemas.iter().any(|s| s == "reports"), "{schemas:?}");
    assert!(!schemas.iter().any(|s| s == "sys"));

    let objects = conn.list_objects("cobalt_test").await.unwrap();
    let has = |schema: &str, name: &str, kind: ObjectKind| objects.iter().any(|o| o.schema == schema && o.name == name && o.kind == kind && o.database == "cobalt_test" && o.object_id.is_some());
    assert!(has("dbo", "big", ObjectKind::Table), "{objects:?}");
    assert!(has("dbo", "v_big_summary", ObjectKind::View));
    assert!(has("dbo", "p_multi", ObjectKind::Procedure));
    assert!(has("dbo", "f_scalar", ObjectKind::ScalarFunction));
    assert!(has("dbo", "f_tvf", ObjectKind::TableFunction));
    assert!(has("dbo", "big_syn", ObjectKind::Synonym));
    assert!(has("dbo", "seq_test", ObjectKind::Sequence));
    assert!(has("dbo", "IdList", ObjectKind::TableType));
    assert!(has("reports", "AllCaseDetails", ObjectKind::View));

    let all_types = objects.iter().find(|o| o.name == "all_types").unwrap().clone();
    let cols = conn.list_columns(&all_types).await.unwrap();
    assert_eq!(cols.len(), 31);
    let col = |n: &str| cols.iter().find(|c| c.name == n).unwrap_or_else(|| panic!("column {n}")).clone();
    assert_eq!(col("c_decimal").sql_type, SqlType::Decimal { precision: 18, scale: 4 });
    assert_eq!(col("c_nvarcharmax").sql_type, SqlType::NVarChar { len: None });
    assert_eq!(col("c_nvarchar").sql_type, SqlType::NVarChar { len: Some(100) });
    assert_eq!(col("c_time").sql_type, SqlType::Time { scale: 7 });
    assert_eq!(col("c_sqlvariant").sql_type, SqlType::SqlVariant);
    let id = col("id");
    assert!(id.is_identity && id.in_primary_key && !id.nullable && id.ordinal == 0);
    assert!(col("c_bit").nullable && !col("c_bit").in_primary_key);

    // columns by name only (no object_id) and through the table type path
    let by_name = ObjectRef { database: "cobalt_test".into(), schema: "dbo".into(), name: "child".into(), kind: ObjectKind::Table, object_id: None };
    assert_eq!(conn.list_columns(&by_name).await.unwrap().len(), 3);
    let idlist = ObjectRef { database: "cobalt_test".into(), schema: "dbo".into(), name: "IdList".into(), kind: ObjectKind::TableType, object_id: None };
    let tt_cols = conn.list_columns(&idlist).await.unwrap();
    assert_eq!(tt_cols.len(), 1);
    assert!(tt_cols[0].in_primary_key);

    let child = objects.iter().find(|o| o.name == "child").unwrap().clone();
    let indexes = conn.list_indexes(&child).await.unwrap();
    assert!(indexes.iter().any(|i| i.is_primary_key && i.is_clustered && i.key_columns == vec![("id".to_string(), false)]), "{indexes:?}");
    let ix = indexes.iter().find(|i| i.name == "IX_child_qty").unwrap();
    assert_eq!(ix.key_columns, vec![("qty".to_string(), false)]);
    assert_eq!(ix.included_columns, vec!["big_id".to_string()]);
    assert!(!ix.is_unique && !ix.is_clustered);
    assert!(indexes.iter().any(|i| i.name == "UQ_child" && i.is_unique_constraint && i.key_columns.len() == 2));

    let keys = conn.list_keys(&child).await.unwrap();
    let fk = keys.iter().find(|k| k.name == "FK_child_big").unwrap();
    assert_eq!(fk.kind, KeyKind::ForeignKey);
    assert_eq!(fk.columns, vec!["big_id".to_string()]);
    let (referenced, ref_cols) = fk.references.clone().unwrap();
    assert_eq!((referenced.schema.as_str(), referenced.name.as_str()), ("dbo", "big"));
    assert_eq!(ref_cols, vec!["id".to_string()]);
    let ck = keys.iter().find(|k| k.name == "CK_qty").unwrap();
    assert_eq!(ck.kind, KeyKind::Check);
    assert!(ck.definition.as_deref().unwrap().contains("qty"));
    assert!(keys.iter().any(|k| k.name == "UQ_child" && k.kind == KeyKind::Unique && k.columns == vec!["big_id".to_string(), "qty".to_string()]));
    assert!(keys.iter().any(|k| k.kind == KeyKind::PrimaryKey && k.columns == vec!["id".to_string()]));

    let params = conn.list_parameters(&objects.iter().find(|o| o.name == "p_multi").unwrap().clone()).await.unwrap();
    assert_eq!(params.len(), 1);
    assert_eq!(params[0].name, "@n");
    assert_eq!(params[0].sql_type, SqlType::Int);
    assert!(!params[0].is_output);

    let cat = conn.load_catalog("cobalt_test").await.unwrap();
    assert_eq!(cat.database, "cobalt_test");
    assert!(cat.schemas.contains(&"reports".to_string()));
    assert!(cat.objects.len() >= 10);
    let big = cat.find(Some("dbo"), "big").unwrap();
    assert_eq!(cat.columns[&big.object_id.unwrap()].len(), 5);
    let all = cat.find(None, "ALL_TYPES").unwrap();
    assert_eq!(cat.columns[&all.object_id.unwrap()].len(), 31);
    assert_eq!(cat.columns[&all.object_id.unwrap()][6].sql_type, SqlType::Decimal { precision: 18, scale: 4 });
    let idlist = cat.find(Some("dbo"), "IdList").unwrap();
    assert_eq!(cat.columns[&idlist.object_id.unwrap()].len(), 1);
    assert!(cat.refreshed_at.is_some());

    // catalog of another database through three-part names, without changing context
    let master_objects = conn.list_objects("master").await.unwrap();
    assert!(master_objects.iter().all(|o| o.database == "master"), "{master_objects:?}");
    let spt = ObjectRef { database: "master".into(), schema: "dbo".into(), name: "spt_values".into(), kind: ObjectKind::Table, object_id: None };
    let spt_cols = conn.list_columns(&spt).await.unwrap();
    assert!(spt_cols.iter().any(|c| c.name == "number"), "{spt_cols:?}");
    assert_eq!(conn.current_database(), "cobalt_test");
    let master_schemas = conn.list_schemas("master").await.unwrap();
    assert!(master_schemas.contains(&"dbo".to_string()));
}

#[tokio::test]
async fn t11_scripting() {
    require_server!();
    let mut conn = connect().await;
    let objects = conn.list_objects("cobalt_test").await.unwrap();
    let find = |n: &str| objects.iter().find(|o| o.name == n).unwrap().clone();

    let s = conn.script(&find("child"), ScriptKind::Create).await.unwrap();
    assert!(s.starts_with("USE [cobalt_test]\nGO\n"), "{s}");
    assert!(s.contains("CREATE TABLE [dbo].[child] ("), "{s}");
    assert!(s.contains("[id] int NOT NULL IDENTITY(1,1)"), "{s}");
    assert!(s.contains("PRIMARY KEY CLUSTERED ([id] ASC)"), "{s}");
    assert!(s.contains("CONSTRAINT [UQ_child] UNIQUE NONCLUSTERED ([big_id] ASC, [qty] ASC)"), "{s}");
    assert!(s.contains("ADD CONSTRAINT [FK_child_big] FOREIGN KEY ([big_id]) REFERENCES [dbo].[big] ([id]);"), "{s}");
    assert!(s.contains("ADD CONSTRAINT [CK_qty] CHECK ([qty]>(0));"), "{s}");
    assert!(s.contains("CREATE NONCLUSTERED INDEX [IX_child_qty] ON [dbo].[child] ([qty] ASC) INCLUDE ([big_id]);"), "{s}");
    // the generated DDL must parse: run it as a temp copy via a renamed table
    let ddl = s.replace("[dbo].[child]", "[dbo].[child_script_check]").replace("[PK__child__", "[PK__child_sc__").replace("[UQ_child]", "[UQ_child_sc]").replace("[FK_child_big]", "[FK_child_big_sc]").replace("[CK_qty]", "[CK_qty_sc]").replace("[IX_child_qty]", "[IX_child_qty_sc]");
    let ddl = ddl.replacen("USE [cobalt_test]\nGO\n\n", "", 1);
    for batch in cobalt_driver::split_batches(&ddl) {
        let items = run(&mut *conn, &batch.sql, &ExecOptions::default()).await;
        assert_eq!(done(&items).0, None, "batch failed:\n{}", batch.sql);
    }
    let items = run(&mut *conn, "DROP TABLE dbo.child_script_check", &ExecOptions::default()).await;
    assert_eq!(done(&items).0, None);

    let s = conn.script(&find("big"), ScriptKind::Select).await.unwrap();
    assert_eq!(s, "USE [cobalt_test]\nGO\n\nSELECT TOP (1000) [id],\n       [category],\n       [amount],\n       [created],\n       [note]\nFROM [dbo].[big];\n");

    let s = conn.script(&find("p_multi"), ScriptKind::Execute).await.unwrap();
    assert!(s.contains("DECLARE @n int;"), "{s}");
    assert!(s.contains("EXEC [dbo].[p_multi]\n    @n = @n;"), "{s}");

    let s = conn.script(&find("p_multi"), ScriptKind::Create).await.unwrap();
    assert!(s.contains("CREATE   PROCEDURE dbo.p_multi") || s.contains("CREATE OR ALTER PROCEDURE dbo.p_multi"), "{s}");
    assert!(s.contains("PRINT 'starting p_multi'"));
    let s = conn.script(&find("v_big_summary"), ScriptKind::Alter).await.unwrap();
    assert!(s.contains("CREATE OR ALTER VIEW dbo.v_big_summary"), "{s}");
    let s = conn.script(&find("f_scalar"), ScriptKind::Drop).await.unwrap();
    assert!(s.contains("DROP FUNCTION [dbo].[f_scalar];"), "{s}");
    let s = conn.script(&find("big_syn"), ScriptKind::Create).await.unwrap();
    assert!(s.contains("CREATE SYNONYM [dbo].[big_syn] FOR [dbo].[big];"), "{s}");
    let s = conn.script(&find("seq_test"), ScriptKind::Create).await.unwrap();
    assert!(s.contains("CREATE SEQUENCE [dbo].[seq_test] AS bigint\n    START WITH 1\n    INCREMENT BY 1"), "{s}");
    let s = conn.script(&find("IdList"), ScriptKind::Create).await.unwrap();
    assert!(s.contains("CREATE TYPE [dbo].[IdList] AS TABLE (\n    [id] int NOT NULL,\n    PRIMARY KEY CLUSTERED ([id] ASC)\n);"), "{s}");
    let s = conn.script(&find("f_tvf"), ScriptKind::Select).await.unwrap();
    assert!(s.contains("DECLARE @cat varchar(10);") && s.contains("FROM [dbo].[f_tvf](@cat);"), "{s}");
}

#[tokio::test]
async fn t12_command_timeout() {
    require_server!();
    let mut conn = connect().await;
    let opts = ExecOptions { timeout_secs: 1, ..Default::default() };
    let started = Instant::now();
    let items = run(&mut *conn, "WAITFOR DELAY '00:00:05'; SELECT 1", &opts).await;
    let elapsed = started.elapsed();
    assert!(elapsed < Duration::from_secs(2), "took {elapsed:?}");
    let (err, cancelled) = done(&items);
    let err = err.expect("timeout must be reported as an error");
    assert!(err.message.to_ascii_lowercase().contains("timeout"), "{}", err.message);
    assert!(!cancelled);
    assert!(result_sets(&items).is_empty());
    // usable afterwards
    conn.ping().await.expect("ping after timeout");
    let items = run(&mut *conn, "SELECT 2", &ExecOptions::default()).await;
    assert_eq!(shape(&items), vec!["RS:1", "DONE"]);
}

#[tokio::test]
async fn t13_batch_error_has_line_number() {
    require_server!();
    let mut conn = connect().await;
    let items = run(&mut *conn, "SELECT 1 AS ok;\n\nSELECT 1/0 AS boom;", &ExecOptions::default()).await;
    let (err, _) = done(&items);
    let err = err.unwrap();
    assert_eq!(err.number, 8134);
    assert_eq!(err.line, 3, "{err:?}");
    assert_eq!(err.class, 16);
    assert_eq!(err.headline().as_deref(), Some("Msg 8134, Level 16, State 1, Line 3"));
    let sets = result_sets(&items);
    assert_eq!(sets[0].2, 1, "the first statement's rows are delivered before the error");
    conn.ping().await.unwrap();

    // RAISERROR 16 does not abort the batch: rows after it still arrive, Done carries the error
    let items = run(&mut *conn, "RAISERROR('custom', 16, 1); SELECT 7 AS after", &ExecOptions::default()).await;
    assert_eq!(shape(&items), vec!["M:custom", "RS:1", "ERR:50000"]);
}

#[tokio::test]
async fn t14_change_database() {
    require_server!();
    let mut conn = connect().await;
    assert_eq!(conn.current_database(), "cobalt_test");
    conn.change_database("master").await.unwrap();
    assert_eq!(conn.current_database(), "master");
    let items = run(&mut *conn, "SELECT DB_NAME() AS db", &ExecOptions::default()).await;
    assert_eq!(result_sets(&items)[0].1[0].column(0).as_string::<i32>().value(0), "master");
    // USE inside a batch is tracked through ENVCHANGE too
    let items = run(&mut *conn, "USE cobalt_test;", &ExecOptions::default()).await;
    assert_eq!(done(&items).0, None);
    assert_eq!(conn.current_database(), "cobalt_test");
    assert!(matches!(conn.change_database("no_such_db_xyz").await, Err(cobalt_driver::DriverError::Server(m)) if m.number == 911));
    assert_eq!(conn.current_database(), "cobalt_test");
}

#[tokio::test]
async fn t15_login_failure_and_dropped_stream() {
    require_server!();
    let bad = ResolvedCredentials::SqlLogin { user: "sa".into(), password: Secret::new("wrong") };
    let err = MssqlDriver::new().connect(&profile(), &bad, ConnectionRole::Query).await.err().expect("must fail");
    assert!(matches!(err, cobalt_driver::DriverError::Login(_)), "{err:?}");
    assert!(err.hint().is_some());

    // dropping a stream mid-way must not break the next call
    let mut conn = connect().await;
    {
        let mut stream = conn.execute("SELECT * FROM dbo.big", &ExecOptions::default()).await.unwrap();
        let _ = stream.next().await;
        let _ = stream.next().await;
    }
    let items = run(&mut *conn, "SELECT 3 AS three", &ExecOptions::default()).await;
    assert_eq!(shape(&items), vec!["RS:1", "DONE"]);

    // dropping an estimated-plan run must still switch showplan off
    {
        let opts = ExecOptions { plan: PlanMode::Estimated, ..Default::default() };
        let mut stream = conn.execute("SELECT TOP 10 * FROM dbo.big", &opts).await.unwrap();
        let _ = stream.next().await;
    }
    let items = run(&mut *conn, "SELECT TOP 2 id FROM dbo.big", &ExecOptions::default()).await;
    let sets = result_sets(&items);
    assert!(!is_showplan_result(&sets[0].0));
    assert_eq!(sets[0].2, 2);
}
