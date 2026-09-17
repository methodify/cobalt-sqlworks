//! Catalog reads against `sys.*`.
//!
//! Objects in a database other than the session's current one are read through three-part
//! names (`[db].sys.objects`), which works on SQL Server and Fabric. If that fails on an engine
//! that supports `USE`, the query is retried inside `USE [db]` … `USE [original]`.

use super::{cell_bool, cell_i64, cell_string, MssqlConnection};
use crate::{DriverError, Result};
use cobalt_core::*;
use std::collections::{BTreeMap, HashMap};
use tiberius::TokenRow;

/// `[ident]` with `]` escaped.
pub(crate) fn br(ident: &str) -> String {
    format!("[{}]", bracket(ident))
}

/// `N'literal'` with quotes escaped.
pub(crate) fn lit(s: &str) -> String {
    format!("N'{}'", s.replace('\'', "''"))
}

/// `[db].` when `db` is not the current database, else empty.
fn prefix(conn: &MssqlConnection, db: &str) -> String {
    if db.is_empty() || db.eq_ignore_ascii_case(&conn.database) {
        String::new()
    } else {
        format!("{}.", br(db))
    }
}

/// SQL expression that yields the object id of `obj` (`{p}` = catalog prefix).
fn object_id_expr(obj: &ObjectRef, p: &str) -> String {
    if let Some(id) = obj.object_id {
        return id.to_string();
    }
    match obj.kind {
        ObjectKind::TableType => format!(
            "(SELECT tt.type_table_object_id FROM {p}sys.table_types tt JOIN {p}sys.schemas s ON s.schema_id = tt.schema_id WHERE tt.name = {} AND s.name = {})",
            lit(&obj.name),
            lit(&obj.schema)
        ),
        _ => {
            let full = if obj.database.is_empty() {
                format!("{}.{}", br(&obj.schema), br(&obj.name))
            } else {
                format!("{}.{}.{}", br(&obj.database), br(&obj.schema), br(&obj.name))
            };
            format!("OBJECT_ID({})", lit(&full))
        }
    }
}

/// Run `build(prefix)` against `db`, falling back to `USE` when three-part names fail.
async fn query_in_db(conn: &mut MssqlConnection, db: &str, build: impl Fn(&str) -> String) -> Result<Vec<TokenRow<'static>>> {
    let p = prefix(conn, db);
    match conn.query_rows(&build(&p)).await {
        Ok(rows) => Ok(rows),
        Err(DriverError::Server(m)) if !p.is_empty() && conn.engine.capabilities.multiple_databases => {
            tracing::debug!("three-part catalog query failed ({}); retrying inside USE", m.message);
            let original = conn.database.clone();
            conn.run_silent(&format!("USE {}", br(db))).await?;
            let result = conn.query_rows(&build("")).await;
            if let Err(e) = conn.run_silent(&format!("USE {}", br(&original))).await {
                tracing::warn!("could not restore database context to {original}: {e}");
            }
            result
        }
        Err(e) => Err(e),
    }
}

/// The `COALESCE(bt.name, t.name)` projection: base system type for alias types, the CLR/user
/// name for UDTs. Requires the joins from [`type_joins`].
const TYPE_NAME: &str = "COALESCE(bt.name, t.name)";

fn type_joins(p: &str, col_alias: &str) -> String {
    format!(
        "JOIN {p}sys.types t ON t.user_type_id = {col_alias}.user_type_id \
         LEFT JOIN {p}sys.types bt ON bt.user_type_id = t.system_type_id AND t.user_type_id <> t.system_type_id AND t.system_type_id <> 240"
    )
}

// ---------------------------------------------------------------------------------------------

pub(crate) async fn list_databases(conn: &mut MssqlConnection) -> Result<Vec<DatabaseInfo>> {
    let rows = conn.query_rows("SELECT name, state_desc, is_read_only, database_id FROM sys.databases ORDER BY name").await?;
    Ok(rows
        .iter()
        .map(|r| {
            let name = cell_string(r, 0).unwrap_or_default();
            let id = cell_i64(r, 3).unwrap_or(0);
            let is_system = id <= 4 || ["master", "tempdb", "model", "msdb"].iter().any(|s| s.eq_ignore_ascii_case(&name));
            DatabaseInfo { name, is_system, state: cell_string(r, 1).unwrap_or_default(), is_read_only: cell_bool(r, 2) }
        })
        .collect())
}

pub(crate) async fn list_schemas(conn: &mut MssqlConnection, database: &str) -> Result<Vec<String>> {
    let rows = query_in_db(conn, database, |p| {
        format!(
            "SELECT name FROM {p}sys.schemas WHERE schema_id < 16384 AND name NOT IN ('sys', 'INFORMATION_SCHEMA', 'guest') ORDER BY name"
        )
    })
    .await?;
    Ok(rows.iter().filter_map(|r| cell_string(r, 0)).collect())
}

pub(crate) async fn list_objects(conn: &mut MssqlConnection, database: &str) -> Result<Vec<ObjectRef>> {
    let rows = query_in_db(conn, database, |p| {
        format!(
            "SELECT o.object_id, s.name AS schema_name, o.name, RTRIM(o.type) AS type \
             FROM {p}sys.objects o JOIN {p}sys.schemas s ON s.schema_id = o.schema_id \
             WHERE o.type IN ('U','V','P','PC','X','FN','FS','IF','TF','FT','AF','SN','SO') AND o.is_ms_shipped = 0 \
             UNION ALL \
             SELECT tt.type_table_object_id, s.name, tt.name, 'TT' \
             FROM {p}sys.table_types tt JOIN {p}sys.schemas s ON s.schema_id = tt.schema_id \
             ORDER BY schema_name, name"
        )
    })
    .await?;
    let db = if database.is_empty() { conn.database.clone() } else { database.to_string() };
    Ok(rows
        .iter()
        .filter_map(|r| {
            let kind = ObjectKind::from_sys_type(&cell_string(r, 3)?)?;
            Some(ObjectRef {
                database: db.clone(),
                schema: cell_string(r, 1)?,
                name: cell_string(r, 2)?,
                kind,
                object_id: cell_i64(r, 0).map(|v| v as i32),
            })
        })
        .collect())
}

fn column_from_row(r: &TokenRow<'static>, base: usize, ordinal: usize) -> ColumnInfo {
    // base.. : name, type_name, max_length, precision, scale, is_nullable, is_identity, is_computed, in_pk, default_definition, collation
    let s = |i: usize| cell_string(r, base + i);
    let n = |i: usize| cell_i64(r, base + i).unwrap_or(0);
    let sql_type = SqlType::from_catalog(&s(1).unwrap_or_default(), n(2) as i32, n(3) as u8, n(4) as u8);
    let mut c = ColumnInfo::new(s(0).unwrap_or_default(), sql_type, cell_bool(r, base + 5), ordinal);
    c.is_identity = cell_bool(r, base + 6);
    c.is_computed = cell_bool(r, base + 7);
    c.in_primary_key = cell_bool(r, base + 8);
    c.default_definition = s(9);
    c.collation = s(10);
    c
}

const COLUMN_PROJECTION: &str = "c.name, COALESCE(bt.name, t.name), c.max_length, c.precision, c.scale, c.is_nullable, c.is_identity, c.is_computed, \
    CASE WHEN pk.column_id IS NULL THEN 0 ELSE 1 END, d.definition, c.collation_name";

fn column_joins(p: &str) -> String {
    format!(
        "{} \
         LEFT JOIN (SELECT ic.object_id, ic.column_id FROM {p}sys.index_columns ic JOIN {p}sys.indexes i ON i.object_id = ic.object_id AND i.index_id = ic.index_id WHERE i.is_primary_key = 1) pk \
             ON pk.object_id = c.object_id AND pk.column_id = c.column_id \
         LEFT JOIN {p}sys.default_constraints d ON d.parent_object_id = c.object_id AND d.parent_column_id = c.column_id",
        type_joins(p, "c")
    )
}

pub(crate) async fn list_columns(conn: &mut MssqlConnection, obj: &ObjectRef) -> Result<Vec<ColumnInfo>> {
    let rows = query_in_db(conn, &obj.database, |p| {
        format!(
            "SELECT {COLUMN_PROJECTION} FROM {p}sys.columns c {} WHERE c.object_id = {} ORDER BY c.column_id",
            column_joins(p),
            object_id_expr(obj, p)
        )
    })
    .await?;
    Ok(rows.iter().enumerate().map(|(i, r)| column_from_row(r, 0, i)).collect())
}

pub(crate) async fn list_parameters(conn: &mut MssqlConnection, obj: &ObjectRef) -> Result<Vec<ParameterInfo>> {
    let rows = query_in_db(conn, &obj.database, |p| {
        format!(
            "SELECT p.name, {TYPE_NAME}, p.max_length, p.precision, p.scale, p.is_output, p.has_default_value \
             FROM {p}sys.parameters p {} WHERE p.object_id = {} AND p.parameter_id > 0 ORDER BY p.parameter_id",
            type_joins(p, "p"),
            object_id_expr(obj, p)
        )
    })
    .await?;
    Ok(rows
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let n = |j: usize| cell_i64(r, j).unwrap_or(0);
            ParameterInfo {
                name: cell_string(r, 0).unwrap_or_default(),
                sql_type: SqlType::from_catalog(&cell_string(r, 1).unwrap_or_default(), n(2) as i32, n(3) as u8, n(4) as u8),
                is_output: cell_bool(r, 5),
                has_default: cell_bool(r, 6),
                ordinal: i,
            }
        })
        .collect())
}

pub(crate) async fn list_indexes(conn: &mut MssqlConnection, obj: &ObjectRef) -> Result<Vec<IndexInfo>> {
    let rows = query_in_db(conn, &obj.database, |p| {
        format!(
            "SELECT i.index_id, i.name, i.is_unique, i.type_desc, i.is_primary_key, i.is_unique_constraint, i.filter_definition, \
                    ic.is_included_column, ic.is_descending_key, c.name \
             FROM {p}sys.indexes i \
             JOIN {p}sys.index_columns ic ON ic.object_id = i.object_id AND ic.index_id = i.index_id \
             JOIN {p}sys.columns c ON c.object_id = ic.object_id AND c.column_id = ic.column_id \
             WHERE i.object_id = {} AND i.index_id > 0 \
             ORDER BY i.index_id, ic.is_included_column, ic.key_ordinal, ic.index_column_id",
            object_id_expr(obj, p)
        )
    })
    .await?;
    let mut map: BTreeMap<i64, IndexInfo> = BTreeMap::new();
    for r in &rows {
        let id = cell_i64(r, 0).unwrap_or(0);
        let entry = map.entry(id).or_insert_with(|| {
            let type_desc = cell_string(r, 3).unwrap_or_default();
            IndexInfo {
                name: cell_string(r, 1).unwrap_or_default(),
                is_unique: cell_bool(r, 2),
                is_clustered: type_desc.to_ascii_uppercase().starts_with("CLUSTERED"),
                is_primary_key: cell_bool(r, 4),
                is_unique_constraint: cell_bool(r, 5),
                index_type: type_desc,
                key_columns: Vec::new(),
                included_columns: Vec::new(),
                filter: cell_string(r, 6).filter(|f| !f.is_empty()),
            }
        });
        let col = cell_string(r, 9).unwrap_or_default();
        if cell_bool(r, 7) {
            entry.included_columns.push(col);
        } else {
            entry.key_columns.push((col, cell_bool(r, 8)));
        }
    }
    Ok(map.into_values().collect())
}

pub(crate) async fn list_keys(conn: &mut MssqlConnection, obj: &ObjectRef) -> Result<Vec<KeyInfo>> {
    let mut keys = Vec::new();

    // Primary key / unique constraints
    let rows = query_in_db(conn, &obj.database, |p| {
        format!(
            "SELECT kc.name, RTRIM(kc.type), c.name FROM {p}sys.key_constraints kc \
             JOIN {p}sys.index_columns ic ON ic.object_id = kc.parent_object_id AND ic.index_id = kc.unique_index_id \
             JOIN {p}sys.columns c ON c.object_id = ic.object_id AND c.column_id = ic.column_id \
             WHERE kc.parent_object_id = {} ORDER BY kc.name, ic.key_ordinal",
            object_id_expr(obj, p)
        )
    })
    .await?;
    let mut grouped: Vec<(String, KeyKind, Vec<String>)> = Vec::new();
    for r in &rows {
        let name = cell_string(r, 0).unwrap_or_default();
        let kind = if cell_string(r, 1).unwrap_or_default() == "PK" { KeyKind::PrimaryKey } else { KeyKind::Unique };
        let col = cell_string(r, 2).unwrap_or_default();
        match grouped.last_mut() {
            Some((n, _, cols)) if *n == name => cols.push(col),
            _ => grouped.push((name, kind, vec![col])),
        }
    }
    keys.extend(grouped.into_iter().map(|(name, kind, columns)| KeyInfo { name, kind, columns, references: None, definition: None }));

    // Foreign keys
    let db = if obj.database.is_empty() { conn.database.clone() } else { obj.database.clone() };
    let rows = query_in_db(conn, &obj.database, |p| {
        format!(
            "SELECT fk.name, pc.name, rs.name, rt.name, rc.name, rt.object_id, fk.delete_referential_action_desc, fk.update_referential_action_desc \
             FROM {p}sys.foreign_keys fk \
             JOIN {p}sys.foreign_key_columns fkc ON fkc.constraint_object_id = fk.object_id \
             JOIN {p}sys.columns pc ON pc.object_id = fkc.parent_object_id AND pc.column_id = fkc.parent_column_id \
             JOIN {p}sys.objects rt ON rt.object_id = fk.referenced_object_id \
             JOIN {p}sys.schemas rs ON rs.schema_id = rt.schema_id \
             JOIN {p}sys.columns rc ON rc.object_id = fkc.referenced_object_id AND rc.column_id = fkc.referenced_column_id \
             WHERE fk.parent_object_id = {} ORDER BY fk.name, fkc.constraint_column_id",
            object_id_expr(obj, p)
        )
    })
    .await?;
    let mut fks: Vec<KeyInfo> = Vec::new();
    for r in &rows {
        let name = cell_string(r, 0).unwrap_or_default();
        let col = cell_string(r, 1).unwrap_or_default();
        let ref_col = cell_string(r, 4).unwrap_or_default();
        match fks.last_mut() {
            Some(k) if k.name == name => {
                k.columns.push(col);
                if let Some((_, cols)) = k.references.as_mut() {
                    cols.push(ref_col);
                }
            }
            _ => {
                let referenced = ObjectRef {
                    database: db.clone(),
                    schema: cell_string(r, 2).unwrap_or_default(),
                    name: cell_string(r, 3).unwrap_or_default(),
                    kind: ObjectKind::Table,
                    object_id: cell_i64(r, 5).map(|v| v as i32),
                };
                let mut actions = Vec::new();
                for (i, verb) in [(6, "ON DELETE"), (7, "ON UPDATE")] {
                    let a = cell_string(r, i).unwrap_or_default();
                    if !a.is_empty() && a != "NO_ACTION" {
                        actions.push(format!("{verb} {}", a.replace('_', " ")));
                    }
                }
                fks.push(KeyInfo {
                    name,
                    kind: KeyKind::ForeignKey,
                    columns: vec![col],
                    references: Some((referenced, vec![ref_col])),
                    definition: if actions.is_empty() { None } else { Some(actions.join(" ")) },
                });
            }
        }
    }
    keys.extend(fks);

    // Check constraints
    let rows = query_in_db(conn, &obj.database, |p| {
        format!(
            "SELECT cc.name, cc.definition, c.name FROM {p}sys.check_constraints cc \
             LEFT JOIN {p}sys.columns c ON c.object_id = cc.parent_object_id AND c.column_id = cc.parent_column_id \
             WHERE cc.parent_object_id = {} ORDER BY cc.name",
            object_id_expr(obj, p)
        )
    })
    .await?;
    keys.extend(rows.iter().map(|r| KeyInfo {
        name: cell_string(r, 0).unwrap_or_default(),
        kind: KeyKind::Check,
        columns: cell_string(r, 2).into_iter().collect(),
        references: None,
        definition: cell_string(r, 1),
    }));

    // Defaults
    let rows = query_in_db(conn, &obj.database, |p| {
        format!(
            "SELECT dc.name, dc.definition, c.name FROM {p}sys.default_constraints dc \
             JOIN {p}sys.columns c ON c.object_id = dc.parent_object_id AND c.column_id = dc.parent_column_id \
             WHERE dc.parent_object_id = {} ORDER BY c.column_id",
            object_id_expr(obj, p)
        )
    })
    .await?;
    keys.extend(rows.iter().map(|r| KeyInfo {
        name: cell_string(r, 0).unwrap_or_default(),
        kind: KeyKind::Default,
        columns: cell_string(r, 2).into_iter().collect(),
        references: None,
        definition: cell_string(r, 1),
    }));

    Ok(keys)
}

/// Objects + columns of every table / view / table function / table type in ≤ 3 queries.
pub(crate) async fn load_catalog(conn: &mut MssqlConnection, database: &str) -> Result<DatabaseCatalog> {
    let db = if database.is_empty() { conn.database.clone() } else { database.to_string() };
    let schemas = list_schemas(conn, &db).await?;
    let objects = list_objects(conn, &db).await?;
    let rows = query_in_db(conn, &db, |p| {
        format!(
            "SELECT c.object_id, {COLUMN_PROJECTION} FROM {p}sys.columns c \
             JOIN {p}sys.objects o ON o.object_id = c.object_id \
             {} \
             WHERE o.type IN ('U','V','TF','IF','FT','TT') AND (o.is_ms_shipped = 0 OR o.type = 'TT') \
             ORDER BY c.object_id, c.column_id",
            column_joins(p)
        )
    })
    .await?;
    let mut columns: HashMap<i32, Vec<ColumnInfo>> = HashMap::new();
    for r in &rows {
        let id = cell_i64(r, 0).unwrap_or(0) as i32;
        let list = columns.entry(id).or_default();
        let ordinal = list.len();
        list.push(column_from_row(r, 1, ordinal));
    }
    Ok(DatabaseCatalog { database: db, schemas, objects, columns, refreshed_at: Some(chrono::Utc::now()) })
}

/// `sys.sql_modules.definition` for a module object.
pub(crate) async fn object_definition(conn: &mut MssqlConnection, obj: &ObjectRef) -> Result<Option<String>> {
    let rows = query_in_db(conn, &obj.database, |p| {
        format!("SELECT definition FROM {p}sys.sql_modules WHERE object_id = {}", object_id_expr(obj, p))
    })
    .await?;
    Ok(rows.first().and_then(|r| cell_string(r, 0)))
}

/// Computed-column definitions and identity seed/increment for a table.
pub(crate) struct TableExtras {
    pub computed: HashMap<String, String>,
    pub identity: HashMap<String, (i64, i64)>,
}

pub(crate) async fn table_extras(conn: &mut MssqlConnection, obj: &ObjectRef) -> Result<TableExtras> {
    let computed_rows = query_in_db(conn, &obj.database, |p| {
        format!("SELECT name, definition FROM {p}sys.computed_columns WHERE object_id = {}", object_id_expr(obj, p))
    })
    .await?;
    let identity_rows = query_in_db(conn, &obj.database, |p| {
        format!(
            "SELECT name, CAST(seed_value AS bigint), CAST(increment_value AS bigint) FROM {p}sys.identity_columns WHERE object_id = {}",
            object_id_expr(obj, p)
        )
    })
    .await?;
    Ok(TableExtras {
        computed: computed_rows.iter().filter_map(|r| Some((cell_string(r, 0)?, cell_string(r, 1)?))).collect(),
        identity: identity_rows
            .iter()
            .filter_map(|r| Some((cell_string(r, 0)?, (cell_i64(r, 1).unwrap_or(1), cell_i64(r, 2).unwrap_or(1)))))
            .collect(),
    })
}

/// `base_object_name` of a synonym.
pub(crate) async fn synonym_target(conn: &mut MssqlConnection, obj: &ObjectRef) -> Result<Option<String>> {
    let rows = query_in_db(conn, &obj.database, |p| {
        format!("SELECT base_object_name FROM {p}sys.synonyms WHERE object_id = {}", object_id_expr(obj, p))
    })
    .await?;
    Ok(rows.first().and_then(|r| cell_string(r, 0)))
}

pub(crate) struct SequenceInfo {
    pub type_name: String,
    pub start: i64,
    pub increment: i64,
    pub min: Option<i64>,
    pub max: Option<i64>,
    pub cycle: bool,
}

pub(crate) async fn sequence_info(conn: &mut MssqlConnection, obj: &ObjectRef) -> Result<Option<SequenceInfo>> {
    let rows = query_in_db(conn, &obj.database, |p| {
        format!(
            "SELECT TYPE_NAME(user_type_id), CAST(start_value AS bigint), CAST(increment AS bigint), CAST(minimum_value AS bigint), CAST(maximum_value AS bigint), is_cycling \
             FROM {p}sys.sequences WHERE object_id = {}",
            object_id_expr(obj, p)
        )
    })
    .await?;
    Ok(rows.first().map(|r| SequenceInfo {
        type_name: cell_string(r, 0).unwrap_or_else(|| "bigint".into()),
        start: cell_i64(r, 1).unwrap_or(1),
        increment: cell_i64(r, 2).unwrap_or(1),
        min: cell_i64(r, 3),
        max: cell_i64(r, 4),
        cycle: cell_bool(r, 5),
    }))
}
