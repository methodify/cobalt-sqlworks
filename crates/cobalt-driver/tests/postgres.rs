//! Integration tests against a live PostgreSQL (see tests/seed_postgres.sql at the workspace root).
//!
//! Gated on `COBALT_TEST_POSTGRES=1`; without it every test prints "skipped" and passes.
//! Overrides: `COBALT_TEST_POSTGRES_SERVER` (default `localhost:5433`), `COBALT_TEST_POSTGRES_USER`
//! (`postgres`), `COBALT_TEST_POSTGRES_PASSWORD`, `COBALT_TEST_POSTGRES_DB` (`cobalt_test`).
//! Run with `--test-threads=1`: the tests share one server and some measure timing.

use arrow::array::{Array, AsArray};
use arrow::datatypes::{DataType, Float64Type, Int16Type, Int32Type, Int64Type, TimeUnit, TimestampMicrosecondType};
use cobalt_core::*;
use cobalt_driver::postgres::PostgresDriver;
use cobalt_driver::{Connection, Driver, ScriptKind, StreamItem};
use futures_util::StreamExt;
use std::time::{Duration, Instant};

fn enabled() -> bool {
    std::env::var("COBALT_TEST_POSTGRES").map(|v| !v.is_empty() && v != "0").unwrap_or(false)
}

macro_rules! require_server {
    () => {
        if !enabled() {
            eprintln!("skipped (set COBALT_TEST_POSTGRES=1 to run against a live server)");
            return;
        }
    };
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| default.to_string())
}

fn profile() -> ConnectionProfile {
    let mut p = ConnectionProfile::postgres(env_or("COBALT_TEST_POSTGRES_SERVER", "localhost:5433"), Some(env_or("COBALT_TEST_POSTGRES_DB", "cobalt_test")), env_or("COBALT_TEST_POSTGRES_USER", "postgres"));
    p.options.trust_server_certificate = true;
    p.options.encrypt = Encrypt::Optional;
    p.options.connect_timeout_secs = 15;
    p
}

fn creds() -> ResolvedCredentials {
    ResolvedCredentials::SqlLogin { user: env_or("COBALT_TEST_POSTGRES_USER", "postgres"), password: Secret::new(env_or("COBALT_TEST_POSTGRES_PASSWORD", "Cobalt!Dev2026pw")) }
}

async fn connect() -> Box<dyn Connection> {
    PostgresDriver.connect(&profile(), &creds(), ConnectionRole::Query).await.expect("connect")
}

async fn run(conn: &mut dyn Connection, sql: &str, opts: &ExecOptions) -> Vec<StreamItem> {
    let stream = conn.execute(sql, opts).await.expect("execute");
    stream.collect().await
}

/// Compact rendering for order assertions: `M:text`, `RS:<rows>`, `RA:<n>`, `DONE`, `ERR`.
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
            StreamItem::Message(m) => out.push(format!("M:{}", m.message.lines().next().unwrap_or(""))),
            StreamItem::Done { error: None, cancelled } => out.push(if *cancelled { "CANCELLED".into() } else { "DONE".into() }),
            StreamItem::Done { error: Some(_), .. } => out.push("ERR".into()),
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

fn messages(items: &[StreamItem]) -> Vec<&ServerMessage> {
    items.iter().filter_map(|i| if let StreamItem::Message(m) = i { Some(m) } else { None }).collect()
}

fn us(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32, micro: u32) -> i64 {
    chrono::NaiveDate::from_ymd_opt(y, mo, d).unwrap().and_hms_micro_opt(h, mi, s, micro).unwrap().and_utc().timestamp_micros()
}

fn obj(schema: &str, name: &str, kind: ObjectKind) -> ObjectRef {
    ObjectRef { database: env_or("COBALT_TEST_POSTGRES_DB", "cobalt_test"), schema: schema.into(), name: name.into(), kind, object_id: None }
}

// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn t01_connect_engine_info() {
    require_server!();
    let mut conn = connect().await;
    let e = conn.engine();
    assert_eq!(e.kind, EngineKind::Postgres, "{e:?}");
    assert!(e.major_version() >= 12, "version {}", e.version);
    assert!(e.version_text.starts_with("PostgreSQL"), "{}", e.version_text);
    assert!(e.capabilities.schemas && e.capabilities.transactions && !e.capabilities.multiple_databases);
    assert_eq!(conn.current_database(), "cobalt_test");
    assert!(conn.is_usable());
    conn.ping().await.unwrap();
    let dbs = conn.list_databases().await.unwrap();
    assert!(dbs.iter().any(|d| d.name == "cobalt_test"), "{dbs:?}");
}

#[tokio::test]
async fn t02_all_types_values() {
    require_server!();
    let mut conn = connect().await;
    let items = run(&mut *conn, "SELECT * FROM public.all_types ORDER BY id", &ExecOptions::default()).await;
    assert_eq!(shape(&items), vec!["RS:3", "DONE"]);
    let sets = result_sets(&items);
    let (cols, batches, _) = &sets[0];
    let b = arrow::compute::concat_batches(&batches[0].schema(), batches).unwrap();
    let col = |name: &str| cols.iter().position(|c| c.name == name).unwrap_or_else(|| panic!("no column {name}"));
    let ty = |name: &str| cols[col(name)].sql_type.clone();
    assert_eq!(ty("id"), SqlType::Int);
    assert_eq!(ty("b"), SqlType::Bit);
    assert_eq!(ty("si"), SqlType::SmallInt);
    assert_eq!(ty("bi"), SqlType::BigInt);
    assert_eq!(ty("r"), SqlType::Real);
    assert_eq!(ty("f"), SqlType::Float);
    assert_eq!(ty("n"), SqlType::Other { name: "numeric".into() });
    assert_eq!(ty("t"), SqlType::NVarChar { len: None });
    assert_eq!(ty("d"), SqlType::Date);
    assert_eq!(ty("ts"), SqlType::DateTime2 { scale: 6 });
    assert_eq!(ty("tz"), SqlType::DateTimeOffset { scale: 6 });
    assert_eq!(ty("tm"), SqlType::Time { scale: 6 });
    assert_eq!(ty("u"), SqlType::UniqueIdentifier);
    assert_eq!(ty("j"), SqlType::Json);
    assert_eq!(ty("by"), SqlType::VarBinary { len: None });
    assert_eq!(ty("arr"), SqlType::Other { name: "_int4".into() });
    assert_eq!(ty("iv"), SqlType::Other { name: "interval".into() });

    assert_eq!(b.column(col("id")).as_primitive::<Int32Type>().value(0), 1);
    assert!(b.column(col("b")).as_boolean().value(0));
    assert!(!b.column(col("b")).as_boolean().value(1));
    assert!(b.column(col("b")).is_null(2));
    assert_eq!(b.column(col("si")).as_primitive::<Int16Type>().value(2), -32768);
    assert_eq!(b.column(col("bi")).as_primitive::<Int64Type>().value(0), 9007199254740993);
    assert_eq!(b.column(col("f")).as_primitive::<Float64Type>().value(0), 2.25);
    assert_eq!(b.column(col("n")).as_string::<i64>().value(0), "1234.567");
    assert_eq!(b.column(col("n")).as_string::<i64>().value(1), "-0.001");
    assert_eq!(b.column(col("t")).as_string::<i64>().value(2), "ünïcödé ✓ 日本語");
    assert_eq!(b.column(col("vc")).as_string::<i64>().value(2), "q\"uote");
    assert_eq!(b.column(col("d")).data_type(), &DataType::Date32);
    assert_eq!(b.column(col("d")).as_primitive::<arrow::datatypes::Date32Type>().value(0), 19782);
    assert_eq!(b.column(col("ts")).data_type(), &DataType::Timestamp(TimeUnit::Microsecond, None));
    assert_eq!(b.column(col("ts")).as_primitive::<TimestampMicrosecondType>().value(0), us(2024, 2, 29, 13, 45, 30, 123456));
    // 13:45:30+02 is 11:45:30 UTC
    assert_eq!(b.column(col("tz")).as_primitive::<TimestampMicrosecondType>().value(0), us(2024, 2, 29, 11, 45, 30, 0));
    assert_eq!(b.column(col("u")).as_string::<i32>().value(0), "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11");
    assert_eq!(b.column(col("j")).as_string::<i64>().value(0), "{\"k\": [1, 2, {\"x\": null}]}");
    assert_eq!(b.column(col("by")).as_binary::<i64>().value(0), &[0u8, 0xff, 0x10]);
    assert_eq!(b.column(col("by")).as_binary::<i64>().value(2), &[] as &[u8]);
    assert_eq!(b.column(col("arr")).as_string::<i64>().value(0), "{1,2,3}");
    assert_eq!(b.column(col("iv")).as_string::<i64>().value(0), "1 day 02:03:04");
    assert!(b.column(col("t")).is_null(1));
    assert!(b.column(col("arr")).is_null(1));
    // nothing failed to parse
    assert!(messages(&items).is_empty(), "{:?}", shape(&items));
}

#[tokio::test]
async fn t03_stream_big_table() {
    require_server!();
    let mut conn = connect().await;
    let started = Instant::now();
    let opts = ExecOptions { batch_rows: 10_000, ..Default::default() };
    let mut stream = conn.execute("SELECT * FROM public.big ORDER BY id", &opts).await.unwrap();
    let mut rows = 0u64;
    let mut batches = 0;
    let mut first_batch_at = None;
    while let Some(item) = stream.next().await {
        match item {
            StreamItem::Rows(b) => {
                rows += b.num_rows() as u64;
                batches += 1;
                first_batch_at.get_or_insert(started.elapsed());
            }
            StreamItem::ResultSetEnd { rows: n } => assert_eq!(n, 200_000),
            StreamItem::Done { error, .. } => assert!(error.is_none(), "{error:?}"),
            _ => {}
        }
    }
    assert_eq!(rows, 200_000);
    assert!(batches >= 20, "batches {batches}");
    eprintln!("200k rows in {:?}, first batch after {:?}", started.elapsed(), first_batch_at.unwrap());
}

#[tokio::test]
async fn t04_cancel_from_another_task() {
    require_server!();
    let mut conn = connect().await;
    let handle = conn.cancel_handle();
    let started = Instant::now();
    let canceller = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(400)).await;
        handle.cancel();
    });
    let items = run(&mut *conn, "SELECT pg_sleep(20)", &ExecOptions::default()).await;
    canceller.await.unwrap();
    let (error, cancelled) = done(&items);
    assert!(cancelled, "{:?}", shape(&items));
    assert!(error.map(|e| e.message.contains("cancelled")).unwrap_or(true));
    assert!(started.elapsed() < Duration::from_secs(5), "cancel took {:?}", started.elapsed());
    // the connection stays usable
    assert!(conn.is_usable());
    let items = run(&mut *conn, "SELECT 1", &ExecOptions::default()).await;
    assert_eq!(shape(&items), vec!["RS:1", "DONE"]);
}

#[tokio::test]
async fn t05_script_with_dollar_quotes_and_rows_affected() {
    require_server!();
    let mut conn = connect().await;
    let sql = "DROP TABLE IF EXISTS public.t05;\nCREATE TABLE public.t05 (id int PRIMARY KEY, name text);\nINSERT INTO public.t05 VALUES (1, 'a'), (2, 'b');\n\
               CREATE OR REPLACE FUNCTION public.t05_count() RETURNS bigint LANGUAGE sql AS $$ SELECT count(*) FROM public.t05; $$;\n\
               SELECT public.t05_count() AS n;\nUPDATE public.t05 SET name = 'c';\nSELECT * FROM public.t05 ORDER BY id;\nDROP TABLE public.t05;\nDROP FUNCTION public.t05_count();";
    let items = run(&mut *conn, sql, &ExecOptions::default()).await;
    // the DROP IF EXISTS notice reaches the messages first
    let shape: Vec<String> = shape(&items).into_iter().filter(|s| !s.starts_with("M:NOTICE")).collect();
    assert_eq!(shape, vec!["RA:2", "RS:1", "RA:2", "RS:2", "DONE"]);
    let sets = result_sets(&items);
    // the function's type came from the server's parse of the statement
    assert_eq!(sets[0].0[0].sql_type, SqlType::BigInt);
    assert_eq!(sets[0].1[0].column(0).as_primitive::<Int64Type>().value(0), 2);
    assert_eq!(sets[1].0[1].name, "name");
    assert_eq!(sets[1].1[0].column(1).as_string::<i64>().value(1), "c");
}

#[tokio::test]
async fn t06_error_has_sqlstate_and_line() {
    require_server!();
    let mut conn = connect().await;
    let items = run(&mut *conn, "SELECT 1 AS a;\n\nSELECT *\nFROM public.no_such_table;\nSELECT 2;", &ExecOptions::default()).await;
    let (error, cancelled) = done(&items);
    assert!(!cancelled);
    let e = error.expect("an error");
    assert!(e.message.starts_with("42P01: "), "{}", e.message);
    assert!(e.message.contains("no_such_table"), "{}", e.message);
    assert_eq!(e.line, 4, "{e:?}");
    // the first result set arrived, the third statement did not run
    assert_eq!(shape(&items)[0], "RS:1");
    assert!(!shape(&items).contains(&"RS:1".to_string()) || shape(&items).iter().filter(|s| s.starts_with("RS:")).count() == 1);
    assert!(conn.is_usable());
}

#[tokio::test]
async fn t07_failed_transaction_is_rolled_back() {
    require_server!();
    let mut conn = connect().await;
    let _ = run(&mut *conn, "DROP TABLE IF EXISTS public.t07; CREATE TABLE public.t07 (id int PRIMARY KEY)", &ExecOptions::default()).await;
    let items = run(&mut *conn, "BEGIN;\nINSERT INTO public.t07 VALUES (1);\nINSERT INTO public.t07 VALUES (1);\nCOMMIT;", &ExecOptions::default()).await;
    let (error, _) = done(&items);
    assert!(error.map(|e| e.message.starts_with("23505: ")).unwrap_or(false), "{:?}", shape(&items));
    assert!(messages(&items).iter().any(|m| m.message.contains("rolled back")), "{:?}", shape(&items));
    // nothing stayed behind and the session is clean
    let items = run(&mut *conn, "SELECT count(*) FROM public.t07", &ExecOptions::default()).await;
    assert_eq!(shape(&items), vec!["RS:1", "DONE"]);
    assert_eq!(result_sets(&items)[0].1[0].column(0).as_primitive::<Int64Type>().value(0), 0);
    let _ = run(&mut *conn, "DROP TABLE public.t07", &ExecOptions::default()).await;
}

#[tokio::test]
async fn t08_notices_reach_the_messages() {
    require_server!();
    let mut conn = connect().await;
    let items = run(&mut *conn, "DO $$ BEGIN RAISE NOTICE 'hello from plpgsql'; END $$;\nSELECT 1;", &ExecOptions::default()).await;
    assert!(messages(&items).iter().any(|m| !m.is_error && m.message.contains("hello from plpgsql")), "{:?}", shape(&items));
    assert_eq!(done(&items).0, None);
}

#[tokio::test]
async fn t09_plans() {
    require_server!();
    let mut conn = connect().await;
    let opts = ExecOptions { plan: PlanMode::Estimated, ..Default::default() };
    let items = run(&mut *conn, "SELECT * FROM shop.orders WHERE customer_id = 1", &opts).await;
    let plan = messages(&items).into_iter().find(|m| m.procedure.as_deref() == Some("plan")).expect("a plan message");
    assert!(plan.message.contains("Scan"), "{}", plan.message);
    assert!(!plan.message.contains("actual time"), "{}", plan.message);
    let opts = ExecOptions { plan: PlanMode::Actual, ..Default::default() };
    let items = run(&mut *conn, "SELECT count(*) FROM shop.orders", &opts).await;
    let plan = messages(&items).into_iter().find(|m| m.procedure.as_deref() == Some("plan")).expect("a plan message");
    assert!(plan.message.contains("actual time"), "{}", plan.message);
}

#[tokio::test]
async fn t10_catalog() {
    require_server!();
    let mut conn = connect().await;
    let schemas = conn.list_schemas("cobalt_test").await.unwrap();
    assert!(schemas.contains(&"shop".to_string()) && schemas.contains(&"public".to_string()), "{schemas:?}");
    let objects = conn.list_objects("cobalt_test").await.unwrap();
    let find = |s: &str, n: &str| objects.iter().find(|o| o.schema == s && o.name == n).unwrap_or_else(|| panic!("{s}.{n} missing"));
    assert_eq!(find("shop", "customers").kind, ObjectKind::Table);
    assert_eq!(find("shop", "v_customer_totals").kind, ObjectKind::View);
    assert_eq!(find("shop", "order_total").kind, ObjectKind::ScalarFunction);
    assert_eq!(find("shop", "touch_customer").kind, ObjectKind::Procedure);
    assert_eq!(find("shop", "ticket_seq").kind, ObjectKind::Sequence);
    assert!(find("shop", "orders").object_id.is_some());

    let cols = conn.list_columns(&obj("shop", "orders", ObjectKind::Table)).await.unwrap();
    assert_eq!(cols.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), vec!["id", "customer_id", "amount", "status", "created"]);
    assert!(cols[0].in_primary_key && !cols[0].nullable);
    assert_eq!(cols[2].sql_type, SqlType::Numeric { precision: 10, scale: 2 });
    assert_eq!(cols[4].sql_type, SqlType::DateTimeOffset { scale: 6 });
    assert_eq!(cols[4].default_definition.as_deref(), Some("now()"));
    let keys = conn.list_keys(&obj("shop", "orders", ObjectKind::Table)).await.unwrap();
    let fk = keys.iter().find(|k| k.kind == KeyKind::ForeignKey).expect("fk");
    assert_eq!(fk.columns, vec!["customer_id"]);
    assert_eq!(fk.references.as_ref().map(|(t, c)| (t.schema.as_str(), t.name.as_str(), c.clone())), Some(("shop", "customers", vec!["id".to_string()])));
    assert!(keys.iter().any(|k| k.kind == KeyKind::PrimaryKey && k.columns == vec!["id"]));
    assert!(keys.iter().any(|k| k.kind == KeyKind::Check));
    let idx = conn.list_indexes(&obj("shop", "orders", ObjectKind::Table)).await.unwrap();
    let ci = idx.iter().find(|i| i.name == "orders_customer_idx").expect("index");
    assert_eq!(ci.key_columns, vec![("customer_id".to_string(), false), ("created".to_string(), true)]);
    assert!(idx.iter().any(|i| i.is_primary_key && i.is_unique));
    let params = conn.list_parameters(&obj("shop", "order_total", ObjectKind::ScalarFunction)).await.unwrap();
    assert_eq!(params.len(), 1);
    assert_eq!(params[0].name, "p_customer");
    assert_eq!(params[0].sql_type, SqlType::Int);

    let cat = conn.load_catalog("cobalt_test").await.unwrap();
    let o = cat.find(Some("shop"), "customers").expect("in catalog");
    let cols = &cat.columns[&o.object_id.unwrap()];
    assert_eq!(cols[0].name, "id");
    assert!(cols[0].in_primary_key);
    assert!(cat.find(Some("shop"), "v_customer_totals").and_then(|v| cat.columns.get(&v.object_id.unwrap())).map(|c| c.len() == 4).unwrap_or(false));

    let stats = conn.table_stats(&obj("shop", "orders", ObjectKind::Table)).await.unwrap();
    assert_eq!(stats.rows, 2001);
    assert!(stats.reserved_bytes > 0);
    let values = conn.distinct_values(&obj("shop", "orders", ObjectKind::Table), "status", 10).await.unwrap();
    assert_eq!(values, vec!["cancelled", "new", "shipped"]);
}

#[tokio::test]
async fn t11_scripting() {
    require_server!();
    let mut conn = connect().await;
    let s = conn.script(&obj("shop", "orders", ObjectKind::Table), ScriptKind::Create).await.unwrap();
    assert!(s.starts_with("CREATE TABLE \"shop\".\"orders\" (\n"), "{s}");
    assert!(s.contains("\"amount\" numeric(10,2) NOT NULL"), "{s}");
    assert!(s.contains("CONSTRAINT \"orders_pkey\" PRIMARY KEY (id)"), "{s}");
    assert!(s.contains("FOREIGN KEY (customer_id) REFERENCES shop.customers(id)"), "{s}");
    let s = conn.script(&obj("shop", "v_customer_totals", ObjectKind::View), ScriptKind::Create).await.unwrap();
    assert!(s.starts_with("CREATE OR REPLACE VIEW \"shop\".\"v_customer_totals\" AS\n"), "{s}");
    assert!(s.contains("LEFT JOIN shop.orders"), "{s}");
    let s = conn.script(&obj("shop", "order_total", ObjectKind::ScalarFunction), ScriptKind::Create).await.unwrap();
    assert!(s.contains("CREATE OR REPLACE FUNCTION shop.order_total(p_customer integer)"), "{s}");
    let s = conn.script(&obj("shop", "touch_customer", ObjectKind::Procedure), ScriptKind::Execute).await.unwrap();
    assert_eq!(s, "CALL \"shop\".\"touch_customer\"(<p_id>);\n");
    let s = conn.script(&obj("shop", "customers", ObjectKind::Table), ScriptKind::Select).await.unwrap();
    assert!(s.contains("\"email\"") && s.ends_with("FROM \"shop\".\"customers\"\nLIMIT 1000;\n"), "{s}");
    let s = conn.script(&obj("shop", "ticket_seq", ObjectKind::Sequence), ScriptKind::Drop).await.unwrap();
    assert_eq!(s, "DROP SEQUENCE \"shop\".\"ticket_seq\";\n");
}

#[tokio::test]
async fn t12_bulk_insert_by_copy() {
    require_server!();
    let mut conn = connect().await;
    let _ = run(&mut *conn, "DROP TABLE IF EXISTS public.t12", &ExecOptions::default()).await;
    let cols = vec![("id".to_string(), SqlType::Int, false), ("name".to_string(), SqlType::NVarChar { len: None }, true), ("ok".to_string(), SqlType::Bit, true), ("blob".to_string(), SqlType::VarBinary { len: None }, true), ("when".to_string(), SqlType::DateTime2 { scale: 6 }, true)];
    let ddl = cobalt_driver::postgres::create_table_sql("public", "t12", &cols);
    assert!(ddl.contains("\"blob\" bytea") && ddl.contains("\"when\" timestamp"), "{ddl}");
    let items = run(&mut *conn, &ddl, &ExecOptions::default()).await;
    assert_eq!(done(&items).0, None, "{:?}", shape(&items));
    let schema = std::sync::Arc::new(arrow::datatypes::Schema::new(vec![
        arrow::datatypes::Field::new("id", DataType::Int32, false),
        arrow::datatypes::Field::new("name", DataType::Utf8, true),
        arrow::datatypes::Field::new("ok", DataType::Boolean, true),
        arrow::datatypes::Field::new("blob", DataType::Binary, true),
        arrow::datatypes::Field::new("when", DataType::Timestamp(TimeUnit::Microsecond, None), true),
    ]));
    let batch = arrow::array::RecordBatch::try_new(
        schema,
        vec![
            std::sync::Arc::new(arrow::array::Int32Array::from(vec![1, 2, 3])),
            std::sync::Arc::new(arrow::array::StringArray::from(vec![Some("a,b"), Some("say \"hi\"\nline two"), None])),
            std::sync::Arc::new(arrow::array::BooleanArray::from(vec![Some(true), None, Some(false)])),
            std::sync::Arc::new(arrow::array::BinaryArray::from(vec![Some(&[0u8, 255][..]), None, Some(&[][..])])),
            std::sync::Arc::new(arrow::array::TimestampMicrosecondArray::from(vec![Some(us(2024, 2, 29, 13, 45, 30, 5)), None, None])),
        ],
    )
    .unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    tx.send(Ok(batch)).unwrap();
    drop(tx);
    let columns: Vec<ColumnInfo> = cols.iter().enumerate().map(|(i, (n, t, nullable))| ColumnInfo::new(n, t.clone(), *nullable, i)).collect();
    let n = conn.bulk_insert("\"public\".\"t12\"", &columns, rx, &mut |_| true).await.unwrap();
    assert_eq!(n, 3);
    let items = run(&mut *conn, "SELECT id, name, ok, blob, \"when\" FROM public.t12 ORDER BY id", &ExecOptions::default()).await;
    let sets = result_sets(&items);
    let b = &sets[0].1[0];
    assert_eq!(b.num_rows(), 3);
    assert_eq!(b.column(1).as_string::<i64>().value(0), "a,b");
    assert_eq!(b.column(1).as_string::<i64>().value(1), "say \"hi\"\nline two");
    assert!(b.column(1).is_null(2));
    assert!(b.column(2).as_boolean().value(0) && b.column(2).is_null(1) && !b.column(2).as_boolean().value(2));
    assert_eq!(b.column(3).as_binary::<i64>().value(0), &[0u8, 255]);
    assert!(b.column(3).is_null(1));
    assert_eq!(b.column(4).as_primitive::<TimestampMicrosecondType>().value(0), us(2024, 2, 29, 13, 45, 30, 5));
    let _ = run(&mut *conn, "DROP TABLE public.t12", &ExecOptions::default()).await;
}

#[tokio::test]
async fn t13_login_failure() {
    require_server!();
    let bad = ResolvedCredentials::SqlLogin { user: "postgres".into(), password: Secret::new("wrong") };
    let r = PostgresDriver.connect(&profile(), &bad, ConnectionRole::Query).await;
    let e = r.err().expect("login must fail").to_string();
    assert!(e.contains("28P01") || e.to_ascii_lowercase().contains("password"), "{e}");
    let mut p = profile();
    p.auth = AuthMethod::None;
    let r = PostgresDriver.connect(&p, &ResolvedCredentials::None, ConnectionRole::Query).await;
    assert!(r.is_err());
}
