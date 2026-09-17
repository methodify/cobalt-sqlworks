//! Per-tab and per-profile connection actors.

use super::{Event, MetadataRequest, MetadataResponse, Shared};
use cobalt_core::*;
use cobalt_driver::{split_batches, Connection, DriverError, StreamItem};
use cobalt_results::{ResultSet, RunState};
use futures_util::StreamExt;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;

pub enum TabMsg {
    Run { run: RunId, script: String, opts: ExecOptions, start_line: u32 },
    Cancel,
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
            TabMsg::Run { run, script, opts, start_line } => {
                let lost = run_script(tab, run, &script, &opts, start_line, &mut *conn, &mut rx, &shared).await;
                if lost {
                    break;
                }
            }
        }
    }
    let _ = conn.close().await;
    shared.emit(Event::Disconnected { tab });
}

/// Runs every batch of `script`. Returns true if the connection is unusable afterwards.
async fn run_script(
    tab: TabId,
    run: RunId,
    script: &str,
    opts: &ExecOptions,
    start_line: u32,
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
        let mut cap = if opts.row_cap == 0 { u64::MAX } else { opts.row_cap };
        let mut batch_error: Option<ServerMessage> = None;
        let mut last_repaint = Instant::now();

        loop {
            let item = tokio::select! {
                biased;
                msg = rx.recv() => {
                    match msg {
                        Some(TabMsg::Cancel) | Some(TabMsg::Close) | None => {
                            cancel.cancel();
                            cancelled = true;
                            // keep draining until the driver acknowledges with Done
                            continue;
                        }
                        Some(TabMsg::FetchMore { rows }) => {
                            cap = rows.map(|r| rows_in_set + r).unwrap_or(u64::MAX);
                            if let Some(rs) = &current { rs.set_state(RunState::Streaming); }
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
                    cap = if opts.row_cap == 0 { u64::MAX } else { opts.row_cap };
                    shared.emit(Event::ResultSetStarted { tab, run, rs: rs.clone() });
                    current = Some(rs);
                }
                StreamItem::Rows(batch) => {
                    if let Some(rs) = &current {
                        let n = batch.num_rows() as u64;
                        if let Err(e) = rs.append(batch) {
                            tracing::error!(error = %e, "append failed");
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
                    if let Some(rs) = current.take() {
                        rs.set_state(RunState::Complete);
                        shared.emit(Event::ResultSetDone { tab, run, index: rs.index, rows: rows.max(rows_in_set) });
                        rs_index += 1;
                    }
                }
                StreamItem::RowsAffected(n) => shared.emit(Event::RowsAffected { tab, run, rows: n }),
                StreamItem::Message(m) => shared.emit(Event::Message { tab, run, message: m, batch: bi, batch_start_line: batch_line }),
                StreamItem::Done { error, cancelled: c } => {
                    if let Some(rs) = current.take() {
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
