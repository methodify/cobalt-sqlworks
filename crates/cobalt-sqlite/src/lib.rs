//! Shared SQLite plumbing for the driver (bulk loads into a connected file) and the exporters
//! (writing a result set into a `.sqlite` file): declared column types from [`SqlType`], the
//! `CREATE TABLE` text, and Arrow batches bound row by row into a prepared `INSERT`.
//!
//! SQLite stores whatever it is given, so the declared types here are chosen for their affinity
//! (`INTEGER`, `REAL`, `TEXT`, `BLOB`, `NUMERIC`) and for what a reader expects to see
//! (`BOOLEAN`, `DATE`, `DATETIME`). Dates and times are bound as ISO-8601 text, the form
//! SQLite's own date functions read; decimals as text that `NUMERIC` affinity turns into a
//! number when it can.

pub mod read;

use arrow::array::{Array, ArrayRef, AsArray, RecordBatch};
use arrow::datatypes::{DataType, Float16Type, Float32Type, Float64Type, Int16Type, Int32Type, Int64Type, Int8Type, UInt16Type, UInt32Type, UInt64Type, UInt8Type};
use arrow::util::display::{ArrayFormatter, FormatOptions};
use cobalt_core::{ColumnInfo, SqlType};
use rusqlite::types::Value;
use rusqlite::Connection;
use std::path::Path;
use std::sync::Arc;

/// The 16-byte header every SQLite 3 file starts with.
pub fn is_sqlite_file(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(path) else { return false };
    let mut head = [0u8; 16];
    f.read_exact(&mut head).map(|_| head.starts_with(b"SQLite format 3\0")).unwrap_or(false)
}

/// `"name"`, doubling any quote inside.
pub fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// The declared type a column of this SQL type gets in SQLite.
pub fn declared_type(t: &SqlType) -> String {
    match t {
        SqlType::Bit => "BOOLEAN".into(),
        SqlType::TinyInt | SqlType::SmallInt | SqlType::Int | SqlType::BigInt => "INTEGER".into(),
        SqlType::Decimal { precision, scale } | SqlType::Numeric { precision, scale } => format!("DECIMAL({precision},{scale})"),
        SqlType::Money => "DECIMAL(19,4)".into(),
        SqlType::SmallMoney => "DECIMAL(10,4)".into(),
        SqlType::Float | SqlType::Real => "REAL".into(),
        SqlType::Date => "DATE".into(),
        SqlType::Time { .. } => "TIME".into(),
        SqlType::DateTime | SqlType::DateTime2 { .. } | SqlType::SmallDateTime | SqlType::DateTimeOffset { .. } => "DATETIME".into(),
        SqlType::Binary { .. } | SqlType::VarBinary { .. } | SqlType::Image | SqlType::Timestamp | SqlType::Geography | SqlType::Geometry | SqlType::HierarchyId => "BLOB".into(),
        _ => "TEXT".into(),
    }
}

/// Column names a SQLite table can hold: SQLite compares names without case, so `id` and
/// `ID` (or two `id`s from a join) get `_2`, `_3`… suffixes; an empty name becomes `column_N`.
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

/// `CREATE TABLE "t" ("a" INTEGER NOT NULL, …);`
pub fn create_table_sql(table: &str, columns: &[(String, SqlType, bool)]) -> String {
    let cols: Vec<String> = columns.iter().map(|(n, t, nullable)| format!("    {} {}{}", quote(n), declared_type(t), if *nullable { "" } else { " NOT NULL" })).collect();
    format!("CREATE TABLE {} (\n{}\n);", quote(table), cols.join(",\n"))
}

/// `INSERT INTO "t" ("a", "b") VALUES (?, ?)`
pub fn insert_sql(table: &str, columns: &[String]) -> String {
    let names: Vec<String> = columns.iter().map(|c| quote(c)).collect();
    let marks: Vec<&str> = columns.iter().map(|_| "?").collect();
    format!("INSERT INTO {} ({}) VALUES ({})", quote(table), names.join(", "), marks.join(", "))
}

/// One cell as a SQLite value: integers and booleans as INTEGER, floats as REAL, strings as
/// TEXT, bytes as BLOB, everything else (dates, timestamps, decimals, nested) formatted as text.
pub fn bind_value(array: &ArrayRef, row: usize, formatter: Option<&ArrayFormatter<'_>>) -> Value {
    if array.is_null(row) {
        return Value::Null;
    }
    match array.data_type() {
        DataType::Boolean => Value::Integer(array.as_boolean().value(row) as i64),
        DataType::Int8 => Value::Integer(array.as_primitive::<Int8Type>().value(row) as i64),
        DataType::Int16 => Value::Integer(array.as_primitive::<Int16Type>().value(row) as i64),
        DataType::Int32 => Value::Integer(array.as_primitive::<Int32Type>().value(row) as i64),
        DataType::Int64 => Value::Integer(array.as_primitive::<Int64Type>().value(row)),
        DataType::UInt8 => Value::Integer(array.as_primitive::<UInt8Type>().value(row) as i64),
        DataType::UInt16 => Value::Integer(array.as_primitive::<UInt16Type>().value(row) as i64),
        DataType::UInt32 => Value::Integer(array.as_primitive::<UInt32Type>().value(row) as i64),
        DataType::UInt64 => {
            let v = array.as_primitive::<UInt64Type>().value(row);
            i64::try_from(v).map(Value::Integer).unwrap_or_else(|_| Value::Text(v.to_string()))
        }
        DataType::Float16 => Value::Real(array.as_primitive::<Float16Type>().value(row).to_f64()),
        DataType::Float32 => Value::Real(array.as_primitive::<Float32Type>().value(row) as f64),
        DataType::Float64 => Value::Real(array.as_primitive::<Float64Type>().value(row)),
        DataType::Utf8 => Value::Text(array.as_string::<i32>().value(row).to_string()),
        DataType::LargeUtf8 => Value::Text(array.as_string::<i64>().value(row).to_string()),
        DataType::Utf8View => Value::Text(array.as_string_view().value(row).to_string()),
        DataType::Binary => Value::Blob(array.as_binary::<i32>().value(row).to_vec()),
        DataType::LargeBinary => Value::Blob(array.as_binary::<i64>().value(row).to_vec()),
        DataType::BinaryView => Value::Blob(array.as_binary_view().value(row).to_vec()),
        DataType::FixedSizeBinary(_) => Value::Blob(array.as_fixed_size_binary().value(row).to_vec()),
        _ => match formatter {
            Some(f) => Value::Text(f.value(row).to_string()),
            None => Value::Null,
        },
    }
}

/// Bind every row of `batch` into the prepared insert. The caller owns the transaction.
pub fn insert_batch(conn: &Connection, insert: &str, batch: &RecordBatch) -> rusqlite::Result<u64> {
    let opts = FormatOptions::default().with_display_error(true).with_null("");
    let formatters: Vec<Option<ArrayFormatter<'_>>> = batch.columns().iter().map(|a| if needs_formatter(a.data_type()) { ArrayFormatter::try_new(a.as_ref(), &opts).ok() } else { None }).collect();
    let mut stmt = conn.prepare_cached(insert)?;
    let mut values: Vec<Value> = Vec::with_capacity(batch.num_columns());
    for row in 0..batch.num_rows() {
        values.clear();
        for (col, array) in batch.columns().iter().enumerate() {
            values.push(bind_value(array, row, formatters[col].as_ref()));
        }
        stmt.execute(rusqlite::params_from_iter(values.iter()))?;
    }
    Ok(batch.num_rows() as u64)
}

fn needs_formatter(t: &DataType) -> bool {
    !matches!(
        t,
        DataType::Boolean
            | DataType::Int8
            | DataType::Int16
            | DataType::Int32
            | DataType::Int64
            | DataType::UInt8
            | DataType::UInt16
            | DataType::UInt32
            | DataType::UInt64
            | DataType::Float16
            | DataType::Float32
            | DataType::Float64
            | DataType::Utf8
            | DataType::LargeUtf8
            | DataType::Utf8View
            | DataType::Binary
            | DataType::LargeBinary
            | DataType::BinaryView
            | DataType::FixedSizeBinary(_)
    )
}

/// The columns of a table or view (`PRAGMA table_xinfo`), typed by declared affinity.
pub fn table_columns(conn: &Connection, table: &str) -> rusqlite::Result<Vec<ColumnInfo>> {
    let mut st = conn.prepare(&format!("PRAGMA table_xinfo({})", quote(table)))?;
    let rows = st.query_map([], |r| {
        let name: String = r.get(1)?;
        let decl: Option<String> = r.get(2)?;
        let notnull: i64 = r.get(3)?;
        let pk: i64 = r.get(5)?;
        Ok((name, decl, notnull != 0, pk > 0))
    })?;
    let mut out = Vec::new();
    for (i, r) in rows.enumerate() {
        let (name, decl, notnull, pk) = r?;
        let mut c = ColumnInfo::new(&name, read::affinity(decl.as_deref()), !notnull, i);
        c.in_primary_key = pk;
        out.push(c);
    }
    Ok(out)
}

/// `SELECT count(*)` of a table.
pub fn table_row_count(conn: &Connection, table: &str) -> rusqlite::Result<u64> {
    conn.query_row(&format!("SELECT count(*) FROM {}", quote(table)), [], |r| r.get::<_, i64>(0)).map(|n| n.max(0) as u64)
}

/// A read-only connection to a database file (no mutex: one thread uses it).
pub fn open_read_only(path: &Path) -> Result<Connection, String> {
    let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX).map_err(|e| format!("{}: {e}", path.display()))?;
    conn.busy_timeout(std::time::Duration::from_secs(5)).map_err(|e| e.to_string())?;
    Ok(conn)
}

/// Read a table (some of its columns, at most `limit` rows) into Arrow batches of `batch_rows`.
/// `columns` are the table's columns as [`table_columns`] gives them; `projection` picks by
/// index (None = all).
pub fn read_table(path: &Path, table: &str, columns: &[ColumnInfo], projection: Option<&[usize]>, limit: Option<usize>, batch_rows: usize) -> Result<Vec<RecordBatch>, String> {
    let conn = open_read_only(path)?;
    let picked: Vec<ColumnInfo> = match projection {
        Some(idx) => idx.iter().enumerate().filter_map(|(o, &i)| columns.get(i).map(|c| ColumnInfo::new(&c.name, c.sql_type.clone(), c.nullable, o))).collect(),
        None => columns.to_vec(),
    };
    if picked.is_empty() {
        // a count-only scan: no columns, just the rows
        let n = table_row_count(&conn, table).map_err(|e| e.to_string())?;
        let n = limit.map(|l| (l as u64).min(n)).unwrap_or(n) as usize;
        let schema = Arc::new(arrow::datatypes::Schema::empty());
        let mut out = Vec::new();
        let mut left = n;
        while left > 0 {
            let take = left.min(batch_rows);
            out.push(RecordBatch::try_new_with_options(schema.clone(), vec![], &arrow::array::RecordBatchOptions::new().with_row_count(Some(take))).map_err(|e| e.to_string())?);
            left -= take;
        }
        return Ok(out);
    }
    let select = format!("SELECT {} FROM {}{}", picked.iter().map(|c| quote(&c.name)).collect::<Vec<_>>().join(", "), quote(table), limit.map(|l| format!(" LIMIT {l}")).unwrap_or_default());
    let mut st = conn.prepare(&select).map_err(|e| e.to_string())?;
    let n = picked.len();
    let mut rows = st.query([]).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    let mut buf: Vec<Vec<Value>> = Vec::with_capacity(batch_rows);
    let mut misfits = vec![0u64; n];
    loop {
        let row = rows.next().map_err(|e| e.to_string())?;
        match row {
            Some(r) => {
                buf.push(read::read_row(r, n));
                if buf.len() >= batch_rows {
                    out.push(read::to_batch(&buf, &picked, &mut misfits)?);
                    buf.clear();
                }
            }
            None => break,
        }
    }
    if !buf.is_empty() || out.is_empty() {
        out.push(read::to_batch(&buf, &picked, &mut misfits)?);
    }
    Ok(out)
}

/// How [`write_table`] treats a table that is already there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteMode {
    /// Fail when the table exists.
    Create,
    /// Drop and recreate.
    Replace,
    /// Keep the table and its rows; the columns are matched by name.
    Append,
}

/// Is `name` a table (not a view) in the main schema?
pub fn is_table(conn: &Connection, name: &str) -> rusqlite::Result<bool> {
    conn.query_row("SELECT count(*) FROM sqlite_schema WHERE type = 'table' AND name = ?1", [name], |r| r.get::<_, i64>(0)).map(|n| n > 0)
}

/// Does `table` exist in the main schema?
pub fn table_exists(conn: &Connection, table: &str) -> rusqlite::Result<bool> {
    conn.query_row("SELECT count(*) FROM sqlite_schema WHERE type IN ('table', 'view') AND name = ?1", [table], |r| r.get::<_, i64>(0)).map(|n| n > 0)
}

/// Write batches into `table` of the database file at `path` (created when missing), in one
/// transaction. `progress(rows_so_far)` returning false rolls back and fails with "cancelled".
/// Returns the rows written.
pub fn write_table(
    path: &Path,
    table: &str,
    mode: WriteMode,
    columns: &[ColumnInfo],
    batches: impl Iterator<Item = Result<RecordBatch, String>>,
    progress: &mut dyn FnMut(u64) -> bool,
) -> Result<u64, String> {
    if table.trim().is_empty() {
        return Err("give the table a name".into());
    }
    if path.exists() && !is_sqlite_file(path) && std::fs::metadata(path).map(|m| m.len() > 0).unwrap_or(false) {
        return Err(format!("{} is not a SQLite database", path.display()));
    }
    if let Some(dir) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    // a file this call creates goes away again when the write fails or is cancelled
    let created = !path.exists();
    let conn = Connection::open(path).map_err(|e| e.to_string())?;
    conn.busy_timeout(std::time::Duration::from_secs(5)).map_err(|e| e.to_string())?;
    let exists = table_exists(&conn, table).map_err(|e| e.to_string())?;
    conn.execute_batch("BEGIN").map_err(|e| e.to_string())?;
    let result = (|| -> Result<u64, String> {
        match (mode, exists) {
            (WriteMode::Create, true) => return Err(format!("table {table} already exists in {}; choose Replace or Append", path.display())),
            (WriteMode::Append, false) => return Err(format!("table {table} does not exist in {}; choose Create", path.display())),
            (WriteMode::Replace, true) => conn.execute_batch(&format!("DROP TABLE {};", quote(table))).map_err(|e| e.to_string())?,
            _ => {}
        }
        let names = unique_names(columns.iter().map(|c| c.name.as_str()));
        if mode != WriteMode::Append {
            let cols: Vec<(String, SqlType, bool)> = columns.iter().zip(&names).map(|(c, n)| (n.clone(), c.sql_type.clone(), c.nullable)).collect();
            conn.execute_batch(&create_table_sql(table, &cols)).map_err(|e| e.to_string())?;
        }
        let insert = insert_sql(table, &names);
        let mut rows = 0u64;
        if !progress(0) {
            return Err("cancelled".into());
        }
        for b in batches {
            let b = b?;
            rows += insert_batch(&conn, &insert, &b).map_err(|e| e.to_string())?;
            if !progress(rows) {
                return Err("cancelled".into());
            }
        }
        Ok(rows)
    })();
    match result {
        Ok(rows) => {
            conn.execute_batch("COMMIT").map_err(|e| e.to_string())?;
            Ok(rows)
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            drop(conn);
            if created {
                let _ = std::fs::remove_file(path);
            }
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::{BooleanArray, Date32Array, Decimal128Array, Float64Array, Int64Array, StringArray, TimestampMicrosecondArray};
    use arrow::datatypes::{Field, Schema};
    use std::sync::Arc;

    fn batch() -> (Vec<ColumnInfo>, RecordBatch) {
        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Int64, false),
            Field::new("ok", DataType::Boolean, true),
            Field::new("amount", DataType::Decimal128(10, 2), true),
            Field::new("ratio", DataType::Float64, true),
            Field::new("name", DataType::Utf8, true),
            Field::new("day", DataType::Date32, true),
            Field::new("at", DataType::Timestamp(arrow::datatypes::TimeUnit::Microsecond, None), true),
        ]));
        let b = RecordBatch::try_new(
            schema,
            vec![
                Arc::new(Int64Array::from(vec![1, 2])),
                Arc::new(BooleanArray::from(vec![Some(true), None])),
                Arc::new(Decimal128Array::from(vec![Some(1234), None]).with_precision_and_scale(10, 2).unwrap()),
                Arc::new(Float64Array::from(vec![Some(0.5), Some(1.5)])),
                Arc::new(StringArray::from(vec![Some("a\"b"), None])),
                Arc::new(Date32Array::from(vec![Some(19723), None])),
                Arc::new(TimestampMicrosecondArray::from(vec![Some(1_700_000_000_000_000), None])),
            ],
        )
        .unwrap();
        let cols = vec![
            ColumnInfo::new("id", SqlType::BigInt, false, 0),
            ColumnInfo::new("ok", SqlType::Bit, true, 1),
            ColumnInfo::new("amount", SqlType::Decimal { precision: 10, scale: 2 }, true, 2),
            ColumnInfo::new("ratio", SqlType::Float, true, 3),
            ColumnInfo::new("name", SqlType::NVarChar { len: Some(50) }, true, 4),
            ColumnInfo::new("day", SqlType::Date, true, 5),
            ColumnInfo::new("at", SqlType::DateTime2 { scale: 7 }, true, 6),
        ];
        (cols, b)
    }

    #[test]
    fn names_are_made_unique_without_case() {
        assert_eq!(unique_names(["id", "ID", "", "id"].into_iter()), vec!["id", "ID_2", "column_3", "id_3"]);
    }

    #[test]
    fn ddl_uses_affinities() {
        let (cols, _) = batch();
        let sql = create_table_sql("t", &cols.iter().map(|c| (c.name.clone(), c.sql_type.clone(), c.nullable)).collect::<Vec<_>>());
        assert!(sql.contains("\"id\" INTEGER NOT NULL"), "{sql}");
        assert!(sql.contains("\"ok\" BOOLEAN"), "{sql}");
        assert!(sql.contains("\"amount\" DECIMAL(10,2)"), "{sql}");
        assert!(sql.contains("\"day\" DATE"), "{sql}");
        assert!(sql.contains("\"at\" DATETIME"), "{sql}");
    }

    #[test]
    fn reads_back_with_projection_and_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r.sqlite");
        let (cols, b) = batch();
        write_table(&path, "t", WriteMode::Create, &cols, vec![Ok(b)].into_iter(), &mut |_| true).unwrap();
        let conn = Connection::open(&path).unwrap();
        let tc = table_columns(&conn, "t").unwrap();
        assert_eq!(tc.len(), 7);
        assert_eq!(tc[0].sql_type, SqlType::BigInt);
        assert!(!tc[0].nullable);
        assert_eq!(tc[2].sql_type, SqlType::Float, "DECIMAL(10,2) reads as a float");
        assert_eq!(tc[4].sql_type, SqlType::NVarChar { len: None });
        assert_eq!(table_row_count(&conn, "t").unwrap(), 2);
        drop(conn);
        let batches = read_table(&path, "t", &tc, Some(&[4, 0]), Some(1), 1000).unwrap();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].num_rows(), 1);
        assert_eq!(batches[0].num_columns(), 2);
        assert_eq!(batches[0].schema().field(0).name(), "name");
        let empty = read_table(&path, "t", &tc, Some(&[]), None, 1000).unwrap();
        assert_eq!(empty.iter().map(|b| b.num_rows()).sum::<usize>(), 2, "a count-only scan keeps the row count");
    }

    #[test]
    fn writes_then_appends_then_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.sqlite");
        let (cols, b) = batch();
        let mut seen = Vec::new();
        let n = write_table(&path, "t", WriteMode::Create, &cols, vec![Ok(b.clone())].into_iter(), &mut |r| {
            seen.push(r);
            true
        })
        .unwrap();
        assert_eq!(n, 2);
        assert_eq!(seen, vec![0, 2]);
        let conn = Connection::open(&path).unwrap();
        let (name, day, at, amount, ok): (String, String, String, f64, i64) = conn
            .query_row("SELECT name, day, at, amount, ok FROM t WHERE id = 1", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
            .unwrap();
        assert_eq!(name, "a\"b");
        assert_eq!(day, "2024-01-01");
        assert!(at.starts_with("2023-11-14T22:13:20"), "{at}");
        assert_eq!(amount, 12.34);
        assert_eq!(ok, 1);
        let nulls: i64 = conn.query_row("SELECT count(*) FROM t WHERE id = 2 AND name IS NULL AND day IS NULL AND ok IS NULL", [], |r| r.get(0)).unwrap();
        assert_eq!(nulls, 1);
        drop(conn);
        assert!(write_table(&path, "t", WriteMode::Create, &cols, vec![Ok(b.clone())].into_iter(), &mut |_| true).unwrap_err().contains("already exists"));
        write_table(&path, "t", WriteMode::Append, &cols, vec![Ok(b.clone())].into_iter(), &mut |_| true).unwrap();
        let conn = Connection::open(&path).unwrap();
        assert_eq!(conn.query_row("SELECT count(*) FROM t", [], |r| r.get::<_, i64>(0)).unwrap(), 4);
        drop(conn);
        write_table(&path, "t", WriteMode::Replace, &cols, vec![Ok(b.clone())].into_iter(), &mut |_| true).unwrap();
        let conn = Connection::open(&path).unwrap();
        assert_eq!(conn.query_row("SELECT count(*) FROM t", [], |r| r.get::<_, i64>(0)).unwrap(), 2);
        drop(conn);
        // a cancel rolls back: the table keeps its two rows
        let e = write_table(&path, "t", WriteMode::Append, &cols, vec![Ok(b)].into_iter(), &mut |r| r == 0).unwrap_err();
        assert_eq!(e, "cancelled");
        let conn = Connection::open(&path).unwrap();
        assert_eq!(conn.query_row("SELECT count(*) FROM t", [], |r| r.get::<_, i64>(0)).unwrap(), 2);
    }
}
