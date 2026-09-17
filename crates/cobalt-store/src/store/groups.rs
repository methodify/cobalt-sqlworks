//! Server groups: named, colored, nestable.

use super::{opt_uuid, parse_uuid, Store};
use crate::{Result, StoreError};
use cobalt_core::{Color, GroupId, ServerGroup};
use rusqlite::{params, OptionalExtension, Row};

const COLS: &str = "id, name, color, parent_id, description, sort_order";

fn row_to_group(row: &Row<'_>) -> rusqlite::Result<ServerGroup> {
    let color_s: String = row.get(2)?;
    let color = Color::parse(&color_s).unwrap_or(Color::PALETTE[0]);
    Ok(ServerGroup {
        id: GroupId(parse_uuid(&row.get::<_, String>(0)?)?),
        name: row.get(1)?,
        color,
        parent: opt_uuid(row.get(3)?)?.map(GroupId),
        description: row.get(4)?,
        sort_order: row.get(5)?,
    })
}

impl Store {
    /// All groups ordered by `sort_order`, then name. Callers build the tree from `parent`.
    pub fn list_groups(&self) -> Result<Vec<ServerGroup>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare_cached(&format!("SELECT {COLS} FROM groups ORDER BY sort_order, name COLLATE NOCASE"))?;
        let rows = stmt.query_map([], row_to_group)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn get_group(&self, id: GroupId) -> Result<Option<ServerGroup>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare_cached(&format!("SELECT {COLS} FROM groups WHERE id = ?1"))?;
        Ok(stmt.query_row(params![id.to_string()], row_to_group).optional()?)
    }

    /// Insert or update. The parent must already exist (or be `None`).
    pub fn upsert_group(&self, g: &ServerGroup) -> Result<()> {
        if g.parent == Some(g.id) {
            return Err(StoreError::Invalid("a group cannot be its own parent".into()));
        }
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO groups (id, name, color, parent_id, description, sort_order)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(id) DO UPDATE SET
                name = excluded.name, color = excluded.color, parent_id = excluded.parent_id,
                description = excluded.description, sort_order = excluded.sort_order",
            params![
                g.id.to_string(),
                g.name,
                g.color.hex(),
                g.parent.map(|p| p.to_string()),
                g.description,
                g.sort_order
            ],
        )?;
        Ok(())
    }

    /// Delete a group. Its profiles and child groups move to `reassign_to` (or become
    /// ungrouped / top-level when `None`). Returns `false` if no such group existed.
    pub fn delete_group(&self, id: GroupId, reassign_to: Option<GroupId>) -> Result<bool> {
        if reassign_to == Some(id) {
            return Err(StoreError::Invalid("cannot reassign to the group being deleted".into()));
        }
        let id_s = id.to_string();
        let target = reassign_to.map(|g| g.to_string());
        self.tx(|tx| {
            tx.execute("UPDATE profiles SET group_id = ?2 WHERE group_id = ?1", params![id_s, target])?;
            tx.execute("UPDATE groups SET parent_id = ?2 WHERE parent_id = ?1", params![id_s, target])?;
            let n = tx.execute("DELETE FROM groups WHERE id = ?1", params![id_s])?;
            Ok(n > 0)
        })
    }

    /// Re-parent a group. Refuses cycles (moving a group under one of its own descendants).
    pub fn move_group(&self, id: GroupId, new_parent: Option<GroupId>) -> Result<()> {
        if new_parent == Some(id) {
            return Err(StoreError::Invalid("a group cannot be its own parent".into()));
        }
        self.tx(|tx| {
            // Walk up from the proposed parent; if we meet `id`, it's a cycle.
            let mut cursor = new_parent;
            let mut hops = 0;
            while let Some(p) = cursor {
                if p == id {
                    return Err(StoreError::Invalid("cannot move a group under its own descendant".into()));
                }
                cursor = tx
                    .query_row("SELECT parent_id FROM groups WHERE id = ?1", params![p.to_string()], |r| r.get::<_, Option<String>>(0))
                    .optional()?
                    .ok_or_else(|| StoreError::NotFound(format!("group {p}")))?
                    .and_then(|s| GroupId::parse(&s));
                hops += 1;
                if hops > 1000 {
                    return Err(StoreError::Invalid("group hierarchy too deep".into()));
                }
            }
            let n = tx.execute(
                "UPDATE groups SET parent_id = ?2 WHERE id = ?1",
                params![id.to_string(), new_parent.map(|g| g.to_string())],
            )?;
            if n == 0 {
                return Err(StoreError::NotFound(format!("group {id}")));
            }
            Ok(())
        })
    }

    /// Set `sort_order` of the given groups to their index in `ids`.
    pub fn reorder_groups(&self, ids: &[GroupId]) -> Result<()> {
        self.tx(|tx| {
            let mut stmt = tx.prepare_cached("UPDATE groups SET sort_order = ?2 WHERE id = ?1")?;
            for (i, id) in ids.iter().enumerate() {
                stmt.execute(params![id.to_string(), i as i32])?;
            }
            Ok(())
        })
    }
}
