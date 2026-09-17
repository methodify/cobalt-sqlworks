//! Small JSON key/value bag: window geometry, dock layout, "last used" bits.

use super::Store;
use crate::Result;
use rusqlite::{params, OptionalExtension};
use serde::de::DeserializeOwned;
use serde::Serialize;

impl Store {
    /// `None` if the key is absent. A value that no longer deserializes is treated as absent
    /// (and logged) so a schema change in a UI struct never blocks startup.
    pub fn get_kv<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        let conn = self.lock()?;
        let raw: Option<String> = conn.query_row("SELECT value FROM kv WHERE key = ?1", params![key], |r| r.get(0)).optional()?;
        match raw {
            None => Ok(None),
            Some(s) => match serde_json::from_str(&s) {
                Ok(v) => Ok(Some(v)),
                Err(e) => {
                    tracing::warn!(key, error = %e, "ignoring unreadable kv value");
                    Ok(None)
                }
            },
        }
    }

    pub fn set_kv<T: Serialize + ?Sized>(&self, key: &str, value: &T) -> Result<()> {
        let json = serde_json::to_string(value)?;
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO kv (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, json],
        )?;
        Ok(())
    }

    pub fn delete_kv(&self, key: &str) -> Result<bool> {
        let conn = self.lock()?;
        Ok(conn.execute("DELETE FROM kv WHERE key = ?1", params![key])? > 0)
    }
}
