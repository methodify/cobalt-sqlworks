//! Editor tab snapshots: hot exit and "reopen closed tab".

use super::{fmt_ts, opt_ts, opt_uuid, parse_ts, parse_uuid, Store};
use crate::Result;
use chrono::{DateTime, Utc};
use cobalt_core::{ProfileId, TabId};
use rusqlite::{params, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TabSnapshot {
    pub tab_id: TabId,
    pub profile_id: Option<ProfileId>,
    pub database: Option<String>,
    pub title: String,
    pub text: String,
    /// Byte offset of the caret.
    pub cursor: usize,
    /// Set when the tab is bound to a file on disk (the snapshot still holds unsaved text).
    pub file_path: Option<PathBuf>,
    pub updated_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
}

impl TabSnapshot {
    pub fn new(tab_id: TabId, title: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            tab_id,
            profile_id: None,
            database: None,
            title: title.into(),
            text: text.into(),
            cursor: 0,
            file_path: None,
            updated_at: Utc::now(),
            closed_at: None,
        }
    }
}

const COLS: &str = "tab_id, profile_id, database, title, text, cursor, file_path, updated_at, closed_at";

fn row_to_tab(row: &Row<'_>) -> rusqlite::Result<TabSnapshot> {
    Ok(TabSnapshot {
        tab_id: TabId(parse_uuid(&row.get::<_, String>(0)?)?),
        profile_id: opt_uuid(row.get(1)?)?.map(ProfileId),
        database: row.get(2)?,
        title: row.get(3)?,
        text: row.get(4)?,
        cursor: row.get::<_, i64>(5)?.max(0) as usize,
        file_path: row.get::<_, Option<String>>(6)?.map(PathBuf::from),
        updated_at: parse_ts(&row.get::<_, String>(7)?)?,
        closed_at: opt_ts(row.get(8)?)?,
    })
}

impl Store {
    /// Insert or replace the snapshot for a tab (all fields, including `closed_at`).
    pub fn save_tab(&self, t: &TabSnapshot) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO tab_snapshots (tab_id, profile_id, database, title, text, cursor, file_path, updated_at, closed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(tab_id) DO UPDATE SET
                profile_id = excluded.profile_id, database = excluded.database, title = excluded.title,
                text = excluded.text, cursor = excluded.cursor, file_path = excluded.file_path,
                updated_at = excluded.updated_at, closed_at = excluded.closed_at",
            params![
                t.tab_id.to_string(),
                t.profile_id.map(|p| p.to_string()),
                t.database,
                t.title,
                t.text,
                t.cursor as i64,
                t.file_path.as_ref().map(|p| p.to_string_lossy().into_owned()),
                fmt_ts(&t.updated_at),
                t.closed_at.as_ref().map(fmt_ts),
            ],
        )?;
        Ok(())
    }

    pub fn get_tab(&self, tab_id: TabId) -> Result<Option<TabSnapshot>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare_cached(&format!("SELECT {COLS} FROM tab_snapshots WHERE tab_id = ?1"))?;
        Ok(stmt.query_row(params![tab_id.to_string()], row_to_tab).optional()?)
    }

    /// Tabs that were open at last exit, oldest first (so re-adding them preserves order).
    pub fn load_open_tabs(&self) -> Result<Vec<TabSnapshot>> {
        let conn = self.lock()?;
        let mut stmt =
            conn.prepare_cached(&format!("SELECT {COLS} FROM tab_snapshots WHERE closed_at IS NULL ORDER BY updated_at, tab_id"))?;
        let rows = stmt.query_map([], row_to_tab)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Mark a tab closed; its text is retained for `recently_closed` / `restore_tab`.
    pub fn close_tab(&self, tab_id: TabId) -> Result<bool> {
        let conn = self.lock()?;
        let n = conn.execute(
            "UPDATE tab_snapshots SET closed_at = ?2 WHERE tab_id = ?1 AND closed_at IS NULL",
            params![tab_id.to_string(), fmt_ts(&Utc::now())],
        )?;
        Ok(n > 0)
    }

    /// Closed tabs, most recently closed first.
    pub fn recently_closed(&self, limit: usize) -> Result<Vec<TabSnapshot>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {COLS} FROM tab_snapshots WHERE closed_at IS NOT NULL ORDER BY closed_at DESC, tab_id LIMIT ?1"
        ))?;
        let rows = stmt.query_map(params![limit as i64], row_to_tab)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Reopen a closed tab. Returns the snapshot to rebuild the editor from.
    pub fn restore_tab(&self, tab_id: TabId) -> Result<Option<TabSnapshot>> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE tab_snapshots SET closed_at = NULL, updated_at = ?2 WHERE tab_id = ?1",
            params![tab_id.to_string(), fmt_ts(&Utc::now())],
        )?;
        let mut stmt = conn.prepare_cached(&format!("SELECT {COLS} FROM tab_snapshots WHERE tab_id = ?1"))?;
        Ok(stmt.query_row(params![tab_id.to_string()], row_to_tab).optional()?)
    }

    /// Forget a tab entirely.
    pub fn delete_tab(&self, tab_id: TabId) -> Result<bool> {
        let conn = self.lock()?;
        Ok(conn.execute("DELETE FROM tab_snapshots WHERE tab_id = ?1", params![tab_id.to_string()])? > 0)
    }

    /// Keep only the `keep` most recently closed tabs. Returns rows removed.
    pub fn prune_closed_tabs(&self, keep: usize) -> Result<usize> {
        let conn = self.lock()?;
        Ok(conn.execute(
            "DELETE FROM tab_snapshots WHERE closed_at IS NOT NULL AND tab_id NOT IN
                (SELECT tab_id FROM tab_snapshots WHERE closed_at IS NOT NULL ORDER BY closed_at DESC, tab_id LIMIT ?1)",
            params![keep as i64],
        )?)
    }
}
