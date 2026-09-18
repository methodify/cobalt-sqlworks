//! Fabric explorer persistence: pinned items and a JSON cache of what the REST API returned.

use super::{fmt_ts, parse_ts, Store};
use crate::error::Result;
use chrono::{DateTime, Utc};
use rusqlite::params;

/// A pinned Fabric item. `display` is a cached label so pins render before the tree loads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FabricPin {
    pub workspace_id: String,
    pub item_id: String,
    pub item_kind: String,
    pub display_name: String,
    pub workspace_name: String,
    pub position: i64,
}

impl Store {
    pub fn fabric_pins(&self) -> Result<Vec<FabricPin>> {
        let conn = self.lock()?;
        let mut st = conn.prepare("SELECT workspace_id, item_id, item_kind, display_name, workspace_name, position FROM fabric_pins ORDER BY position, display_name")?;
        let rows = st.query_map([], |r| {
            Ok(FabricPin { workspace_id: r.get(0)?, item_id: r.get(1)?, item_kind: r.get(2)?, display_name: r.get(3)?, workspace_name: r.get(4)?, position: r.get(5)? })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn fabric_pin(&self, pin: &FabricPin) -> Result<()> {
        let conn = self.lock()?;
        let next: i64 = conn.query_row("SELECT COALESCE(MAX(position), 0) + 1 FROM fabric_pins", [], |r| r.get(0))?;
        conn.execute(
            "INSERT INTO fabric_pins (workspace_id, item_id, item_kind, display_name, workspace_name, position, pinned_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(item_id) DO UPDATE SET display_name = excluded.display_name, workspace_name = excluded.workspace_name, item_kind = excluded.item_kind",
            params![pin.workspace_id, pin.item_id, pin.item_kind, pin.display_name, pin.workspace_name, if pin.position > 0 { pin.position } else { next }, fmt_ts(&Utc::now())],
        )?;
        Ok(())
    }

    pub fn fabric_unpin(&self, item_id: &str) -> Result<()> {
        self.lock()?.execute("DELETE FROM fabric_pins WHERE item_id = ?1", params![item_id])?;
        Ok(())
    }

    /// Refresh the cached labels of a pin after the tree reloads (renames follow the item id).
    pub fn fabric_pin_relabel(&self, item_id: &str, display_name: &str, workspace_name: &str) -> Result<()> {
        self.lock()?.execute("UPDATE fabric_pins SET display_name = ?2, workspace_name = ?3 WHERE item_id = ?1", params![item_id, display_name, workspace_name])?;
        Ok(())
    }

    /// Record that an item was opened (keeps the newest 30).
    pub fn fabric_touch_recent(&self, pin: &FabricPin) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO fabric_recent (item_id, workspace_id, item_kind, display_name, workspace_name, opened_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(item_id) DO UPDATE SET opened_at = excluded.opened_at, display_name = excluded.display_name, workspace_name = excluded.workspace_name",
            params![pin.item_id, pin.workspace_id, pin.item_kind, pin.display_name, pin.workspace_name, fmt_ts(&Utc::now())],
        )?;
        conn.execute("DELETE FROM fabric_recent WHERE item_id NOT IN (SELECT item_id FROM fabric_recent ORDER BY opened_at DESC LIMIT 30)", [])?;
        Ok(())
    }

    /// Most recently opened items, newest first.
    pub fn fabric_recent(&self, limit: usize) -> Result<Vec<FabricPin>> {
        let conn = self.lock()?;
        let mut st = conn.prepare("SELECT workspace_id, item_id, item_kind, display_name, workspace_name FROM fabric_recent ORDER BY opened_at DESC LIMIT ?1")?;
        let rows = st.query_map(params![limit as i64], |r| {
            Ok(FabricPin { workspace_id: r.get(0)?, item_id: r.get(1)?, item_kind: r.get(2)?, display_name: r.get(3)?, workspace_name: r.get(4)?, position: 0 })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Cached REST payload under `key` (e.g. `workspaces`, `items:<ws>`, `detail:<item>`).
    pub fn fabric_cache_get(&self, key: &str) -> Result<Option<(String, DateTime<Utc>)>> {
        let conn = self.lock()?;
        let mut st = conn.prepare("SELECT value, fetched_at FROM fabric_cache WHERE key = ?1")?;
        let mut rows = st.query(params![key])?;
        match rows.next()? {
            Some(r) => {
                let v: String = r.get(0)?;
                let ts: String = r.get(1)?;
                Ok(Some((v, parse_ts(&ts)?)))
            }
            None => Ok(None),
        }
    }

    pub fn fabric_cache_put(&self, key: &str, value: &str) -> Result<()> {
        self.lock()?.execute(
            "INSERT INTO fabric_cache (key, value, fetched_at) VALUES (?1, ?2, ?3) ON CONFLICT(key) DO UPDATE SET value = excluded.value, fetched_at = excluded.fetched_at",
            params![key, value, fmt_ts(&Utc::now())],
        )?;
        Ok(())
    }

    /// Drop cached REST payloads. Keys under `pref:` are small user preferences (e.g. the last
    /// lakehouse exported to) and survive.
    pub fn fabric_cache_clear(&self) -> Result<()> {
        self.lock()?.execute("DELETE FROM fabric_cache WHERE key NOT LIKE 'pref:%'", [])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pins_round_trip_and_relabel() {
        let s = Store::open_in_memory().unwrap();
        let p = FabricPin { workspace_id: "w1".into(), item_id: "i1".into(), item_kind: "Warehouse".into(), display_name: "Sales".into(), workspace_name: "Finance".into(), position: 0 };
        s.fabric_pin(&p).unwrap();
        s.fabric_pin(&FabricPin { item_id: "i2".into(), display_name: "Bronze".into(), ..p.clone() }).unwrap();
        let pins = s.fabric_pins().unwrap();
        assert_eq!(pins.iter().map(|p| p.display_name.as_str()).collect::<Vec<_>>(), ["Sales", "Bronze"]);
        assert!(pins[1].position > pins[0].position);
        s.fabric_pin_relabel("i1", "Sales_DW", "Finance").unwrap();
        assert_eq!(s.fabric_pins().unwrap()[0].display_name, "Sales_DW");
        s.fabric_unpin("i1").unwrap();
        assert_eq!(s.fabric_pins().unwrap().len(), 1);
        // pinning again is idempotent
        s.fabric_pin(&FabricPin { item_id: "i2".into(), ..p.clone() }).unwrap();
        assert_eq!(s.fabric_pins().unwrap().len(), 1);
    }

    #[test]
    fn recent_keeps_newest_first() {
        let s = Store::open_in_memory().unwrap();
        let p = FabricPin { workspace_id: "w".into(), item_id: "a".into(), item_kind: "Warehouse".into(), display_name: "A".into(), workspace_name: "W".into(), position: 0 };
        s.fabric_touch_recent(&p).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        s.fabric_touch_recent(&FabricPin { item_id: "b".into(), display_name: "B".into(), ..p.clone() }).unwrap();
        assert_eq!(s.fabric_recent(10).unwrap().iter().map(|p| p.item_id.as_str()).collect::<Vec<_>>(), ["b", "a"]);
        std::thread::sleep(std::time::Duration::from_millis(2));
        s.fabric_touch_recent(&p).unwrap();
        assert_eq!(s.fabric_recent(1).unwrap()[0].item_id, "a");
    }

    #[test]
    fn cache_round_trip() {
        let s = Store::open_in_memory().unwrap();
        assert!(s.fabric_cache_get("workspaces").unwrap().is_none());
        s.fabric_cache_put("workspaces", "[1]").unwrap();
        s.fabric_cache_put("workspaces", "[2]").unwrap();
        let (v, ts) = s.fabric_cache_get("workspaces").unwrap().unwrap();
        assert_eq!(v, "[2]");
        assert!((Utc::now() - ts).num_seconds() < 5);
        s.fabric_cache_put("pref:last_lakehouse", "lh1").unwrap();
        s.fabric_cache_clear().unwrap();
        assert!(s.fabric_cache_get("workspaces").unwrap().is_none());
        assert_eq!(s.fabric_cache_get("pref:last_lakehouse").unwrap().unwrap().0, "lh1");
    }
}
