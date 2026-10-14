//! A DAX run against a semantic model: one task per run, no connection actor. The task mints
//! the Power BI token, posts the query to the Arrow query API (`cobalt_tabular`), and emits the
//! same run events a connection's batch does, so the grid, Messages, history, Run to File and
//! dashboard tiles serve it unchanged. Cancel is a watch flag: the HTTP stream is dropped,
//! which closes the connection (the service finishes the query on its own clock).
//! Design: `docs/design/semantic_models.md` §4.

use super::{Event, RunSink, Shared, SinkMsg, TabularAuth};
use cobalt_core::tabular::TabularTarget;
use cobalt_core::*;
use cobalt_results::{ResultSet, RunState};
use cobalt_tabular::{Credential, DaxItem, DaxOptions, Transport};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::watch;

pub struct TabularRun {
    pub tab: TabId,
    pub run: RunId,
    pub target: TabularTarget,
    pub auth: TabularAuth,
    pub text: String,
    pub opts: DaxOptions,
    pub sink: Option<RunSink>,
}

/// The message when the stored sign-in cannot mint a token (the panel is told to ask for one).
pub const NEED_SIGN_IN: &str = "Sign in on the Fabric panel: the stored sign-in no longer refreshes (semantic model queries use that account).";

/// A Power BI token for the slot, silently (the panel's sign-in already consented).
pub async fn powerbi_token(auth: &TabularAuth) -> Result<String, String> {
    let resource = cobalt_auth::provider::POWERBI_RESOURCE;
    let Some(slot) = auth.slot else { return Err(NEED_SIGN_IN.into()) };
    let mut ts = match auth.resolver.resource_token_silent(slot, resource, auth.tenant.as_deref()).await {
        Ok(Some(ts)) => ts,
        Ok(None) => return Err(NEED_SIGN_IN.into()),
        Err(e) => return Err(format!("Could not get a Power BI token: {e}")),
    };
    if !cobalt_auth::provider::missing_powerbi_scopes(&ts.access.scope).is_empty() {
        // a consent granted since the token was cached (tenant admin consent) shows up only on a
        // fresh refresh: try once before refusing
        auth.resolver.forget_resource(slot, resource);
        if let Ok(Some(fresh)) = auth.resolver.resource_token_silent(slot, resource, auth.tenant.as_deref()).await {
            ts = fresh;
        }
    }
    match Ok::<_, String>(Some(ts)) {
        Ok(Some(ts)) => {
            // the Power BI API answers a token without the dataset permission with a bare 401,
            // so say what is missing before sending anything
            let missing = cobalt_auth::provider::missing_powerbi_scopes(&ts.access.scope);
            if !missing.is_empty() {
                return Err(format!(
                    "The sign-in lacks the Power BI permission {} (semantic model queries need it). Add it to Cobalt's app registration as a delegated Power BI Service permission, then use Grant permissions on the Fabric panel.",
                    missing.join(", ")
                ));
            }
            Ok(ts.access.token.expose().to_string())
        }
        Ok(None) => Err("Sign in on the Fabric panel first: semantic model queries use that account.".into()),
        Err(e) => Err(format!("Could not get a Power BI token: {e}")),
    }
}

/// The credential a target needs: a Power BI or Azure AS bearer token from the slot, the
/// profile's basic credentials, or nothing.
pub async fn credential_for(auth: &TabularAuth, target: &TabularTarget) -> Result<Credential, String> {
    if let Some((user, password)) = &auth.basic {
        return Ok(Credential::Basic { user: user.clone(), password: password.clone() });
    }
    if auth.anonymous {
        return Ok(Credential::Anonymous);
    }
    match cobalt_tabular::credential_resource(target, auth.force_xmla) {
        None => Ok(Credential::Anonymous),
        Some(cobalt_auth::provider::POWERBI_RESOURCE) => powerbi_token(auth).await.map(Credential::Bearer),
        Some(resource) => {
            let Some(slot) = auth.slot else { return Err(NEED_SIGN_IN.into()) };
            match auth.resolver.resource_token_silent(slot, resource, auth.tenant.as_deref()).await {
                Ok(Some(ts)) => Ok(Credential::Bearer(ts.access.token.expose().to_string())),
                Ok(None) => Err(NEED_SIGN_IN.into()),
                Err(e) => Err(format!("Could not get a token for {resource}: {e}")),
            }
        }
    }
}

/// A connected transport for the target: the Arrow query API for Power BI (unless XMLA is
/// forced), XMLA for Azure AS and HTTP endpoints.
pub async fn connect(auth: &TabularAuth, target: &TabularTarget) -> Result<Transport, String> {
    let cred = credential_for(auth, target).await?;
    Transport::connect(target, cred, auth.force_xmla).await.map_err(|e| match e.hint() {
        Some(h) => format!("{e}
{h}"),
        None => e.to_string(),
    })
}

pub async fn run_tabular(r: TabularRun, mut cancel: watch::Receiver<bool>, shared: Shared) {
    let TabularRun { tab, run, target, auth, text, opts, sink } = r;
    let started = Instant::now();
    shared.emit(Event::RunStarted { tab, run, batches: 1 });
    shared.emit(Event::BatchStarted { tab, run, batch: 0, start_line: 1 });
    let fail = |m: String, shared: &Shared, sink: &Option<RunSink>| {
        let msg = ServerMessage::error(0, m, 0);
        shared.emit(Event::Message { tab, run, message: msg.clone(), batch_start_line: 1 });
        shared.emit(Event::BatchDone { tab, run, batch: 0, error: Some(msg), elapsed: started.elapsed() });
        if let Some(s) = sink {
            let _ = s.tx.send(SinkMsg::Failed("the query failed".into()));
        }
        shared.emit(Event::RunDone { tab, run, cancelled: false, failed: true, elapsed: started.elapsed(), total_rows: 0 });
    };
    let client = tokio::select! {
        biased;
        _ = cancel.changed() => {
            shared.emit(Event::BatchDone { tab, run, batch: 0, error: None, elapsed: started.elapsed() });
            if let Some(s) = &sink {
                let _ = s.tx.send(SinkMsg::Failed("cancelled".into()));
            }
            shared.emit(Event::RunDone { tab, run, cancelled: true, failed: false, elapsed: started.elapsed(), total_rows: 0 });
            return;
        }
        c = connect(&auth, &target) => match c {
            Ok(c) => c,
            Err(e) => return fail(e, &shared, &sink),
        },
    };
    shared.emit(Event::Message { tab, run, message: ServerMessage::info(format!("transport: {}", client.name())), batch_start_line: 1 });
    let mut stream = tokio::select! {
        biased;
        _ = cancel.changed() => {
            shared.emit(Event::BatchDone { tab, run, batch: 0, error: None, elapsed: started.elapsed() });
            if let Some(s) = &sink {
                let _ = s.tx.send(SinkMsg::Failed("cancelled".into()));
            }
            shared.emit(Event::RunDone { tab, run, cancelled: true, failed: false, elapsed: started.elapsed(), total_rows: 0 });
            return;
        }
        r = client.execute(&target, &text, &opts) => match r {
            Ok(s) => s,
            Err(e) => {
                let hint = e.hint();
                let m = match hint {
                    Some(h) => format!("{e}\n{h}"),
                    None => e.to_string(),
                };
                return fail(m, &shared, &sink);
            }
        },
    };

    let mut total_rows = 0u64;
    let mut rows_in_set = 0u64;
    let mut rs_index = 0usize;
    let mut current: Option<Arc<ResultSet>> = None;
    let mut cancelled = false;
    let mut failed = false;
    let mut last_repaint = Instant::now();
    loop {
        let item = tokio::select! {
            biased;
            _ = cancel.changed() => {
                cancelled = true;
                // XMLA: tell the server too (the HTTP stream is dropped below either way)
                let _ = client.cancel().await;
                break;
            }
            item = stream.next() => item,
        };
        match item {
            Ok(Some(DaxItem::ResultSetStart { columns })) => {
                let rs = ResultSet::new(rs_index, columns, shared.budget.clone(), shared.spill_dir.clone());
                rows_in_set = 0;
                shared.emit(Event::ResultSetStarted { tab, run, rs: rs.clone() });
                if let Some(s) = &sink {
                    if !sink_push(s, SinkMsg::SetStart { index: rs.index, columns: rs.columns.clone(), schema: rs.schema.clone() }, &mut cancel).await {
                        cancelled = true;
                        break;
                    }
                }
                current = Some(rs);
            }
            Ok(Some(DaxItem::Batch(batch))) => {
                let Some(rs) = &current else { continue };
                let n = batch.num_rows() as u64;
                if let Some(s) = &sink {
                    if !sink_push(s, SinkMsg::Batch(batch.clone()), &mut cancel).await {
                        cancelled = true;
                        break;
                    }
                    let room = s.preview_rows.saturating_sub(rows_in_set);
                    if room > 0 {
                        let take = room.min(n) as usize;
                        if let Err(e) = rs.append(batch.slice(0, take)) {
                            tracing::error!(error = %e, "preview append failed");
                        }
                    }
                } else if let Err(e) = rs.append(batch) {
                    tracing::error!(error = %e, "append failed");
                }
                rows_in_set += n;
                total_rows += n;
                if last_repaint.elapsed().as_millis() >= 33 {
                    (shared.repaint)();
                    last_repaint = Instant::now();
                }
            }
            Ok(Some(DaxItem::ResultSetEnd { rows })) => {
                if let Some(s) = &sink {
                    if !sink_push(s, SinkMsg::SetEnd, &mut cancel).await {
                        cancelled = true;
                        break;
                    }
                }
                if let Some(rs) = current.take() {
                    rs.set_state(RunState::Complete);
                    shared.emit(Event::ResultSetDone { tab, run, index: rs.index, rows: rows.max(rows_in_set) });
                    rs_index += 1;
                }
            }
            Ok(Some(DaxItem::Error { message, code })) => {
                failed = true;
                let text = match code {
                    Some(c) if !c.is_empty() => format!("{message} ({c})"),
                    _ => message,
                };
                shared.emit(Event::Message { tab, run, message: ServerMessage::error(0, text, 0), batch_start_line: 1 });
            }
            Ok(None) => break,
            Err(e) => {
                failed = true;
                let text = match e.hint() {
                    Some(h) => format!("{e}\n{h}"),
                    None => e.to_string(),
                };
                shared.emit(Event::Message { tab, run, message: ServerMessage::error(0, text, 0), batch_start_line: 1 });
                break;
            }
        }
    }
    drop(stream);
    if let Some(rs) = current.take() {
        rs.set_state(if cancelled { RunState::Cancelled } else if failed { RunState::Error { message: "the query failed".into() } } else { RunState::Complete });
        shared.emit(Event::ResultSetDone { tab, run, index: rs.index, rows: rows_in_set });
        if let Some(s) = &sink {
            let _ = s.tx.send(SinkMsg::Failed(if cancelled { "cancelled".into() } else { "the query ended unexpectedly".into() }));
        }
    }
    let error = if failed && !cancelled { Some(ServerMessage::error(0, "the query failed".to_string(), 0)) } else { None };
    shared.emit(Event::BatchDone { tab, run, batch: 0, error, elapsed: started.elapsed() });
    if let Some(s) = &sink {
        let end = if cancelled { SinkMsg::Failed("cancelled".into()) } else if failed { SinkMsg::Failed("the query failed".into()) } else { SinkMsg::RunEnd };
        let _ = s.tx.send(end);
    }
    shared.emit(Event::RunDone { tab, run, cancelled, failed, elapsed: started.elapsed(), total_rows });
}

/// Push to the export sink, waiting while the writer is busy but letting Cancel through.
async fn sink_push(sink: &RunSink, msg: SinkMsg, cancel: &mut watch::Receiver<bool>) -> bool {
    use std::sync::mpsc::TrySendError;
    let mut msg = msg;
    loop {
        match sink.tx.try_send(msg) {
            Ok(()) => return true,
            Err(TrySendError::Disconnected(_)) => return false,
            Err(TrySendError::Full(m)) => {
                msg = m;
                tokio::select! {
                    biased;
                    _ = cancel.changed() => return false,
                    _ = tokio::time::sleep(std::time::Duration::from_millis(20)) => {}
                }
            }
        }
    }
}
