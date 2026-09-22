//! Driver abstraction: everything the app needs from a database, engine-agnostic.
//!
//! The UI never sees driver-specific types. A `Connection` yields Arrow `RecordBatch`es and
//! `ServerMessage`s through a `QueryStream`; catalog reads return `cobalt_core` types.

pub mod error;
#[cfg(feature = "mssql")]
pub mod mssql;

pub use error::DriverError;

use arrow::array::RecordBatch;
use async_trait::async_trait;
use cobalt_core::*;
use futures_util::stream::BoxStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub type Result<T> = std::result::Result<T, DriverError>;

/// Items produced while a batch executes. Order for one batch:
/// `(ResultSetStart, Rows*, ResultSetEnd | RowsAffected)* , Message* , Done`.
/// Messages (PRINT, RAISERROR ≤ 10, rows-affected DONE tokens) can appear anywhere between.
#[derive(Debug, Clone)]
pub enum StreamItem {
    ResultSetStart { columns: Vec<ColumnInfo> },
    Rows(RecordBatch),
    ResultSetEnd { rows: u64 },
    /// DML/DDL statement completed without a result set.
    RowsAffected(u64),
    /// PRINT / RAISERROR ≤ 10 / STATISTICS output (`is_error: false`) and server errors
    /// (`is_error: true`), in the order the server sent them.
    Message(ServerMessage),
    /// The batch finished. `error` is set when the batch was aborted by a server error
    /// (severity > 10); prior items are still valid. `error` repeats the *first* error that was
    /// already delivered as a `Message` (so the run can be marked failed) — do not append it to
    /// the messages pane a second time. A command timeout also ends here with an error and
    /// `cancelled: false`; a user cancel ends with `cancelled: true` and no error.
    Done { error: Option<ServerMessage>, cancelled: bool },
}

/// A stream of [`StreamItem`] that borrows the connection for the duration of the batch.
pub type QueryStream<'a> = BoxStream<'a, StreamItem>;

/// Signals cancellation of the in-flight batch on a connection. Cloneable and usable from another task.
#[derive(Clone)]
pub struct CancelHandle {
    flag: Arc<AtomicBool>,
    signal: Arc<dyn Fn() + Send + Sync>,
}

impl CancelHandle {
    pub fn new(signal: impl Fn() + Send + Sync + 'static) -> Self {
        Self { flag: Arc::new(AtomicBool::new(false)), signal: Arc::new(signal) }
    }
    pub fn noop() -> Self {
        Self::new(|| {})
    }
    pub fn cancel(&self) {
        if !self.flag.swap(true, Ordering::SeqCst) {
            (self.signal)();
        }
    }
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
    pub fn reset(&self) {
        self.flag.store(false, Ordering::SeqCst);
    }
}

impl std::fmt::Debug for CancelHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CancelHandle(cancelled={})", self.is_cancelled())
    }
}

/// Object-scripting requests.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScriptKind {
    Create,
    Alter,
    Drop,
    Select,
    Execute,
}

#[async_trait]
pub trait Driver: Send + Sync {
    /// Human name, e.g. "SQL Server".
    fn name(&self) -> &'static str;

    async fn connect(
        &self,
        profile: &ConnectionProfile,
        creds: &ResolvedCredentials,
        role: ConnectionRole,
    ) -> Result<Box<dyn Connection>>;
}

/// One live session. All methods take `&mut self`; a session runs one thing at a time
/// (exactly SQL Server's semantics without MARS).
#[async_trait]
pub trait Connection: Send {
    fn engine(&self) -> &EngineInfo;
    fn spid(&self) -> Option<i32>;
    fn current_database(&self) -> &str;
    fn profile_id(&self) -> ProfileId;
    fn role(&self) -> ConnectionRole;

    /// Execute one batch (no `GO` inside). Applies `opts.plan`, `opts.isolation`, `opts.nocount`,
    /// etc. as SET statements before the text when they differ from the session's current state.
    /// The returned stream borrows the connection until it is dropped or yields `Done`.
    async fn execute<'a>(&'a mut self, sql: &str, opts: &ExecOptions) -> Result<QueryStream<'a>>;

    /// Obtain before calling `execute` if you may need to cancel from another task.
    fn cancel_handle(&self) -> CancelHandle;

    async fn change_database(&mut self, database: &str) -> Result<()>;
    async fn ping(&mut self) -> Result<()>;
    async fn close(&mut self) -> Result<()>;

    // ----- catalog -----
    async fn list_databases(&mut self) -> Result<Vec<DatabaseInfo>>;
    async fn list_schemas(&mut self, database: &str) -> Result<Vec<String>>;
    /// Tables, views, procedures, functions, synonyms, sequences, table types of one database.
    async fn list_objects(&mut self, database: &str) -> Result<Vec<ObjectRef>>;
    async fn list_columns(&mut self, obj: &ObjectRef) -> Result<Vec<ColumnInfo>>;
    async fn list_parameters(&mut self, obj: &ObjectRef) -> Result<Vec<ParameterInfo>>;
    async fn list_indexes(&mut self, obj: &ObjectRef) -> Result<Vec<IndexInfo>>;
    async fn list_keys(&mut self, obj: &ObjectRef) -> Result<Vec<KeyInfo>>;
    /// Full catalog snapshot for completion (objects + columns for tables/views).
    async fn load_catalog(&mut self, database: &str) -> Result<DatabaseCatalog>;
    /// T-SQL for "Script as ...".
    async fn script(&mut self, obj: &ObjectRef, kind: ScriptKind) -> Result<String>;
    /// Fetch the full value of one cell by primary key (for values the driver truncated). May be unsupported.
    async fn fetch_cell(&mut self, obj: &ObjectRef, key: &[(String, String)], column: &str) -> Result<Option<String>> {
        let _ = (obj, key, column);
        Err(DriverError::Unsupported("fetch_cell"))
    }
    /// Bulk-load Arrow batches into `table` (`columns` in batch order; batches are cast to each
    /// column's SQL type). Batches arrive on `rx` until it closes. `progress(rows_so_far)`
    /// returning false stops the load with `Cancelled`. Wrap the call in a transaction: rows already
    /// sent are otherwise committed by the server.
    async fn bulk_insert(
        &mut self,
        table: &str,
        columns: &[ColumnInfo],
        rx: std::sync::mpsc::Receiver<std::result::Result<RecordBatch, String>>,
        progress: &mut (dyn FnMut(u64) -> bool + Send),
    ) -> Result<u64> {
        let _ = (table, columns, rx, progress);
        Err(DriverError::Unsupported("bulk_insert"))
    }
}

/// Split a script into batches on `GO` lines (`GO`, `go`, `GO 3`), ignoring `GO` inside
/// strings/comments is *not* attempted (matches SSMS/ADS behavior: GO must be alone on a line).
pub fn split_batches(script: &str) -> Vec<Batch> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut start_line = 1u32;
    for (line_no, line) in (1u32..).zip(script.split_inclusive('\n')) {
        let trimmed = line.trim();
        let upper = trimmed.to_ascii_uppercase();
        let is_go = upper == "GO" || upper.starts_with("GO ") && upper[3..].trim().parse::<u32>().is_ok() || upper == "GO;";
        if is_go {
            let repeat = upper.strip_prefix("GO").map(|r| r.trim().trim_end_matches(';').parse::<u32>().unwrap_or(1)).unwrap_or(1).max(1);
            if !current.trim().is_empty() {
                for _ in 0..repeat {
                    out.push(Batch { sql: current.clone(), start_line });
                }
            }
            current.clear();
            start_line = line_no + 1;
        } else {
            if current.trim().is_empty() && !trimmed.is_empty() {
                start_line = line_no;
            }
            current.push_str(line);
        }
    }
    if !current.trim().is_empty() {
        out.push(Batch { sql: current, start_line });
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Batch {
    pub sql: String,
    /// 1-based line in the original script where this batch starts (for error line mapping).
    pub start_line: u32,
}

/// Column name SQL Server uses for showplan result sets.
pub const SHOWPLAN_COLUMN: &str = "Microsoft SQL Server 2005 XML Showplan";

pub fn is_showplan_result(columns: &[ColumnInfo]) -> bool {
    columns.len() == 1 && columns[0].name == SHOWPLAN_COLUMN
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn go_splitting() {
        let b = split_batches("select 1\nGO\nselect 2\ngo 2\n\nselect 3");
        assert_eq!(b.len(), 4);
        assert_eq!(b[0].sql.trim(), "select 1");
        assert_eq!(b[1].sql.trim(), "select 2");
        assert_eq!(b[2].sql.trim(), "select 2");
        assert_eq!(b[3].sql.trim(), "select 3");
        assert_eq!(b[3].start_line, 6);
    }
    #[test]
    fn no_go() {
        let b = split_batches("select 1;\nselect 2;");
        assert_eq!(b.len(), 1);
    }
}
