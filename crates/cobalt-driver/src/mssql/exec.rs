//! Batch execution: raw TDS tokens → `StreamItem`s.
//!
//! One `execute` call:
//! 1. settles the session (aborts an abandoned response, switches a lingering plan mode off),
//! 2. sends a `SET …` prelude when `ExecOptions` differ from the session's last applied state
//!    (its own batch, so the user's line numbers stay intact),
//! 3. for plan modes sends `SET SHOWPLAN_XML ON` / `SET STATISTICS XML ON` as their own batch
//!    (SHOWPLAN must be alone in a batch) and the matching `OFF` after the run,
//! 4. sends the user batch and adapts the token stream: `COLMETADATA` → `ResultSetStart`, rows
//!    → `Rows(RecordBatch)` every `batch_rows`, `DONE` → `ResultSetEnd` / `RowsAffected`, `INFO`
//!    → `Message`, `ERROR` → `Message(is_error)` *and* `Done { error: Some(first) }`.
//!
//! Server errors after the batch started never surface as `Err`: they end the stream with
//! `Done { error }`. `Done.error` repeats the first error message so the UI can mark the run as
//! failed; the messages pane should render the `Message` items and not append `Done.error` again.
//! Transport failures and timeouts follow the same shape: a `Message` describing what happened,
//! then `Done { error }`.
//!
//! Cancellation: [`CancelSignal`] is shared with every `CancelHandle`; the stream `select!`s on
//! it (and on the deadline) while waiting for the next token, sends a TDS attention through
//! `Client::cancel_query` (packet-level resync, see the vendored patch), emits what was received
//! so far, then `Done { cancelled: true }`. A timeout ends with `Done { error: Some(timeout) }`
//! and `cancelled: false`.
//!
//! `ExecOptions::row_cap` is not enforced here: the driver streams every row and the result
//! layer decides when to stop pulling (and cancels).

use super::convert::{self, BatchBuilder, MetaColumn};
use super::{server_message_from_error, server_message_from_info, MssqlConnection, CANCEL_ACK_TIMEOUT};
use crate::{DriverError, QueryStream, Result, StreamItem};
use cobalt_core::{Capabilities, ExecOptions, IsolationLevel, PlanMode, ServerMessage};
use futures_util::{FutureExt, StreamExt};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tiberius::{ReceivedToken, TokenColMetaData, TokenDone, TokenEnvChange, TokenRow};
use tokio::sync::Notify;
use tokio::time::Instant;

// ---------------------------------------------------------------------------------------------
// Cancellation
// ---------------------------------------------------------------------------------------------

/// Cross-task cancellation signal shared by a connection and its `CancelHandle`s.
#[derive(Default)]
pub struct CancelSignal {
    requested: AtomicBool,
    notify: Notify,
}

impl CancelSignal {
    pub fn trigger(&self) {
        self.requested.store(true, Ordering::SeqCst);
        self.notify.notify_one();
    }
    pub fn is_requested(&self) -> bool {
        self.requested.load(Ordering::SeqCst)
    }
    /// Clear a stale request (and a stored permit) before a new batch starts.
    pub fn arm(&self) {
        self.requested.store(false, Ordering::SeqCst);
        let _ = self.notify.notified().now_or_never();
    }
    pub async fn wait(&self) {
        self.notify.notified().await
    }
}

// ---------------------------------------------------------------------------------------------
// Session SET state
// ---------------------------------------------------------------------------------------------

/// The `SET` options this driver last applied to the session. `None` = unknown (fresh session).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionState {
    pub nocount: Option<bool>,
    pub arithabort: Option<bool>,
    pub xact_abort: Option<bool>,
    pub statistics_io: Option<bool>,
    pub statistics_time: Option<bool>,
    pub isolation: Option<IsolationLevel>,
}

impl SessionState {
    /// The `SET` batch needed to bring the session to `opts`, or `None` when already there.
    /// Options the engine rejects (see [`Capabilities`]) are left out.
    pub fn prelude_for(&self, opts: &ExecOptions, caps: &Capabilities) -> Option<String> {
        let onoff = |b: bool| if b { "ON" } else { "OFF" };
        let mut parts: Vec<String> = Vec::new();
        if self.nocount != Some(opts.nocount) {
            parts.push(format!("SET NOCOUNT {}", onoff(opts.nocount)));
        }
        if self.arithabort != Some(opts.arithabort) {
            parts.push(format!("SET ARITHABORT {}", onoff(opts.arithabort)));
        }
        if caps.set_xact_abort && self.xact_abort != Some(opts.xact_abort) {
            parts.push(format!("SET XACT_ABORT {}", onoff(opts.xact_abort)));
        }
        if caps.set_statistics && self.statistics_io != Some(opts.statistics_io) {
            parts.push(format!("SET STATISTICS IO {}", onoff(opts.statistics_io)));
        }
        if caps.set_statistics && self.statistics_time != Some(opts.statistics_time) {
            parts.push(format!("SET STATISTICS TIME {}", onoff(opts.statistics_time)));
        }
        if let Some(iso) = opts.isolation {
            if self.isolation != Some(iso) {
                parts.push(format!("SET TRANSACTION ISOLATION LEVEL {}", iso.sql()));
            }
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join(";\n") + ";")
        }
    }

    pub fn apply(&mut self, opts: &ExecOptions) {
        self.nocount = Some(opts.nocount);
        self.arithabort = Some(opts.arithabort);
        self.xact_abort = Some(opts.xact_abort);
        self.statistics_io = Some(opts.statistics_io);
        self.statistics_time = Some(opts.statistics_time);
        if let Some(iso) = opts.isolation {
            self.isolation = Some(iso);
        }
    }
}

pub(crate) fn plan_on_sql(mode: PlanMode) -> Option<&'static str> {
    match mode {
        PlanMode::None => None,
        PlanMode::Estimated => Some("SET SHOWPLAN_XML ON"),
        PlanMode::Actual => Some("SET STATISTICS XML ON"),
    }
}

pub(crate) fn plan_off_sql(mode: PlanMode) -> &'static str {
    match mode {
        PlanMode::None => "",
        PlanMode::Estimated => "SET SHOWPLAN_XML OFF",
        PlanMode::Actual => "SET STATISTICS XML OFF",
    }
}

// ---------------------------------------------------------------------------------------------
// execute
// ---------------------------------------------------------------------------------------------

pub(crate) async fn execute<'a>(conn: &'a mut MssqlConnection, sql: &str, opts: &ExecOptions) -> Result<QueryStream<'a>> {
    conn.settle().await?;
    conn.cancel.arm();

    let mut initial: VecDeque<StreamItem> = VecDeque::new();

    if let Some(prelude) = conn.session.prelude_for(opts, &conn.engine.capabilities) {
        match conn.run_silent(&prelude).await {
            Ok(()) => {}
            // Keep executing: the user's batch still runs, but tell them the option wasn't applied.
            Err(DriverError::Server(mut m)) => {
                m.message = format!("session option could not be applied ({}): {}", prelude.replace('\n', " "), m.message);
                initial.push_back(StreamItem::Message(m));
            }
            Err(e) => return Err(e),
        }
        conn.session.apply(opts);
    }

    if let Some(on) = plan_on_sql(opts.plan) {
        conn.run_silent(on).await?;
        conn.plan_mode_on = Some(opts.plan);
    }

    let timeout_secs = if opts.timeout_secs > 0 { opts.timeout_secs } else { conn.default_timeout_secs };
    let deadline = (timeout_secs > 0).then(|| Instant::now() + Duration::from_secs(timeout_secs as u64));

    if let Err(e) = conn.tds().simple_query_send(sql).await {
        let err = conn.fail(e);
        if let Some(mode) = conn.plan_mode_on.take() {
            let _ = conn.run_silent(plan_off_sql(mode)).await;
        }
        return Err(err);
    }
    conn.in_flight = true;

    let cancel = conn.cancel.clone();
    let run = Run {
        conn,
        cancel,
        batch_rows: opts.batch_rows.max(1),
        deadline,
        timeout_secs,
        pending: initial,
        current: None,
        error: None,
        finished: false,
    };
    Ok(futures_util::stream::unfold(run, |mut run| async move { run.next().await.map(|item| (item, run)) }).boxed())
}

// ---------------------------------------------------------------------------------------------
// Run: the state machine behind the stream
// ---------------------------------------------------------------------------------------------

struct ResultSetState {
    columns: Vec<MetaColumn>,
    /// `None` while `ResultSetStart` is held back for scale inference.
    builder: Option<BatchBuilder>,
    held: Vec<TokenRow<'static>>,
    rows: u64,
}

#[allow(clippy::large_enum_variant)] // transient per-poll value
enum Step {
    Cancel,
    Timeout,
    Token(std::result::Result<Option<ReceivedToken>, tiberius::error::Error>),
}

enum Abort {
    Cancelled,
    Timeout,
}

struct Run<'a> {
    conn: &'a mut MssqlConnection,
    cancel: Arc<CancelSignal>,
    batch_rows: usize,
    deadline: Option<Instant>,
    timeout_secs: u32,
    pending: VecDeque<StreamItem>,
    current: Option<ResultSetState>,
    error: Option<ServerMessage>,
    finished: bool,
}

impl<'a> Run<'a> {
    async fn next(&mut self) -> Option<StreamItem> {
        loop {
            if let Some(item) = self.pending.pop_front() {
                return Some(item);
            }
            if self.finished {
                return None;
            }
            if self.cancel.is_requested() {
                self.abort(Abort::Cancelled).await;
                continue;
            }

            let cancel = self.cancel.clone();
            let step = match self.deadline {
                Some(deadline) => tokio::select! {
                    biased;
                    _ = cancel.wait() => Step::Cancel,
                    _ = tokio::time::sleep_until(deadline) => Step::Timeout,
                    t = self.conn.tds().next_token() => Step::Token(t),
                },
                None => tokio::select! {
                    biased;
                    _ = cancel.wait() => Step::Cancel,
                    t = self.conn.tds().next_token() => Step::Token(t),
                },
            };

            match step {
                Step::Cancel => self.abort(Abort::Cancelled).await,
                Step::Timeout => self.abort(Abort::Timeout).await,
                Step::Token(Ok(Some(token))) => self.handle(token),
                Step::Token(Ok(None)) => self.complete().await,
                Step::Token(Err(e)) => self.transport_failure(e),
            }
        }
    }

    fn handle(&mut self, token: ReceivedToken) {
        match token {
            ReceivedToken::NewResultset(meta) => {
                self.close_result_set();
                self.open_result_set(&meta);
            }
            ReceivedToken::Row(row) => self.push_row(row),
            ReceivedToken::Done(d) | ReceivedToken::DoneInProc(d) => self.handle_done(&d, false),
            ReceivedToken::DoneProc(d) => self.handle_done(&d, true),
            ReceivedToken::Info(i) => self.pending.push_back(StreamItem::Message(server_message_from_info(&i))),
            ReceivedToken::Error(e) => {
                let m = server_message_from_error(&e);
                if self.error.is_none() {
                    self.error = Some(m.clone());
                }
                if m.class >= 20 {
                    // severity 20+ terminates the session: SQL Server closes the connection after it
                    self.conn.broken.get_or_insert_with(|| format!("the server ended the session: {}", m.message));
                }
                self.pending.push_back(StreamItem::Message(m));
            }
            ReceivedToken::EnvChange(TokenEnvChange::Database(new, _)) => self.conn.database = new,
            _ => {}
        }
    }

    fn handle_done(&mut self, done: &TokenDone, is_proc: bool) {
        if done.is_attention() {
            return;
        }
        if self.current.is_some() {
            self.close_result_set();
            return;
        }
        // DONEPROC only summarises the procedure call; the statements inside reported their
        // own DONEINPROC counts (unless NOCOUNT). Report counts only when DONE_COUNT is set.
        if !is_proc && done.has_count() {
            self.pending.push_back(StreamItem::RowsAffected(done.rows()));
        }
    }

    fn open_result_set(&mut self, meta: &TokenColMetaData<'static>) {
        let columns = convert::columns_from_meta(meta);
        let needs_inference = columns.iter().any(|c| c.needs_scale_inference);
        let mut rs = ResultSetState { columns, builder: None, held: Vec::new(), rows: 0 };
        if !needs_inference {
            self.start(&mut rs);
        }
        self.current = Some(rs);
    }

    /// Emit `ResultSetStart` and create the builder (after inference, if any).
    fn start(&mut self, rs: &mut ResultSetState) {
        convert::infer_decimal_scales(&mut rs.columns, &rs.held);
        let infos: Vec<_> = rs.columns.iter().map(|c| c.info.clone()).collect();
        let mut builder = BatchBuilder::new(&infos, self.batch_rows);
        self.pending.push_back(StreamItem::ResultSetStart { columns: infos });
        for row in rs.held.drain(..) {
            builder.push(&row);
            if builder.len() >= self.batch_rows {
                if let Some(batch) = builder.take() {
                    self.pending.push_back(StreamItem::Rows(batch));
                }
            }
        }
        rs.builder = Some(builder);
    }

    fn push_row(&mut self, row: TokenRow<'static>) {
        let batch_rows = self.batch_rows;
        let Some(mut rs) = self.current.take() else {
            tracing::warn!("ROW token without column metadata; ignored");
            return;
        };
        rs.rows += 1;
        match rs.builder.as_mut() {
            Some(builder) => {
                builder.push(&row);
                if builder.len() >= batch_rows {
                    if let Some(batch) = builder.take() {
                        self.pending.push_back(StreamItem::Rows(batch));
                    }
                }
            }
            None => {
                rs.held.push(row);
                let resolved = rs.columns.iter().enumerate().all(|(i, c)| {
                    !c.needs_scale_inference
                        || rs.held.iter().any(|r| matches!(r.get(i), Some(tiberius::ColumnData::Numeric(Some(_)))))
                });
                if resolved || rs.held.len() >= batch_rows {
                    self.start(&mut rs);
                }
            }
        }
        self.current = Some(rs);
    }

    /// Flush the open result set: remaining rows, then `ResultSetEnd`.
    fn close_result_set(&mut self) {
        let Some(mut rs) = self.current.take() else { return };
        if rs.builder.is_none() {
            self.start(&mut rs);
        }
        if let Some(builder) = rs.builder.as_mut() {
            if let Some(batch) = builder.take() {
                self.pending.push_back(StreamItem::Rows(batch));
            }
            if builder.conversion_failures > 0 {
                self.pending.push_back(StreamItem::Message(ServerMessage::info(format!(
                    "{} value(s) could not be represented in the result grid's column type and were shown as NULL (see log)",
                    builder.conversion_failures
                ))));
            }
        }
        self.pending.push_back(StreamItem::ResultSetEnd { rows: rs.rows });
    }

    async fn restore_plan_mode(&mut self) {
        if let Some(mode) = self.conn.plan_mode_on.take() {
            if self.conn.broken.is_none() {
                if let Err(e) = self.conn.run_silent(plan_off_sql(mode)).await {
                    tracing::warn!("could not switch plan mode off: {e}");
                }
            }
        }
    }

    async fn complete(&mut self) {
        self.close_result_set();
        self.conn.in_flight = false;
        self.restore_plan_mode().await;
        self.pending.push_back(StreamItem::Done { error: self.error.take(), cancelled: false });
        self.finished = true;
    }

    async fn abort(&mut self, why: Abort) {
        match tokio::time::timeout(CANCEL_ACK_TIMEOUT, self.conn.tds().cancel_query()).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                let err = self.conn.fail(e);
                self.conn.broken.get_or_insert_with(|| err.to_string());
            }
            Err(_) => {
                self.conn.broken = Some("timed out waiting for the server to acknowledge the cancellation".into());
            }
        }
        self.conn.in_flight = false;
        self.close_result_set();
        self.restore_plan_mode().await;
        let (error, cancelled) = match why {
            Abort::Cancelled => (None, true),
            Abort::Timeout => (
                Some(ServerMessage {
                    number: -2,
                    state: 0,
                    class: 11,
                    message: format!("Execution timeout expired: the batch did not complete within {} s and was cancelled.", self.timeout_secs),
                    server: None,
                    procedure: None,
                    line: 0,
                    is_error: true,
                }),
                false,
            ),
        };
        if let Some(m) = &error {
            self.pending.push_back(StreamItem::Message(m.clone()));
        }
        self.pending.push_back(StreamItem::Done { error, cancelled });
        self.finished = true;
    }

    fn transport_failure(&mut self, e: tiberius::error::Error) {
        let err = self.conn.fail(e);
        self.conn.in_flight = false;
        self.conn.plan_mode_on = None;
        self.close_result_set();
        let msg = ServerMessage {
            number: 0,
            state: 0,
            class: 20,
            message: format!("A transport-level error occurred while receiving results from the server ({err}). The connection is closed."),
            server: None,
            procedure: None,
            line: 0,
            is_error: true,
        };
        self.pending.push_back(StreamItem::Message(msg.clone()));
        self.pending.push_back(StreamItem::Done { error: Some(msg), cancelled: false });
        self.finished = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cobalt_core::EngineKind;

    #[test]
    fn prelude_only_when_state_differs() {
        let caps = EngineKind::SqlServer.capabilities();
        let mut state = SessionState::default();
        let opts = ExecOptions::default();
        let p = state.prelude_for(&opts, &caps).unwrap();
        assert!(p.contains("SET NOCOUNT OFF"));
        assert!(p.contains("SET ARITHABORT ON"));
        assert!(!p.contains("ISOLATION"));
        state.apply(&opts);
        assert_eq!(state.prelude_for(&opts, &caps), None);

        let opts2 = ExecOptions { nocount: true, isolation: Some(IsolationLevel::ReadUncommitted), ..opts.clone() };
        let p = state.prelude_for(&opts2, &caps).unwrap();
        assert_eq!(p, "SET NOCOUNT ON;\nSET TRANSACTION ISOLATION LEVEL READ UNCOMMITTED;");
        state.apply(&opts2);
        // isolation None = leave as is
        assert_eq!(state.prelude_for(&ExecOptions { nocount: true, ..opts }, &caps), None);
    }

    #[test]
    fn prelude_skips_options_the_engine_rejects() {
        let caps = EngineKind::FabricWarehouse.capabilities();
        let state = SessionState::default();
        let p = state.prelude_for(&ExecOptions::default(), &caps).unwrap();
        assert!(p.contains("SET NOCOUNT OFF"));
        assert!(p.contains("SET ARITHABORT ON"));
        assert!(!p.contains("XACT_ABORT"), "{p}");
        assert!(!p.contains("STATISTICS"), "{p}");
    }

    #[test]
    fn plan_sql() {
        assert_eq!(plan_on_sql(PlanMode::None), None);
        assert_eq!(plan_on_sql(PlanMode::Estimated), Some("SET SHOWPLAN_XML ON"));
        assert_eq!(plan_off_sql(PlanMode::Actual), "SET STATISTICS XML OFF");
    }

    #[tokio::test]
    async fn cancel_signal_arm_clears_stale_permit() {
        let s = CancelSignal::default();
        s.trigger();
        assert!(s.is_requested());
        s.arm();
        assert!(!s.is_requested());
        assert!(tokio::time::timeout(Duration::from_millis(20), s.wait()).await.is_err(), "stale permit must be consumed by arm()");
        s.trigger();
        tokio::time::timeout(Duration::from_millis(20), s.wait()).await.expect("wakes after trigger");
    }
}
