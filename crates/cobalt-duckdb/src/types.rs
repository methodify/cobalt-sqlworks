//! DuckDB's type names to and from [`SqlType`], DDL text, and reading / writing whole tables
//! (the local engine's bridge and the exporters).

use crate::{open, quote, Connection};
use arrow::array::{Array, RecordBatch, RecordBatchIterator};
use arrow::datatypes::{DataType, SchemaRef};
use arrow::error::ArrowError;
use cobalt_core::{ColumnInfo, SqlType};
use std::path::Path;
use std::sync::Arc;

/// The DuckDB type a column of this SQL type is declared with.
pub fn duckdb_type(t: &SqlType) -> String {
    match t {
        SqlType::Bit => "BOOLEAN".into(),
        SqlType::TinyInt => "UTINYINT".into(),
        SqlType::SmallInt => "SMALLINT".into(),
        SqlType::Int => "INTEGER".into(),
        SqlType::BigInt => "BIGINT".into(),
        SqlType::Decimal { precision, scale } | SqlType::Numeric { precision, scale } => format!("DECIMAL({},{})", (*precision).clamp(1, 38), (*scale).min(38)),
        SqlType::Money => "DECIMAL(19,4)".into(),
        SqlType::SmallMoney => "DECIMAL(10,4)".into(),
        SqlType::Float => "DOUBLE".into(),
        SqlType::Real => "FLOAT".into(),
        SqlType::Date => "DATE".into(),
        SqlType::Time { .. } => "TIME".into(),
        SqlType::DateTime | SqlType::DateTime2 { .. } | SqlType::SmallDateTime => "TIMESTAMP".into(),
        SqlType::DateTimeOffset { .. } => "TIMESTAMP WITH TIME ZONE".into(),
        SqlType::Binary { .. } | SqlType::VarBinary { .. } | SqlType::Image | SqlType::Timestamp | SqlType::Geography | SqlType::Geometry | SqlType::HierarchyId => "BLOB".into(),
        SqlType::UniqueIdentifier => "UUID".into(),
        _ => "VARCHAR".into(),
    }
}

/// A DuckDB type name (`duckdb_columns().data_type`, `typeof()`) as a [`SqlType`].
pub fn sql_type_from_duckdb(name: &str) -> SqlType {
    let n = name.trim();
    let upper = n.to_ascii_uppercase();
    let base = upper.split(['(', '[']).next().unwrap_or("").trim().to_string();
    let args = || -> Vec<u8> { upper.split_once('(').and_then(|(_, r)| r.split(')').next()).map(|a| a.split(',').filter_map(|x| x.trim().parse::<u8>().ok()).collect()).unwrap_or_default() };
    if upper.ends_with("[]") {
        return SqlType::Other { name: n.to_string() };
    }
    match base.as_str() {
        "BOOLEAN" | "BOOL" => SqlType::Bit,
        "TINYINT" | "INT1" => SqlType::SmallInt,
        "UTINYINT" => SqlType::TinyInt,
        "SMALLINT" | "INT2" | "SHORT" => SqlType::SmallInt,
        "USMALLINT" | "INTEGER" | "INT4" | "INT" | "SIGNED" => SqlType::Int,
        "UINTEGER" | "BIGINT" | "INT8" | "LONG" => SqlType::BigInt,
        "UBIGINT" => SqlType::Decimal { precision: 20, scale: 0 },
        "HUGEINT" | "UHUGEINT" => SqlType::Decimal { precision: 38, scale: 0 },
        "DECIMAL" | "NUMERIC" => {
            let a = args();
            SqlType::Decimal { precision: a.first().copied().unwrap_or(18), scale: a.get(1).copied().unwrap_or(3) }
        }
        "DOUBLE" | "FLOAT8" => SqlType::Float,
        "FLOAT" | "REAL" | "FLOAT4" => SqlType::Real,
        "DATE" => SqlType::Date,
        "TIME" | "TIME WITH TIME ZONE" => SqlType::Time { scale: 6 },
        "TIMESTAMP" | "DATETIME" | "TIMESTAMP_S" | "TIMESTAMP_MS" | "TIMESTAMP_NS" => SqlType::DateTime2 { scale: 6 },
        "TIMESTAMP WITH TIME ZONE" | "TIMESTAMPTZ" => SqlType::DateTimeOffset { scale: 6 },
        "VARCHAR" | "TEXT" | "STRING" | "CHAR" | "BPCHAR" => SqlType::NVarChar { len: None },
        "BLOB" | "BYTEA" | "BINARY" | "VARBINARY" => SqlType::VarBinary { len: None },
        "UUID" => SqlType::UniqueIdentifier,
        "JSON" => SqlType::Json,
        _ => SqlType::Other { name: n.to_string() },
    }
}

/// `CREATE TABLE "t" ("a" INTEGER NOT NULL, …);` (the table name arrives quoted).
pub fn create_table_sql(table: &str, columns: &[(String, SqlType, bool)]) -> String {
    let cols: Vec<String> = columns.iter().map(|(n, t, nullable)| format!("    {} {}{}", quote(n), duckdb_type(t), if *nullable { "" } else { " NOT NULL" })).collect();
    format!("CREATE TABLE {table} (\n{}\n);", cols.join(",\n"))
}

/// Column names a DuckDB table can hold: names are compared without case, so collisions get
/// `_2`, `_3`… and an empty name becomes `column_N`.
pub fn unique_names<'a>(names: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out = Vec::new();
    for (i, n) in names.enumerate() {
        let base = if n.trim().is_empty() { format!("column_{}", i + 1) } else { n.to_string() };
        let mut name = base.clone();
        let mut k = 2;
        while !seen.insert(name.to_lowercase()) {
            name = format!("{base}_{k}");
            k += 1;
        }
        out.push(name);
    }
    out
}

/// How [`write_table`] treats a table that is already there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteMode {
    Create,
    Replace,
    Append,
}

/// A table's columns (`duckdb_columns`), with the primary key marked (`duckdb_constraints`).
/// `table` may be `schema.name` or a bare name (main).
pub fn table_columns(conn: &Connection, table: &str) -> std::result::Result<Vec<ColumnInfo>, String> {
    let (schema, name) = split_name(table);
    let sql = format!(
        "SELECT column_name, data_type, is_nullable, column_index FROM duckdb_columns() WHERE schema_name = {} AND table_name = {} ORDER BY column_index",
        crate::lit(&schema),
        crate::lit(&name)
    );
    let (_, batches) = conn.query(&sql)?;
    let mut cols = Vec::new();
    for b in &batches {
        let names = b.column(0).as_any().downcast_ref::<arrow::array::StringArray>().ok_or("duckdb_columns: column_name")?;
        let types = b.column(1).as_any().downcast_ref::<arrow::array::StringArray>().ok_or("duckdb_columns: data_type")?;
        let nulls = b.column(2).as_any().downcast_ref::<arrow::array::BooleanArray>().ok_or("duckdb_columns: is_nullable")?;
        for i in 0..b.num_rows() {
            let ord = cols.len();
            cols.push(ColumnInfo::new(names.value(i), sql_type_from_duckdb(types.value(i)), nulls.value(i), ord));
        }
    }
    let pk_sql = format!("SELECT unnest(constraint_column_names) FROM duckdb_constraints() WHERE schema_name = {} AND table_name = {} AND constraint_type = 'PRIMARY KEY'", crate::lit(&schema), crate::lit(&name));
    if let Ok((_, pk)) = conn.query(&pk_sql) {
        for b in &pk {
            if let Some(a) = b.column(0).as_any().downcast_ref::<arrow::array::StringArray>() {
                for i in 0..a.len() {
                    if let Some(c) = cols.iter_mut().find(|c| c.name.eq_ignore_ascii_case(a.value(i))) {
                        c.in_primary_key = true;
                    }
                }
            }
        }
    }
    Ok(cols)
}

/// Is `table` a table (not a view)?
pub fn is_table(conn: &Connection, table: &str) -> std::result::Result<bool, String> {
    let (schema, name) = split_name(table);
    let (_, b) = conn.query(&format!("SELECT count(*) FROM duckdb_tables() WHERE schema_name = {} AND table_name = {}", crate::lit(&schema), crate::lit(&name)))?;
    let n = b.first().and_then(|b| b.column(0).as_any().downcast_ref::<arrow::array::Int64Array>().map(|a| a.value(0))).unwrap_or(0);
    Ok(n > 0)
}

/// `schema.name` → (schema, name); a bare name is in `main`.
pub fn split_name(table: &str) -> (String, String) {
    match table.split_once('.') {
        Some((s, n)) if !s.is_empty() => (s.trim_matches('"').to_string(), n.trim_matches('"').to_string()),
        _ => ("main".to_string(), table.trim_matches('"').to_string()),
    }
}

/// `"schema"."name"` for the SQL text.
pub fn qualified(table: &str) -> String {
    let (s, n) = split_name(table);
    format!("{}.{}", quote(&s), quote(&n))
}

/// Does the table (or view) exist?
pub fn table_exists(conn: &Connection, table: &str) -> std::result::Result<bool, String> {
    let (schema, name) = split_name(table);
    let (_, b) = conn.query(&format!("SELECT count(*) FROM (SELECT table_name FROM duckdb_tables() WHERE schema_name = {s} AND table_name = {n} UNION ALL SELECT view_name FROM duckdb_views() WHERE schema_name = {s} AND view_name = {n})", s = crate::lit(&schema), n = crate::lit(&name)))?;
    let n = b.first().and_then(|b| b.column(0).as_any().downcast_ref::<arrow::array::Int64Array>().map(|a| a.value(0))).unwrap_or(0);
    Ok(n > 0)
}

/// Read a table (projected columns, at most `limit` rows) as Arrow batches with DuckDB's own
/// Arrow types; `projection` names columns of the table.
pub fn read_table(root: &Path, path: &Path, table: &str, projection: Option<&[String]>, limit: Option<usize>) -> std::result::Result<(SchemaRef, Vec<RecordBatch>), String> {
    let db = open(root, path, true)?;
    let conn = Connection::new(&db)?;
    let cols = match projection {
        Some(p) if !p.is_empty() => p.iter().map(|c| quote(c)).collect::<Vec<_>>().join(", "),
        Some(_) => "1 AS __one".to_string(),
        None => "*".to_string(),
    };
    let sql = format!("SELECT {cols} FROM {}{}", qualified(table), limit.map(|l| format!(" LIMIT {l}")).unwrap_or_default());
    conn.query(&sql)
}

/// The Arrow schema DuckDB gives a table's rows.
pub fn table_schema(root: &Path, path: &Path, table: &str) -> std::result::Result<SchemaRef, String> {
    read_table(root, path, table, None, Some(0)).map(|(s, _)| s)
}

/// Write batches into `table` of the database file at `path` (made when missing) as one
/// `CREATE TABLE … AS` / `INSERT … SELECT` over an Arrow stream, in one transaction. Returns the
/// rows written.
pub fn write_table(root: &Path, path: &Path, table: &str, mode: WriteMode, columns: &[ColumnInfo], batches: impl Iterator<Item = std::result::Result<RecordBatch, String>> + Send + 'static, progress: &mut dyn FnMut(u64) -> bool) -> std::result::Result<u64, String> {
    if table.trim().is_empty() {
        return Err("give the table a name".into());
    }
    let db = open(root, path, false)?;
    let mut conn = Connection::new(&db)?;
    let exists = table_exists(&conn, table)?;
    match (mode, exists) {
        (WriteMode::Create, true) => return Err(format!("table {table} already exists in {}; choose Replace or Append", path.display())),
        (WriteMode::Append, false) => return Err(format!("table {table} does not exist in {}; choose Create", path.display())),
        _ => {}
    }
    let names = unique_names(columns.iter().map(|c| c.name.as_str()));
    let fields: Vec<arrow::datatypes::Field> = columns.iter().zip(&names).map(|(c, n)| arrow::datatypes::Field::new(n.clone(), c.sql_type.arrow_type(), true)).collect();
    let schema: SchemaRef = Arc::new(arrow::datatypes::Schema::new(fields));
    let counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let c2 = Arc::clone(&counter);
    let s2 = Arc::clone(&schema);
    let reader = RecordBatchIterator::new(
        batches.map(move |b| {
            let b = b.map_err(|e| ArrowError::ExternalError(e.into()))?;
            c2.fetch_add(b.num_rows() as u64, std::sync::atomic::Ordering::Relaxed);
            conform(b, &s2).map_err(|e| ArrowError::ExternalError(e.into()))
        }),
        schema,
    );
    let view = "__cobalt_write";
    if !progress(0) {
        return Err("cancelled".into());
    }
    conn.register_arrow_view(view, Box::new(reader))?;
    let target = qualified(table);
    let result = (|| -> std::result::Result<(), String> {
        conn.execute("BEGIN")?;
        if mode == WriteMode::Replace && exists {
            conn.execute(&format!("DROP TABLE {target}"))?;
        }
        let sql = if mode == WriteMode::Append { format!("INSERT INTO {target} BY NAME SELECT * FROM {}", quote(view)) } else { format!("CREATE TABLE {target} AS SELECT * FROM {}", quote(view)) };
        conn.execute(&sql)?;
        conn.execute("COMMIT")?;
        Ok(())
    })();
    conn.drop_arrow_view(view);
    match result {
        Ok(()) => {
            let rows = counter.load(std::sync::atomic::Ordering::Relaxed);
            progress(rows);
            Ok(rows)
        }
        Err(e) => {
            let _ = conn.execute("ROLLBACK");
            Err(e)
        }
    }
}

/// A batch with the columns renamed and cast to `schema`'s types (what the Arrow view declares).
pub fn conform(batch: RecordBatch, schema: &SchemaRef) -> std::result::Result<RecordBatch, String> {
    let cols: std::result::Result<Vec<_>, String> = batch
        .columns()
        .iter()
        .zip(schema.fields())
        .map(|(c, f)| {
            if c.data_type() == f.data_type() {
                Ok(Arc::clone(c))
            } else {
                match arrow::compute::cast(c, f.data_type()) {
                    Ok(a) => Ok(a),
                    Err(_) if matches!(f.data_type(), DataType::Utf8 | DataType::LargeUtf8) => Ok(to_text(c, f.data_type())),
                    Err(e) => Err(format!("{}: {e}", f.name())),
                }
            }
        })
        .collect();
    RecordBatch::try_new(Arc::clone(schema), cols?).map_err(|e| e.to_string())
}

/// Any array as text (nested, interval, dictionary types) with Arrow's own formatter.
pub fn to_text(array: &arrow::array::ArrayRef, target: &DataType) -> arrow::array::ArrayRef {
    use arrow::util::display::{ArrayFormatter, FormatOptions};
    let opts = FormatOptions::default().with_display_error(true);
    let f = ArrayFormatter::try_new(array.as_ref(), &opts).ok();
    let values: Vec<Option<String>> = (0..array.len()).map(|i| if array.is_null(i) { None } else { f.as_ref().map(|f| f.value(i).to_string()) }).collect();
    if *target == DataType::LargeUtf8 {
        Arc::new(arrow::array::LargeStringArray::from(values))
    } else {
        Arc::new(arrow::array::StringArray::from(values))
    }
}

/// The columns of a result (the grid's types) from DuckDB's Arrow schema, and the batches as
/// the grid expects them: cast to the SQL types' Arrow types, nested values as text.
pub fn columns_for(schema: &SchemaRef) -> Vec<ColumnInfo> {
    schema.fields().iter().enumerate().map(|(i, f)| ColumnInfo::new(f.name().clone(), SqlType::from_arrow(f.data_type()), f.is_nullable(), i)).collect()
}

pub fn conform_to_columns(batch: RecordBatch, cols: &[ColumnInfo]) -> std::result::Result<RecordBatch, String> {
    let schema: SchemaRef = Arc::new(arrow::datatypes::Schema::new(cols.iter().map(|c| arrow::datatypes::Field::new(&c.name, c.sql_type.arrow_type(), true)).collect::<Vec<_>>()));
    conform(batch, &schema)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_names_round_trip() {
        assert_eq!(sql_type_from_duckdb("DECIMAL(18,3)"), SqlType::Decimal { precision: 18, scale: 3 });
        assert_eq!(sql_type_from_duckdb("TIMESTAMP WITH TIME ZONE"), SqlType::DateTimeOffset { scale: 6 });
        assert_eq!(sql_type_from_duckdb("VARCHAR"), SqlType::NVarChar { len: None });
        assert_eq!(sql_type_from_duckdb("INTEGER[]"), SqlType::Other { name: "INTEGER[]".into() });
        assert_eq!(duckdb_type(&SqlType::Money), "DECIMAL(19,4)");
        assert_eq!(duckdb_type(&SqlType::NVarChar { len: Some(50) }), "VARCHAR");
        assert_eq!(split_name("main.t"), ("main".into(), "t".into()));
        assert_eq!(split_name("t"), ("main".into(), "t".into()));
        assert_eq!(qualified("s.t"), "\"s\".\"t\"");
    }

    #[test]
    fn unique_names_fold_case() {
        assert_eq!(unique_names(["id", "ID", ""].into_iter()), vec!["id", "ID_2", "column_3"]);
    }

    #[test]
    fn writes_reads_and_appends() {
        let Some(root) = crate::tests::test_root() else { return };
        use arrow::array::{Int64Array, StringArray};
        use arrow::datatypes::{Field, Schema};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("w.duckdb");
        let cols = vec![ColumnInfo::new("id", SqlType::BigInt, false, 0), ColumnInfo::new("name", SqlType::NVarChar { len: Some(50) }, true, 1)];
        let schema = Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, false), Field::new("name", DataType::Utf8, true)]));
        let batch = RecordBatch::try_new(schema, vec![Arc::new(Int64Array::from(vec![1, 2, 3])), Arc::new(StringArray::from(vec![Some("a"), None, Some("c")]))]).unwrap();
        let n = write_table(&root, &path, "t", WriteMode::Create, &cols, vec![Ok(batch.clone())].into_iter(), &mut |_| true).unwrap();
        assert_eq!(n, 3);
        assert!(write_table(&root, &path, "t", WriteMode::Create, &cols, vec![Ok(batch.clone())].into_iter(), &mut |_| true).unwrap_err().contains("already exists"));
        write_table(&root, &path, "t", WriteMode::Append, &cols, vec![Ok(batch.clone())].into_iter(), &mut |_| true).unwrap();
        let (schema, b) = read_table(&root, &path, "t", Some(&["name".into()]), Some(4)).unwrap();
        assert_eq!(schema.fields().len(), 1);
        assert_eq!(b.iter().map(|b| b.num_rows()).sum::<usize>(), 4);
        let db = open(&root, &path, true).unwrap();
        let conn = Connection::new(&db).unwrap();
        let tc = table_columns(&conn, "t").unwrap();
        assert_eq!(tc.len(), 2);
        assert_eq!(tc[0].sql_type, SqlType::BigInt);
        assert!(table_exists(&conn, "t").unwrap());
        assert!(!table_exists(&conn, "nope").unwrap());
        // the file is open read-only through `db`: a write must wait for those handles to go
        assert!(write_table(&root, &path, "t", WriteMode::Replace, &cols, vec![Ok(batch.clone())].into_iter(), &mut |_| true).unwrap_err().contains("read-only"));
        drop(conn);
        drop(db);
        write_table(&root, &path, "t", WriteMode::Replace, &cols, vec![Ok(batch)].into_iter(), &mut |_| true).unwrap();
        let (_, b) = read_table(&root, &path, "t", None, None).unwrap();
        assert_eq!(b.iter().map(|b| b.num_rows()).sum::<usize>(), 3);
    }
}

#[cfg(test)]
mod lifetime {
    use super::*;
    /// Batches must stay valid after their chunks are destroyed and after lots of other work.
    #[test]
    fn batches_outlive_chunks() {
        let Some(root) = crate::tests::test_root() else { return };
        let dir = tempfile::tempdir().unwrap();
        let db = open(&root, &dir.path().join("o.duckdb"), false).unwrap();
        let conn = Connection::new(&db).unwrap();
        conn.execute("CREATE TABLE t AS SELECT range AS id, 'v' || range AS s, ['x', 'y'] AS tags, now() AS \"at\" FROM range(100000)").unwrap();
        let (schema, batches) = conn.query("SELECT * FROM t ORDER BY id").unwrap();
        let cols = columns_for(&schema);
        let conformed: Vec<RecordBatch> = batches.iter().map(|b| conform_to_columns(b.clone(), &cols).unwrap()).collect();
        // churn the allocator
        for _ in 0..20 {
            let _ = conn.query("SELECT 'z' || range AS s, [range, range] AS l FROM range(50000)").unwrap();
        }
        let mut total = 0;
        for b in &conformed {
            let s = b.column(1).as_any().downcast_ref::<arrow::array::StringArray>().unwrap();
            let tags = b.column(2).as_any().downcast_ref::<arrow::array::LargeStringArray>().unwrap();
            for i in 0..b.num_rows() {
                assert!(s.value(i).starts_with('v'), "{}", s.value(i));
                assert_eq!(tags.value(i), "[x, y]");
                total += 1;
            }
        }
        assert_eq!(total, 100000);
        let raw_total: usize = batches.iter().map(|b| b.num_rows()).sum();
        assert_eq!(raw_total, 100000);
        for b in &batches {
            let s = b.column(1).as_any().downcast_ref::<arrow::array::StringArray>().unwrap();
            assert!(s.value(0).starts_with('v'));
        }
    }
}
