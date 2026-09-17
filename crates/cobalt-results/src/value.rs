use arrow::array::{Array, ArrayRef, AsArray};
use arrow::datatypes::{DataType, Decimal128Type, Float32Type, Float64Type, Int16Type, Int32Type, Int64Type, TimeUnit, UInt8Type};
use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};

/// An owned, typed cell value. Used by the cell viewer, JSON/INSERT copy, and summaries.
#[derive(Clone, Debug, PartialEq)]
pub enum CellValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    /// Decimal as (unscaled value, scale).
    Decimal(i128, i8),
    Text(String),
    Bytes(Vec<u8>),
    Date(NaiveDate),
    Time(NaiveTime),
    DateTime(NaiveDateTime),
    /// Timestamp with a UTC offset (datetimeoffset).
    DateTimeTz(DateTime<Utc>),
}

impl CellValue {
    pub fn from_array(arr: &ArrayRef, i: usize) -> CellValue {
        if arr.is_null(i) {
            return CellValue::Null;
        }
        match arr.data_type() {
            DataType::Boolean => CellValue::Bool(arr.as_boolean().value(i)),
            DataType::UInt8 => CellValue::Int(arr.as_primitive::<UInt8Type>().value(i) as i64),
            DataType::Int16 => CellValue::Int(arr.as_primitive::<Int16Type>().value(i) as i64),
            DataType::Int32 => CellValue::Int(arr.as_primitive::<Int32Type>().value(i) as i64),
            DataType::Int64 => CellValue::Int(arr.as_primitive::<Int64Type>().value(i)),
            DataType::Float32 => CellValue::Float(arr.as_primitive::<Float32Type>().value(i) as f64),
            DataType::Float64 => CellValue::Float(arr.as_primitive::<Float64Type>().value(i)),
            DataType::Decimal128(_, s) => CellValue::Decimal(arr.as_primitive::<Decimal128Type>().value(i), *s),
            DataType::Utf8 => CellValue::Text(arr.as_string::<i32>().value(i).to_owned()),
            DataType::LargeUtf8 => CellValue::Text(arr.as_string::<i64>().value(i).to_owned()),
            DataType::Binary => CellValue::Bytes(arr.as_binary::<i32>().value(i).to_vec()),
            DataType::LargeBinary => CellValue::Bytes(arr.as_binary::<i64>().value(i).to_vec()),
            DataType::Date32 => {
                let days = arr.as_primitive::<arrow::datatypes::Date32Type>().value(i);
                CellValue::Date(NaiveDate::from_ymd_opt(1970, 1, 1).unwrap() + chrono::Duration::days(days as i64))
            }
            DataType::Time64(TimeUnit::Nanosecond) => {
                let ns = arr.as_primitive::<arrow::datatypes::Time64NanosecondType>().value(i);
                CellValue::Time(NaiveTime::from_num_seconds_from_midnight_opt((ns / 1_000_000_000) as u32, (ns % 1_000_000_000) as u32).unwrap_or_default())
            }
            DataType::Timestamp(unit, tz) => {
                let raw = match unit {
                    TimeUnit::Second => arr.as_primitive::<arrow::datatypes::TimestampSecondType>().value(i) as i128 * 1_000_000_000,
                    TimeUnit::Millisecond => arr.as_primitive::<arrow::datatypes::TimestampMillisecondType>().value(i) as i128 * 1_000_000,
                    TimeUnit::Microsecond => arr.as_primitive::<arrow::datatypes::TimestampMicrosecondType>().value(i) as i128 * 1_000,
                    TimeUnit::Nanosecond => arr.as_primitive::<arrow::datatypes::TimestampNanosecondType>().value(i) as i128,
                };
                let secs = raw.div_euclid(1_000_000_000) as i64;
                let nsec = raw.rem_euclid(1_000_000_000) as u32;
                match DateTime::<Utc>::from_timestamp(secs, nsec) {
                    Some(dt) if tz.is_some() => CellValue::DateTimeTz(dt),
                    Some(dt) => CellValue::DateTime(dt.naive_utc()),
                    None => CellValue::Null,
                }
            }
            _ => CellValue::Text(arrow::util::display::array_value_to_string(arr, i).unwrap_or_default()),
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, CellValue::Null)
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            CellValue::Int(i) => Some(*i as f64),
            CellValue::Float(f) => Some(*f),
            CellValue::Decimal(v, s) => Some(*v as f64 / 10f64.powi(*s as i32)),
            CellValue::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            _ => None,
        }
    }

    /// Decimal rendered with its scale, e.g. (12345, 2) → "123.45".
    pub fn decimal_string(v: i128, scale: i8) -> String {
        if scale <= 0 {
            return format!("{}{}", v, "0".repeat((-scale) as usize));
        }
        let neg = v < 0;
        let digits = v.unsigned_abs().to_string();
        let scale = scale as usize;
        let s = if digits.len() <= scale {
            format!("0.{}{}", "0".repeat(scale - digits.len()), digits)
        } else {
            format!("{}.{}", &digits[..digits.len() - scale], &digits[digits.len() - scale..])
        };
        if neg { format!("-{s}") } else { s }
    }

    /// JSON representation (for copy-as-JSON / export).
    pub fn to_json(&self) -> serde_json::Value {
        use serde_json::Value;
        match self {
            CellValue::Null => Value::Null,
            CellValue::Bool(b) => Value::Bool(*b),
            CellValue::Int(i) => Value::from(*i),
            CellValue::Float(f) => serde_json::Number::from_f64(*f).map(Value::Number).unwrap_or(Value::Null),
            CellValue::Decimal(v, s) => {
                let s = Self::decimal_string(*v, *s);
                s.parse::<f64>().ok().and_then(serde_json::Number::from_f64).map(Value::Number).unwrap_or(Value::String(s))
            }
            CellValue::Text(t) => Value::String(t.clone()),
            CellValue::Bytes(b) => Value::String(format!("0x{}", hex(b))),
            CellValue::Date(d) => Value::String(d.to_string()),
            CellValue::Time(t) => Value::String(t.format("%H:%M:%S%.f").to_string()),
            CellValue::DateTime(dt) => Value::String(dt.format("%Y-%m-%dT%H:%M:%S%.f").to_string()),
            CellValue::DateTimeTz(dt) => Value::String(dt.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true)),
        }
    }

    /// T-SQL literal (for copy-as-INSERT / IN-list).
    pub fn to_sql_literal(&self) -> String {
        match self {
            CellValue::Null => "NULL".into(),
            CellValue::Bool(b) => if *b { "1" } else { "0" }.into(),
            CellValue::Int(i) => i.to_string(),
            CellValue::Float(f) => {
                if f.is_finite() { format!("{f:?}") } else { "NULL".into() }
            }
            CellValue::Decimal(v, s) => Self::decimal_string(*v, *s),
            CellValue::Text(t) => format!("N'{}'", t.replace('\'', "''")),
            CellValue::Bytes(b) => format!("0x{}", hex(b)),
            CellValue::Date(d) => format!("'{d}'"),
            CellValue::Time(t) => format!("'{}'", t.format("%H:%M:%S%.f")),
            CellValue::DateTime(dt) => format!("'{}'", dt.format("%Y-%m-%dT%H:%M:%S%.f")),
            CellValue::DateTimeTz(dt) => format!("'{}'", dt.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, false)),
        }
    }
}

pub fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0xf) as usize] as char);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn decimals() {
        assert_eq!(CellValue::decimal_string(12345, 2), "123.45");
        assert_eq!(CellValue::decimal_string(-5, 2), "-0.05");
        assert_eq!(CellValue::decimal_string(7, 0), "7");
        assert_eq!(CellValue::decimal_string(12, -2), "1200");
    }
    #[test]
    fn literals() {
        assert_eq!(CellValue::Text("it's".into()).to_sql_literal(), "N'it''s'");
        assert_eq!(CellValue::Bytes(vec![0xde, 0xad]).to_sql_literal(), "0xDEAD");
    }
}
