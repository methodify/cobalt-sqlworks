//! Whole-library export/import (Cobalt's own JSON format, also the target of ADS import).

use super::{fmt_ts, Store};
use crate::Result;
use chrono::{DateTime, Utc};
use cobalt_core::{AuthMethod, ConnectionProfile, GroupId, ServerGroup};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Groups and profiles, minus secrets. `SecretRef`s are stripped on export because they only
/// mean something on the machine whose keyring holds them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LibraryExport {
    /// Format version; bump when the shape changes incompatibly.
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exported_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub groups: Vec<ServerGroup>,
    #[serde(default)]
    pub profiles: Vec<ConnectionProfile>,
}

impl LibraryExport {
    pub const VERSION: u32 = 1;

    pub fn new(groups: Vec<ServerGroup>, profiles: Vec<ConnectionProfile>) -> Self {
        Self {
            version: Self::VERSION,
            exported_at: Some(Utc::now()),
            groups,
            profiles,
        }
    }

    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    pub fn from_json(json: &str) -> Result<Self> {
        let lib: Self = serde_json::from_str(json)?;
        if lib.version > Self::VERSION {
            return Err(crate::StoreError::Invalid(format!(
                "library file is version {}, newer than this build understands ({})",
                lib.version,
                Self::VERSION
            )));
        }
        Ok(lib)
    }

    /// Remove keyring references (passwords, client secrets) from every profile.
    pub fn strip_secrets(&mut self) {
        for p in &mut self.profiles {
            match &mut p.auth {
                AuthMethod::SqlLogin { password, .. } => *password = None,
                AuthMethod::EntraServicePrincipal { secret, .. } => *secret = None,
                _ => {}
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ImportSummary {
    pub groups: usize,
    pub profiles: usize,
    /// Profiles whose group was not in the import (or the store) and became ungrouped.
    pub orphaned_profiles: usize,
}

impl Store {
    /// Snapshot the whole library. Secret references are stripped.
    pub fn export_library(&self) -> Result<LibraryExport> {
        let mut lib = LibraryExport::new(self.list_groups()?, self.list_profiles()?);
        lib.strip_secrets();
        Ok(lib)
    }

    /// Load a library. With `merge = false` the existing groups and profiles are replaced
    /// wholesale; with `merge = true` entries are upserted by id and everything else is kept.
    /// Group parents / profile groups that do not exist afterwards are cleared rather than
    /// failing the import.
    pub fn import_library(&self, lib: &LibraryExport, merge: bool) -> Result<ImportSummary> {
        let now = fmt_ts(&Utc::now());
        self.tx(|tx| {
            if !merge {
                tx.execute("DELETE FROM recent_connections", [])?;
                tx.execute("DELETE FROM catalog_cache", [])?;
                tx.execute("DELETE FROM profiles", [])?;
                tx.execute("DELETE FROM groups", [])?;
            }

            // Groups: insert without parents first (any order is then valid), then link.
            let mut known: HashSet<GroupId> = if merge {
                let mut stmt = tx.prepare("SELECT id FROM groups")?;
                let ids: HashSet<GroupId> = stmt
                    .query_map([], |r| r.get::<_, String>(0))?
                    .filter_map(|r| r.ok())
                    .filter_map(|s| GroupId::parse(&s))
                    .collect();
                ids
            } else {
                HashSet::new()
            };
            {
                let mut ins = tx.prepare(
                    "INSERT INTO groups (id, name, color, parent_id, description, sort_order)
                     VALUES (?1, ?2, ?3, NULL, ?4, ?5)
                     ON CONFLICT(id) DO UPDATE SET name = excluded.name, color = excluded.color,
                        parent_id = NULL, description = excluded.description, sort_order = excluded.sort_order",
                )?;
                for g in &lib.groups {
                    ins.execute(params![g.id.to_string(), g.name, g.color.hex(), g.description, g.sort_order])?;
                    known.insert(g.id);
                }
            }
            {
                let mut link = tx.prepare("UPDATE groups SET parent_id = ?2 WHERE id = ?1")?;
                for g in &lib.groups {
                    let parent = g.parent.filter(|p| *p != g.id && known.contains(p));
                    if let Some(p) = parent {
                        link.execute(params![g.id.to_string(), p.to_string()])?;
                    }
                }
            }

            let mut orphaned = 0;
            {
                let mut ins = tx.prepare(
                    "INSERT INTO profiles (id, group_id, name, server, port, database, auth_json, options_json,
                                           color, read_only_guard, created_at, last_used, sort_order)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                     ON CONFLICT(id) DO UPDATE SET
                        group_id = excluded.group_id, name = excluded.name, server = excluded.server,
                        port = excluded.port, database = excluded.database, auth_json = excluded.auth_json,
                        options_json = excluded.options_json, color = excluded.color,
                        read_only_guard = excluded.read_only_guard,
                        last_used = COALESCE(excluded.last_used, profiles.last_used),
                        sort_order = excluded.sort_order",
                )?;
                for (i, p) in lib.profiles.iter().enumerate() {
                    let group = match p.group {
                        Some(g) if known.contains(&g) => Some(g),
                        Some(_) => {
                            orphaned += 1;
                            None
                        }
                        None => None,
                    };
                    ins.execute(params![
                        p.id.to_string(),
                        group.map(|g| g.to_string()),
                        p.name,
                        p.server,
                        p.port.map(i64::from),
                        p.database,
                        serde_json::to_string(&p.auth)?,
                        serde_json::to_string(&p.options)?,
                        p.color.map(|c| c.hex()),
                        p.read_only_guard as i64,
                        now,
                        p.last_used.as_ref().map(fmt_ts),
                        i as i64,
                    ])?;
                }
            }
            Ok(ImportSummary { groups: lib.groups.len(), profiles: lib.profiles.len(), orphaned_profiles: orphaned })
        })
    }
}
