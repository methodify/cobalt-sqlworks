use cobalt_core::*;
use cobalt_driver::ScriptKind;
use cobalt_results::ResultSet;
use std::sync::Arc;
use std::time::Duration;

/// Run-to-export: every result set's batches go to this channel (a blocking export thread on the
/// other end) instead of the grid, which keeps only the first `preview_rows` of each set. The
/// channel is bounded, so a slow writer back-pressures the query; a dropped receiver cancels it.
/// A std channel on purpose: the writer pulls from inside its own tokio runtime (delta-rs), where
/// tokio's blocking receive would panic.
#[derive(Debug)]
pub struct RunSink {
    pub tx: std::sync::mpsc::SyncSender<SinkMsg>,
    pub preview_rows: u64,
}

pub enum SinkMsg {
    SetStart { index: usize, columns: Vec<ColumnInfo>, schema: arrow::datatypes::SchemaRef },
    Batch(arrow::array::RecordBatch),
    SetEnd,
    /// The run failed or was cancelled; anything written for the current set is discarded.
    Failed(String),
    RunEnd,
}

/// UI → session runtime.
#[derive(Debug)]
pub enum Command {
    /// Open (or replace) the connection behind an editor tab. Credentials are already resolved
    /// by the UI (password from keychain / prompt, Entra token from the auth flow).
    Connect { tab: TabId, profile: ConnectionProfile, creds: ResolvedCredentials, database: Option<String> },
    Disconnect { tab: TabId },
    /// Execute a whole script (split into GO batches by the actor).
    /// `start_line` maps batch line numbers back to the editor (1-based line of the selection start).
    Run { tab: TabId, run: RunId, script: String, opts: ExecOptions, start_line: u32, sink: Option<RunSink> },
    Cancel { tab: TabId },
    /// Import Data: bulk-load Arrow batches (arriving on `rx` from a reader thread) into `table` on
    /// the tab's connection, creating it first when `create_sql` is given. One transaction.
    Import { tab: TabId, table: String, create_sql: Option<String>, columns: Vec<ColumnInfo>, rx: std::sync::mpsc::Receiver<std::result::Result<arrow::array::RecordBatch, String>>, cancel: Arc<std::sync::atomic::AtomicBool> },
    /// Resume a paused (row-capped) result set. `None` = fetch everything.
    FetchMore { tab: TabId, rows: Option<u64> },
    ChangeDatabase { tab: TabId, database: String },
    Ping { tab: TabId },
    /// Dev/agent: make the tab's actor treat its connection as dead before the next command, so the
    /// idle-reconnect path can be exercised without waiting for a gateway to drop the session.
    SimulateLost { tab: TabId },
    /// Object explorer / completion reads on the per-profile metadata connection.
    Metadata { req: RequestId, profile: ConnectionProfile, creds: ResolvedCredentials, kind: MetadataRequest },
    CloseMetadata { profile: ProfileId },
    Shutdown,
}

#[derive(Debug, Clone)]
pub enum MetadataRequest {
    Probe,
    ListDatabases,
    ListSchemas { database: String },
    ListObjects { database: String },
    ListColumns { obj: ObjectRef },
    ListParameters { obj: ObjectRef },
    ListIndexes { obj: ObjectRef },
    ListKeys { obj: ObjectRef },
    LoadCatalog { database: String },
    Script { obj: ObjectRef, kind: ScriptKind },
}

/// Session runtime → UI.
#[derive(Debug)]
pub enum Event {
    Connected { tab: TabId, engine: EngineInfo, spid: Option<i32>, database: String },
    ConnectFailed { tab: TabId, error: String, hint: Option<String> },
    Disconnected { tab: TabId },
    /// The tab's connection had died while idle (the server, a gateway or a NAT closed it) and the
    /// actor replaced it with a new one to the same database before running the command.
    Reconnected { tab: TabId, engine: EngineInfo, spid: Option<i32>, database: String, idle: Duration },
    /// The connection had died and could not be replaced (sign-in expired, server unreachable).
    /// `rerun`: the command that found it dead was a Run, worth repeating once reconnected.
    ConnectionLost { tab: TabId, error: String, hint: Option<String>, idle: Duration, rerun: bool },
    NotConnected { tab: TabId },
    RunStarted { tab: TabId, run: RunId, batches: usize },
    BatchStarted { tab: TabId, run: RunId, batch: usize, start_line: u32 },
    /// A new result set; the UI keeps the Arc and reads rows from it directly.
    ResultSetStarted { tab: TabId, run: RunId, rs: Arc<ResultSet> },
    ResultSetDone { tab: TabId, run: RunId, index: usize, rows: u64 },
    /// Row cap reached on result set `index`; waiting for `FetchMore` or `Cancel`.
    Paused { tab: TabId, run: RunId, index: usize, rows: u64 },
    Message { tab: TabId, run: RunId, message: ServerMessage, batch: usize, batch_start_line: u32 },
    RowsAffected { tab: TabId, run: RunId, rows: u64 },
    BatchDone { tab: TabId, run: RunId, batch: usize, error: Option<ServerMessage>, elapsed: Duration },
    RunDone { tab: TabId, run: RunId, cancelled: bool, failed: bool, elapsed: Duration, total_rows: u64 },
    DatabaseChanged { tab: TabId, database: String },
    DatabaseChangeFailed { tab: TabId, error: String },
    ImportStarted { tab: TabId },
    ImportProgress { tab: TabId, rows: u64 },
    /// Rows loaded and elapsed, or the error (after ROLLBACK).
    ImportDone { tab: TabId, result: Result<(u64, Duration), String> },
    Pong { tab: TabId, ok: bool },
    Metadata { req: RequestId, profile: ProfileId, result: Result<MetadataResponse, String> },
}

#[derive(Debug)]
pub enum MetadataResponse {
    Probe(EngineInfo),
    Databases(Vec<DatabaseInfo>),
    Schemas(Vec<String>),
    Objects(Vec<ObjectRef>),
    Columns(Vec<ColumnInfo>),
    Parameters(Vec<ParameterInfo>),
    Indexes(Vec<IndexInfo>),
    Keys(Vec<KeyInfo>),
    Catalog(DatabaseCatalog),
    Script(String),
}
