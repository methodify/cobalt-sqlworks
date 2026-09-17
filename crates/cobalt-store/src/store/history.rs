//! Query history with FTS5 full-text search.

use super::{fmt_ts, opt_ts, opt_uuid, parse_ts, Store};
use crate::{Result, StoreError};
use chrono::{DateTime, Duration, Utc};
use cobalt_core::{ProfileId, TabId};
use rusqlite::types::Value;
use rusqlite::{params, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryStatus {
    /// Recorded at start; replaced by `finish_history`. A crash leaves entries in this state.
    Running,
    Success,
    Error,
    Cancelled,
}

impl HistoryStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            HistoryStatus::Running => "running",
            HistoryStatus::Success => "success",
            HistoryStatus::Error => "error",
            HistoryStatus::Cancelled => "cancelled",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "running" => HistoryStatus::Running,
            "success" => HistoryStatus::Success,
            "error" => HistoryStatus::Error,
            "cancelled" => HistoryStatus::Cancelled,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub id: i64,
    pub profile_id: Option<ProfileId>,
    pub server: String,
    pub database: Option<String>,
    pub sql: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub duration_ms: Option<u64>,
    /// Rows returned or affected.
    pub rows: Option<u64>,
    pub status: HistoryStatus,
    pub error: Option<String>,
    pub tab_id: Option<TabId>,
    pub starred: bool,
}

/// What is known when an execution starts.
#[derive(Clone, Debug, PartialEq)]
pub struct NewHistoryEntry {
    pub profile_id: Option<ProfileId>,
    pub server: String,
    pub database: Option<String>,
    pub sql: String,
    pub started_at: DateTime<Utc>,
    pub tab_id: Option<TabId>,
}

impl NewHistoryEntry {
    pub fn new(server: impl Into<String>, sql: impl Into<String>) -> Self {
        Self { profile_id: None, server: server.into(), database: None, sql: sql.into(), started_at: Utc::now(), tab_id: None }
    }
}

/// Filters for [`Store::search_history`]. All are ANDed; `Default` = everything, newest first.
#[derive(Clone, Debug, PartialEq)]
pub struct HistoryQuery {
    /// Full-text query (FTS5 syntax: bare words, `"phrases"`, `prefix*`, AND/OR/NOT). If FTS5
    /// rejects it, falls back to a case-insensitive substring match.
    pub text: Option<String>,
    pub profile_id: Option<ProfileId>,
    pub server: Option<String>,
    pub database: Option<String>,
    pub status: Option<HistoryStatus>,
    pub starred_only: bool,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub limit: usize,
    pub offset: usize,
}

impl Default for HistoryQuery {
    fn default() -> Self {
        Self {
            text: None,
            profile_id: None,
            server: None,
            database: None,
            status: None,
            starred_only: false,
            since: None,
            until: None,
            limit: 200,
            offset: 0,
        }
    }
}

impl HistoryQuery {
    pub fn text(text: impl Into<String>) -> Self {
        Self { text: Some(text.into()), ..Default::default() }
    }
}

const COLS: &str = "id, profile_id, server, database, sql, started_at, ended_at, duration_ms, rows, status, error, tab_id, starred";

fn row_to_entry(row: &Row<'_>) -> rusqlite::Result<HistoryEntry> {
    let status_s: String = row.get(9)?;
    Ok(HistoryEntry {
        id: row.get(0)?,
        profile_id: opt_uuid(row.get(1)?)?.map(ProfileId),
        server: row.get(2)?,
        database: row.get(3)?,
        sql: row.get(4)?,
        started_at: parse_ts(&row.get::<_, String>(5)?)?,
        ended_at: opt_ts(row.get(6)?)?,
        duration_ms: row.get::<_, Option<i64>>(7)?.map(|v| v.max(0) as u64),
        rows: row.get::<_, Option<i64>>(8)?.map(|v| v.max(0) as u64),
        status: HistoryStatus::parse(&status_s).unwrap_or(HistoryStatus::Error),
        error: row.get(10)?,
        tab_id: opt_uuid(row.get(11)?)?.map(TabId),
        starred: row.get::<_, i64>(12)? != 0,
    })
}

impl Store {
    /// Record the start of an execution. Returns the row id to pass to `finish_history`.
    pub fn add_history(&self, e: &NewHistoryEntry) -> Result<i64> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO history (profile_id, server, database, sql, started_at, status, tab_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                e.profile_id.map(|p| p.to_string()),
                e.server,
                e.database,
                e.sql,
                fmt_ts(&e.started_at),
                HistoryStatus::Running.as_str(),
                e.tab_id.map(|t| t.to_string()),
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Record the outcome of an execution.
    pub fn finish_history(
        &self,
        id: i64,
        ended_at: DateTime<Utc>,
        duration_ms: Option<u64>,
        rows: Option<u64>,
        status: HistoryStatus,
        error: Option<&str>,
    ) -> Result<()> {
        let conn = self.lock()?;
        let n = conn.execute(
            "UPDATE history SET ended_at = ?2, duration_ms = ?3, rows = ?4, status = ?5, error = ?6 WHERE id = ?1",
            params![
                id,
                fmt_ts(&ended_at),
                duration_ms.map(|v| v as i64),
                rows.map(|v| v as i64),
                status.as_str(),
                error
            ],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("history entry {id}")));
        }
        Ok(())
    }

    pub fn get_history(&self, id: i64) -> Result<Option<HistoryEntry>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare_cached(&format!("SELECT {COLS} FROM history WHERE id = ?1"))?;
        Ok(stmt.query_row(params![id], row_to_entry).optional()?)
    }

    /// Search, newest first. See [`HistoryQuery`].
    pub fn search_history(&self, q: &HistoryQuery) -> Result<Vec<HistoryEntry>> {
        let mut clauses: Vec<String> = Vec::new();
        let mut args: Vec<Value> = Vec::new();
        let mut push = |clause: &str, v: Value| {
            args.push(v);
            clauses.push(clause.replace("{}", &format!("?{}", args.len())));
        };
        if let Some(p) = q.profile_id {
            push("profile_id = {}", Value::Text(p.to_string()));
        }
        if let Some(s) = q.server.as_deref().filter(|s| !s.is_empty()) {
            push("server = {} COLLATE NOCASE", Value::Text(s.to_owned()));
        }
        if let Some(d) = q.database.as_deref().filter(|s| !s.is_empty()) {
            push("database = {} COLLATE NOCASE", Value::Text(d.to_owned()));
        }
        if let Some(st) = q.status {
            push("status = {}", Value::Text(st.as_str().to_owned()));
        }
        if q.starred_only {
            push("starred = {}", Value::Integer(1));
        }
        if let Some(t) = &q.since {
            push("started_at >= {}", Value::Text(fmt_ts(t)));
        }
        if let Some(t) = &q.until {
            push("started_at <= {}", Value::Text(fmt_ts(t)));
        }

        let text = q.text.as_deref().map(str::trim).filter(|t| !t.is_empty());
        let conn = self.lock()?;

        let run = |extra: Option<(&str, Value)>| -> Result<Vec<HistoryEntry>> {
            let mut clauses = clauses.clone();
            let mut args = args.clone();
            if let Some((clause, v)) = extra {
                args.push(v);
                clauses.push(clause.replace("{}", &format!("?{}", args.len())));
            }
            let where_sql = if clauses.is_empty() { String::new() } else { format!("WHERE {}", clauses.join(" AND ")) };
            args.push(Value::Integer(q.limit as i64));
            args.push(Value::Integer(q.offset as i64));
            let sql = format!(
                "SELECT {COLS} FROM history {where_sql} ORDER BY started_at DESC, id DESC LIMIT ?{} OFFSET ?{}",
                args.len() - 1,
                args.len()
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(args.iter()), row_to_entry)?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        };

        let Some(text) = text else { return run(None) };

        match run(Some(("id IN (SELECT rowid FROM history_fts WHERE history_fts MATCH {})", Value::Text(text.to_owned())))) {
            Ok(v) => Ok(v),
            Err(StoreError::Sqlite(rusqlite::Error::SqliteFailure(_, msg))) => {
                tracing::debug!(query = text, error = ?msg, "FTS rejected query; falling back to LIKE");
                let pattern = format!("%{}%", like_escape(text));
                run(Some(("sql LIKE {} ESCAPE '\\'", Value::Text(pattern))))
            }
            Err(e) => Err(e),
        }
    }

    pub fn set_starred(&self, id: i64, starred: bool) -> Result<()> {
        let conn = self.lock()?;
        let n = conn.execute("UPDATE history SET starred = ?2 WHERE id = ?1", params![id, starred as i64])?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("history entry {id}")));
        }
        Ok(())
    }

    pub fn delete_history(&self, id: i64) -> Result<bool> {
        let conn = self.lock()?;
        Ok(conn.execute("DELETE FROM history WHERE id = ?1", params![id])? > 0)
    }

    /// Delete everything (or everything unstarred). Returns rows removed.
    pub fn clear_history(&self, keep_starred: bool) -> Result<usize> {
        let conn = self.lock()?;
        let n = if keep_starred {
            conn.execute("DELETE FROM history WHERE starred = 0", [])?
        } else {
            conn.execute("DELETE FROM history", [])?
        };
        Ok(n)
    }

    /// Apply retention: drop unstarred entries older than `retention_days` (0 = no age limit)
    /// and, if more than `max_entries` remain (0 = no cap), drop the oldest unstarred ones.
    /// Starred entries are never removed. Returns rows removed.
    pub fn prune_history(&self, retention_days: u32, max_entries: u32) -> Result<usize> {
        self.tx(|tx| {
            let mut removed = 0;
            if retention_days > 0 {
                let cutoff = Utc::now() - Duration::days(i64::from(retention_days));
                removed += tx.execute("DELETE FROM history WHERE starred = 0 AND started_at < ?1", params![fmt_ts(&cutoff)])?;
            }
            if max_entries > 0 {
                removed += tx.execute(
                    "DELETE FROM history WHERE starred = 0 AND id NOT IN
                        (SELECT id FROM history ORDER BY started_at DESC, id DESC LIMIT ?1)",
                    params![i64::from(max_entries)],
                )?;
            }
            Ok(removed)
        })
    }

    pub fn history_count(&self) -> Result<usize> {
        let conn = self.lock()?;
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM history", [], |r| r.get(0))?;
        Ok(n as usize)
    }

    /// Rebuild the FTS index from the `history` table (after a restore or if search looks off).
    pub fn rebuild_history_index(&self) -> Result<()> {
        let conn = self.lock()?;
        conn.execute("INSERT INTO history_fts(history_fts) VALUES ('rebuild')", [])?;
        Ok(())
    }
}

fn like_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}
