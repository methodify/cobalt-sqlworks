//! "Script as …" generation. The text builders are pure functions over catalog data so they
//! can be unit-tested without a server.

use super::catalog::{self, br, TableExtras};
use super::MssqlConnection;
use crate::{DriverError, Result, ScriptKind};
use cobalt_core::*;
use std::collections::HashMap;

pub(crate) async fn script(conn: &mut MssqlConnection, obj: &ObjectRef, kind: ScriptKind) -> Result<String> {
    use ObjectKind as K;
    let head = header(&obj.database);
    match (kind, obj.kind) {
        (ScriptKind::Drop, _) => Ok(format!("{head}{}", drop_script(obj))),

        (ScriptKind::Select, K::Table | K::View | K::Synonym) => {
            let columns = catalog::list_columns(conn, obj).await?;
            Ok(format!("{head}{}", select_script(obj, &columns)))
        }
        (ScriptKind::Select | ScriptKind::Execute, K::TableFunction) => {
            let params = catalog::list_parameters(conn, obj).await?;
            Ok(format!("{head}{}", table_function_script(obj, &params)))
        }
        (ScriptKind::Execute, K::Procedure) => {
            let params = catalog::list_parameters(conn, obj).await?;
            Ok(format!("{head}{}", execute_script(obj, &params)))
        }
        (ScriptKind::Execute, K::ScalarFunction) => {
            let params = catalog::list_parameters(conn, obj).await?;
            Ok(format!("{head}{}", scalar_function_script(obj, &params)))
        }

        (ScriptKind::Create | ScriptKind::Alter, K::Table) => {
            let columns = catalog::list_columns(conn, obj).await?;
            let keys = catalog::list_keys(conn, obj).await?;
            let indexes = catalog::list_indexes(conn, obj).await?;
            let extras = catalog::table_extras(conn, obj).await?;
            let body = create_table_script(obj, &columns, &keys, &indexes, &extras);
            if kind == ScriptKind::Alter {
                Ok(format!("{head}-- SQL Server has no ALTER form of a full table definition; edit the CREATE below\n-- or add ALTER TABLE {} statements.\n{body}", obj.bracketed()))
            } else {
                Ok(format!("{head}{body}"))
            }
        }
        (ScriptKind::Create | ScriptKind::Alter, K::View | K::Procedure | K::ScalarFunction | K::TableFunction | K::AggregateFunction | K::Trigger) => {
            let def = catalog::object_definition(conn, obj)
                .await?
                .ok_or_else(|| DriverError::Other(format!("no definition available for {} (encrypted or not a module)", obj)))?;
            let def = if kind == ScriptKind::Alter { alter_definition(&def) } else { def };
            Ok(format!("{head}{}\nGO\n", def.trim_end()))
        }
        (ScriptKind::Create | ScriptKind::Alter, K::Synonym) => {
            let target = catalog::synonym_target(conn, obj).await?.unwrap_or_default();
            let verb = if kind == ScriptKind::Alter { "DROP SYNONYM IF EXISTS " } else { "" };
            let pre = if verb.is_empty() { String::new() } else { format!("{verb}{};\nGO\n", obj.bracketed()) };
            Ok(format!("{head}{pre}CREATE SYNONYM {} FOR {target};\nGO\n", obj.bracketed()))
        }
        (ScriptKind::Create | ScriptKind::Alter, K::Sequence) => {
            let info = catalog::sequence_info(conn, obj).await?.ok_or_else(|| DriverError::Other(format!("sequence {} not found", obj)))?;
            let verb = if kind == ScriptKind::Alter { "ALTER" } else { "CREATE" };
            let as_type = if kind == ScriptKind::Alter { String::new() } else { format!(" AS {}", info.type_name) };
            let start = if kind == ScriptKind::Alter { format!("RESTART WITH {}", info.start) } else { format!("START WITH {}", info.start) };
            let mut s = format!("{verb} SEQUENCE {}{as_type}\n    {start}\n    INCREMENT BY {}", obj.bracketed(), info.increment);
            if let Some(min) = info.min {
                s.push_str(&format!("\n    MINVALUE {min}"));
            }
            if let Some(max) = info.max {
                s.push_str(&format!("\n    MAXVALUE {max}"));
            }
            s.push_str(if info.cycle { "\n    CYCLE;" } else { "\n    NO CYCLE;" });
            Ok(format!("{head}{s}\nGO\n"))
        }
        (ScriptKind::Create | ScriptKind::Alter, K::TableType) => {
            let columns = catalog::list_columns(conn, obj).await?;
            let indexes = catalog::list_indexes(conn, obj).await?;
            Ok(format!("{head}{}", create_table_type_script(obj, &columns, &indexes)))
        }
        (k, o) => Err(DriverError::Other(format!("Script as {k:?} is not available for {}", o.label()))),
    }
}

pub fn header(database: &str) -> String {
    if database.is_empty() {
        String::new()
    } else {
        format!("USE {}\nGO\n\n", br(database))
    }
}

/// `sys.objects.type` filter for `OBJECT_ID(name, type)`.
fn object_type_code(kind: ObjectKind) -> Option<&'static str> {
    Some(match kind {
        ObjectKind::Table => "U",
        ObjectKind::View => "V",
        ObjectKind::Procedure => "P",
        ObjectKind::Synonym => "SN",
        ObjectKind::Sequence => "SO",
        ObjectKind::Trigger => "TR",
        _ => return None,
    })
}

fn drop_verb(kind: ObjectKind) -> &'static str {
    match kind {
        ObjectKind::Table => "TABLE",
        ObjectKind::View => "VIEW",
        ObjectKind::Procedure => "PROCEDURE",
        ObjectKind::ScalarFunction | ObjectKind::TableFunction | ObjectKind::AggregateFunction => "FUNCTION",
        ObjectKind::Synonym => "SYNONYM",
        ObjectKind::Sequence => "SEQUENCE",
        ObjectKind::TableType => "TYPE",
        ObjectKind::Trigger => "TRIGGER",
        _ => "OBJECT",
    }
}

pub fn drop_script(obj: &ObjectRef) -> String {
    let name = obj.bracketed();
    let verb = drop_verb(obj.kind);
    let guard = match obj.kind {
        ObjectKind::TableType => format!("IF TYPE_ID(N'{name}') IS NOT NULL"),
        k => match object_type_code(k) {
            Some(code) => format!("IF OBJECT_ID(N'{name}', N'{code}') IS NOT NULL"),
            None => format!("IF OBJECT_ID(N'{name}') IS NOT NULL"),
        },
    };
    format!("{guard}\n    DROP {verb} {name};\nGO\n")
}

pub fn select_script(obj: &ObjectRef, columns: &[ColumnInfo]) -> String {
    if columns.is_empty() {
        return format!("SELECT TOP (1000) *\nFROM {};\n", obj.bracketed());
    }
    let cols: Vec<String> = columns.iter().map(|c| format!("       {}", br(&c.name))).collect();
    format!("SELECT TOP (1000){}\nFROM {};\n", cols.join(",\n").trim_start_matches("      "), obj.bracketed())
}

fn declare_block(params: &[ParameterInfo]) -> String {
    params.iter().map(|p| format!("DECLARE {} {};\n", p.name, p.sql_type)).collect()
}

pub fn execute_script(obj: &ObjectRef, params: &[ParameterInfo]) -> String {
    let mut s = declare_block(params);
    if !params.is_empty() {
        s.push('\n');
    }
    s.push_str(&format!("EXEC {}", obj.bracketed()));
    let args: Vec<String> = params
        .iter()
        .map(|p| if p.is_output { format!("    {0} = {0} OUTPUT", p.name) } else { format!("    {0} = {0}", p.name) })
        .collect();
    if args.is_empty() {
        s.push_str(";\n");
    } else {
        s.push('\n');
        s.push_str(&args.join(",\n"));
        s.push_str(";\n");
    }
    let outs: Vec<String> = params.iter().filter(|p| p.is_output).map(|p| format!("{0} AS [{0}]", p.name)).collect();
    if !outs.is_empty() {
        s.push_str(&format!("\nSELECT {};\n", outs.join(", ")));
    }
    s.push_str("GO\n");
    s
}

pub fn scalar_function_script(obj: &ObjectRef, params: &[ParameterInfo]) -> String {
    let args: Vec<String> = params.iter().map(|p| p.name.clone()).collect();
    format!("{}\nSELECT {}({}) AS result;\nGO\n", declare_block(params), obj.bracketed(), args.join(", "))
}

pub fn table_function_script(obj: &ObjectRef, params: &[ParameterInfo]) -> String {
    let args: Vec<String> = params.iter().map(|p| p.name.clone()).collect();
    format!("{}\nSELECT TOP (1000) *\nFROM {}({});\nGO\n", declare_block(params), obj.bracketed(), args.join(", "))
}

fn column_ddl(c: &ColumnInfo, extras: &TableExtras) -> String {
    if let Some(def) = extras.computed.get(&c.name) {
        return format!("{} AS {def}", br(&c.name));
    }
    let mut s = format!("{} {}", br(&c.name), c.sql_type);
    if let Some(coll) = &c.collation {
        if c.sql_type.is_string() && !coll.is_empty() && !matches!(c.sql_type, SqlType::Xml | SqlType::Json) {
            s.push_str(&format!(" COLLATE {coll}"));
        }
    }
    s.push_str(if c.nullable { " NULL" } else { " NOT NULL" });
    if c.is_identity {
        let (seed, inc) = extras.identity.get(&c.name).copied().unwrap_or((1, 1));
        s.push_str(&format!(" IDENTITY({seed},{inc})"));
    }
    s
}

fn key_columns_ddl(cols: &[(String, bool)]) -> String {
    cols.iter().map(|(c, desc)| format!("{} {}", br(c), if *desc { "DESC" } else { "ASC" })).collect::<Vec<_>>().join(", ")
}

fn constraint_index_ddl(ix: &IndexInfo) -> String {
    let what = if ix.is_primary_key { "PRIMARY KEY" } else { "UNIQUE" };
    let clustered = if ix.is_clustered { "CLUSTERED" } else { "NONCLUSTERED" };
    format!("CONSTRAINT {} {what} {clustered} ({})", br(&ix.name), key_columns_ddl(&ix.key_columns))
}

pub fn create_table_script(obj: &ObjectRef, columns: &[ColumnInfo], keys: &[KeyInfo], indexes: &[IndexInfo], extras: &TableExtras) -> String {
    let name = obj.bracketed();
    let mut lines: Vec<String> = columns.iter().map(|c| format!("    {}", column_ddl(c, extras))).collect();
    for ix in indexes.iter().filter(|i| i.is_primary_key || i.is_unique_constraint) {
        lines.push(format!("    {}", constraint_index_ddl(ix)));
    }
    let mut s = format!("CREATE TABLE {name} (\n{}\n);\nGO\n", lines.join(",\n"));

    let mut alters: Vec<String> = Vec::new();
    for k in keys {
        match k.kind {
            KeyKind::ForeignKey => {
                let (referenced, ref_cols) = match &k.references {
                    Some(r) => r,
                    None => continue,
                };
                let cols: Vec<String> = k.columns.iter().map(|c| br(c)).collect();
                let rcols: Vec<String> = ref_cols.iter().map(|c| br(c)).collect();
                let actions = k.definition.as_deref().map(|d| format!(" {d}")).unwrap_or_default();
                alters.push(format!(
                    "ALTER TABLE {name} ADD CONSTRAINT {} FOREIGN KEY ({}) REFERENCES {} ({}){actions};",
                    br(&k.name),
                    cols.join(", "),
                    referenced.bracketed(),
                    rcols.join(", ")
                ));
            }
            KeyKind::Check => {
                alters.push(format!("ALTER TABLE {name} ADD CONSTRAINT {} CHECK {};", br(&k.name), k.definition.clone().unwrap_or_default()));
            }
            KeyKind::Default => {
                if let Some(col) = k.columns.first() {
                    alters.push(format!("ALTER TABLE {name} ADD CONSTRAINT {} DEFAULT {} FOR {};", br(&k.name), k.definition.clone().unwrap_or_default(), br(col)));
                }
            }
            KeyKind::PrimaryKey | KeyKind::Unique => {}
        }
    }
    if !alters.is_empty() {
        s.push('\n');
        s.push_str(&alters.join("\n"));
        s.push_str("\nGO\n");
    }

    let mut ix_lines: Vec<String> = Vec::new();
    for ix in indexes.iter().filter(|i| !i.is_primary_key && !i.is_unique_constraint) {
        if ix.key_columns.is_empty() && ix.included_columns.is_empty() {
            continue;
        }
        let unique = if ix.is_unique { "UNIQUE " } else { "" };
        let clustered = if ix.is_clustered { "CLUSTERED" } else { "NONCLUSTERED" };
        let mut line = format!("CREATE {unique}{clustered} INDEX {} ON {name} ({})", br(&ix.name), key_columns_ddl(&ix.key_columns));
        if !ix.included_columns.is_empty() {
            line.push_str(&format!(" INCLUDE ({})", ix.included_columns.iter().map(|c| br(c)).collect::<Vec<_>>().join(", ")));
        }
        if let Some(f) = &ix.filter {
            line.push_str(&format!(" WHERE {f}"));
        }
        line.push(';');
        ix_lines.push(line);
    }
    if !ix_lines.is_empty() {
        s.push('\n');
        s.push_str(&ix_lines.join("\n"));
        s.push_str("\nGO\n");
    }
    s
}

pub fn create_table_type_script(obj: &ObjectRef, columns: &[ColumnInfo], indexes: &[IndexInfo]) -> String {
    let extras = TableExtras { computed: HashMap::new(), identity: HashMap::new() };
    let mut lines: Vec<String> = columns.iter().map(|c| format!("    {}", column_ddl(c, &extras))).collect();
    for ix in indexes.iter().filter(|i| i.is_primary_key || i.is_unique_constraint) {
        let what = if ix.is_primary_key { "PRIMARY KEY" } else { "UNIQUE" };
        let clustered = if ix.is_clustered { "CLUSTERED" } else { "NONCLUSTERED" };
        lines.push(format!("    {what} {clustered} ({})", key_columns_ddl(&ix.key_columns)));
    }
    format!("CREATE TYPE {} AS TABLE (\n{}\n);\nGO\n", obj.bracketed(), lines.join(",\n"))
}

/// Rewrite the first `CREATE PROC|PROCEDURE|VIEW|FUNCTION|TRIGGER` into `CREATE OR ALTER …`.
pub fn alter_definition(def: &str) -> String {
    let lower = def.to_ascii_lowercase();
    let mut i = 0;
    while let Some(pos) = lower[i..].find("create") {
        let start = i + pos;
        let end = start + "create".len();
        let word_boundary_before = start == 0 || !lower.as_bytes()[start - 1].is_ascii_alphanumeric();
        if word_boundary_before {
            let rest = &lower[end..];
            let ws = rest.len() - rest.trim_start().len();
            if ws > 0 {
                let after = rest.trim_start();
                let is_or_alter = after.starts_with("or ") || after.starts_with("or\t") || after.starts_with("or\n") || after.starts_with("or\r");
                let keyword = ["procedure", "proc", "view", "function", "trigger"].iter().any(|k| after.starts_with(k) && !after[k.len()..].starts_with(|c: char| c.is_ascii_alphanumeric()));
                if is_or_alter {
                    return def.to_string();
                }
                if keyword {
                    // Byte offsets are shared between `lower` and `def` (ASCII lowercasing keeps lengths).
                    // Collapse the gap: SQL Server stores `CREATE OR ALTER X` modules as `CREATE   X`.
                    return format!("{}CREATE OR ALTER {}", &def[..start], &def[end + ws..]);
                }
            }
        }
        i = end;
    }
    def.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(schema: &str, name: &str, kind: ObjectKind) -> ObjectRef {
        ObjectRef { database: "cobalt_test".into(), schema: schema.into(), name: name.into(), kind, object_id: Some(1) }
    }

    fn child_table() -> (ObjectRef, Vec<ColumnInfo>, Vec<KeyInfo>, Vec<IndexInfo>, TableExtras) {
        let t = obj("dbo", "child", ObjectKind::Table);
        let mut id = ColumnInfo::new("id", SqlType::Int, false, 0);
        id.is_identity = true;
        id.in_primary_key = true;
        let columns = vec![id, ColumnInfo::new("big_id", SqlType::Int, false, 1), ColumnInfo::new("qty", SqlType::Int, false, 2)];
        let keys = vec![
            KeyInfo { name: "PK_child".into(), kind: KeyKind::PrimaryKey, columns: vec!["id".into()], references: None, definition: None },
            KeyInfo { name: "UQ_child".into(), kind: KeyKind::Unique, columns: vec!["big_id".into(), "qty".into()], references: None, definition: None },
            KeyInfo {
                name: "FK_child_big".into(),
                kind: KeyKind::ForeignKey,
                columns: vec!["big_id".into()],
                references: Some((obj("dbo", "big", ObjectKind::Table), vec!["id".into()])),
                definition: Some("ON DELETE CASCADE".into()),
            },
            KeyInfo { name: "CK_qty".into(), kind: KeyKind::Check, columns: vec!["qty".into()], references: None, definition: Some("([qty]>(0))".into()) },
            KeyInfo { name: "DF_qty".into(), kind: KeyKind::Default, columns: vec!["qty".into()], references: None, definition: Some("((1))".into()) },
        ];
        let indexes = vec![
            IndexInfo {
                name: "PK_child".into(),
                is_unique: true,
                is_clustered: true,
                is_primary_key: true,
                is_unique_constraint: false,
                index_type: "CLUSTERED".into(),
                key_columns: vec![("id".into(), false)],
                included_columns: vec![],
                filter: None,
            },
            IndexInfo {
                name: "UQ_child".into(),
                is_unique: true,
                is_clustered: false,
                is_primary_key: false,
                is_unique_constraint: true,
                index_type: "NONCLUSTERED".into(),
                key_columns: vec![("big_id".into(), false), ("qty".into(), false)],
                included_columns: vec![],
                filter: None,
            },
            IndexInfo {
                name: "IX_child_qty".into(),
                is_unique: false,
                is_clustered: false,
                is_primary_key: false,
                is_unique_constraint: false,
                index_type: "NONCLUSTERED".into(),
                key_columns: vec![("qty".into(), true)],
                included_columns: vec!["big_id".into()],
                filter: Some("([qty]>(1))".into()),
            },
        ];
        let extras = TableExtras { computed: HashMap::new(), identity: HashMap::from([("id".to_string(), (1, 1))]) };
        (t, columns, keys, indexes, extras)
    }

    #[test]
    fn scripts_create_table() {
        let (t, columns, keys, indexes, extras) = child_table();
        let s = create_table_script(&t, &columns, &keys, &indexes, &extras);
        assert!(s.starts_with("CREATE TABLE [dbo].[child] (\n    [id] int NOT NULL IDENTITY(1,1),\n    [big_id] int NOT NULL,\n    [qty] int NOT NULL,\n"), "{s}");
        assert!(s.contains("    CONSTRAINT [PK_child] PRIMARY KEY CLUSTERED ([id] ASC),\n    CONSTRAINT [UQ_child] UNIQUE NONCLUSTERED ([big_id] ASC, [qty] ASC)\n);\nGO\n"), "{s}");
        assert!(s.contains("ALTER TABLE [dbo].[child] ADD CONSTRAINT [FK_child_big] FOREIGN KEY ([big_id]) REFERENCES [dbo].[big] ([id]) ON DELETE CASCADE;"), "{s}");
        assert!(s.contains("ALTER TABLE [dbo].[child] ADD CONSTRAINT [CK_qty] CHECK ([qty]>(0));"), "{s}");
        assert!(s.contains("ALTER TABLE [dbo].[child] ADD CONSTRAINT [DF_qty] DEFAULT ((1)) FOR [qty];"), "{s}");
        assert!(s.contains("CREATE NONCLUSTERED INDEX [IX_child_qty] ON [dbo].[child] ([qty] DESC) INCLUDE ([big_id]) WHERE ([qty]>(1));"), "{s}");
    }

    #[test]
    fn scripts_select_execute_drop() {
        let (t, columns, ..) = child_table();
        assert_eq!(select_script(&t, &columns), "SELECT TOP (1000) [id],\n       [big_id],\n       [qty]\nFROM [dbo].[child];\n");

        let p = obj("dbo", "p_multi", ObjectKind::Procedure);
        let params = vec![
            ParameterInfo { name: "@n".into(), sql_type: SqlType::Int, is_output: false, has_default: true, ordinal: 0 },
            ParameterInfo { name: "@out".into(), sql_type: SqlType::NVarChar { len: Some(50) }, is_output: true, has_default: false, ordinal: 1 },
        ];
        let s = execute_script(&p, &params);
        assert_eq!(
            s,
            "DECLARE @n int;\nDECLARE @out nvarchar(50);\n\nEXEC [dbo].[p_multi]\n    @n = @n,\n    @out = @out OUTPUT;\n\nSELECT @out AS [@out];\nGO\n"
        );
        assert_eq!(execute_script(&p, &[]), "EXEC [dbo].[p_multi];\nGO\n");

        assert_eq!(drop_script(&t), "IF OBJECT_ID(N'[dbo].[child]', N'U') IS NOT NULL\n    DROP TABLE [dbo].[child];\nGO\n");
        let f = obj("dbo", "f_scalar", ObjectKind::ScalarFunction);
        assert_eq!(drop_script(&f), "IF OBJECT_ID(N'[dbo].[f_scalar]') IS NOT NULL\n    DROP FUNCTION [dbo].[f_scalar];\nGO\n");
        let tt = obj("dbo", "IdList", ObjectKind::TableType);
        assert!(drop_script(&tt).starts_with("IF TYPE_ID(N'[dbo].[IdList]') IS NOT NULL\n    DROP TYPE [dbo].[IdList];"));
        assert_eq!(header("cobalt_test"), "USE [cobalt_test]\nGO\n\n");
    }

    #[test]
    fn alter_rewrites_create() {
        assert_eq!(alter_definition("CREATE PROCEDURE dbo.p AS SELECT 1"), "CREATE OR ALTER PROCEDURE dbo.p AS SELECT 1");
        assert_eq!(alter_definition("-- created by x\ncreate   view v as select 1"), "-- created by x\nCREATE OR ALTER view v as select 1");
        assert_eq!(alter_definition("CREATE OR ALTER FUNCTION f() RETURNS int AS BEGIN RETURN 1 END"), "CREATE OR ALTER FUNCTION f() RETURNS int AS BEGIN RETURN 1 END");
        assert_eq!(alter_definition("CREATE TABLE t (x int)"), "CREATE TABLE t (x int)");
    }
}
