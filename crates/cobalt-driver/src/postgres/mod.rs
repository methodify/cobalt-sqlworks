//! PostgreSQL behind the same `Driver` / `Connection` traits as SQL Server, over tokio-postgres.
//!
//! Scripts run through the simple query protocol (`simple_query_raw`), which takes a whole
//! script, splits it server-side (dollar-quoted bodies and all) and streams every result set;
//! values arrive as text. Column types come from preparing each statement first (the server's
//! own parse), so the grid gets integers, floats, booleans, dates, times and timestamps as
//! themselves and everything else (numerics, arrays, json, intervals, user types) as text
//! labelled with the PostgreSQL type name. Cancel is the backend's cancel token; notices reach
//! the Messages pane; an error inside a script's own `BEGIN` rolls the transaction back so the
//! connection is usable again. Bulk loads are `COPY … FROM STDIN` in CSV form.

use crate::*;
use arrow::array::{ArrayRef, BooleanBuilder, Date32Builder, Float32Builder, Float64Builder, Int16Builder, Int32Builder, Int64Builder, LargeBinaryBuilder, LargeStringBuilder, StringBuilder, Time64NanosecondBuilder, TimestampMicrosecondBuilder};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};
use arrow::record_batch::RecordBatch;
use async_trait::async_trait;
use cobalt_core::*;
use futures_util::StreamExt;
use std::sync::Arc;
use tokio_postgres::types::Type;
use tokio_postgres::SimpleQueryMessage;

pub struct PostgresDriver;

#[async_trait]
impl Driver for PostgresDriver {
    fn name(&self) -> &'static str {
        "PostgreSQL"
    }

    async fn connect(&self, profile: &ConnectionProfile, creds: &ResolvedCredentials, role: ConnectionRole) -> Result<Box<dyn Connection>> {
        let c = PostgresConnection::open(profile, creds, role).await?;
        Ok(Box::new(c))
    }
}

pub struct PostgresConnection {
    client: Arc<tokio_postgres::Client>,
    /// Notices the backend sent (the connection task collects them).
    notices: Arc<parking_lot::Mutex<Vec<ServerMessage>>>,
    engine: EngineInfo,
    profile: ProfileId,
    role: ConnectionRole,
    database: String,
    cancel: CancelHandle,
    /// Set by the cancel handle; the running script's stream sees it.
    cancelled: Arc<std::sync::atomic::AtomicBool>,
    usable: Arc<std::sync::atomic::AtomicBool>,
}

/// `"name"` (PostgreSQL folds bare names to lower case).
pub fn q(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn lit(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn qualified(obj: &ObjectRef) -> String {
    if obj.schema.is_empty() {
        q(&obj.name)
    } else {
        format!("{}.{}", q(&obj.schema), q(&obj.name))
    }
}

fn info(text: impl Into<String>) -> ServerMessage {
    ServerMessage { number: 0, state: 0, class: 0, message: text.into(), server: None, procedure: None, line: 0, is_error: false }
}

/// A backend error (or notice) as a message: `SQLSTATE: text`, the line from the error position
/// (counted in the script that was running).
fn db_message(e: &tokio_postgres::error::DbError, script: &str, is_error: bool) -> ServerMessage {
    let line = match e.position() {
        Some(tokio_postgres::error::ErrorPosition::Original(p)) => script.chars().take((*p as usize).saturating_sub(1)).filter(|c| *c == '\n').count() as u32 + 1,
        _ => 0,
    };
    let mut text = e.message().to_string();
    if let Some(d) = e.detail() {
        text.push_str("\nDETAIL: ");
        text.push_str(d);
    }
    if let Some(h) = e.hint() {
        text.push_str("\nHINT: ");
        text.push_str(h);
    }
    ServerMessage { number: 0, state: 0, class: 0, message: format!("{}: {text}", e.code().code()), server: None, procedure: None, line, is_error }
}

fn error_message(e: &tokio_postgres::Error, script: &str) -> ServerMessage {
    match e.as_db_error() {
        Some(db) => db_message(db, script, true),
        None => ServerMessage { number: 0, state: 0, class: 0, message: e.to_string(), server: None, procedure: None, line: 0, is_error: true },
    }
}

fn is_lost(e: &tokio_postgres::Error) -> bool {
    e.is_closed() || (e.as_db_error().is_none() && e.to_string().contains("connection"))
}

/// The grid's type for a PostgreSQL type.
pub fn sql_type_for(t: &Type) -> SqlType {
    match *t {
        Type::BOOL => SqlType::Bit,
        Type::INT2 => SqlType::SmallInt,
        Type::INT4 | Type::OID => SqlType::Int,
        Type::INT8 => SqlType::BigInt,
        Type::FLOAT4 => SqlType::Real,
        Type::FLOAT8 => SqlType::Float,
        Type::DATE => SqlType::Date,
        Type::TIME => SqlType::Time { scale: 6 },
        Type::TIMESTAMP => SqlType::DateTime2 { scale: 6 },
        Type::TIMESTAMPTZ => SqlType::DateTimeOffset { scale: 6 },
        Type::BYTEA => SqlType::VarBinary { len: None },
        Type::UUID => SqlType::UniqueIdentifier,
        Type::JSON | Type::JSONB => SqlType::Json,
        Type::XML => SqlType::Xml,
        Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME | Type::CHAR | Type::UNKNOWN => SqlType::NVarChar { len: None },
        _ => SqlType::Other { name: t.name().to_string() },
    }
}

/// A type name as the catalog prints it (`format_type`) to the grid's type.
pub fn sql_type_from_name(name: &str) -> SqlType {
    let n = name.trim().to_ascii_lowercase();
    let base = n.split(['(', '[']).next().unwrap_or("").trim();
    if n.ends_with("[]") {
        return SqlType::Other { name: name.to_string() };
    }
    let args = || -> Vec<u8> { n.split_once('(').and_then(|(_, r)| r.split(')').next()).map(|a| a.split(',').filter_map(|x| x.trim().parse::<u8>().ok()).collect()).unwrap_or_default() };
    match base {
        "boolean" | "bool" => SqlType::Bit,
        "smallint" | "int2" => SqlType::SmallInt,
        "integer" | "int4" | "int" | "oid" | "serial" => SqlType::Int,
        "bigint" | "int8" | "bigserial" => SqlType::BigInt,
        "real" | "float4" => SqlType::Real,
        "double precision" | "float8" => SqlType::Float,
        "numeric" | "decimal" => {
            let a = args();
            SqlType::Numeric { precision: a.first().copied().unwrap_or(38), scale: a.get(1).copied().unwrap_or(0) }
        }
        "money" => SqlType::Money,
        "date" => SqlType::Date,
        "time" | "time without time zone" | "time with time zone" => SqlType::Time { scale: 6 },
        "timestamp" | "timestamp without time zone" => SqlType::DateTime2 { scale: 6 },
        "timestamp with time zone" | "timestamptz" => SqlType::DateTimeOffset { scale: 6 },
        "bytea" => SqlType::VarBinary { len: None },
        "uuid" => SqlType::UniqueIdentifier,
        "json" | "jsonb" => SqlType::Json,
        "xml" => SqlType::Xml,
        "text" | "citext" | "name" | "character varying" | "varchar" => SqlType::NVarChar { len: args().first().map(|&v| v as u32) },
        "character" | "char" | "bpchar" => SqlType::NChar { len: args().first().map(|&v| v as u32) },
        _ => SqlType::Other { name: name.to_string() },
    }
}

/// The Arrow type the rows of a result column are built in (text for everything the grid
/// shows as text).
fn arrow_type_of(t: &SqlType) -> DataType {
    match t {
        SqlType::Bit => DataType::Boolean,
        SqlType::SmallInt => DataType::Int16,
        SqlType::Int => DataType::Int32,
        SqlType::BigInt => DataType::Int64,
        SqlType::Real => DataType::Float32,
        SqlType::Float => DataType::Float64,
        SqlType::Date => DataType::Date32,
        SqlType::Time { .. } => DataType::Time64(TimeUnit::Nanosecond),
        SqlType::DateTime2 { .. } => DataType::Timestamp(TimeUnit::Microsecond, None),
        SqlType::DateTimeOffset { .. } => DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
        SqlType::VarBinary { .. } => DataType::LargeBinary,
        SqlType::UniqueIdentifier => DataType::Utf8,
        _ => DataType::LargeUtf8,
    }
}

fn parse_timestamp(s: &str) -> Option<i64> {
    // "2024-02-29 13:45:30.123456" (fractional part optional); BC and infinity are text
    let s = s.trim();
    let (date, time) = s.split_once([' ', 'T'])?;
    let d = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
    let t = chrono::NaiveTime::parse_from_str(time, "%H:%M:%S%.f").ok()?;
    Some(d.and_time(t).and_utc().timestamp_micros())
}

fn parse_timestamptz(s: &str) -> Option<i64> {
    // "2024-02-29 11:45:30.123456+00" / "+02:30" / "-05"
    let s = s.trim();
    let idx = s.rfind(['+', '-']).filter(|&i| i > 10)?;
    let (main, off) = s.split_at(idx);
    let base = parse_timestamp(main)?;
    let sign = if off.starts_with('-') { -1 } else { 1 };
    let rest = &off[1..];
    let (h, m) = match rest.split_once(':') {
        Some((h, m)) => (h.parse::<i64>().ok()?, m.parse::<i64>().ok()?),
        None => (rest.parse::<i64>().ok()?, 0),
    };
    Some(base - sign * (h * 3600 + m * 60) * 1_000_000)
}

fn parse_time_ns(s: &str) -> Option<i64> {
    let t = chrono::NaiveTime::parse_from_str(s.trim().split(['+', '-']).next()?, "%H:%M:%S%.f").ok()?;
    t.signed_duration_since(chrono::NaiveTime::MIN).num_nanoseconds()
}

fn parse_date(s: &str) -> Option<i32> {
    let d = chrono::NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").ok()?;
    Some(d.signed_duration_since(chrono::NaiveDate::from_ymd_opt(1970, 1, 1)?).num_days() as i32)
}

fn parse_bytea(s: &str) -> Vec<u8> {
    if let Some(hex) = s.strip_prefix("\\x") {
        (0..hex.len()).step_by(2).filter_map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok()).collect()
    } else {
        s.as_bytes().to_vec()
    }
}

/// Text cells into one batch typed by `cols`; a value that does not parse becomes NULL and is counted.
fn to_batch(rows: &[Vec<Option<String>>], cols: &[ColumnInfo], misfits: &mut [u64]) -> std::result::Result<RecordBatch, String> {
    let mut arrays: Vec<ArrayRef> = Vec::with_capacity(cols.len());
    for (i, c) in cols.iter().enumerate() {
        let cell = |r: &Vec<Option<String>>| r.get(i).cloned().flatten();
        let arr: ArrayRef = match arrow_type_of(&c.sql_type) {
            DataType::Boolean => {
                let mut b = BooleanBuilder::new();
                for r in rows {
                    match cell(r).as_deref() {
                        Some("t") | Some("true") => b.append_value(true),
                        Some("f") | Some("false") => b.append_value(false),
                        Some(_) => {
                            misfits[i] += 1;
                            b.append_null();
                        }
                        None => b.append_null(),
                    }
                }
                Arc::new(b.finish())
            }
            DataType::Int16 => {
                let mut b = Int16Builder::new();
                for r in rows {
                    let v = cell(r);
                    let had = v.is_some();
                    let p = v.and_then(|v| v.trim().parse::<i16>().ok());
                    if had && p.is_none() {
                        misfits[i] += 1;
                    }
                    b.append_option(p);
                }
                Arc::new(b.finish())
            }
            DataType::Int32 => {
                let mut b = Int32Builder::new();
                for r in rows {
                    let v = cell(r);
                    let had = v.is_some();
                    let p = v.and_then(|v| v.trim().parse::<i32>().ok());
                    if had && p.is_none() {
                        misfits[i] += 1;
                    }
                    b.append_option(p);
                }
                Arc::new(b.finish())
            }
            DataType::Int64 => {
                let mut b = Int64Builder::new();
                for r in rows {
                    let v = cell(r);
                    let had = v.is_some();
                    let p = v.and_then(|v| v.trim().parse::<i64>().ok());
                    if had && p.is_none() {
                        misfits[i] += 1;
                    }
                    b.append_option(p);
                }
                Arc::new(b.finish())
            }
            DataType::Float32 => {
                let mut b = Float32Builder::new();
                for r in rows {
                    b.append_option(cell(r).and_then(|v| parse_float(&v).map(|f| f as f32)));
                }
                Arc::new(b.finish())
            }
            DataType::Float64 => {
                let mut b = Float64Builder::new();
                for r in rows {
                    b.append_option(cell(r).and_then(|v| parse_float(&v)));
                }
                Arc::new(b.finish())
            }
            DataType::Date32 => {
                let mut b = Date32Builder::new();
                for r in rows {
                    let v = cell(r);
                    let had = v.is_some();
                    let p = v.and_then(|v| parse_date(&v));
                    if had && p.is_none() {
                        misfits[i] += 1;
                    }
                    b.append_option(p);
                }
                Arc::new(b.finish())
            }
            DataType::Time64(_) => {
                let mut b = Time64NanosecondBuilder::new();
                for r in rows {
                    b.append_option(cell(r).and_then(|v| parse_time_ns(&v)));
                }
                Arc::new(b.finish())
            }
            DataType::Timestamp(_, tz) => {
                let mut b = TimestampMicrosecondBuilder::new().with_timezone_opt(tz.clone());
                for r in rows {
                    let v = cell(r);
                    let had = v.is_some();
                    let p = v.and_then(|v| if tz.is_some() { parse_timestamptz(&v) } else { parse_timestamp(&v) });
                    if had && p.is_none() {
                        misfits[i] += 1;
                    }
                    b.append_option(p);
                }
                Arc::new(b.finish())
            }
            DataType::LargeBinary => {
                let mut b = LargeBinaryBuilder::new();
                for r in rows {
                    match cell(r) {
                        Some(v) => b.append_value(parse_bytea(&v)),
                        None => b.append_null(),
                    }
                }
                Arc::new(b.finish())
            }
            DataType::Utf8 => {
                let mut b = StringBuilder::new();
                for r in rows {
                    b.append_option(cell(r));
                }
                Arc::new(b.finish())
            }
            _ => {
                let mut b = LargeStringBuilder::new();
                for r in rows {
                    b.append_option(cell(r));
                }
                Arc::new(b.finish())
            }
        };
        arrays.push(arr);
    }
    let schema = Arc::new(Schema::new(cols.iter().map(|c| Field::new(&c.name, arrow_type_of(&c.sql_type), true)).collect::<Vec<_>>()));
    RecordBatch::try_new(schema, arrays).map_err(|e| e.to_string())
}

fn parse_float(s: &str) -> Option<f64> {
    match s.trim() {
        "NaN" => Some(f64::NAN),
        "Infinity" => Some(f64::INFINITY),
        "-Infinity" => Some(f64::NEG_INFINITY),
        other => other.parse().ok(),
    }
}

/// The TLS connector for a profile: `Optional` tries TLS and falls back, `Mandatory` and
/// `Strict` require it; `trust_server_certificate` accepts any certificate.
fn tls(profile: &ConnectionProfile) -> std::result::Result<postgres_native_tls::MakeTlsConnector, String> {
    let mut b = native_tls::TlsConnector::builder();
    if profile.options.trust_server_certificate {
        b.danger_accept_invalid_certs(true);
        b.danger_accept_invalid_hostnames(true);
    }
    let c = b.build().map_err(|e| e.to_string())?;
    Ok(postgres_native_tls::MakeTlsConnector::new(c))
}

impl PostgresConnection {
    async fn open(profile: &ConnectionProfile, creds: &ResolvedCredentials, role: ConnectionRole) -> Result<Self> {
        let (user, password) = match creds {
            ResolvedCredentials::SqlLogin { user, password } => (user.clone(), password.expose().to_string()),
            _ => return Err(DriverError::Login("PostgreSQL connections sign in with a user name and password".into())),
        };
        let mut cfg = tokio_postgres::Config::new();
        let server = profile.server.trim();
        let (host, port_in_host) = match server.rsplit_once(':') {
            Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) && !h.is_empty() => (h.to_string(), p.parse::<u16>().ok()),
            _ => (server.to_string(), None),
        };
        cfg.host(&host);
        cfg.port(profile.port.or(port_in_host).unwrap_or(5432));
        cfg.user(&user);
        cfg.password(&password);
        let database = profile.database.clone().filter(|d| !d.trim().is_empty()).unwrap_or_else(|| "postgres".into());
        cfg.dbname(&database);
        cfg.application_name(if profile.options.application_name.is_empty() { "Cobalt SQL Works" } else { &profile.options.application_name });
        cfg.connect_timeout(std::time::Duration::from_secs(profile.options.connect_timeout_secs.max(5) as u64));
        cfg.ssl_mode(match profile.options.encrypt {
            Encrypt::Optional => tokio_postgres::config::SslMode::Prefer,
            _ => tokio_postgres::config::SslMode::Require,
        });
        let connector = tls(profile).map_err(DriverError::Connect)?;
        let (client, connection) = cfg.connect(connector).await.map_err(|e| DriverError::Connect(match e.as_db_error() {
            Some(db) => format!("{}: {}", db.code().code(), db.message()),
            None => e.to_string(),
        }))?;
        // the connection task drives the socket and keeps the notices
        let notices: Arc<parking_lot::Mutex<Vec<ServerMessage>>> = Default::default();
        let sink = Arc::clone(&notices);
        tokio::spawn(async move {
            let mut connection = connection;
            loop {
                let msg = futures_util::future::poll_fn(|cx| connection.poll_message(cx)).await;
                match msg {
                    Some(Ok(tokio_postgres::AsyncMessage::Notice(n))) => {
                        let severity = n.severity().to_string();
                        sink.lock().push(ServerMessage { number: 0, state: 0, class: 0, message: format!("{severity}: {}", n.message()), server: None, procedure: None, line: 0, is_error: false });
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => {
                        tracing::info!(error = %e, "postgres connection ended");
                        break;
                    }
                    None => break,
                }
            }
        });
        let rows = client.simple_query("SELECT version(), current_setting('server_version'), inet_server_addr()::text").await.map_err(|e| DriverError::Connect(e.to_string()))?;
        let (version_text, version) = rows
            .iter()
            .find_map(|m| if let SimpleQueryMessage::Row(r) = m { Some((r.get(0).unwrap_or("").to_string(), r.get(1).unwrap_or("").to_string())) } else { None })
            .unwrap_or_default();
        let mut capabilities = EngineKind::Postgres.capabilities();
        capabilities.read_only = false;
        let engine = EngineInfo { kind: EngineKind::Postgres, version: version.clone(), product_level: String::new(), edition: String::new(), server_name: host.clone(), version_text, capabilities };
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let token = client.cancel_token();
        let flag = Arc::clone(&cancelled);
        let tls_for_cancel = tls(profile).map_err(DriverError::Connect)?;
        let cancel = CancelHandle::new(move || {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
            let token = token.clone();
            let connector = tls_for_cancel.clone();
            tokio::spawn(async move {
                if let Err(e) = token.cancel_query(connector).await {
                    tracing::info!(error = %e, "postgres cancel");
                }
            });
        });
        Ok(Self { client: Arc::new(client), notices, engine, profile: profile.id, role, database, cancel, cancelled, usable: Arc::new(std::sync::atomic::AtomicBool::new(true)) })
    }

    /// Rows of a catalog query as text cells.
    async fn rows(&self, sql: &str) -> Result<Vec<Vec<Option<String>>>> {
        let msgs = self.client.simple_query(sql).await.map_err(|e| self.map_err(e, sql))?;
        Ok(msgs
            .into_iter()
            .filter_map(|m| match m {
                SimpleQueryMessage::Row(r) => Some((0..r.len()).map(|i| r.get(i).map(str::to_string)).collect()),
                _ => None,
            })
            .collect())
    }

    fn map_err(&self, e: tokio_postgres::Error, script: &str) -> DriverError {
        if is_lost(&e) {
            DriverError::Disconnected(e.to_string())
        } else {
            DriverError::Server(error_message(&e, script))
        }
    }

}

/// One statement of a run: its text and the 1-based line it starts on in the script.
struct Stmt {
    text: String,
    line: u32,
}

/// A statement whose result columns are worth describing first (a prepare round trip): one
/// that returns rows. DDL and plain DML skip it; a result set they return anyway shows as text.
fn wants_describe(text: &str) -> bool {
    let first = text.trim_start().split(|c: char| !c.is_ascii_alphanumeric() && c != '_').next().unwrap_or("").to_ascii_uppercase();
    matches!(first.as_str(), "SELECT" | "WITH" | "VALUES" | "TABLE" | "SHOW" | "CALL" | "FETCH") || text.to_ascii_uppercase().contains("RETURNING")
}

/// The columns a statement returns, from the server's own parse (None when it cannot be
/// prepared: a utility statement, or a parse error the run will report).
async fn describe(client: &tokio_postgres::Client, statement: &str) -> Option<Vec<ColumnInfo>> {
    let st = client.prepare(statement).await.ok()?;
    Some(st.columns().iter().enumerate().map(|(i, c)| ColumnInfo::new(c.name(), sql_type_for(c.type_()), true, i)).collect())
}

/// One running script: its statements one after another through the simple query protocol,
/// each described (typed) just before it runs so DDL earlier in the script is seen, turned
/// into `StreamItem`s on a task of its own so the session reads results as they arrive.
struct RunState {
    client: Arc<tokio_postgres::Client>,
    notices: Arc<parking_lot::Mutex<Vec<ServerMessage>>>,
    cancelled: Arc<std::sync::atomic::AtomicBool>,
    usable: Arc<std::sync::atomic::AtomicBool>,
    statements: Vec<Stmt>,
    plan: PlanMode,
    batch_rows: usize,
}

impl RunState {
    async fn drive(self, tx: tokio::sync::mpsc::Sender<StreamItem>) {
        let send = |item: StreamItem| tx.send(item);
        let plan = self.plan;
        let mut first_error: Option<ServerMessage> = None;
        let mut lost = false;
        let flush = |rows: &mut Vec<Vec<Option<String>>>, cols: &[ColumnInfo], misfits: &mut Vec<u64>| -> Option<RecordBatch> {
            if rows.is_empty() {
                return None;
            }
            let b = to_batch(rows, cols, misfits).ok();
            rows.clear();
            b
        };
        'statements: for st in &self.statements {
            if self.cancelled.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            let typed = if plan == PlanMode::None && wants_describe(&st.text) { describe(&self.client, &st.text).await } else { None };
            let stream = match self.client.simple_query_raw(&st.text).await {
                Ok(s) => s,
                Err(e) => {
                    lost = is_lost(&e);
                    let mut m = error_message(&e, &st.text);
                    if m.line > 0 {
                        m.line += st.line.saturating_sub(1);
                    }
                    let _ = send(StreamItem::Message(m.clone())).await;
                    first_error = Some(m);
                    break;
                }
            };
            futures_util::pin_mut!(stream);
            let mut cols: Vec<ColumnInfo> = Vec::new();
            let mut rows: Vec<Vec<Option<String>>> = Vec::with_capacity(self.batch_rows);
            let mut misfits: Vec<u64> = Vec::new();
            let mut in_set = false;
            let mut total = 0u64;
            loop {
                let pending: Vec<ServerMessage> = self.notices.lock().drain(..).collect();
                for n in pending {
                    if send(StreamItem::Message(n)).await.is_err() {
                        return;
                    }
                }
                match stream.next().await {
                    Some(Ok(SimpleQueryMessage::RowDescription(desc))) => {
                        let names: Vec<String> = desc.iter().map(|c| c.name().to_string()).collect();
                        // the prepared description with the same column names, else text
                        let described = typed.clone().filter(|d| d.len() == names.len() && d.iter().zip(&names).all(|(c, n)| c.name == *n));
                        cols = described.unwrap_or_else(|| names.iter().enumerate().map(|(i, n)| ColumnInfo::new(n, SqlType::NVarChar { len: None }, true, i)).collect());
                        misfits = vec![0; cols.len()];
                        total = 0;
                        in_set = true;
                        if send(StreamItem::ResultSetStart { columns: cols.clone() }).await.is_err() {
                            return;
                        }
                    }
                    Some(Ok(SimpleQueryMessage::Row(r))) => {
                        rows.push((0..r.len()).map(|i| r.get(i).map(str::to_string)).collect());
                        total += 1;
                        if plan != PlanMode::None {
                            continue; // a plan's lines are sent whole at the end of the set
                        }
                        if rows.len() >= self.batch_rows {
                            if let Some(b) = flush(&mut rows, &cols, &mut misfits) {
                                if send(StreamItem::Rows(b)).await.is_err() {
                                    return;
                                }
                            }
                        }
                    }
                    Some(Ok(SimpleQueryMessage::CommandComplete(n))) => {
                        if in_set {
                            if plan != PlanMode::None && cols.len() == 1 && cols[0].name == "QUERY PLAN" {
                                let text = rows.iter().map(|r| cell(r, 0)).collect::<Vec<_>>().join("\n");
                                let _ = send(StreamItem::Message(ServerMessage { procedure: Some("plan".into()), ..info(format!("QUERY PLAN\n{text}")) })).await;
                            }
                            if let Some(b) = flush(&mut rows, &cols, &mut misfits) {
                                if send(StreamItem::Rows(b)).await.is_err() {
                                    return;
                                }
                            }
                            for (i, m) in misfits.iter().enumerate() {
                                if *m > 0 {
                                    let _ = send(StreamItem::Message(info(format!("{} value{} in column {} did not parse as {} and show as NULL", m, if *m == 1 { "" } else { "s" }, cols[i].name, cols[i].sql_type)))).await;
                                }
                            }
                            if send(StreamItem::ResultSetEnd { rows: total }).await.is_err() {
                                return;
                            }
                            in_set = false;
                        } else if n > 0 && send(StreamItem::RowsAffected(n)).await.is_err() {
                            return;
                        }
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => {
                        lost = is_lost(&e);
                        let mut m = error_message(&e, &st.text);
                        if m.line > 0 {
                            m.line += st.line.saturating_sub(1);
                        }
                        if self.cancelled.load(std::sync::atomic::Ordering::SeqCst) || e.as_db_error().map(|d| d.code().code() == "57014").unwrap_or(false) {
                            m.message = "Query was cancelled.".into();
                        }
                        if in_set {
                            if let Some(b) = flush(&mut rows, &cols, &mut misfits) {
                                let _ = send(StreamItem::Rows(b)).await;
                            }
                            let _ = send(StreamItem::ResultSetEnd { rows: total }).await;
                        }
                        let _ = send(StreamItem::Message(m.clone())).await;
                        first_error = Some(m);
                        break 'statements;
                    }
                    None => break,
                }
            }
        }
        if lost {
            self.usable.store(false, std::sync::atomic::Ordering::SeqCst);
        } else if first_error.is_some() {
            // an error inside a transaction leaves it aborted (every statement fails until a
            // ROLLBACK): end it so the next run works; without one the server only warns
            self.notices.lock().clear();
            if self.client.simple_query("ROLLBACK").await.is_ok() {
                tokio::task::yield_now().await;
                let warned = self.notices.lock().drain(..).any(|n| n.message.contains("no transaction in progress"));
                if !warned {
                    let _ = send(StreamItem::Message(info("The transaction was rolled back."))).await;
                }
            }
        }
        let pending: Vec<ServerMessage> = self.notices.lock().drain(..).collect();
        for n in pending {
            let _ = send(StreamItem::Message(n)).await;
        }
        let was_cancelled = self.cancelled.load(std::sync::atomic::Ordering::SeqCst);
        let _ = send(StreamItem::Done { error: first_error, cancelled: was_cancelled }).await;
    }
}

fn cell(row: &[Option<String>], i: usize) -> String {
    row.get(i).cloned().flatten().unwrap_or_default()
}

#[async_trait]
impl Connection for PostgresConnection {
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
        let batch_rows = opts.batch_rows.clamp(256, 65_536);
        let plan = opts.plan;
        self.cancel.reset();
        self.cancelled.store(false, std::sync::atomic::Ordering::SeqCst);
        // the script's statements (dollar-quoted bodies kept whole); the plan modes wrap each in EXPLAIN
        let prefix = match plan {
            PlanMode::None => "",
            PlanMode::Estimated => "EXPLAIN (FORMAT TEXT) ",
            PlanMode::Actual => "EXPLAIN (ANALYZE, FORMAT TEXT) ",
        };
        let spans = cobalt_sql::statements::statements_dialect(sql, cobalt_sql::Dialect::Postgres);
        let mut statements: Vec<Stmt> = Vec::new();
        if spans.is_empty() {
            if !sql.trim().is_empty() {
                statements.push(Stmt { text: format!("{prefix}{}", sql.trim()), line: 1 });
            }
        } else {
            for s in spans {
                let text = sql[s.start..s.end].trim().trim_end_matches(';').trim();
                if text.is_empty() {
                    continue;
                }
                statements.push(Stmt { text: format!("{prefix}{text}"), line: s.line });
            }
        }
        let run = RunState { client: Arc::clone(&self.client), notices: Arc::clone(&self.notices), cancelled: Arc::clone(&self.cancelled), usable: Arc::clone(&self.usable), statements, plan, batch_rows };
        tokio::spawn(async move { run.drive(tx).await });
        Ok(Box::pin(futures_util::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|i| (i, rx)) })))
    }

    fn cancel_handle(&self) -> CancelHandle {
        self.cancel.clone()
    }

    async fn bulk_insert(&mut self, table: &str, columns: &[ColumnInfo], rx: std::sync::mpsc::Receiver<std::result::Result<RecordBatch, String>>, progress: &mut (dyn FnMut(u64) -> bool + Send)) -> Result<u64> {
        use arrow::util::display::{ArrayFormatter, FormatOptions};
        use futures_util::SinkExt;
        if !progress(0) {
            return Err(DriverError::Cancelled);
        }
        let cols = columns.iter().map(|c| q(&c.name)).collect::<Vec<_>>().join(", ");
        let copy = format!("COPY {table} ({cols}) FROM STDIN WITH (FORMAT csv, NULL '', QUOTE '\"', ESCAPE '\"')");
        let sink = self.client.copy_in::<_, bytes::Bytes>(&copy).await.map_err(|e| self.map_err(e, &copy))?;
        futures_util::pin_mut!(sink);
        let opts = FormatOptions::default().with_display_error(true);
        let mut rows = 0u64;
        loop {
            let batch = match rx.recv() {
                Ok(Ok(b)) => b,
                Ok(Err(e)) => return Err(DriverError::Other(e)),
                Err(_) => break,
            };
            let text = {
            let formatters: Vec<Option<ArrayFormatter<'_>>> = batch.columns().iter().map(|a| ArrayFormatter::try_new(a.as_ref(), &opts).ok()).collect();
            let mut text = String::with_capacity(batch.num_rows() * 64);
            for r in 0..batch.num_rows() {
                for (c, a) in batch.columns().iter().enumerate() {
                    if c > 0 {
                        text.push(',');
                    }
                    if a.is_null(r) {
                        continue;
                    }
                    let v = match a.data_type() {
                        DataType::Binary | DataType::LargeBinary | DataType::BinaryView | DataType::FixedSizeBinary(_) => {
                            let bytes: &[u8] = match a.data_type() {
                                DataType::Binary => arrow::array::AsArray::as_binary::<i32>(a).value(r),
                                DataType::LargeBinary => arrow::array::AsArray::as_binary::<i64>(a).value(r),
                                _ => &[],
                            };
                            format!("\\x{}", bytes.iter().map(|b| format!("{b:02x}")).collect::<String>())
                        }
                        DataType::Boolean => if arrow::array::AsArray::as_boolean(a).value(r) { "true".into() } else { "false".into() },
                        _ => formatters[c].as_ref().map(|f| f.value(r).to_string()).unwrap_or_default(),
                    };
                    text.push('"');
                    text.push_str(&v.replace('"', "\"\""));
                    text.push('"');
                }
                text.push('\n');
            }
            text
            };
            sink.send(bytes::Bytes::from(text)).await.map_err(|e| self.map_err(e, &copy))?;
            rows += batch.num_rows() as u64;
            if !progress(rows) {
                return Err(DriverError::Cancelled);
            }
        }
        sink.close().await.map_err(|e| self.map_err(e, &copy))?;
        Ok(rows)
    }

    async fn change_database(&mut self, database: &str) -> Result<()> {
        if database == self.database {
            return Ok(());
        }
        Err(DriverError::Unsupported("a PostgreSQL connection is to one database: open a new connection to switch"))
    }

    async fn ping(&mut self) -> Result<()> {
        self.client.simple_query("SELECT 1").await.map(|_| ()).map_err(|e| DriverError::Disconnected(e.to_string()))
    }

    fn is_usable(&self) -> bool {
        self.usable.load(std::sync::atomic::Ordering::SeqCst) && !self.client.is_closed()
    }

    async fn close(&mut self) -> Result<()> {
        self.usable.store(false, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    async fn list_databases(&mut self) -> Result<Vec<DatabaseInfo>> {
        let rows = self.rows("SELECT datname, datistemplate, datallowconn FROM pg_database WHERE NOT datistemplate ORDER BY datname").await?;
        Ok(rows.into_iter().map(|r| DatabaseInfo { name: cell(&r, 0), is_system: matches!(cell(&r, 0).as_str(), "postgres"), state: if cell(&r, 2) == "f" { "no connections".into() } else { "online".into() }, is_read_only: false }).collect())
    }

    async fn list_schemas(&mut self, _database: &str) -> Result<Vec<String>> {
        let rows = self.rows("SELECT nspname FROM pg_namespace WHERE nspname NOT LIKE 'pg\\_%' AND nspname <> 'information_schema' ORDER BY nspname").await?;
        Ok(rows.into_iter().map(|r| cell(&r, 0)).collect())
    }

    async fn list_objects(&mut self, database: &str) -> Result<Vec<ObjectRef>> {
        let sql = "SELECT n.nspname, c.relname, c.relkind::text, c.oid FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE c.relkind IN ('r', 'p', 'v', 'm', 'f', 'S') AND n.nspname NOT LIKE 'pg\\_%' AND n.nspname <> 'information_schema' \
                   UNION ALL SELECT n.nspname, p.proname, CASE p.prokind WHEN 'p' THEN 'P' WHEN 'a' THEN 'A' ELSE CASE WHEN p.proretset THEN 'T' ELSE 'F' END END, p.oid FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace WHERE n.nspname NOT LIKE 'pg\\_%' AND n.nspname <> 'information_schema' AND p.prokind IN ('f', 'p', 'a') ORDER BY 1, 3, 2";
        let rows = self.rows(sql).await?;
        Ok(rows
            .into_iter()
            .map(|r| {
                let kind = match cell(&r, 2).as_str() {
                    "v" | "m" => ObjectKind::View,
                    "S" => ObjectKind::Sequence,
                    "P" => ObjectKind::Procedure,
                    "T" => ObjectKind::TableFunction,
                    "F" => ObjectKind::ScalarFunction,
                    "A" => ObjectKind::AggregateFunction,
                    _ => ObjectKind::Table,
                };
                ObjectRef { database: database.to_string(), schema: cell(&r, 0), name: cell(&r, 1), kind, object_id: cell(&r, 3).parse::<i64>().ok().map(|v| v as i32) }
            })
            .collect())
    }

    async fn list_columns(&mut self, obj: &ObjectRef) -> Result<Vec<ColumnInfo>> {
        let sql = format!(
            "SELECT a.attname, format_type(a.atttypid, a.atttypmod), a.attnotnull, pg_get_expr(d.adbin, d.adrelid), a.attidentity <> '' OR a.attgenerated <> '', \
             EXISTS (SELECT 1 FROM pg_constraint k WHERE k.conrelid = c.oid AND k.contype = 'p' AND a.attnum = ANY (k.conkey)) \
             FROM pg_attribute a JOIN pg_class c ON c.oid = a.attrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
             LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
             WHERE n.nspname = {} AND c.relname = {} AND a.attnum > 0 AND NOT a.attisdropped ORDER BY a.attnum",
            lit(if obj.schema.is_empty() { "public" } else { &obj.schema }),
            lit(&obj.name)
        );
        let rows = self.rows(&sql).await?;
        Ok(rows
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let mut c = ColumnInfo::new(cell(r, 0), sql_type_from_name(&cell(r, 1)), cell(r, 2) != "t", i);
                c.default_definition = r.get(3).cloned().flatten();
                c.is_identity = cell(r, 4) == "t";
                c.in_primary_key = cell(r, 5) == "t";
                c
            })
            .collect())
    }

    async fn list_parameters(&mut self, obj: &ObjectRef) -> Result<Vec<ParameterInfo>> {
        let sql = format!(
            "SELECT p.proargnames, pg_get_function_arguments(p.oid), pg_get_function_result(p.oid) FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace WHERE n.nspname = {} AND p.proname = {} LIMIT 1",
            lit(if obj.schema.is_empty() { "public" } else { &obj.schema }),
            lit(&obj.name)
        );
        let rows = self.rows(&sql).await?;
        let Some(r) = rows.first() else { return Ok(Vec::new()) };
        let args = cell(r, 1);
        let mut out = Vec::new();
        for (i, a) in args.split(',').map(str::trim).filter(|s| !s.is_empty()).enumerate() {
            // `IN p_id integer`, `OUT total numeric`, `VARIADIC xs integer[]`: the mode first
            let (mode, a) = match a.split_once(' ') {
                Some((m, rest)) if matches!(m, "IN" | "OUT" | "INOUT" | "VARIADIC") => (m, rest.trim()),
                _ => ("IN", a),
            };
            let (name, ty) = match a.split_once(' ') {
                Some((n, t)) if !n.contains('(') => (n.to_string(), t.to_string()),
                _ => (format!("${}", i + 1), a.to_string()),
            };
            out.push(ParameterInfo { name, sql_type: sql_type_from_name(&ty), is_output: matches!(mode, "OUT" | "INOUT"), has_default: ty.contains("DEFAULT"), ordinal: i });
        }
        Ok(out)
    }

    async fn list_indexes(&mut self, obj: &ObjectRef) -> Result<Vec<IndexInfo>> {
        let sql = format!(
            "SELECT i.relname, x.indisunique, x.indisprimary, pg_get_indexdef(x.indexrelid), am.amname, pg_get_expr(x.indpred, x.indrelid) \
             FROM pg_index x JOIN pg_class i ON i.oid = x.indexrelid JOIN pg_class t ON t.oid = x.indrelid JOIN pg_namespace n ON n.oid = t.relnamespace JOIN pg_am am ON am.oid = i.relam \
             WHERE n.nspname = {} AND t.relname = {} ORDER BY i.relname",
            lit(if obj.schema.is_empty() { "public" } else { &obj.schema }),
            lit(&obj.name)
        );
        let rows = self.rows(&sql).await?;
        Ok(rows
            .into_iter()
            .map(|r| {
                let def = cell(&r, 3);
                let cols: Vec<(String, bool)> = def.rsplit_once('(').map(|(_, c)| c.trim_end_matches(')').split(',').map(|s| { let s = s.trim(); (s.trim_end_matches(" DESC").trim_matches('"').to_string(), s.ends_with(" DESC")) }).collect()).unwrap_or_default();
                IndexInfo { name: cell(&r, 0), is_unique: cell(&r, 1) == "t", is_clustered: false, is_primary_key: cell(&r, 2) == "t", is_unique_constraint: false, index_type: cell(&r, 4), key_columns: cols, included_columns: Vec::new(), filter: r.get(5).cloned().flatten() }
            })
            .collect())
    }

    async fn list_keys(&mut self, obj: &ObjectRef) -> Result<Vec<KeyInfo>> {
        let sql = format!(
            "SELECT k.conname, k.contype, (SELECT string_agg(a.attname, ',' ORDER BY ord) FROM unnest(k.conkey) WITH ORDINALITY u(attnum, ord) JOIN pg_attribute a ON a.attrelid = k.conrelid AND a.attnum = u.attnum), \
             fn.nspname, fc.relname, (SELECT string_agg(a.attname, ',' ORDER BY ord) FROM unnest(k.confkey) WITH ORDINALITY u(attnum, ord) JOIN pg_attribute a ON a.attrelid = k.confrelid AND a.attnum = u.attnum), pg_get_constraintdef(k.oid) \
             FROM pg_constraint k JOIN pg_class c ON c.oid = k.conrelid JOIN pg_namespace n ON n.oid = c.relnamespace LEFT JOIN pg_class fc ON fc.oid = k.confrelid LEFT JOIN pg_namespace fn ON fn.oid = fc.relnamespace \
             WHERE n.nspname = {} AND c.relname = {} AND k.contype IN ('p', 'u', 'f', 'c') ORDER BY k.contype, k.conname",
            lit(if obj.schema.is_empty() { "public" } else { &obj.schema }),
            lit(&obj.name)
        );
        let rows = self.rows(&sql).await?;
        Ok(rows
            .into_iter()
            .map(|r| {
                let kind = match cell(&r, 1).as_str() {
                    "p" => KeyKind::PrimaryKey,
                    "u" => KeyKind::Unique,
                    "f" => KeyKind::ForeignKey,
                    _ => KeyKind::Check,
                };
                let columns: Vec<String> = cell(&r, 2).split(',').filter(|s| !s.is_empty()).map(str::to_string).collect();
                let references = if kind == KeyKind::ForeignKey {
                    Some((ObjectRef { database: obj.database.clone(), schema: cell(&r, 3), name: cell(&r, 4), kind: ObjectKind::Table, object_id: None }, cell(&r, 5).split(',').filter(|s| !s.is_empty()).map(str::to_string).collect()))
                } else {
                    None
                };
                KeyInfo { name: cell(&r, 0), kind, columns, references, definition: r.get(6).cloned().flatten() }
            })
            .collect())
    }

    async fn load_catalog(&mut self, database: &str) -> Result<DatabaseCatalog> {
        let objects = self.list_objects(database).await?;
        let schemas = self.list_schemas(database).await.unwrap_or_default();
        let mut cat = DatabaseCatalog { database: database.to_string(), schemas, objects: objects.clone(), columns: Default::default(), refreshed_at: Some(chrono::Utc::now()) };
        let rows = self
            .rows(
                "SELECT c.oid, a.attname, format_type(a.atttypid, a.atttypmod), a.attnotnull, EXISTS (SELECT 1 FROM pg_constraint k WHERE k.conrelid = c.oid AND k.contype = 'p' AND a.attnum = ANY (k.conkey)) \
                 FROM pg_attribute a JOIN pg_class c ON c.oid = a.attrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
                 WHERE c.relkind IN ('r', 'p', 'v', 'm', 'f') AND n.nspname NOT LIKE 'pg\\_%' AND n.nspname <> 'information_schema' AND a.attnum > 0 AND NOT a.attisdropped ORDER BY c.oid, a.attnum",
            )
            .await
            .unwrap_or_default();
        for r in rows {
            if let Some(id) = cell(&r, 0).parse::<i64>().ok().map(|v| v as i32) {
                let cols = cat.columns.entry(id).or_default();
                let ord = cols.len();
                let mut c = ColumnInfo::new(cell(&r, 1), sql_type_from_name(&cell(&r, 2)), cell(&r, 3) != "t", ord);
                c.in_primary_key = cell(&r, 4) == "t";
                cols.push(c);
            }
        }
        Ok(cat)
    }

    async fn script(&mut self, obj: &ObjectRef, kind: ScriptKind) -> Result<String> {
        let schema = if obj.schema.is_empty() { "public".to_string() } else { obj.schema.clone() };
        let target = qualified(obj);
        match (kind, obj.kind) {
            (ScriptKind::Create | ScriptKind::Alter, ObjectKind::View) => {
                let rows = self.rows(&format!("SELECT pg_get_viewdef({}::regclass, true)", lit(&format!("{}.{}", q(&schema), q(&obj.name))))).await?;
                Ok(format!("CREATE OR REPLACE VIEW {target} AS\n{}\n", rows.first().map(|r| cell(r, 0)).unwrap_or_default().trim_end_matches(';')))
            }
            (ScriptKind::Create | ScriptKind::Alter, ObjectKind::Procedure | ObjectKind::ScalarFunction | ObjectKind::TableFunction | ObjectKind::AggregateFunction) => {
                let rows = self.rows(&format!("SELECT pg_get_functiondef(p.oid) FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace WHERE n.nspname = {} AND p.proname = {} LIMIT 1", lit(&schema), lit(&obj.name))).await?;
                Ok(rows.first().map(|r| cell(r, 0)).unwrap_or_default() + "\n")
            }
            (ScriptKind::Create, ObjectKind::Sequence) => Ok(format!("CREATE SEQUENCE {target};\n")),
            (ScriptKind::Create, _) => {
                let cols = self.list_columns(obj).await?;
                let keys = self.list_keys(obj).await.unwrap_or_default();
                let mut lines: Vec<String> = cols
                    .iter()
                    .map(|c| {
                        let ty = match &c.sql_type {
                            SqlType::Other { name } => name.clone(),
                            other => pg_type(other),
                        };
                        format!("    {} {}{}{}", q(&c.name), ty, if c.nullable { "" } else { " NOT NULL" }, c.default_definition.as_ref().map(|d| format!(" DEFAULT {d}")).unwrap_or_default())
                    })
                    .collect();
                for k in keys {
                    if let Some(def) = k.definition {
                        lines.push(format!("    CONSTRAINT {} {def}", q(&k.name)));
                    }
                }
                Ok(format!("CREATE TABLE {target} (\n{}\n);\n", lines.join(",\n")))
            }
            (ScriptKind::Drop, k) => {
                let word = match k {
                    ObjectKind::View => "VIEW",
                    ObjectKind::Sequence => "SEQUENCE",
                    ObjectKind::Procedure => "PROCEDURE",
                    ObjectKind::ScalarFunction | ObjectKind::TableFunction | ObjectKind::AggregateFunction => "FUNCTION",
                    _ => "TABLE",
                };
                Ok(format!("DROP {word} {target};\n"))
            }
            (ScriptKind::Select, _) => {
                let cols = self.list_columns(obj).await?;
                let list = if cols.is_empty() { "*".to_string() } else { cols.iter().map(|c| format!("    {}", q(&c.name))).collect::<Vec<_>>().join(",\n") };
                Ok(format!("SELECT\n{list}\nFROM {target}\nLIMIT 1000;\n"))
            }
            (ScriptKind::Execute, _) => {
                let params = self.list_parameters(obj).await.unwrap_or_default();
                let args = params.iter().map(|p| format!("<{}>", p.name)).collect::<Vec<_>>().join(", ");
                Ok(if obj.kind == ObjectKind::Procedure { format!("CALL {target}({args});\n") } else { format!("SELECT * FROM {target}({args});\n") })
            }
            _ => Err(DriverError::Unsupported("script kind")),
        }
    }

    async fn table_stats(&mut self, obj: &ObjectRef) -> Result<TableStats> {
        let target = qualified(obj);
        let rows = self.rows(&format!("SELECT count(*), pg_total_relation_size({}::regclass) FROM {target}", lit(&target))).await?;
        Ok(TableStats { rows: rows.first().map(|r| cell(r, 0)).and_then(|s| s.parse().ok()).unwrap_or(0), reserved_bytes: rows.first().map(|r| cell(r, 1)).and_then(|s| s.parse().ok()).unwrap_or(0) })
    }

    async fn distinct_values(&mut self, obj: &ObjectRef, column: &str, limit: usize) -> Result<Vec<String>> {
        let rows = self.rows(&format!("SELECT DISTINCT {c}::text FROM {t} WHERE {c} IS NOT NULL ORDER BY 1 LIMIT {}", limit.max(1), c = q(column), t = qualified(obj))).await?;
        Ok(rows.into_iter().map(|r| cell(&r, 0)).collect())
    }
}

/// The PostgreSQL type a column of this SQL type is declared with (Save as table, Import).
pub fn pg_type(t: &SqlType) -> String {
    match t {
        SqlType::Bit => "boolean".into(),
        SqlType::TinyInt | SqlType::SmallInt => "smallint".into(),
        SqlType::Int => "integer".into(),
        SqlType::BigInt => "bigint".into(),
        SqlType::Decimal { precision, scale } | SqlType::Numeric { precision, scale } => format!("numeric({precision},{scale})"),
        SqlType::Money => "numeric(19,4)".into(),
        SqlType::SmallMoney => "numeric(10,4)".into(),
        SqlType::Float => "double precision".into(),
        SqlType::Real => "real".into(),
        SqlType::Date => "date".into(),
        SqlType::Time { .. } => "time".into(),
        SqlType::DateTime | SqlType::DateTime2 { .. } | SqlType::SmallDateTime => "timestamp".into(),
        SqlType::DateTimeOffset { .. } => "timestamptz".into(),
        SqlType::Char { len } | SqlType::NChar { len } => len.map(|n| format!("char({n})")).unwrap_or_else(|| "text".into()),
        SqlType::VarChar { len: Some(n) } | SqlType::NVarChar { len: Some(n) } => format!("varchar({n})"),
        SqlType::Binary { .. } | SqlType::VarBinary { .. } | SqlType::Image | SqlType::Timestamp => "bytea".into(),
        SqlType::UniqueIdentifier => "uuid".into(),
        SqlType::Json => "jsonb".into(),
        SqlType::Xml => "xml".into(),
        _ => "text".into(),
    }
}

/// `CREATE TABLE "s"."t" ("a" integer NOT NULL, …);`
pub fn create_table_sql(schema: &str, table: &str, columns: &[(String, SqlType, bool)]) -> String {
    let cols: Vec<String> = columns.iter().map(|(n, t, nullable)| format!("    {} {}{}", q(n), pg_type(t), if *nullable { "" } else { " NOT NULL" })).collect();
    format!("CREATE TABLE {}.{} (\n{}\n);", q(schema), q(table), cols.join(",\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_values_parse() {
        assert_eq!(parse_date("2024-02-29"), Some(19782));
        assert_eq!(parse_timestamp("2024-02-29 13:45:30.5"), Some(1_709_214_330_500_000));
        assert_eq!(parse_timestamptz("2024-02-29 13:45:30+02"), Some(1_709_214_330_000_000 - 2 * 3600 * 1_000_000));
        assert_eq!(parse_timestamptz("2024-02-29 13:45:30.25-05:30"), Some(1_709_214_330_250_000 + (5 * 3600 + 1800) * 1_000_000));
        assert_eq!(parse_bytea("\\x00ff10"), vec![0, 255, 16]);
        assert_eq!(parse_time_ns("13:45:30.5"), Some((13 * 3600 + 45 * 60 + 30) * 1_000_000_000 + 500_000_000));
        assert_eq!(sql_type_from_name("numeric(12,3)"), SqlType::Numeric { precision: 12, scale: 3 });
        assert_eq!(sql_type_from_name("character varying(40)"), SqlType::NVarChar { len: Some(40) });
        assert_eq!(sql_type_from_name("timestamp with time zone"), SqlType::DateTimeOffset { scale: 6 });
        assert_eq!(sql_type_from_name("integer[]"), SqlType::Other { name: "integer[]".into() });
        assert_eq!(pg_type(&SqlType::Money), "numeric(19,4)");
    }

    #[test]
    fn batches_type_the_text() {
        let cols = vec![ColumnInfo::new("i", SqlType::Int, true, 0), ColumnInfo::new("b", SqlType::Bit, true, 1), ColumnInfo::new("d", SqlType::Date, true, 2), ColumnInfo::new("x", SqlType::Other { name: "integer[]".into() }, true, 3)];
        let rows = vec![vec![Some("1".into()), Some("t".into()), Some("2024-01-02".into()), Some("{1,2}".into())], vec![Some("oops".into()), None, Some("bad".into()), None]];
        let mut misfits = vec![0; 4];
        let b = to_batch(&rows, &cols, &mut misfits).unwrap();
        assert_eq!(b.num_rows(), 2);
        assert_eq!(misfits, vec![1, 0, 1, 0]);
        assert!(b.column(0).is_null(1));
        assert_eq!(b.schema().field(3).data_type(), &DataType::LargeUtf8);
    }
}
