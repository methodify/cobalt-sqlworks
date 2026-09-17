//! Cached `DatabaseCatalog` per (profile, database) for completion and the object tree.

use super::{fmt_ts, parse_ts, Store};
use crate::Result;
use chrono::{DateTime, Utc};
use cobalt_core::{DatabaseCatalog, ProfileId};
use rusqlite::{params, OptionalExtension};

impl Store {
    /// The cached catalog and when it was stored.
    pub fn get_catalog(
        &self,
        profile: ProfileId,
        database: &str,
    ) -> Result<Option<(DatabaseCatalog, DateTime<Utc>)>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare_cached(
            "SELECT json, refreshed_at FROM catalog_cache WHERE profile_id = ?1 AND database = ?2",
        )?;
        let row = stmt
            .query_row(params![profile.to_string(), database], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .optional()?;
        match row {
            None => Ok(None),
            Some((json, ts)) => {
                let catalog: DatabaseCatalog = match serde_json::from_str(&json) {
                    Ok(c) => c,
                    Err(e) => {
                        // A stale/incompatible blob is just a cache miss.
                        tracing::warn!(%profile, database, error = %e, "discarding unreadable catalog cache entry");
                        return Ok(None);
                    }
                };
                Ok(Some((catalog, parse_ts(&ts)?)))
            }
        }
    }

    pub fn put_catalog(
        &self,
        profile: ProfileId,
        database: &str,
        catalog: &DatabaseCatalog,
    ) -> Result<()> {
        let json = serde_json::to_string(catalog)?;
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO catalog_cache (profile_id, database, json, refreshed_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(profile_id, database) DO UPDATE SET json = excluded.json, refreshed_at = excluded.refreshed_at",
            params![profile.to_string(), database, json, fmt_ts(&Utc::now())],
        )?;
        Ok(())
    }

    /// Drop the cache for one database, or for every database of the profile. Returns rows removed.
    pub fn invalidate_catalog(&self, profile: ProfileId, database: Option<&str>) -> Result<usize> {
        let conn = self.lock()?;
        let n = match database {
            Some(db) => conn.execute(
                "DELETE FROM catalog_cache WHERE profile_id = ?1 AND database = ?2",
                params![profile.to_string(), db],
            )?,
            None => conn.execute(
                "DELETE FROM catalog_cache WHERE profile_id = ?1",
                params![profile.to_string()],
            )?,
        };
        Ok(n)
    }
}
