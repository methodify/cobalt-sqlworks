//! SQL Server / Azure SQL / Fabric implementation over `tiberius-ng`.
//!
//! The crate is vendored with a small additive patch (`vendor/tiberius-ng/COBALT-PATCH.md`)
//! that exposes the raw TDS token stream: that is what gives us PRINT/RAISERROR messages in
//! order, DONE row counts, error placement, and a cancellation that resynchronises the
//! connection on packet boundaries.
//!
//! * [`config`]  — `ConnectionProfile` → `tiberius::Config` (server-string parsing, encryption floor)
//! * [`connect`] — TCP (+ SQL Browser) → TLS → login → engine probe
//! * [`exec`]    — one batch as a `StreamItem` stream (session SET state, plans, cancel, timeout)
//! * [`convert`] — `COLMETADATA`/rows → `ColumnInfo` / Arrow `RecordBatch`
//! * [`catalog`] — `sys.*` reads, [`script`] — "Script as …"

mod bulk;
mod catalog;
mod config;
mod connect;
mod convert;
mod exec;
mod script;

pub use config::{parse_server, ServerSpec};
pub use exec::SessionState;

use crate::*;
use async_trait::async_trait;
use cobalt_core::*;
use std::sync::Arc;
use tiberius::error::TokenError;
use tiberius::{ColumnData, ReceivedToken, TokenEnvChange, TokenInfo, TokenRow};
use tokio::net::TcpStream;
use tokio_util::compat::Compat;

pub(crate) type TdsClient = tiberius::Client<Compat<TcpStream>>;

/// How long we wait for the server to acknowledge an attention (cancel) before giving the
/// connection up.
pub(crate) const CANCEL_ACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

pub struct MssqlDriver;

impl MssqlDriver {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MssqlDriver {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Driver for MssqlDriver {
    fn name(&self) -> &'static str {
        "SQL Server"
    }

    async fn connect(&self, profile: &ConnectionProfile, creds: &ResolvedCredentials, role: ConnectionRole) -> Result<Box<dyn Connection>> {
        let conn = connect::connect(profile, creds, role).await?;
        Ok(Box::new(conn))
    }
}

/// One live TDS session.
pub struct MssqlConnection {
    /// `None` after `close()`.
    client: Option<TdsClient>,
    engine: EngineInfo,
    spid: Option<i32>,
    database: String,
    profile_id: ProfileId,
    role: ConnectionRole,
    /// `ConnectionOptions::command_timeout_secs`; used when `ExecOptions::timeout_secs` is 0.
    default_timeout_secs: u32,
    session: SessionState,
    cancel: Arc<exec::CancelSignal>,
    /// Set after a transport/protocol failure: every further call fails with `Disconnected`.
    broken: Option<String>,
    /// A previous `execute` stream was dropped before `Done`; its response may still be in flight.
    in_flight: bool,
    /// `SET SHOWPLAN_XML/STATISTICS XML ON` is active and must be switched off before the next batch.
    plan_mode_on: Option<PlanMode>,
}

impl MssqlConnection {
    pub(crate) fn new(client: TdsClient, engine: EngineInfo, spid: Option<i32>, database: String, profile: &ConnectionProfile, role: ConnectionRole) -> Self {
        Self {
            client: Some(client),
            engine,
            spid,
            database,
            profile_id: profile.id,
            role,
            default_timeout_secs: profile.options.command_timeout_secs,
            session: SessionState::default(),
            cancel: Arc::new(exec::CancelSignal::default()),
            broken: None,
            in_flight: false,
            plan_mode_on: None,
        }
    }

    pub(crate) fn ensure_usable(&self) -> Result<()> {
        if let Some(why) = &self.broken {
            return Err(DriverError::Disconnected(why.clone()));
        }
        if self.client.is_none() {
            return Err(DriverError::Disconnected("connection closed".into()));
        }
        Ok(())
    }

    /// The client. Call `ensure_usable` first; panics only if `close()` was called.
    pub(crate) fn tds(&mut self) -> &mut TdsClient {
        self.client.as_mut().expect("MssqlConnection used after close()")
    }

    /// Make the session quiescent before a new request: abort a response left in flight by a
    /// dropped stream and switch a lingering plan mode off.
    pub(crate) async fn settle(&mut self) -> Result<()> {
        self.ensure_usable()?;
        if self.in_flight {
            self.in_flight = false;
            if !self.tds().is_response_complete() {
                tracing::debug!("previous batch was abandoned; sending attention");
                self.cancel_in_flight().await?;
            }
        }
        if let Some(mode) = self.plan_mode_on.take() {
            self.run_silent(exec::plan_off_sql(mode)).await?;
        }
        Ok(())
    }

    /// Send an attention and wait for its acknowledgement (packet-level resync in tiberius).
    pub(crate) async fn cancel_in_flight(&mut self) -> Result<()> {
        match tokio::time::timeout(CANCEL_ACK_TIMEOUT, self.tds().cancel_query()).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => {
                let err = self.fail(e);
                self.broken.get_or_insert_with(|| err.to_string());
                Err(err)
            }
            Err(_) => {
                let why = "timed out waiting for the server to acknowledge the cancellation".to_string();
                self.broken = Some(why.clone());
                Err(DriverError::Disconnected(why))
            }
        }
    }

    /// Run a batch and return the rows of its first result set. A server error becomes
    /// `Err(DriverError::Server)`; info messages are discarded.
    pub(crate) async fn query_rows(&mut self, sql: &str) -> Result<Vec<TokenRow<'static>>> {
        self.run_batch(sql, true).await
    }

    /// Run a batch, discarding rows.
    pub(crate) async fn run_silent(&mut self, sql: &str) -> Result<()> {
        self.run_batch(sql, false).await.map(|_| ())
    }

    async fn run_batch(&mut self, sql: &str, keep_rows: bool) -> Result<Vec<TokenRow<'static>>> {
        self.ensure_usable()?;
        if let Err(e) = self.tds().simple_query_send(sql).await {
            return Err(self.fail(e));
        }
        let mut rows = Vec::new();
        let mut first_error: Option<ServerMessage> = None;
        let mut result_sets = 0usize;
        loop {
            match self.tds().next_token().await {
                Ok(None) => break,
                Ok(Some(token)) => match token {
                    ReceivedToken::NewResultset(_) => result_sets += 1,
                    ReceivedToken::Row(row) if keep_rows && result_sets == 1 => rows.push(row),
                    ReceivedToken::Error(e) => {
                        let m = server_message_from_error(&e);
                        if m.class >= 20 {
                            self.broken.get_or_insert_with(|| format!("the server ended the session: {}", m.message));
                        }
                        if first_error.is_none() {
                            first_error = Some(m);
                        }
                    }
                    ReceivedToken::EnvChange(TokenEnvChange::Database(new, _)) => self.database = new,
                    _ => {}
                },
                Err(e) => return Err(self.fail(e)),
            }
        }
        match first_error {
            Some(m) => Err(DriverError::Server(m)),
            None => Ok(rows),
        }
    }

    /// Map a tiberius error raised while executing; transport failures poison the connection.
    pub(crate) fn fail(&mut self, e: tiberius::error::Error) -> DriverError {
        let mapped = map_error(e, ErrorPhase::Execute);
        if let DriverError::Disconnected(why) = &mapped {
            if self.broken.is_none() {
                tracing::warn!(spid = ?self.spid, "connection marked broken: {why}");
                self.broken = Some(why.clone());
            }
        }
        mapped
    }
}

#[async_trait]
impl Connection for MssqlConnection {
    fn engine(&self) -> &EngineInfo {
        &self.engine
    }
    fn spid(&self) -> Option<i32> {
        self.spid
    }
    fn current_database(&self) -> &str {
        &self.database
    }
    fn profile_id(&self) -> ProfileId {
        self.profile_id
    }
    fn role(&self) -> ConnectionRole {
        self.role
    }

    async fn execute<'a>(&'a mut self, sql: &str, opts: &ExecOptions) -> Result<QueryStream<'a>> {
        exec::execute(self, sql, opts).await
    }

    async fn bulk_insert(
        &mut self,
        table: &str,
        columns: &[ColumnInfo],
        rx: std::sync::mpsc::Receiver<std::result::Result<RecordBatch, String>>,
        progress: &mut (dyn FnMut(u64) -> bool + Send),
    ) -> Result<u64> {
        bulk::bulk_insert(self, table, columns, rx, progress).await
    }

    fn cancel_handle(&self) -> CancelHandle {
        let signal = self.cancel.clone();
        CancelHandle::new(move || signal.trigger())
    }

    async fn change_database(&mut self, database: &str) -> Result<()> {
        self.settle().await?;
        if !self.engine.capabilities.multiple_databases {
            return Err(DriverError::Unsupported(
                "USE is not supported by this engine (Azure SQL Database / Fabric): open a new connection to the target database instead",
            ));
        }
        self.run_silent(&format!("USE {}", catalog::br(database))).await?;
        // The ENVCHANGE token already updated `database`; keep the caller's spelling otherwise.
        if !self.database.eq_ignore_ascii_case(database) {
            self.database = database.to_string();
        }
        Ok(())
    }

    async fn ping(&mut self) -> Result<()> {
        self.settle().await?;
        self.run_silent("SELECT 1").await
    }

    fn is_usable(&self) -> bool {
        self.broken.is_none() && self.client.is_some()
    }

    async fn close(&mut self) -> Result<()> {
        if let Some(client) = self.client.take() {
            if self.broken.is_none() {
                if let Err(e) = tokio::time::timeout(std::time::Duration::from_secs(5), client.close()).await {
                    tracing::debug!("close: {e}");
                }
            }
        }
        Ok(())
    }

    async fn list_databases(&mut self) -> Result<Vec<DatabaseInfo>> {
        self.settle().await?;
        catalog::list_databases(self).await
    }
    async fn list_schemas(&mut self, database: &str) -> Result<Vec<String>> {
        self.settle().await?;
        catalog::list_schemas(self, database).await
    }
    async fn list_objects(&mut self, database: &str) -> Result<Vec<ObjectRef>> {
        self.settle().await?;
        catalog::list_objects(self, database).await
    }
    async fn list_columns(&mut self, obj: &ObjectRef) -> Result<Vec<ColumnInfo>> {
        self.settle().await?;
        catalog::list_columns(self, obj).await
    }
    async fn list_parameters(&mut self, obj: &ObjectRef) -> Result<Vec<ParameterInfo>> {
        self.settle().await?;
        catalog::list_parameters(self, obj).await
    }
    async fn list_indexes(&mut self, obj: &ObjectRef) -> Result<Vec<IndexInfo>> {
        self.settle().await?;
        catalog::list_indexes(self, obj).await
    }
    async fn list_keys(&mut self, obj: &ObjectRef) -> Result<Vec<KeyInfo>> {
        self.settle().await?;
        catalog::list_keys(self, obj).await
    }
    async fn load_catalog(&mut self, database: &str) -> Result<DatabaseCatalog> {
        self.settle().await?;
        catalog::load_catalog(self, database).await
    }
    async fn script(&mut self, obj: &ObjectRef, kind: ScriptKind) -> Result<String> {
        self.settle().await?;
        script::script(self, obj, kind).await
    }
}

// ---------------------------------------------------------------------------------------------
// Error mapping
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ErrorPhase {
    Connect,
    Execute,
}

/// Server error numbers that mean "the login itself was rejected".
const LOGIN_ERRORS: &[u32] = &[18456, 18452, 18470, 18486, 18487, 18488, 4060, 40532, 40613];

pub(crate) fn map_error(e: tiberius::error::Error, phase: ErrorPhase) -> DriverError {
    use tiberius::error::Error as E;
    match e {
        E::Server(t) => {
            let m = server_message_from_error(&t);
            if LOGIN_ERRORS.contains(&t.code()) {
                DriverError::Login(m.message)
            } else if t.code() == 40615 {
                DriverError::Connect(m.message)
            } else {
                DriverError::Server(m)
            }
        }
        E::Io { kind, message } => match phase {
            ErrorPhase::Connect => DriverError::Connect(format!("{message} ({kind:?})")),
            ErrorPhase::Execute => DriverError::Disconnected(message),
        },
        E::Tls(s) => DriverError::Tls(s),
        E::Routing { host, port } => DriverError::Connect(format!("the server redirected the connection to {host}:{port}")),
        E::Protocol(s) | E::Encoding(s) | E::Conversion(s) => match phase {
            ErrorPhase::Connect => DriverError::Connect(s.into_owned()),
            // The token stream is desynchronised after any of these: the session is unusable.
            ErrorPhase::Execute => DriverError::Disconnected(format!("protocol error: {s}")),
        },
        other => DriverError::Other(other.to_string()),
    }
}

pub(crate) fn server_message_from_error(e: &TokenError) -> ServerMessage {
    ServerMessage {
        number: e.code() as i32,
        state: e.state(),
        class: e.class(),
        message: e.message().to_string(),
        server: Some(e.server().to_string()).filter(|s| !s.is_empty()),
        procedure: Some(e.procedure().to_string()).filter(|s| !s.is_empty()),
        line: e.line(),
        is_error: true,
    }
}

pub(crate) fn server_message_from_info(i: &TokenInfo) -> ServerMessage {
    ServerMessage {
        number: i.number() as i32,
        state: i.state(),
        class: i.class(),
        message: i.message().to_string(),
        server: Some(i.server().to_string()).filter(|s| !s.is_empty()),
        procedure: Some(i.procedure().to_string()).filter(|s| !s.is_empty()),
        line: i.line(),
        is_error: false,
    }
}

// ---------------------------------------------------------------------------------------------
// Cell helpers for catalog / probe rows
// ---------------------------------------------------------------------------------------------

pub(crate) fn cell_string(row: &TokenRow<'static>, i: usize) -> Option<String> {
    match row.get(i)? {
        ColumnData::String(s) => s.as_deref().map(str::to_string),
        other => convert::display_value(other),
    }
}

pub(crate) fn cell_i64(row: &TokenRow<'static>, i: usize) -> Option<i64> {
    Some(match row.get(i)? {
        ColumnData::U8(x) => (*x)? as i64,
        ColumnData::I16(x) => (*x)? as i64,
        ColumnData::I32(x) => (*x)? as i64,
        ColumnData::I64(x) => (*x)?,
        ColumnData::Bit(x) => (*x)? as i64,
        ColumnData::Numeric(x) => (*x)?.int_part() as i64,
        ColumnData::F64(x) => (*x)? as i64,
        _ => return None,
    })
}

pub(crate) fn cell_bool(row: &TokenRow<'static>, i: usize) -> bool {
    cell_i64(row, i).unwrap_or(0) != 0
}
