//! The SQLite database behind the connection library, history, tab snapshots, and caches.

pub mod catalog;
pub mod fabric;
pub mod groups;
pub mod history;
pub mod kv;
pub mod library;
pub mod profiles;
pub mod tabs;

use crate::{Result, StoreError};
use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::Connection;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

/// Schema version this build expects (= `MIGRATIONS.len()`).
pub const SCHEMA_VERSION: i64 = 3;

/// Ordered migrations; `MIGRATIONS[n]` brings the schema to version `n + 1`. Each runs in
/// its own transaction and is recorded in `schema_version`. Never edit a shipped entry;
/// append a new one.
pub const MIGRATIONS: &[&str] = &[SCHEMA_V1, SCHEMA_V2, SCHEMA_V3];

const SCHEMA_V1: &str = r#"
CREATE TABLE groups (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    color       TEXT NOT NULL,
    parent_id   TEXT NULL REFERENCES groups(id) ON DELETE SET NULL,
    description TEXT NULL,
    sort_order  INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX groups_parent ON groups(parent_id, sort_order);

CREATE TABLE profiles (
    id              TEXT PRIMARY KEY,
    group_id        TEXT NULL REFERENCES groups(id) ON DELETE SET NULL,
    name            TEXT NULL,
    server          TEXT NOT NULL,
    port            INTEGER NULL,
    database        TEXT NULL,
    auth_json       TEXT NOT NULL,
    options_json    TEXT NOT NULL,
    color           TEXT NULL,
    read_only_guard INTEGER NOT NULL DEFAULT 0,
    created_at      TEXT NOT NULL,
    last_used       TEXT NULL,
    sort_order      INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX profiles_group ON profiles(group_id, sort_order);

CREATE TABLE history (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    profile_id  TEXT NULL,
    server      TEXT NOT NULL,
    database    TEXT NULL,
    sql         TEXT NOT NULL,
    started_at  TEXT NOT NULL,
    ended_at    TEXT NULL,
    duration_ms INTEGER NULL,
    rows        INTEGER NULL,
    status      TEXT NOT NULL,
    error       TEXT NULL,
    tab_id      TEXT NULL,
    starred     INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX history_started ON history(started_at DESC);
CREATE INDEX history_profile ON history(profile_id, started_at DESC);
CREATE INDEX history_starred ON history(starred) WHERE starred = 1;

CREATE VIRTUAL TABLE history_fts USING fts5(sql, content='history', content_rowid='id');
CREATE TRIGGER history_ai AFTER INSERT ON history BEGIN
    INSERT INTO history_fts(rowid, sql) VALUES (new.id, new.sql);
END;
CREATE TRIGGER history_ad AFTER DELETE ON history BEGIN
    INSERT INTO history_fts(history_fts, rowid, sql) VALUES ('delete', old.id, old.sql);
END;
CREATE TRIGGER history_au AFTER UPDATE OF sql ON history BEGIN
    INSERT INTO history_fts(history_fts, rowid, sql) VALUES ('delete', old.id, old.sql);
    INSERT INTO history_fts(rowid, sql) VALUES (new.id, new.sql);
END;

CREATE TABLE tab_snapshots (
    tab_id      TEXT PRIMARY KEY,
    profile_id  TEXT NULL,
    database    TEXT NULL,
    title       TEXT NOT NULL,
    text        TEXT NOT NULL,
    cursor      INTEGER NOT NULL DEFAULT 0,
    file_path   TEXT NULL,
    updated_at  TEXT NOT NULL,
    closed_at   TEXT NULL
);
CREATE INDEX tab_snapshots_closed ON tab_snapshots(closed_at);

CREATE TABLE catalog_cache (
    profile_id   TEXT NOT NULL,
    database     TEXT NOT NULL,
    json         TEXT NOT NULL,
    refreshed_at TEXT NOT NULL,
    PRIMARY KEY (profile_id, database)
);

CREATE TABLE kv (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE recent_connections (
    profile_id TEXT PRIMARY KEY REFERENCES profiles(id) ON DELETE CASCADE,
    used_at    TEXT NOT NULL
);
"#;

/// v3: recently opened Fabric items.
const SCHEMA_V3: &str = r#"
CREATE TABLE fabric_recent (
    item_id        TEXT PRIMARY KEY,
    workspace_id   TEXT NOT NULL,
    item_kind      TEXT NOT NULL,
    display_name   TEXT NOT NULL,
    workspace_name TEXT NOT NULL,
    opened_at      TEXT NOT NULL
);
"#;

/// v2: Fabric explorer pins and REST cache.
const SCHEMA_V2: &str = r#"
CREATE TABLE fabric_pins (
    item_id        TEXT PRIMARY KEY,
    workspace_id   TEXT NOT NULL,
    item_kind      TEXT NOT NULL,
    display_name   TEXT NOT NULL,
    workspace_name TEXT NOT NULL,
    position       INTEGER NOT NULL,
    pinned_at      TEXT NOT NULL
);

CREATE TABLE fabric_cache (
    key        TEXT PRIMARY KEY,
    value      TEXT NOT NULL,
    fetched_at TEXT NOT NULL
);
"#;

/// Handle to the local database. Cheap to share via `Arc`; every method takes `&self`.
pub struct Store {
    conn: Mutex<Connection>,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Store")
    }
}

impl Store {
    /// Open (creating if needed) the database at `path`, in WAL mode with foreign keys on,
    /// and apply any pending migrations.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    /// A private in-memory database; for tests and "don't persist anything" modes.
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> Result<Self> {
        conn.busy_timeout(Duration::from_secs(5))?;
        // WAL is not available for in-memory databases; the pragma reports "memory" then.
        let _mode: String =
            conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "temp_store", "MEMORY")?;
        migrate(&mut conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Current schema version recorded in the database.
    pub fn schema_version(&self) -> Result<i64> {
        let conn = self.lock()?;
        Ok(current_version(&conn)?)
    }

    /// Flush the WAL into the main file and reclaim space. Call rarely (on exit is fine).
    pub fn vacuum(&self) -> Result<()> {
        let conn = self.lock()?;
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM;")?;
        Ok(())
    }

    /// Run `f` inside a transaction with exclusive access to the connection.
    pub(crate) fn tx<T>(
        &self,
        f: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<T>,
    ) -> Result<T> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    }

    pub(crate) fn lock(&self) -> Result<MutexGuard<'_, Connection>> {
        self.conn.lock().map_err(|_| StoreError::Poisoned)
    }
}

fn current_version(conn: &Connection) -> rusqlite::Result<i64> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_version (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL)",
    )?;
    conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_version",
        [],
        |r| r.get(0),
    )
}

fn migrate(conn: &mut Connection) -> Result<()> {
    let current = current_version(conn)?;
    if current > MIGRATIONS.len() as i64 {
        return Err(StoreError::Invalid(format!(
            "database schema is version {current}, newer than this build understands ({SCHEMA_VERSION})"
        )));
    }
    for (idx, sql) in MIGRATIONS.iter().enumerate().skip(current as usize) {
        let version = idx as i64 + 1;
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.execute(
            "INSERT INTO schema_version (version, applied_at) VALUES (?1, ?2)",
            rusqlite::params![version, fmt_ts(&Utc::now())],
        )?;
        tx.commit()?;
        tracing::info!(version, "applied store migration");
    }
    Ok(())
}

// ---- shared row/value helpers -------------------------------------------------------------

/// Timestamps are stored as RFC 3339 UTC text with microseconds (`2026-09-16T10:11:12.123456Z`),
/// which sorts and compares lexicographically.
pub(crate) fn fmt_ts(t: &DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Micros, true)
}

pub(crate) fn parse_ts(s: &str) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })
}

pub(crate) fn opt_ts(s: Option<String>) -> rusqlite::Result<Option<DateTime<Utc>>> {
    s.as_deref().map(parse_ts).transpose()
}

/// Wrap any conversion error as a rusqlite error so it can surface from inside a row mapper.
pub(crate) fn conv<E: std::error::Error + Send + Sync + 'static>(e: E) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
}

#[derive(Debug)]
struct BadId(String);
impl std::fmt::Display for BadId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "bad uuid in store: {:?}", self.0)
    }
}
impl std::error::Error for BadId {}

pub(crate) fn parse_uuid(s: &str) -> rusqlite::Result<uuid::Uuid> {
    uuid::Uuid::parse_str(s).map_err(|_| conv(BadId(s.to_owned())))
}

pub(crate) fn opt_uuid(s: Option<String>) -> rusqlite::Result<Option<uuid::Uuid>> {
    s.as_deref().map(parse_uuid).transpose()
}
