//! Connection profiles.

use super::{conv, fmt_ts, opt_ts, opt_uuid, parse_uuid, Store};
use crate::Result;
use chrono::Utc;
use cobalt_core::{AuthMethod, Color, ConnectionOptions, ConnectionProfile, GroupId, ProfileId};
use rusqlite::{params, OptionalExtension, Row};

const COLS: &str = "p.id, p.group_id, p.name, p.server, p.port, p.database, p.auth_json, p.options_json, \
                    p.color, p.read_only_guard, p.last_used";

fn row_to_profile(row: &Row<'_>) -> rusqlite::Result<ConnectionProfile> {
    let auth: AuthMethod = serde_json::from_str(&row.get::<_, String>(6)?).map_err(conv)?;
    let options: ConnectionOptions = serde_json::from_str(&row.get::<_, String>(7)?).map_err(conv)?;
    let port: Option<i64> = row.get(4)?;
    Ok(ConnectionProfile {
        id: ProfileId(parse_uuid(&row.get::<_, String>(0)?)?),
        group: opt_uuid(row.get(1)?)?.map(GroupId),
        name: row.get(2)?,
        server: row.get(3)?,
        port: port.and_then(|p| u16::try_from(p).ok()),
        database: row.get(5)?,
        auth,
        options,
        color: row.get::<_, Option<String>>(8)?.as_deref().and_then(Color::parse),
        read_only_guard: row.get::<_, i64>(9)? != 0,
        last_used: opt_ts(row.get(10)?)?,
    })
}

impl Store {
    /// Every profile, ordered by group (group sort order, then name; ungrouped first),
    /// then the profile's own sort order, then display name.
    pub fn list_profiles(&self) -> Result<Vec<ConnectionProfile>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {COLS} FROM profiles p LEFT JOIN groups g ON g.id = p.group_id
             ORDER BY p.group_id IS NOT NULL, g.sort_order, g.name COLLATE NOCASE, p.group_id,
                      p.sort_order, COALESCE(p.name, p.server) COLLATE NOCASE"
        ))?;
        let rows = stmt.query_map([], row_to_profile)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Profiles in one group (`None` = ungrouped), by sort order then display name.
    pub fn list_profiles_in_group(&self, group: Option<GroupId>) -> Result<Vec<ConnectionProfile>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {COLS} FROM profiles p WHERE p.group_id IS ?1
             ORDER BY p.sort_order, COALESCE(p.name, p.server) COLLATE NOCASE"
        ))?;
        let rows = stmt.query_map(params![group.map(|g| g.to_string())], row_to_profile)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn get_profile(&self, id: ProfileId) -> Result<Option<ConnectionProfile>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare_cached(&format!("SELECT {COLS} FROM profiles p WHERE p.id = ?1"))?;
        Ok(stmt.query_row(params![id.to_string()], row_to_profile).optional()?)
    }

    /// Insert or update. New profiles go to the end of their group; existing ones keep their
    /// position and `created_at`. `last_used` is only moved forward, never cleared.
    pub fn upsert_profile(&self, p: &ConnectionProfile) -> Result<()> {
        let auth_json = serde_json::to_string(&p.auth)?;
        let options_json = serde_json::to_string(&p.options)?;
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO profiles (id, group_id, name, server, port, database, auth_json, options_json,
                                   color, read_only_guard, created_at, last_used, sort_order)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                     (SELECT COALESCE(MAX(sort_order), -1) + 1 FROM profiles WHERE group_id IS ?2))
             ON CONFLICT(id) DO UPDATE SET
                group_id = excluded.group_id, name = excluded.name, server = excluded.server,
                port = excluded.port, database = excluded.database, auth_json = excluded.auth_json,
                options_json = excluded.options_json, color = excluded.color,
                read_only_guard = excluded.read_only_guard,
                last_used = MAX(COALESCE(excluded.last_used, ''), COALESCE(profiles.last_used, '')),
                sort_order = CASE WHEN profiles.group_id IS excluded.group_id THEN profiles.sort_order
                                  ELSE excluded.sort_order END",
            params![
                p.id.to_string(),
                p.group.map(|g| g.to_string()),
                p.name,
                p.server,
                p.port.map(i64::from),
                p.database,
                auth_json,
                options_json,
                p.color.map(|c| c.hex()),
                p.read_only_guard as i64,
                fmt_ts(&Utc::now()),
                p.last_used.as_ref().map(fmt_ts),
            ],
        )?;
        // MAX('' , '') yields '' rather than NULL; normalise.
        conn.execute("UPDATE profiles SET last_used = NULL WHERE id = ?1 AND last_used = ''", params![p.id.to_string()])?;
        Ok(())
    }

    /// Remove a profile, its recent-connection entry, and any cached catalogs. History is kept.
    pub fn delete_profile(&self, id: ProfileId) -> Result<bool> {
        let id_s = id.to_string();
        self.tx(|tx| {
            tx.execute("DELETE FROM catalog_cache WHERE profile_id = ?1", params![id_s])?;
            tx.execute("DELETE FROM recent_connections WHERE profile_id = ?1", params![id_s])?;
            let n = tx.execute("DELETE FROM profiles WHERE id = ?1", params![id_s])?;
            Ok(n > 0)
        })
    }

    /// Mark a profile as used now: bumps `last_used` and the recent-connections list.
    pub fn touch_profile(&self, id: ProfileId) -> Result<()> {
        let now = fmt_ts(&Utc::now());
        let id_s = id.to_string();
        self.tx(|tx| {
            let n = tx.execute("UPDATE profiles SET last_used = ?2 WHERE id = ?1", params![id_s, now])?;
            if n == 0 {
                return Err(crate::StoreError::NotFound(format!("profile {id}")));
            }
            tx.execute(
                "INSERT INTO recent_connections (profile_id, used_at) VALUES (?1, ?2)
                 ON CONFLICT(profile_id) DO UPDATE SET used_at = excluded.used_at",
                params![id_s, now],
            )?;
            Ok(())
        })
    }

    /// Most recently used profiles, newest first.
    pub fn recent_profiles(&self, limit: usize) -> Result<Vec<ConnectionProfile>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {COLS} FROM recent_connections r JOIN profiles p ON p.id = r.profile_id
             ORDER BY r.used_at DESC LIMIT ?1"
        ))?;
        let rows = stmt.query_map(params![limit as i64], row_to_profile)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Put `ids` into `group` (`None` = ungrouped) in the given order. Profiles not listed
    /// keep their group and order.
    pub fn reorder_profiles(&self, group: Option<GroupId>, ids: &[ProfileId]) -> Result<()> {
        let group_s = group.map(|g| g.to_string());
        self.tx(|tx| {
            let mut stmt = tx.prepare_cached("UPDATE profiles SET group_id = ?2, sort_order = ?3 WHERE id = ?1")?;
            for (i, id) in ids.iter().enumerate() {
                stmt.execute(params![id.to_string(), group_s, i as i64])?;
            }
            Ok(())
        })
    }

    pub fn profile_count(&self) -> Result<usize> {
        let conn = self.lock()?;
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM profiles", [], |r| r.get(0))?;
        Ok(n as usize)
    }
}
