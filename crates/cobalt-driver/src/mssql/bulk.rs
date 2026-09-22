//! Bulk insert of Arrow batches (Import Data). Each batch is cast to the target columns' Arrow
//! types, then every row is encoded as a TDS bulk-load row. The caller wraps the whole load in a
//! transaction so a failure or cancel leaves nothing behind.

use super::{map_error, ErrorPhase, MssqlConnection};
use crate::{DriverError, Result};
use arrow::array::*;
use arrow::datatypes::{DataType, TimeUnit};
use chrono::{Datelike, NaiveDate, NaiveDateTime, Timelike};
use cobalt_core::{ColumnInfo, SqlType};
use std::borrow::Cow;
use tiberius::numeric::Numeric;
use tiberius::{ColumnData, IntoSql, TokenRow};

pub(crate) async fn bulk_insert(
    conn: &mut MssqlConnection,
    table: &str,
    columns: &[ColumnInfo],
    rx: std::sync::mpsc::Receiver<std::result::Result<RecordBatch, String>>,
    progress: &mut (dyn FnMut(u64) -> bool + Send),
) -> Result<u64> {
    conn.settle().await?;
    let names: Vec<String> = columns.iter().map(|c| c.name.clone()).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut req = match conn.tds().bulk_insert_columns(table, &refs).await {
        Ok(r) => r,
        Err(e) => return Err(conn.fail(e)),
    };
    let mut total = 0u64;
    let mut outcome: Result<()> = Ok(());
    while let Ok(item) = rx.recv() {
        let batch = match item {
            Ok(b) => b,
            Err(e) => {
                outcome = Err(DriverError::Other(format!("reading the file failed: {e}")));
                break;
            }
        };
        if batch.num_columns() != columns.len() {
            outcome = Err(DriverError::Other(format!("the file batch has {} columns but {} were mapped", batch.num_columns(), columns.len())));
            break;
        }
        let casted = match cast_batch(&batch, columns) {
            Ok(c) => c,
            Err(e) => {
                outcome = Err(e);
                break;
            }
        };
        let n = casted.num_rows();
        let mut failed = None;
        'rows: for r in 0..n {
            let mut row = TokenRow::with_capacity(columns.len());
            for (c, col) in columns.iter().enumerate() {
                match cell(casted.column(c), r, &col.sql_type) {
                    Ok(v) => row.push(v),
                    Err(e) => {
                        failed = Some(DriverError::Conversion { column: col.name.clone(), detail: format!("row {}: {e}", total + r as u64 + 1) });
                        break 'rows;
                    }
                }
            }
            if let Err(e) = req.send(row).await {
                failed = Some(map_error(e, ErrorPhase::Execute));
                break 'rows;
            }
        }
        if let Some(e) = failed {
            outcome = Err(e);
            break;
        }
        total += n as u64;
        if !progress(total) {
            outcome = Err(DriverError::Cancelled);
            break;
        }
    }
    // Always finish the bulk request so the connection is usable again; the caller decides
    // whether to COMMIT or ROLLBACK what was sent.
    match req.finalize().await {
        Ok(_) => {}
        Err(e) => {
            let mapped = conn.fail(e);
            if outcome.is_ok() {
                return Err(mapped);
            }
        }
    }
    outcome.map(|_| total)
}

/// Cast every column to the Arrow type the target SQL type expects.
fn cast_batch(batch: &RecordBatch, columns: &[ColumnInfo]) -> Result<RecordBatch> {
    let mut arrays = Vec::with_capacity(columns.len());
    for (i, col) in columns.iter().enumerate() {
        let src = batch.column(i);
        let target = col.sql_type.arrow_type();
        let target = match (&target, src.data_type()) {
            // strings: keep whatever width the source has
            (DataType::Utf8 | DataType::LargeUtf8, DataType::Utf8 | DataType::LargeUtf8 | DataType::Utf8View) => src.data_type().clone(),
            (DataType::Binary | DataType::LargeBinary, DataType::Binary | DataType::LargeBinary | DataType::BinaryView) => src.data_type().clone(),
            _ => target,
        };
        let arr = if src.data_type() == &target {
            src.clone()
        } else {
            arrow::compute::cast(src, &target).map_err(|e| DriverError::Conversion { column: col.name.clone(), detail: format!("cannot convert {:?} to {}: {e}", src.data_type(), col.sql_type) })?
        };
        arrays.push(arr);
    }
    let schema = std::sync::Arc::new(arrow::datatypes::Schema::new(columns.iter().zip(arrays.iter()).map(|(c, a)| arrow::datatypes::Field::new(c.name.clone(), a.data_type().clone(), true)).collect::<Vec<_>>()));
    RecordBatch::try_new(schema, arrays).map_err(|e| DriverError::Other(e.to_string()))
}

fn null_for(ty: &SqlType) -> ColumnData<'static> {
    match ty {
        SqlType::Bit => ColumnData::Bit(None),
        SqlType::TinyInt => ColumnData::U8(None),
        SqlType::SmallInt => ColumnData::I16(None),
        SqlType::Int => ColumnData::I32(None),
        SqlType::BigInt => ColumnData::I64(None),
        SqlType::Float => ColumnData::F64(None),
        SqlType::Real => ColumnData::F32(None),
        SqlType::Decimal { .. } | SqlType::Numeric { .. } | SqlType::Money | SqlType::SmallMoney => ColumnData::Numeric(None),
        SqlType::Date => ColumnData::Date(None),
        SqlType::Time { .. } => ColumnData::Time(None),
        SqlType::DateTime => ColumnData::DateTime(None),
        SqlType::SmallDateTime => ColumnData::SmallDateTime(None),
        SqlType::DateTime2 { .. } => ColumnData::DateTime2(None),
        SqlType::DateTimeOffset { .. } => ColumnData::DateTimeOffset(None),
        SqlType::UniqueIdentifier => ColumnData::Guid(None),
        SqlType::Binary { .. } | SqlType::VarBinary { .. } | SqlType::Image | SqlType::Timestamp => ColumnData::Binary(None),
        _ => ColumnData::String(None),
    }
}

fn downcast<'a, T: 'static>(col: &'a ArrayRef, what: &str) -> std::result::Result<&'a T, String> {
    col.as_any().downcast_ref::<T>().ok_or_else(|| format!("expected {what}, got {:?}", col.data_type()))
}

/// One cell → TDS column data of the variant the bulk encoder accepts for `ty`.
fn cell(col: &ArrayRef, i: usize, ty: &SqlType) -> std::result::Result<ColumnData<'static>, String> {
    if col.is_null(i) {
        return Ok(null_for(ty));
    }
    Ok(match ty {
        SqlType::Bit => ColumnData::Bit(Some(downcast::<BooleanArray>(col, "bool")?.value(i))),
        SqlType::TinyInt => ColumnData::U8(Some(downcast::<UInt8Array>(col, "tinyint")?.value(i))),
        SqlType::SmallInt => ColumnData::I16(Some(downcast::<Int16Array>(col, "smallint")?.value(i))),
        SqlType::Int => ColumnData::I32(Some(downcast::<Int32Array>(col, "int")?.value(i))),
        SqlType::BigInt => ColumnData::I64(Some(downcast::<Int64Array>(col, "bigint")?.value(i))),
        SqlType::Float => ColumnData::F64(Some(downcast::<Float64Array>(col, "float")?.value(i))),
        SqlType::Real => ColumnData::F32(Some(downcast::<Float32Array>(col, "real")?.value(i))),
        SqlType::Decimal { .. } | SqlType::Numeric { .. } | SqlType::Money | SqlType::SmallMoney => {
            let a = downcast::<Decimal128Array>(col, "decimal")?;
            let scale = a.scale().max(0) as u8;
            ColumnData::Numeric(Some(Numeric::new_with_scale(a.value(i), scale)))
        }
        SqlType::Date => {
            let a = downcast::<Date32Array>(col, "date")?;
            let d = a.value_as_date(i).ok_or("date out of range")?;
            d.into_sql()
        }
        SqlType::Time { .. } => {
            let t = match col.data_type() {
                DataType::Time64(TimeUnit::Nanosecond) => downcast::<Time64NanosecondArray>(col, "time")?.value_as_time(i),
                DataType::Time64(TimeUnit::Microsecond) => downcast::<Time64MicrosecondArray>(col, "time")?.value_as_time(i),
                DataType::Time32(TimeUnit::Millisecond) => downcast::<Time32MillisecondArray>(col, "time")?.value_as_time(i),
                DataType::Time32(TimeUnit::Second) => downcast::<Time32SecondArray>(col, "time")?.value_as_time(i),
                other => return Err(format!("expected a time, got {other:?}")),
            }
            .ok_or("time out of range")?;
            t.into_sql()
        }
        SqlType::DateTime2 { .. } => timestamp_naive(col, i)?.into_sql(),
        SqlType::DateTime => {
            let dt = timestamp_naive(col, i)?;
            let days = (dt.date() - NaiveDate::from_ymd_opt(1900, 1, 1).unwrap()).num_days();
            if !(i32::MIN as i64..=i32::MAX as i64).contains(&days) {
                return Err("datetime out of range".into());
            }
            let frac = (dt.time().num_seconds_from_midnight() as u64 * 300 + (dt.time().nanosecond() as u64 * 300) / 1_000_000_000) as u32;
            ColumnData::DateTime(Some(tiberius::time::DateTime::new(days as i32, frac)))
        }
        SqlType::SmallDateTime => {
            let dt = timestamp_naive(col, i)?;
            let days = (dt.date() - NaiveDate::from_ymd_opt(1900, 1, 1).unwrap()).num_days();
            if !(0..=u16::MAX as i64).contains(&days) {
                return Err("smalldatetime out of range (1900-01-01 .. 2079-06-06)".into());
            }
            let mins = (dt.time().num_seconds_from_midnight() / 60) as u16;
            ColumnData::SmallDateTime(Some(tiberius::time::SmallDateTime::new(days as u16, mins)))
        }
        SqlType::DateTimeOffset { .. } => {
            let naive = timestamp_naive(col, i)?; // UTC after the cast
            let utc: chrono::DateTime<chrono::Utc> = chrono::DateTime::from_naive_utc_and_offset(naive, chrono::Utc);
            utc.into_sql()
        }
        SqlType::UniqueIdentifier => {
            let s = text(col, i)?;
            ColumnData::Guid(Some(tiberius::Uuid::parse_str(s.trim()).map_err(|e| format!("not a uniqueidentifier: {e}"))?))
        }
        SqlType::Binary { .. } | SqlType::VarBinary { .. } | SqlType::Image | SqlType::Timestamp => {
            let b: Vec<u8> = if let Some(a) = col.as_any().downcast_ref::<BinaryArray>() {
                a.value(i).to_vec()
            } else if let Some(a) = col.as_any().downcast_ref::<LargeBinaryArray>() {
                a.value(i).to_vec()
            } else {
                return Err(format!("expected binary, got {:?}", col.data_type()));
            };
            ColumnData::Binary(Some(Cow::Owned(b)))
        }
        _ => ColumnData::String(Some(Cow::Owned(text(col, i)?.to_string()))),
    })
}

fn text(col: &ArrayRef, i: usize) -> std::result::Result<&str, String> {
    if let Some(a) = col.as_any().downcast_ref::<StringArray>() {
        return Ok(a.value(i));
    }
    if let Some(a) = col.as_any().downcast_ref::<LargeStringArray>() {
        return Ok(a.value(i));
    }
    Err(format!("expected text, got {:?}", col.data_type()))
}

fn timestamp_naive(col: &ArrayRef, i: usize) -> std::result::Result<NaiveDateTime, String> {
    let dt = match col.data_type() {
        DataType::Timestamp(TimeUnit::Microsecond, _) => downcast::<TimestampMicrosecondArray>(col, "timestamp")?.value_as_datetime(i),
        DataType::Timestamp(TimeUnit::Millisecond, _) => downcast::<TimestampMillisecondArray>(col, "timestamp")?.value_as_datetime(i),
        DataType::Timestamp(TimeUnit::Nanosecond, _) => downcast::<TimestampNanosecondArray>(col, "timestamp")?.value_as_datetime(i),
        DataType::Timestamp(TimeUnit::Second, _) => downcast::<TimestampSecondArray>(col, "timestamp")?.value_as_datetime(i),
        other => return Err(format!("expected a timestamp, got {other:?}")),
    };
    dt.ok_or_else(|| "timestamp out of range".to_string())
}

#[allow(dead_code)]
fn _keep_datelike_in_scope(d: NaiveDate) -> i32 {
    d.year()
}
