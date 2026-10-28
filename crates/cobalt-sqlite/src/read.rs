//! Reading SQLite rows into Arrow: declared affinities to [`SqlType`], storage classes for
//! expressions, and owned rows into a batch with the misfits (values of another storage class
//! that do not coerce) counted per column.

use arrow::array::{ArrayRef, BinaryBuilder, Float64Builder, Int64Builder, LargeBinaryBuilder, LargeStringBuilder, StringBuilder};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use cobalt_core::{ColumnInfo, SqlType};
use rusqlite::types::ValueRef;
use std::sync::Arc;

/// A column's affinity from its declared type (SQLite's rules), as the SQL type the grid shows.
pub fn affinity(decl: Option<&str>) -> SqlType {
    let d = decl.unwrap_or("").to_ascii_uppercase();
    if d.contains("INT") {
        SqlType::BigInt
    } else if d.contains("CHAR") || d.contains("CLOB") || d.contains("TEXT") {
        SqlType::NVarChar { len: None }
    } else if d.contains("BLOB") || d.is_empty() {
        if d.is_empty() { SqlType::NVarChar { len: None } } else { SqlType::VarBinary { len: None } }
    } else if d.contains("REAL") || d.contains("FLOA") || d.contains("DOUB") {
        SqlType::Float
    } else if d.contains("BOOL") {
        SqlType::BigInt
    } else if d.contains("DEC") || d.contains("NUM") || d.contains("MONEY") {
        // NUMERIC affinity holds integers and reals: a float column shows both
        SqlType::Float
    } else {
        // NUMERIC affinity: SQLite keeps what it is given; shown as text
        SqlType::NVarChar { len: None }
    }
}

pub fn storage_class_type(v: &rusqlite::types::Value) -> Option<SqlType> {
    use rusqlite::types::Value as V;
    match v {
        V::Integer(_) => Some(SqlType::BigInt),
        V::Real(_) => Some(SqlType::Float),
        V::Text(_) => Some(SqlType::NVarChar { len: None }),
        V::Blob(_) => Some(SqlType::VarBinary { len: None }),
        V::Null => None,
    }
}

/// Column types for a result: the declared affinity where the statement names a column, else
/// the first non-null storage class in the first chunk, else text.
pub fn column_types(decls: &[Option<String>], first: &[Vec<rusqlite::types::Value>]) -> Vec<SqlType> {
    decls
        .iter()
        .enumerate()
        .map(|(i, d)| match d {
            Some(d) if !d.is_empty() => affinity(Some(d)),
            _ => first.iter().find_map(|r| storage_class_type(&r[i])).unwrap_or(SqlType::NVarChar { len: None }),
        })
        .collect()
}

/// The declared type of every result column (`None` for expressions), read before the rows:
/// rusqlite lets go of the statement once the rows are exhausted.
pub fn declared_types(stmt: &rusqlite::Statement<'_>) -> Vec<Option<String>> {
    stmt.columns().iter().map(|c| c.decl_type().map(str::to_string)).collect()
}

pub fn arrow_schema(cols: &[ColumnInfo]) -> Arc<Schema> {
    Arc::new(Schema::new(cols.iter().map(|c| Field::new(&c.name, c.sql_type.arrow_type(), true)).collect::<Vec<_>>()))
}

/// Rows (as owned SQLite values) into one Arrow batch; `misfits[i]` counts values that could
/// not be coerced into column `i`.
pub fn to_batch(rows: &[Vec<rusqlite::types::Value>], cols: &[ColumnInfo], misfits: &mut [u64]) -> Result<RecordBatch, String> {
    use rusqlite::types::Value as V;
    let mut arrays: Vec<ArrayRef> = Vec::with_capacity(cols.len());
    for (i, c) in cols.iter().enumerate() {
        let arr: ArrayRef = match c.sql_type.arrow_type() {
            DataType::Int64 => {
                let mut b = Int64Builder::with_capacity(rows.len());
                for r in rows {
                    match &r[i] {
                        V::Integer(x) => b.append_value(*x),
                        V::Real(x) if x.fract() == 0.0 => b.append_value(*x as i64),
                        V::Text(s) if s.trim().parse::<i64>().is_ok() => b.append_value(s.trim().parse::<i64>().unwrap()),
                        V::Null => b.append_null(),
                        _ => {
                            misfits[i] += 1;
                            b.append_null();
                        }
                    }
                }
                Arc::new(b.finish())
            }
            DataType::Float64 => {
                let mut b = Float64Builder::with_capacity(rows.len());
                for r in rows {
                    match &r[i] {
                        V::Real(x) => b.append_value(*x),
                        V::Integer(x) => b.append_value(*x as f64),
                        V::Text(s) if s.trim().parse::<f64>().is_ok() => b.append_value(s.trim().parse::<f64>().unwrap()),
                        V::Null => b.append_null(),
                        _ => {
                            misfits[i] += 1;
                            b.append_null();
                        }
                    }
                }
                Arc::new(b.finish())
            }
            DataType::Binary | DataType::LargeBinary => {
                let large = matches!(c.sql_type.arrow_type(), DataType::LargeBinary);
                let mut b = BinaryBuilder::new();
                let mut lb = LargeBinaryBuilder::new();
                for r in rows {
                    let v: Option<&[u8]> = match &r[i] {
                        V::Blob(x) => Some(x.as_slice()),
                        V::Text(s) => Some(s.as_bytes()),
                        V::Null => None,
                        _ => {
                            misfits[i] += 1;
                            None
                        }
                    };
                    match (large, v) {
                        (true, Some(x)) => lb.append_value(x),
                        (true, None) => lb.append_null(),
                        (false, Some(x)) => b.append_value(x),
                        (false, None) => b.append_null(),
                    }
                }
                if large { Arc::new(lb.finish()) } else { Arc::new(b.finish()) }
            }
            other => {
                // text takes everything; `nvarchar(max)` is LargeUtf8 in the grid
                let large = matches!(other, DataType::LargeUtf8);
                let mut b = StringBuilder::new();
                let mut lb = LargeStringBuilder::new();
                for r in rows {
                    let v: Option<String> = match &r[i] {
                        V::Text(s) => Some(s.clone()),
                        V::Integer(x) => Some(x.to_string()),
                        V::Real(x) => Some(x.to_string()),
                        V::Blob(x) => Some(format!("0x{}", x.iter().take(64).map(|b| format!("{b:02x}")).collect::<String>())),
                        V::Null => None,
                    };
                    match (large, v) {
                        (true, Some(x)) => lb.append_value(x),
                        (true, None) => lb.append_null(),
                        (false, Some(x)) => b.append_value(x),
                        (false, None) => b.append_null(),
                    }
                }
                if large { Arc::new(lb.finish()) } else { Arc::new(b.finish()) }
            }
        };
        arrays.push(arr);
    }
    RecordBatch::try_new(arrow_schema(cols), arrays).map_err(|e| e.to_string())
}

pub fn read_row(row: &rusqlite::Row<'_>, n: usize) -> Vec<rusqlite::types::Value> {
    (0..n)
        .map(|i| match row.get_ref(i) {
            Ok(ValueRef::Null) | Err(_) => rusqlite::types::Value::Null,
            Ok(ValueRef::Integer(x)) => rusqlite::types::Value::Integer(x),
            Ok(ValueRef::Real(x)) => rusqlite::types::Value::Real(x),
            Ok(ValueRef::Text(t)) => rusqlite::types::Value::Text(String::from_utf8_lossy(t).into_owned()),
            Ok(ValueRef::Blob(b)) => rusqlite::types::Value::Blob(b.to_vec()),
        })
        .collect()
}

