use serde::{Deserialize, Serialize};

/// An informational or error message from the server (PRINT, RAISERROR, errors, rows affected).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerMessage {
    pub number: i32,
    pub state: u8,
    pub class: u8,
    pub message: String,
    pub server: Option<String>,
    pub procedure: Option<String>,
    /// 1-based line within the batch as reported by the server.
    pub line: u32,
    pub is_error: bool,
}

impl ServerMessage {
    pub fn info(text: impl Into<String>) -> Self {
        Self { number: 0, state: 1, class: 0, message: text.into(), server: None, procedure: None, line: 0, is_error: false }
    }
    pub fn error(number: i32, text: impl Into<String>, line: u32) -> Self {
        Self { number, state: 1, class: 16, message: text.into(), server: None, procedure: None, line, is_error: true }
    }
    /// SSMS-style prefix: `Msg 8134, Level 16, State 1, Line 3`.
    pub fn headline(&self) -> Option<String> {
        if self.is_error || self.number > 0 {
            Some(format!(
                "Msg {}, Level {}, State {}{}, Line {}",
                self.number,
                self.class,
                self.state,
                self.procedure.as_ref().map(|p| format!(", Procedure {p}")).unwrap_or_default(),
                self.line
            ))
        } else {
            None
        }
    }
}

/// Options applied to a single execution.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExecOptions {
    /// Stop fetching (and pause the stream) after this many rows per result set. 0 = unlimited.
    pub row_cap: u64,
    /// 0 = none.
    pub timeout_secs: u32,
    pub plan: PlanMode,
    pub isolation: Option<IsolationLevel>,
    pub nocount: bool,
    pub arithabort: bool,
    pub statistics_io: bool,
    pub statistics_time: bool,
    pub xact_abort: bool,
    /// Rows per Arrow batch handed to the UI.
    pub batch_rows: usize,
    /// Tri-state session options: `None` leaves the server's default alone.
    pub ansi_nulls: Option<bool>,
    pub ansi_padding: Option<bool>,
    pub ansi_warnings: Option<bool>,
    pub quoted_identifier: Option<bool>,
    pub concat_null_yields_null: Option<bool>,
    pub numeric_roundabort: Option<bool>,
    pub implicit_transactions: Option<bool>,
    /// `SET LOCK_TIMEOUT` in milliseconds (-1 = wait forever).
    pub lock_timeout_ms: Option<i32>,
    /// `SET DEADLOCK_PRIORITY`: -10..=10 (LOW = -5, NORMAL = 0, HIGH = 5).
    pub deadlock_priority: Option<i8>,
}

impl ExecOptions {
    /// Every tri-state option as (name, value) for the SET prelude, in SQL Server's names.
    pub fn tri_state_options(&self) -> [(&'static str, Option<bool>); 7] {
        [
            ("ANSI_NULLS", self.ansi_nulls),
            ("ANSI_PADDING", self.ansi_padding),
            ("ANSI_WARNINGS", self.ansi_warnings),
            ("QUOTED_IDENTIFIER", self.quoted_identifier),
            ("CONCAT_NULL_YIELDS_NULL", self.concat_null_yields_null),
            ("NUMERIC_ROUNDABORT", self.numeric_roundabort),
            ("IMPLICIT_TRANSACTIONS", self.implicit_transactions),
        ]
    }
    /// Fill every `None` tri-state from `defaults` (the settings' session defaults).
    pub fn with_defaults(mut self, d: &ExecOptions) -> Self {
        self.ansi_nulls = self.ansi_nulls.or(d.ansi_nulls);
        self.ansi_padding = self.ansi_padding.or(d.ansi_padding);
        self.ansi_warnings = self.ansi_warnings.or(d.ansi_warnings);
        self.quoted_identifier = self.quoted_identifier.or(d.quoted_identifier);
        self.concat_null_yields_null = self.concat_null_yields_null.or(d.concat_null_yields_null);
        self.numeric_roundabort = self.numeric_roundabort.or(d.numeric_roundabort);
        self.implicit_transactions = self.implicit_transactions.or(d.implicit_transactions);
        self.lock_timeout_ms = self.lock_timeout_ms.or(d.lock_timeout_ms);
        self.deadlock_priority = self.deadlock_priority.or(d.deadlock_priority);
        self
    }
}

impl Default for ExecOptions {
    fn default() -> Self {
        Self {
            row_cap: 10_000,
            timeout_secs: 0,
            plan: PlanMode::None,
            isolation: None,
            nocount: false,
            arithabort: true,
            statistics_io: false,
            statistics_time: false,
            xact_abort: false,
            batch_rows: 4096,
            ansi_nulls: None,
            ansi_padding: None,
            ansi_warnings: None,
            quoted_identifier: None,
            concat_null_yields_null: None,
            numeric_roundabort: None,
            implicit_transactions: None,
            lock_timeout_ms: None,
            deadlock_priority: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PlanMode {
    #[default]
    None,
    /// `SET SHOWPLAN_XML ON`: no data, one plan result set per statement.
    Estimated,
    /// `SET STATISTICS XML ON`: data, then a plan result set per statement.
    Actual,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolationLevel {
    ReadUncommitted,
    ReadCommitted,
    RepeatableRead,
    Serializable,
    Snapshot,
}

impl IsolationLevel {
    pub fn sql(&self) -> &'static str {
        match self {
            IsolationLevel::ReadUncommitted => "READ UNCOMMITTED",
            IsolationLevel::ReadCommitted => "READ COMMITTED",
            IsolationLevel::RepeatableRead => "REPEATABLE READ",
            IsolationLevel::Serializable => "SERIALIZABLE",
            IsolationLevel::Snapshot => "SNAPSHOT",
        }
    }
}

/// Where a connection is used; drives pooling/priority decisions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionRole {
    /// An editor tab's session.
    Query,
    /// Object explorer / completion catalog reads.
    Metadata,
    /// Background export runs.
    Export,
}
