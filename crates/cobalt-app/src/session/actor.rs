//! Per-tab and per-profile connection actors.

use super::{Event, MetadataRequest, MetadataResponse, RunSink, Shared, SinkMsg};
use cobalt_core::*;
use cobalt_driver::{split_batches, Connection, DriverError, StreamItem};
use cobalt_results::{ResultSet, RunState};
use futures_util::StreamExt;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;

pub enum TabMsg {
    Run { run: RunId, script: String, opts: ExecOptions, start_line: u32, sink: Option<RunSink> },
    Cancel,
    Import { table: String, create_sql: Option<String>, columns: Vec<ColumnInfo>, rx: std::sync::mpsc::Receiver<std::result::Result<arrow::array::RecordBatch, String>>, cancel: Arc<std::sync::atomic::AtomicBool> },
    FetchMore { rows: Option<u64> },
    ChangeDatabase { database: String },
    Ping,
    Close,
}

pub enum MetaMsg {
    Request { req: RequestId, kind: MetadataRequest },
    Close,
}

async fn open(shared: &Shared, profile: &ConnectionProfile, creds: &ResolvedCredentials, database: Option<&str>, role: ConnectionRole) -> Result<Box<dyn Connection>, DriverError> {
    let mut p = profile.clone();
    if let Some(db) = database {
        p.database = Some(db.to_string());
    }
    shared.driver.connect(&p, creds, role).await
}

pub async fn tab_actor(
    tab: TabId,
    profile: ConnectionProfile,
    creds: ResolvedCredentials,
    database: Option<String>,
    mut rx: mpsc::UnboundedReceiver<TabMsg>,
    shared: Shared,
) {
    let mut conn = match open(&shared, &profile, &creds, database.as_deref(), ConnectionRole::Query).await {
        Ok(c) => c,
        Err(e) => {
            shared.emit(Event::ConnectFailed { tab, error: e.to_string(), hint: e.hint().map(str::to_string) });
            return;
        }
    };
    shared.emit(Event::Connected { tab, engine: conn.engine().clone(), spid: conn.spid(), database: conn.current_database().to_string() });

    while let Some(msg) = rx.recv().await {
        match msg {
            TabMsg::Close => break,
            TabMsg::Ping => {
                let ok = conn.ping().await.is_ok();
                shared.emit(Event::Pong { tab, ok });
                if !ok {
                    break;
                }
            }
            TabMsg::ChangeDatabase { database } => match conn.change_database(&database).await {
                Ok(()) => shared.emit(Event::DatabaseChanged { tab, database: conn.current_database().to_string() }),
                Err(e) => shared.emit(Event::DatabaseChangeFailed { tab, error: e.to_string() }),
            },
            TabMsg::Cancel | TabMsg::FetchMore { .. } => { /* nothing running */ }
            TabMsg::Import { table, create_sql, columns, rx, cancel } => {
                let lost = run_import(tab, &table, create_sql.as_deref(), &columns, rx, cancel, &mut *conn, &shared).await;
                if lost {
                    break;
                }
            }
            TabMsg::Run { run, script, opts, start_line, sink } => {
                let before = conn.current_database().to_string();
                let lost = run_script(tab, run, &script, &opts, start_line, sink, &mut *conn, &mut rx, &shared).await;
                if lost {
                    break;
                }
                // a `USE` inside the script moved the session; tell the tab
                let after = conn.current_database().to_string();
                if after != before {
                    shared.emit(Event::DatabaseChanged { tab, database: after });
                }
            }
        }
    }
    let _ = conn.close().await;
    shared.emit(Event::Disconnected { tab });
}

/// Run one statement and drain its response; a server error becomes `Err`.
async fn run_simple(conn: &mut dyn Connection, sql: &str) -> Result<(), DriverError> {
    let mut stream = conn.execute(sql, &ExecOptions::default()).await?;
    let mut err: Option<ServerMessage> = None;
    while let Some(item) = stream.next().await {
        if let StreamItem::Done { error: Some(e), .. } = item {
            err = Some(e);
        }
    }
    match err {
        Some(e) => Err(DriverError::Server(e)),
        None => Ok(()),
    }
}

/// Import Data: optional CREATE TABLE, then BEGIN TRAN → bulk insert → COMMIT (ROLLBACK on any
/// failure or cancel). Returns true if the connection is unusable afterwards.
async fn run_import(
    tab: TabId,
    table: &str,
    create_sql: Option<&str>,
    columns: &[ColumnInfo],
    rx: std::sync::mpsc::Receiver<std::result::Result<arrow::array::RecordBatch, String>>,
    cancel: Arc<std::sync::atomic::AtomicBool>,
    conn: &mut dyn Connection,
    shared: &Shared,
) -> bool {
    use std::sync::atomic::Ordering;
    shared.emit(Event::ImportStarted { tab });
    let started = Instant::now();
    let mut lost = false;
    let result: Result<u64, String> = async {
        if let Some(sql) = create_sql {
            run_simple(conn, sql).await.map_err(|e| format!("CREATE TABLE failed: {e}"))?;
        }
        run_simple(conn, "BEGIN TRANSACTION").await.map_err(|e| e.to_string())?;
        let mut last = Instant::now();
        let emit = shared.clone();
        let mut progress = move |rows: u64| -> bool {
            if last.elapsed().as_millis() >= 200 {
                emit.emit(Event::ImportProgress { tab, rows });
                last = Instant::now();
            }
            !cancel.load(Ordering::Relaxed)
        };
        let loaded = conn.bulk_insert(table, columns, rx, &mut progress).await;
        match loaded {
            Ok(n) => {
                run_simple(conn, "COMMIT TRANSACTION").await.map_err(|e| format!("COMMIT failed: {e}"))?;
                Ok(n)
            }
            Err(e) => {
                if matches!(e, DriverError::Disconnected(_)) {
                    lost = true;
                } else if let Err(rb) = run_simple(conn, "IF @@TRANCOUNT > 0 ROLLBACK TRANSACTION").await {
                    tracing::warn!(error = %rb, "rollback after failed import");
                }
                Err(match e {
                    DriverError::Cancelled => "cancelled".to_string(),
                    other => other.to_string(),
                })
            }
        }
    }
    .await;
    shared.emit(Event::ImportDone { tab, result: result.map(|n| (n, started.elapsed())) });
    lost
}

/// Hand a message to the run-to-export sink, staying responsive to Cancel/Close while the writer
/// is busy. `false` means the export side is gone or the user cancelled: abort the query.
async fn sink_push(sink: &RunSink, msg: SinkMsg, rx: &mut mpsc::UnboundedReceiver<TabMsg>) -> bool {
    use std::sync::mpsc::TrySendError;
    let mut msg = msg;
    loop {
        match sink.tx.try_send(msg) {
            Ok(()) => return true,
            Err(TrySendError::Disconnected(_)) => return false,
            Err(TrySendError::Full(m)) => {
                msg = m;
                // the writer is busy: wait a moment, but let Cancel/Close through
                tokio::select! {
                    biased;
                    m = rx.recv() => match m {
                        Some(TabMsg::Cancel) | Some(TabMsg::Close) | None => return false,
                        _ => continue,
                    },
                    _ = tokio::time::sleep(std::time::Duration::from_millis(5)) => continue,
                }
            }
        }
    }
}

/// Runs every batch of `script`. Returns true if the connection is unusable afterwards.
async fn run_script(
    tab: TabId,
    run: RunId,
    script: &str,
    opts: &ExecOptions,
    start_line: u32,
    sink: Option<RunSink>,
    conn: &mut dyn Connection,
    rx: &mut mpsc::UnboundedReceiver<TabMsg>,
    shared: &Shared,
) -> bool {
    let batches = split_batches(script);
    let started = Instant::now();
    shared.emit(Event::RunStarted { tab, run, batches: batches.len() });
    let mut total_rows = 0u64;
    let mut cancelled = false;
    let mut failed = false;
    let mut rs_index = 0usize;
    let cancel = conn.cancel_handle();
    cancel.reset();

    'batches: for (bi, batch) in batches.iter().enumerate() {
        let batch_line = start_line + batch.start_line - 1;
        shared.emit(Event::BatchStarted { tab, run, batch: bi, start_line: batch_line });
        let batch_started = Instant::now();
        let mut stream = match conn.execute(&batch.sql, opts).await {
            Ok(s) => s,
            Err(DriverError::Server(m)) => {
                shared.emit(Event::Message { tab, run, message: m.clone(), batch: bi, batch_start_line: batch_line });
                shared.emit(Event::BatchDone { tab, run, batch: bi, error: Some(m), elapsed: batch_started.elapsed() });
                failed = true;
                break;
            }
            Err(e) => {
                let m = ServerMessage::error(0, e.to_string(), 0);
                shared.emit(Event::Message { tab, run, message: m.clone(), batch: bi, batch_start_line: batch_line });
                shared.emit(Event::BatchDone { tab, run, batch: bi, error: Some(m), elapsed: batch_started.elapsed() });
                shared.emit(Event::RunDone { tab, run, cancelled: false, failed: true, elapsed: started.elapsed(), total_rows });
                return matches!(e, DriverError::Disconnected(_));
            }
        };

        let mut current: Option<Arc<ResultSet>> = None;
        let mut rows_in_set = 0u64;
        // run-to-export never pauses at the row cap: the file gets everything
        let mut cap = if sink.is_some() || opts.row_cap == 0 { u64::MAX } else { opts.row_cap };
        let mut batch_error: Option<ServerMessage> = None;
        let mut last_repaint = Instant::now();
        // rows received past the cap, appended when the user asks for more
        let mut held: Option<arrow::array::RecordBatch> = None;

        loop {
            let item = tokio::select! {
                biased;
                msg = rx.recv() => {
                    match msg {
                        Some(TabMsg::Cancel) | Some(TabMsg::Close) | None => {
                            cancel.cancel();
                            cancelled = true;
                            // Keep draining until the driver acknowledges with Done. If we were
                            // paused at the row cap the stream arm is disabled by the cap guard,
                            // so lift the cap (dropping the held remainder) or we would never
                            // poll the driver again and the run would hang in "Cancelling…".
                            held = None;
                            cap = u64::MAX;
                            continue;
                        }
                        Some(TabMsg::FetchMore { rows }) => {
                            cap = rows.map(|r| rows_in_set + r).unwrap_or(u64::MAX);
                            if let Some(rs) = &current {
                                rs.set_state(RunState::Streaming);
                                if let Some(h) = held.take() {
                                    let n = h.num_rows() as u64;
                                    let take = n.min(cap - rows_in_set);
                                    if take < n {
                                        held = Some(h.slice(take as usize, (n - take) as usize));
                                    }
                                    if take > 0 {
                                        let _ = rs.append(h.slice(0, take as usize));
                                        rows_in_set += take;
                                        total_rows += take;
                                    }
                                    if rows_in_set >= cap {
                                        rs.set_state(RunState::Paused);
                                        shared.emit(Event::Paused { tab, run, index: rs.index, rows: rows_in_set });
                                    }
                                }
                            }
                            continue;
                        }
                        Some(_) => continue, // ignore other messages while running
                    }
                }
                item = stream.next(), if rows_in_set < cap || current.is_none() => item,
            };
            let Some(item) = item else { break };
            match item {
                StreamItem::ResultSetStart { columns } => {
                    let rs = ResultSet::new(rs_index, columns, shared.budget.clone(), shared.spill_dir.clone());
                    rows_in_set = 0;
                    cap = if sink.is_some() || opts.row_cap == 0 { u64::MAX } else { opts.row_cap };
                    shared.emit(Event::ResultSetStarted { tab, run, rs: rs.clone() });
                    if let Some(s) = &sink {
                        if !sink_push(s, SinkMsg::SetStart { index: rs.index, columns: rs.columns.clone(), schema: rs.schema.clone() }, rx).await && !cancelled {
                            cancel.cancel();
                            cancelled = true;
                        }
                    }
                    current = Some(rs);
                }
                StreamItem::Rows(batch) => {
                    if cancelled {
                        continue; // rows still in flight after an attention signal are discarded
                    }
                    if let Some(rs) = &current {
                        let mut batch = batch;
                        let mut n = batch.num_rows() as u64;
                        if let Some(s) = &sink {
                            // run-to-export: the sink gets every row; the grid keeps a preview
                            if !sink_push(s, SinkMsg::Batch(batch.clone()), rx).await {
                                cancel.cancel();
                                cancelled = true;
                                continue;
                            }
                            let room = s.preview_rows.saturating_sub(rows_in_set);
                            if room > 0 {
                                let take = room.min(n) as usize;
                                if let Err(e) = rs.append(batch.slice(0, take)) {
                                    tracing::error!(error = %e, "preview append failed");
                                }
                            }
                            rows_in_set += n;
                            total_rows += n;
                            if last_repaint.elapsed().as_millis() >= 33 {
                                (shared.repaint)();
                                last_repaint = Instant::now();
                            }
                            continue;
                        }
                        if rows_in_set + n > cap {
                            let take = cap.saturating_sub(rows_in_set);
                            held = Some(batch.slice(take as usize, (n - take) as usize));
                            batch = batch.slice(0, take as usize);
                            n = take;
                        }
                        if n > 0 {
                            if let Err(e) = rs.append(batch) {
                                tracing::error!(error = %e, "append failed");
                            }
                        }
                        rows_in_set += n;
                        total_rows += n;
                        if rows_in_set >= cap {
                            rs.set_state(RunState::Paused);
                            shared.emit(Event::Paused { tab, run, index: rs.index, rows: rows_in_set });
                        } else if last_repaint.elapsed().as_millis() >= 33 {
                            (shared.repaint)();
                            last_repaint = Instant::now();
                        }
                    }
                }
                StreamItem::ResultSetEnd { rows } => {
                    if let (Some(s), Some(_)) = (&sink, &current) {
                        if !sink_push(s, if cancelled { SinkMsg::Failed("cancelled".into()) } else { SinkMsg::SetEnd }, rx).await && !cancelled {
                            cancel.cancel();
                            cancelled = true;
                        }
                    }
                    if let Some(h) = held.take() {
                        // the server finished while we were capped: keep the remainder appended so nothing is lost
                        if let Some(rs) = &current {
                            let n = h.num_rows() as u64;
                            let _ = rs.append(h);
                            rows_in_set += n;
                            total_rows += n;
                        }
                    }
                    if let Some(rs) = current.take() {
                        rs.set_state(RunState::Complete);
                        // after a cancel the server's count includes rows we discarded; report what is shown
                        let rows = if cancelled { rows_in_set } else { rows.max(rows_in_set) };
                        shared.emit(Event::ResultSetDone { tab, run, index: rs.index, rows });
                        rs_index += 1;
                    }
                }
                StreamItem::RowsAffected(n) => shared.emit(Event::RowsAffected { tab, run, rows: n }),
                StreamItem::Message(m) => shared.emit(Event::Message { tab, run, message: m, batch: bi, batch_start_line: batch_line }),
                StreamItem::Done { error, cancelled: c } => {
                    if let Some(rs) = current.take() {
                        if let Some(s) = &sink {
                            // the set ended without a clean ResultSetEnd: the writer must discard it
                            let why = if c || cancelled { "cancelled".to_string() } else { error.as_ref().map(|e| e.message.clone()).unwrap_or_else(|| "the query ended unexpectedly".into()) };
                            let _ = sink_push(s, SinkMsg::Failed(why), rx).await;
                        }
                        rs.set_state(if c { RunState::Cancelled } else if error.is_some() { RunState::Error { message: error.as_ref().map(|e| e.message.clone()).unwrap_or_default() } } else { RunState::Complete });
                        shared.emit(Event::ResultSetDone { tab, run, index: rs.index, rows: rows_in_set });
                        rs_index += 1;
                    }
                    if c {
                        cancelled = true;
                    }
                    // the driver already emitted the error in-stream as a Message; only record it here
                    batch_error = error;
                    break;
                }
            }
        }
        drop(stream);
        let had_error = batch_error.is_some();
        shared.emit(Event::BatchDone { tab, run, batch: bi, error: batch_error, elapsed: batch_started.elapsed() });
        if cancelled {
            break 'batches;
        }
        if had_error {
            failed = true;
            if opts_stop_on_error(opts) {
                break 'batches;
            }
        }
    }
    if let Some(s) = &sink {
        let end = if cancelled { SinkMsg::Failed("cancelled".into()) } else if failed { SinkMsg::Failed("the query failed".into()) } else { SinkMsg::RunEnd };
        let _ = sink_push(s, end, rx).await;
    }
    shared.emit(Event::RunDone { tab, run, cancelled, failed, elapsed: started.elapsed(), total_rows });
    false
}

fn opts_stop_on_error(_opts: &ExecOptions) -> bool {
    true
}

pub async fn meta_actor(profile: ConnectionProfile, creds: ResolvedCredentials, mut rx: mpsc::UnboundedReceiver<MetaMsg>, shared: Shared) {
    let mut conn: Option<Box<dyn Connection>> = None;
    while let Some(msg) = rx.recv().await {
        match msg {
            MetaMsg::Close => break,
            MetaMsg::Request { req, kind } => {
                if conn.is_none() {
                    match open(&shared, &profile, &creds, None, ConnectionRole::Metadata).await {
                        Ok(c) => conn = Some(c),
                        Err(e) => {
                            let hint = e.hint().map(|h| format!(" ({h})")).unwrap_or_default();
                            shared.emit(Event::Metadata { req, profile: profile.id, result: Err(format!("{e}{hint}")) });
                            break;
                        }
                    }
                }
                let c = conn.as_mut().unwrap();
                let result = serve(c.as_mut(), kind).await;
                let lost = matches!(result, Err(DriverError::Disconnected(_)));
                shared.emit(Event::Metadata { req, profile: profile.id, result: result.map_err(|e| e.to_string()) });
                if lost {
                    break;
                }
            }
        }
    }
    if let Some(mut c) = conn {
        let _ = c.close().await;
    }
}

async fn serve(conn: &mut dyn Connection, kind: MetadataRequest) -> Result<MetadataResponse, DriverError> {
    Ok(match kind {
        MetadataRequest::Probe => MetadataResponse::Probe(conn.engine().clone()),
        MetadataRequest::ListDatabases => MetadataResponse::Databases(conn.list_databases().await?),
        MetadataRequest::ListSchemas { database } => MetadataResponse::Schemas(conn.list_schemas(&database).await?),
        MetadataRequest::ListObjects { database } => MetadataResponse::Objects(conn.list_objects(&database).await?),
        MetadataRequest::ListColumns { obj } => MetadataResponse::Columns(conn.list_columns(&obj).await?),
        MetadataRequest::ListParameters { obj } => MetadataResponse::Parameters(conn.list_parameters(&obj).await?),
        MetadataRequest::ListIndexes { obj } => MetadataResponse::Indexes(conn.list_indexes(&obj).await?),
        MetadataRequest::ListKeys { obj } => MetadataResponse::Keys(conn.list_keys(&obj).await?),
        MetadataRequest::LoadCatalog { database } => MetadataResponse::Catalog(conn.load_catalog(&database).await?),
        MetadataRequest::Script { obj, kind } => MetadataResponse::Script(conn.script(&obj, kind).await?),
    })
}
