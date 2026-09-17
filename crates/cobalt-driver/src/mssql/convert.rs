//! TDS column metadata / row values → `cobalt_core::SqlType` / Arrow arrays.
//!
//! Every array produced here has a `DataType` equal to `ColumnInfo::sql_type.arrow_type()`;
//! `cobalt_results::ResultSet::new` derives its schema from exactly that, so the batches the
//! driver emits can be appended without conversion.
//!
//! ## Precision / scale
//! tiberius-ng's raw `COLMETADATA` (`TokenColMetaData`) carries `TypeInfo::VarLenSizedPrecision
//! { precision, scale }` for decimal/numeric and the scale (`VarLenContext::len`) for
//! time/datetime2/datetimeoffset, so types are known before the first row arrives. As a safety
//! net for a server that sends a decimal without precision (never observed), the column is
//! flagged `needs_scale_inference`; the executor then holds `ResultSetStart` until the first
//! non-null value (or the end of the result set), taking `Decimal { precision: 38, scale }` from
//! that value, scale 0 when the whole column is null. Values whose scale differs from the
//! column's are rescaled (rounded half away from zero) so they fit the fixed Arrow scale.
//!
//! ## Temporal ranges
//! `datetime2`/`datetimeoffset` map to `Timestamp(ns)`, whose i64 range is 1677-09-21 …
//! 2262-04-11. Values outside (e.g. `0001-01-01`) cannot be represented and become NULL with a
//! `tracing::warn!` — see `BatchBuilder::conversion_failures`.

use arrow::array::{
    ArrayRef, BinaryBuilder, BooleanBuilder, Date32Builder, Decimal128Builder, Float32Builder, Float64Builder, Int16Builder, Int32Builder,
    Int64Builder, LargeBinaryBuilder, LargeStringBuilder, RecordBatch, StringBuilder, Time64NanosecondBuilder, TimestampMillisecondBuilder,
    TimestampNanosecondBuilder, UInt8Builder,
};
use arrow::datatypes::{DataType, Field, Schema, SchemaRef, TimeUnit};
use cobalt_core::{ColumnInfo, SqlType};
use std::sync::Arc;
use tiberius::time::{Date, DateTime, DateTime2, DateTimeOffset, SmallDateTime, Time};
use tiberius::{ColumnData, ColumnFlag, FixedLenType, MetaDataColumn, TokenColMetaData, TokenRow, TypeInfo, VarLenType};

const DAYS_0001_TO_1970: i64 = 719_162;
const DAYS_1900_TO_1970: i64 = 25_567;
const NS_PER_DAY: i64 = 86_400_000_000_000;
const MS_PER_DAY: i64 = 86_400_000;
/// `len` value tiberius reports for `(max)` var types.
const PLP_MAX_LEN: usize = 0xFFFF;

// ---------------------------------------------------------------------------------------------
// Metadata
// ---------------------------------------------------------------------------------------------

/// A result-set column as read from `COLMETADATA`.
#[derive(Clone, Debug)]
pub struct MetaColumn {
    pub info: ColumnInfo,
    /// Decimal column whose precision/scale were not in the metadata (see module docs).
    pub needs_scale_inference: bool,
}

pub fn columns_from_meta(meta: &TokenColMetaData<'_>) -> Vec<MetaColumn> {
    meta.columns.iter().enumerate().map(|(i, c)| column_from_meta(c, i)).collect()
}

pub fn column_from_meta(col: &MetaDataColumn<'_>, ordinal: usize) -> MetaColumn {
    let (sql_type, needs_scale_inference) = sql_type_from_type_info(col.base.ty());
    let mut info = ColumnInfo::new(col.col_name().to_string(), sql_type, col.base.is_nullable(), ordinal);
    info.is_identity = col.base.is_identity();
    info.is_computed = col.base.flags().contains(ColumnFlag::Computed);
    MetaColumn { info, needs_scale_inference }
}

/// Map a TDS `TypeInfo` to `SqlType`. The bool is `needs_scale_inference`.
pub fn sql_type_from_type_info(ti: &TypeInfo) -> (SqlType, bool) {
    let len_opt = |n: usize| if n >= PLP_MAX_LEN { None } else { Some(n as u32) };
    match ti {
        TypeInfo::FixedLen(f) => (
            match f {
                FixedLenType::Null => SqlType::Other("null".into()),
                FixedLenType::Int1 => SqlType::TinyInt,
                FixedLenType::Bit => SqlType::Bit,
                FixedLenType::Int2 => SqlType::SmallInt,
                FixedLenType::Int4 => SqlType::Int,
                FixedLenType::Datetime4 => SqlType::SmallDateTime,
                FixedLenType::Float4 => SqlType::Real,
                FixedLenType::Money => SqlType::Money,
                FixedLenType::Datetime => SqlType::DateTime,
                FixedLenType::Float8 => SqlType::Float,
                FixedLenType::Money4 => SqlType::SmallMoney,
                FixedLenType::Int8 => SqlType::BigInt,
            },
            false,
        ),
        TypeInfo::VarLenSized(cx) => {
            let n = cx.len();
            match cx.r#type() {
                VarLenType::Guid => (SqlType::UniqueIdentifier, false),
                VarLenType::Intn => (
                    match n {
                        1 => SqlType::TinyInt,
                        2 => SqlType::SmallInt,
                        8 => SqlType::BigInt,
                        _ => SqlType::Int,
                    },
                    false,
                ),
                VarLenType::Bitn => (SqlType::Bit, false),
                // Precision is normally delivered through VarLenSizedPrecision; this is the fallback.
                VarLenType::Decimaln => (SqlType::Decimal { precision: 38, scale: 0 }, true),
                VarLenType::Numericn => (SqlType::Numeric { precision: 38, scale: 0 }, true),
                VarLenType::Floatn => (if n == 4 { SqlType::Real } else { SqlType::Float }, false),
                VarLenType::Money => (if n == 4 { SqlType::SmallMoney } else { SqlType::Money }, false),
                VarLenType::Datetimen => (if n == 4 { SqlType::SmallDateTime } else { SqlType::DateTime }, false),
                VarLenType::Daten => (SqlType::Date, false),
                VarLenType::Timen => (SqlType::Time { scale: n.min(7) as u8 }, false),
                VarLenType::Datetime2 => (SqlType::DateTime2 { scale: n.min(7) as u8 }, false),
                VarLenType::DatetimeOffsetn => (SqlType::DateTimeOffset { scale: n.min(7) as u8 }, false),
                VarLenType::BigVarBin => (SqlType::VarBinary { len: len_opt(n) }, false),
                VarLenType::BigVarChar => (SqlType::VarChar { len: len_opt(n) }, false),
                VarLenType::BigBinary => (SqlType::Binary { len: Some(n as u32) }, false),
                VarLenType::BigChar => (SqlType::Char { len: Some(n as u32) }, false),
                VarLenType::NVarchar => (SqlType::NVarChar { len: len_opt(n).map(|b| b / 2) }, false),
                VarLenType::NChar => (SqlType::NChar { len: Some((n / 2) as u32) }, false),
                VarLenType::Xml => (SqlType::Xml, false),
                VarLenType::Udt => (SqlType::Other("udt".into()), false),
                VarLenType::Text => (SqlType::Text, false),
                VarLenType::Image => (SqlType::Image, false),
                VarLenType::NText => (SqlType::NText, false),
                VarLenType::SSVariant => (SqlType::SqlVariant, false),
            }
        }
        TypeInfo::VarLenSizedPrecision { ty, precision, scale, .. } => match ty {
            VarLenType::Numericn => (SqlType::Numeric { precision: *precision, scale: *scale }, false),
            _ => (SqlType::Decimal { precision: *precision, scale: *scale }, false),
        },
        TypeInfo::Xml { .. } => (SqlType::Xml, false),
        TypeInfo::Udt(u) => {
            let name = u.type_name.to_ascii_lowercase();
            (
                match name.as_str() {
                    "geography" => SqlType::Geography,
                    "geometry" => SqlType::Geometry,
                    "hierarchyid" => SqlType::HierarchyId,
                    _ => SqlType::Other(u.type_name.clone()),
                },
                false,
            )
        }
    }
}

/// Field name as `cobalt_results` derives it (duplicates get `_2`, `_3`; empty names become
/// `(No column name N)`), so the driver's batch schema equals the result set's schema.
pub fn schema_for(columns: &[ColumnInfo]) -> SchemaRef {
    let fields: Vec<Field> = columns
        .iter()
        .map(|c| {
            let base = if c.name.is_empty() { format!("(No column name {})", c.ordinal + 1) } else { c.name.clone() };
            let dupes_before = columns[..c.ordinal.min(columns.len())].iter().filter(|o| o.name == c.name).count();
            let name = if dupes_before == 0 { base } else { format!("{base}_{}", dupes_before + 1) };
            Field::new(name, c.sql_type.arrow_type(), true)
        })
        .collect();
    Arc::new(Schema::new(fields))
}

// ---------------------------------------------------------------------------------------------
// Scalar helpers
// ---------------------------------------------------------------------------------------------

pub fn time_ns(t: &Time) -> i64 {
    let scale = t.scale().min(7) as u32;
    (t.increments() as i64).saturating_mul(10i64.pow(9 - scale))
}

pub fn date_to_date32(d: &Date) -> i32 {
    (d.days() as i64 - DAYS_0001_TO_1970) as i32
}

/// Nanoseconds since 1970-01-01 (naive). `None` when outside the i64 range.
pub fn datetime2_to_ns(dt: &DateTime2) -> Option<i64> {
    let days = dt.date().days() as i64 - DAYS_0001_TO_1970;
    days.checked_mul(NS_PER_DAY)?.checked_add(time_ns(&dt.time()))
}

/// UTC nanoseconds. Per MS-TDS 2.2.5.5.1.9 the `datetime2` part of a `datetimeoffset` is
/// already UTC; the offset only says how the client should *display* it.
pub fn datetimeoffset_to_utc_ns(dto: &DateTimeOffset) -> Option<i64> {
    datetime2_to_ns(&dto.datetime2())
}

/// Milliseconds since 1970-01-01. `datetime` stores 1/300 s ticks; SQL Server rounds them to
/// .000/.003/.007, which `(ticks * 1000 + 150) / 300` reproduces.
pub fn datetime_to_ms(dt: &DateTime) -> i64 {
    (dt.days() as i64 - DAYS_1900_TO_1970) * MS_PER_DAY + (dt.seconds_fragments() as i64 * 1000 + 150) / 300
}

pub fn smalldatetime_to_ms(dt: &SmallDateTime) -> i64 {
    (dt.days() as i64 - DAYS_1900_TO_1970) * MS_PER_DAY + dt.seconds_fragments() as i64 * 60_000
}

/// Rescale a decimal's unscaled value from one scale to another (round half away from zero).
pub fn rescale(value: i128, from: u8, to: u8) -> i128 {
    use std::cmp::Ordering::*;
    match from.cmp(&to) {
        Equal => value,
        Less => value.saturating_mul(10i128.pow((to - from) as u32)),
        Greater => {
            let d = 10i128.pow((from - to) as u32);
            let q = value / d;
            let r = value % d;
            if r.abs() * 2 >= d {
                if value < 0 { q - 1 } else { q + 1 }
            } else {
                q
            }
        }
    }
}

/// `1234567.8901` style text for an unscaled value.
pub fn decimal_string(value: i128, scale: u8) -> String {
    let digits = value.unsigned_abs().to_string();
    let sign = if value < 0 { "-" } else { "" };
    if scale == 0 {
        return format!("{sign}{digits}");
    }
    let scale = scale as usize;
    let padded = if digits.len() <= scale { format!("{}{digits}", "0".repeat(scale + 1 - digits.len())) } else { digits };
    let (int, frac) = padded.split_at(padded.len() - scale);
    format!("{sign}{int}.{frac}")
}

pub fn guid_string(u: &uuid::Uuid) -> String {
    let mut buf = uuid::Uuid::encode_buffer();
    u.hyphenated().encode_upper(&mut buf).to_string()
}

pub fn hex_string(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(2 + bytes.len() * 2);
    s.push_str("0x");
    for b in bytes {
        s.push_str(&format!("{b:02X}"));
    }
    s
}

fn naive_from_ns(ns: i64) -> chrono::NaiveDateTime {
    chrono::DateTime::<chrono::Utc>::from_timestamp(ns.div_euclid(1_000_000_000), ns.rem_euclid(1_000_000_000) as u32)
        .map(|d| d.naive_utc())
        .unwrap_or_default()
}

fn frac_text(ns_in_second: i64, scale: u8) -> String {
    if scale == 0 {
        return String::new();
    }
    let s = format!("{ns_in_second:09}");
    format!(".{}", &s[..scale.min(7) as usize])
}

/// Text rendering of any value — used for `sql_variant`, unknown types, and as the fallback
/// when a value's wire type does not match its column's builder.
pub fn display_value(v: &ColumnData<'_>) -> Option<String> {
    Some(match v {
        ColumnData::U8(x) => x.as_ref()?.to_string(),
        ColumnData::I16(x) => x.as_ref()?.to_string(),
        ColumnData::I32(x) => x.as_ref()?.to_string(),
        ColumnData::I64(x) => x.as_ref()?.to_string(),
        ColumnData::F32(x) => x.as_ref()?.to_string(),
        ColumnData::F64(x) => x.as_ref()?.to_string(),
        ColumnData::Bit(x) => if *x.as_ref()? { "1".into() } else { "0".into() },
        ColumnData::String(x) => x.as_ref()?.to_string(),
        ColumnData::Guid(x) => guid_string(x.as_ref()?),
        ColumnData::Binary(x) => hex_string(x.as_ref()?),
        ColumnData::Numeric(x) => {
            let n = x.as_ref()?;
            decimal_string(n.value(), n.scale())
        }
        ColumnData::Xml(x) => x.as_ref()?.as_ref().to_string(),
        ColumnData::DateTime(x) => naive_from_ns(datetime_to_ms(x.as_ref()?) * 1_000_000).format("%Y-%m-%d %H:%M:%S%.3f").to_string(),
        ColumnData::SmallDateTime(x) => naive_from_ns(smalldatetime_to_ms(x.as_ref()?) * 1_000_000).format("%Y-%m-%d %H:%M:%S").to_string(),
        ColumnData::Time(x) => {
            let t = x.as_ref()?;
            let ns = time_ns(t);
            let secs = ns / 1_000_000_000;
            format!("{:02}:{:02}:{:02}{}", secs / 3600, (secs / 60) % 60, secs % 60, frac_text(ns % 1_000_000_000, t.scale()))
        }
        ColumnData::Date(x) => {
            let d = x.as_ref()?;
            let date = chrono::NaiveDate::from_ymd_opt(1, 1, 1)? + chrono::Duration::days(d.days() as i64);
            date.format("%Y-%m-%d").to_string()
        }
        ColumnData::DateTime2(x) => {
            let dt = x.as_ref()?;
            let date = chrono::NaiveDate::from_ymd_opt(1, 1, 1)? + chrono::Duration::days(dt.date().days() as i64);
            let ns = time_ns(&dt.time());
            let secs = ns / 1_000_000_000;
            format!(
                "{} {:02}:{:02}:{:02}{}",
                date.format("%Y-%m-%d"),
                secs / 3600,
                (secs / 60) % 60,
                secs % 60,
                frac_text(ns % 1_000_000_000, dt.time().scale())
            )
        }
        ColumnData::DateTimeOffset(x) => {
            let dto = x.as_ref()?;
            let off = dto.offset() as i64;
            let local_ns = datetime2_to_ns(&dto.datetime2()).map(|n| n + off * 60_000_000_000)?;
            let naive = naive_from_ns(local_ns);
            let scale = dto.datetime2().time().scale();
            format!(
                "{}{} {}{:02}:{:02}",
                naive.format("%Y-%m-%d %H:%M:%S"),
                frac_text(local_ns.rem_euclid(1_000_000_000), scale),
                if off < 0 { "-" } else { "+" },
                off.abs() / 60,
                off.abs() % 60
            )
        }
    })
}

// ---------------------------------------------------------------------------------------------
// Builders
// ---------------------------------------------------------------------------------------------

enum ColumnBuilder {
    Boolean(BooleanBuilder),
    UInt8(UInt8Builder),
    Int16(Int16Builder),
    Int32(Int32Builder),
    Int64(Int64Builder),
    Float32(Float32Builder),
    Float64(Float64Builder),
    Decimal(Decimal128Builder, u8),
    Date32(Date32Builder),
    Time64(Time64NanosecondBuilder),
    TimestampMs(TimestampMillisecondBuilder),
    TimestampNs(TimestampNanosecondBuilder),
    TimestampNsUtc(TimestampNanosecondBuilder),
    Utf8(StringBuilder),
    LargeUtf8(LargeStringBuilder),
    Binary(BinaryBuilder),
    LargeBinary(LargeBinaryBuilder),
}

impl ColumnBuilder {
    fn for_type(dt: &DataType, capacity: usize) -> Self {
        match dt {
            DataType::Boolean => Self::Boolean(BooleanBuilder::with_capacity(capacity)),
            DataType::UInt8 => Self::UInt8(UInt8Builder::with_capacity(capacity)),
            DataType::Int16 => Self::Int16(Int16Builder::with_capacity(capacity)),
            DataType::Int32 => Self::Int32(Int32Builder::with_capacity(capacity)),
            DataType::Int64 => Self::Int64(Int64Builder::with_capacity(capacity)),
            DataType::Float32 => Self::Float32(Float32Builder::with_capacity(capacity)),
            DataType::Float64 => Self::Float64(Float64Builder::with_capacity(capacity)),
            DataType::Decimal128(p, s) => Self::Decimal(Decimal128Builder::with_capacity(capacity).with_data_type(DataType::Decimal128(*p, *s)), *s as u8),
            DataType::Date32 => Self::Date32(Date32Builder::with_capacity(capacity)),
            DataType::Time64(TimeUnit::Nanosecond) => Self::Time64(Time64NanosecondBuilder::with_capacity(capacity)),
            DataType::Timestamp(TimeUnit::Millisecond, None) => Self::TimestampMs(TimestampMillisecondBuilder::with_capacity(capacity)),
            DataType::Timestamp(TimeUnit::Nanosecond, None) => Self::TimestampNs(TimestampNanosecondBuilder::with_capacity(capacity)),
            DataType::Timestamp(TimeUnit::Nanosecond, Some(tz)) => {
                Self::TimestampNsUtc(TimestampNanosecondBuilder::with_capacity(capacity).with_data_type(DataType::Timestamp(TimeUnit::Nanosecond, Some(tz.clone()))))
            }
            DataType::Utf8 => Self::Utf8(StringBuilder::with_capacity(capacity, capacity * 16)),
            DataType::Binary => Self::Binary(BinaryBuilder::with_capacity(capacity, capacity * 16)),
            DataType::LargeBinary => Self::LargeBinary(LargeBinaryBuilder::with_capacity(capacity, capacity * 16)),
            // LargeUtf8 and anything unexpected (the SqlType mapping only yields the types above).
            _ => Self::LargeUtf8(LargeStringBuilder::with_capacity(capacity, capacity * 16)),
        }
    }

    /// Append one value. Returns `false` when the value could not be represented (→ NULL).
    fn append(&mut self, v: &ColumnData<'_>) -> bool {
        use ColumnData as C;
        match self {
            Self::Boolean(b) => match v {
                C::Bit(x) => b.append_option(*x),
                C::U8(x) => b.append_option(x.map(|i| i != 0)),
                C::I16(x) => b.append_option(x.map(|i| i != 0)),
                C::I32(x) => b.append_option(x.map(|i| i != 0)),
                C::I64(x) => b.append_option(x.map(|i| i != 0)),
                _ => return false,
            },
            Self::UInt8(b) => match v {
                C::U8(x) => b.append_option(*x),
                C::I16(None) | C::I32(None) | C::I64(None) => b.append_null(),
                _ => return false,
            },
            Self::Int16(b) => match v {
                C::I16(x) => b.append_option(*x),
                C::U8(x) => b.append_option(x.map(i16::from)),
                C::I32(None) | C::I64(None) => b.append_null(),
                _ => return false,
            },
            Self::Int32(b) => match v {
                C::I32(x) => b.append_option(*x),
                C::I16(x) => b.append_option(x.map(i32::from)),
                C::U8(x) => b.append_option(x.map(i32::from)),
                C::I64(None) => b.append_null(),
                _ => return false,
            },
            Self::Int64(b) => match v {
                C::I64(x) => b.append_option(*x),
                C::I32(x) => b.append_option(x.map(i64::from)),
                C::I16(x) => b.append_option(x.map(i64::from)),
                C::U8(x) => b.append_option(x.map(i64::from)),
                _ => return false,
            },
            Self::Float32(b) => match v {
                C::F32(x) => b.append_option(*x),
                C::F64(x) => b.append_option(x.map(|f| f as f32)),
                _ => return false,
            },
            Self::Float64(b) => match v {
                C::F64(x) => b.append_option(*x),
                C::F32(x) => b.append_option(x.map(f64::from)),
                C::Numeric(x) => b.append_option(x.map(f64::from)),
                _ => return false,
            },
            Self::Decimal(b, scale) => match v {
                C::Numeric(Some(n)) => b.append_value(rescale(n.value(), n.scale(), *scale)),
                C::Numeric(None) => b.append_null(),
                C::I64(x) => b.append_option(x.map(|i| rescale(i as i128, 0, *scale))),
                C::I32(x) => b.append_option(x.map(|i| rescale(i as i128, 0, *scale))),
                C::I16(x) => b.append_option(x.map(|i| rescale(i as i128, 0, *scale))),
                C::U8(x) => b.append_option(x.map(|i| rescale(i as i128, 0, *scale))),
                C::F64(x) => b.append_option(x.map(|f| (f * 10f64.powi(*scale as i32)).round() as i128)),
                _ => return false,
            },
            Self::Date32(b) => match v {
                C::Date(x) => b.append_option(x.as_ref().map(date_to_date32)),
                C::DateTime2(x) => b.append_option(x.as_ref().map(|dt| date_to_date32(&dt.date()))),
                _ => return false,
            },
            Self::Time64(b) => match v {
                C::Time(x) => b.append_option(x.as_ref().map(time_ns)),
                _ => return false,
            },
            Self::TimestampMs(b) => match v {
                C::DateTime(x) => b.append_option(x.as_ref().map(datetime_to_ms)),
                C::SmallDateTime(x) => b.append_option(x.as_ref().map(smalldatetime_to_ms)),
                C::DateTime2(Some(dt)) => match datetime2_to_ns(dt) {
                    Some(ns) => b.append_value(ns.div_euclid(1_000_000)),
                    None => return false,
                },
                C::DateTime2(None) => b.append_null(),
                _ => return false,
            },
            Self::TimestampNs(b) => match v {
                C::DateTime2(Some(dt)) => match datetime2_to_ns(dt) {
                    Some(ns) => b.append_value(ns),
                    None => return false,
                },
                C::DateTime2(None) => b.append_null(),
                C::DateTime(x) => b.append_option(x.as_ref().map(|d| datetime_to_ms(d) * 1_000_000)),
                C::SmallDateTime(x) => b.append_option(x.as_ref().map(|d| smalldatetime_to_ms(d) * 1_000_000)),
                _ => return false,
            },
            Self::TimestampNsUtc(b) => match v {
                C::DateTimeOffset(Some(dto)) => match datetimeoffset_to_utc_ns(dto) {
                    Some(ns) => b.append_value(ns),
                    None => return false,
                },
                C::DateTimeOffset(None) => b.append_null(),
                C::DateTime2(Some(dt)) => match datetime2_to_ns(dt) {
                    Some(ns) => b.append_value(ns),
                    None => return false,
                },
                C::DateTime2(None) => b.append_null(),
                _ => return false,
            },
            Self::Utf8(b) => match v {
                C::String(x) => b.append_option(x.as_deref()),
                C::Xml(x) => b.append_option(x.as_ref().map(|x| x.as_ref().as_ref())),
                other => b.append_option(display_value(other)),
            },
            Self::LargeUtf8(b) => match v {
                C::String(x) => b.append_option(x.as_deref()),
                C::Xml(x) => b.append_option(x.as_ref().map(|x| x.as_ref().as_ref())),
                other => b.append_option(display_value(other)),
            },
            Self::Binary(b) => match v {
                C::Binary(x) => b.append_option(x.as_deref()),
                C::String(x) => b.append_option(x.as_deref().map(str::as_bytes)),
                _ => return false,
            },
            Self::LargeBinary(b) => match v {
                C::Binary(x) => b.append_option(x.as_deref()),
                C::String(x) => b.append_option(x.as_deref().map(str::as_bytes)),
                _ => return false,
            },
        }
        true
    }

    fn append_null(&mut self) {
        match self {
            Self::Boolean(b) => b.append_null(),
            Self::UInt8(b) => b.append_null(),
            Self::Int16(b) => b.append_null(),
            Self::Int32(b) => b.append_null(),
            Self::Int64(b) => b.append_null(),
            Self::Float32(b) => b.append_null(),
            Self::Float64(b) => b.append_null(),
            Self::Decimal(b, _) => b.append_null(),
            Self::Date32(b) => b.append_null(),
            Self::Time64(b) => b.append_null(),
            Self::TimestampMs(b) => b.append_null(),
            Self::TimestampNs(b) => b.append_null(),
            Self::TimestampNsUtc(b) => b.append_null(),
            Self::Utf8(b) => b.append_null(),
            Self::LargeUtf8(b) => b.append_null(),
            Self::Binary(b) => b.append_null(),
            Self::LargeBinary(b) => b.append_null(),
        }
        debug_assert!(self.len() > 0);
    }

    fn len(&self) -> usize {
        use arrow::array::ArrayBuilder;
        match self {
            Self::Boolean(b) => b.len(),
            Self::UInt8(b) => b.len(),
            Self::Int16(b) => b.len(),
            Self::Int32(b) => b.len(),
            Self::Int64(b) => b.len(),
            Self::Float32(b) => b.len(),
            Self::Float64(b) => b.len(),
            Self::Decimal(b, _) => b.len(),
            Self::Date32(b) => b.len(),
            Self::Time64(b) => b.len(),
            Self::TimestampMs(b) => b.len(),
            Self::TimestampNs(b) => b.len(),
            Self::TimestampNsUtc(b) => b.len(),
            Self::Utf8(b) => b.len(),
            Self::LargeUtf8(b) => b.len(),
            Self::Binary(b) => b.len(),
            Self::LargeBinary(b) => b.len(),
        }
    }

    fn finish(&mut self) -> ArrayRef {
        match self {
            Self::Boolean(b) => Arc::new(b.finish()),
            Self::UInt8(b) => Arc::new(b.finish()),
            Self::Int16(b) => Arc::new(b.finish()),
            Self::Int32(b) => Arc::new(b.finish()),
            Self::Int64(b) => Arc::new(b.finish()),
            Self::Float32(b) => Arc::new(b.finish()),
            Self::Float64(b) => Arc::new(b.finish()),
            Self::Decimal(b, _) => Arc::new(b.finish()),
            Self::Date32(b) => Arc::new(b.finish()),
            Self::Time64(b) => Arc::new(b.finish()),
            Self::TimestampMs(b) => Arc::new(b.finish()),
            Self::TimestampNs(b) => Arc::new(b.finish()),
            Self::TimestampNsUtc(b) => Arc::new(b.finish()),
            Self::Utf8(b) => Arc::new(b.finish()),
            Self::LargeUtf8(b) => Arc::new(b.finish()),
            Self::Binary(b) => Arc::new(b.finish()),
            Self::LargeBinary(b) => Arc::new(b.finish()),
        }
    }
}

/// `true` for a NULL of any wire type.
pub fn is_null(v: &ColumnData<'_>) -> bool {
    use ColumnData as C;
    match v {
        C::U8(x) => x.is_none(),
        C::I16(x) => x.is_none(),
        C::I32(x) => x.is_none(),
        C::I64(x) => x.is_none(),
        C::F32(x) => x.is_none(),
        C::F64(x) => x.is_none(),
        C::Bit(x) => x.is_none(),
        C::String(x) => x.is_none(),
        C::Guid(x) => x.is_none(),
        C::Binary(x) => x.is_none(),
        C::Numeric(x) => x.is_none(),
        C::Xml(x) => x.is_none(),
        C::DateTime(x) => x.is_none(),
        C::SmallDateTime(x) => x.is_none(),
        C::Time(x) => x.is_none(),
        C::Date(x) => x.is_none(),
        C::DateTime2(x) => x.is_none(),
        C::DateTimeOffset(x) => x.is_none(),
    }
}

/// Accumulates rows of one result set into Arrow batches.
pub struct BatchBuilder {
    schema: SchemaRef,
    builders: Vec<ColumnBuilder>,
    rows: usize,
    /// Values that could not be represented in their Arrow type (stored as NULL).
    pub conversion_failures: usize,
    warned: bool,
}

impl BatchBuilder {
    pub fn new(columns: &[ColumnInfo], capacity: usize) -> Self {
        let schema = schema_for(columns);
        let builders = schema.fields().iter().map(|f| ColumnBuilder::for_type(f.data_type(), capacity)).collect();
        Self { schema, builders, rows: 0, conversion_failures: 0, warned: false }
    }

    #[allow(dead_code)]
    pub fn schema(&self) -> &SchemaRef {
        &self.schema
    }

    pub fn len(&self) -> usize {
        self.rows
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.rows == 0
    }

    pub fn push(&mut self, row: &TokenRow<'static>) {
        for (i, b) in self.builders.iter_mut().enumerate() {
            match row.get(i) {
                Some(v) if is_null(v) => b.append_null(),
                Some(v) => {
                    if !b.append(v) {
                        b.append_null();
                        self.conversion_failures += 1;
                        if !self.warned {
                            self.warned = true;
                            tracing::warn!(column = %self.schema.field(i).name(), value = ?v, "value not representable in Arrow type; stored as NULL");
                        }
                    }
                }
                None => b.append_null(),
            }
        }
        self.rows += 1;
    }

    /// Take the accumulated rows as a batch (the builder is reset).
    pub fn take(&mut self) -> Option<RecordBatch> {
        if self.rows == 0 {
            return None;
        }
        let arrays: Vec<ArrayRef> = self.builders.iter_mut().map(|b| b.finish()).collect();
        self.rows = 0;
        match RecordBatch::try_new(self.schema.clone(), arrays) {
            Ok(b) => Some(b),
            Err(e) => {
                tracing::error!("failed to build RecordBatch: {e}");
                None
            }
        }
    }
}

/// Resolve inferred decimal scales from the first rows of a result set (see module docs).
pub fn infer_decimal_scales(columns: &mut [MetaColumn], rows: &[TokenRow<'static>]) {
    for (i, col) in columns.iter_mut().enumerate() {
        if !col.needs_scale_inference {
            continue;
        }
        let scale = rows
            .iter()
            .find_map(|r| match r.get(i) {
                Some(ColumnData::Numeric(Some(n))) => Some(n.scale()),
                _ => None,
            })
            .unwrap_or(0);
        col.info.sql_type = match &col.info.sql_type {
            SqlType::Numeric { .. } => SqlType::Numeric { precision: 38, scale },
            _ => SqlType::Decimal { precision: 38, scale },
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::{Array, AsArray};
    use arrow::datatypes::{Decimal128Type, TimestampNanosecondType};
    use tiberius::numeric::Numeric;
    use tiberius::{VarLenContext, VarLenType};

    #[test]
    fn type_mapping() {
        let t = |ti: TypeInfo| sql_type_from_type_info(&ti).0;
        assert_eq!(t(TypeInfo::FixedLen(FixedLenType::Int4)), SqlType::Int);
        assert_eq!(t(TypeInfo::FixedLen(FixedLenType::Money4)), SqlType::SmallMoney);
        assert_eq!(t(TypeInfo::VarLenSized(VarLenContext::new(VarLenType::Intn, 8, None))), SqlType::BigInt);
        assert_eq!(t(TypeInfo::VarLenSized(VarLenContext::new(VarLenType::NVarchar, 200, None))), SqlType::NVarChar { len: Some(100) });
        assert_eq!(t(TypeInfo::VarLenSized(VarLenContext::new(VarLenType::NVarchar, 0xFFFF, None))), SqlType::NVarChar { len: None });
        assert_eq!(t(TypeInfo::VarLenSized(VarLenContext::new(VarLenType::BigVarChar, 50, None))), SqlType::VarChar { len: Some(50) });
        assert_eq!(t(TypeInfo::VarLenSized(VarLenContext::new(VarLenType::BigVarBin, 0xFFFF, None))), SqlType::VarBinary { len: None });
        assert_eq!(t(TypeInfo::VarLenSized(VarLenContext::new(VarLenType::Datetime2, 7, None))), SqlType::DateTime2 { scale: 7 });
        assert_eq!(t(TypeInfo::VarLenSized(VarLenContext::new(VarLenType::Timen, 3, None))), SqlType::Time { scale: 3 });
        assert_eq!(
            t(TypeInfo::VarLenSizedPrecision { ty: VarLenType::Decimaln, size: 9, precision: 18, scale: 4 }),
            SqlType::Decimal { precision: 18, scale: 4 }
        );
        assert_eq!(t(TypeInfo::VarLenSized(VarLenContext::new(VarLenType::SSVariant, 0, None))), SqlType::SqlVariant);
        let (ty, infer) = sql_type_from_type_info(&TypeInfo::VarLenSized(VarLenContext::new(VarLenType::Decimaln, 17, None)));
        assert_eq!(ty, SqlType::Decimal { precision: 38, scale: 0 });
        assert!(infer);
    }

    #[test]
    fn temporal_math() {
        // 1970-01-01 is day 719162 after 0001-01-01
        assert_eq!(date_to_date32(&Date::new(DAYS_0001_TO_1970 as u32)), 0);
        assert_eq!(date_to_date32(&Date::new(DAYS_0001_TO_1970 as u32 + 1)), 1);
        // datetime 1900-01-01 00:00:00 → -25567 days from epoch
        assert_eq!(datetime_to_ms(&DateTime::new(0, 0)), -DAYS_1900_TO_1970 * MS_PER_DAY);
        // 30.123 s = 9037 ticks → 30123 ms
        assert_eq!(datetime_to_ms(&DateTime::new(DAYS_1900_TO_1970 as i32, 9037)), 30_123);
        assert_eq!(smalldatetime_to_ms(&SmallDateTime::new(DAYS_1900_TO_1970 as u16, 90)), 90 * 60_000);
        assert_eq!(time_ns(&Time::new(1, 7)), 100);
        assert_eq!(time_ns(&Time::new(1, 0)), 1_000_000_000);
        // 0001-01-01 overflows Timestamp(ns)
        assert_eq!(datetime2_to_ns(&DateTime2::new(Date::new(0), Time::new(0, 7))), None);
        let epoch = DateTime2::new(Date::new(DAYS_0001_TO_1970 as u32), Time::new(0, 7));
        assert_eq!(datetime2_to_ns(&epoch), Some(0));
        // datetimeoffset: datetime2 part is UTC on the wire
        let dto = DateTimeOffset::new(epoch, -420);
        assert_eq!(datetimeoffset_to_utc_ns(&dto), Some(0));
        assert_eq!(display_value(&ColumnData::DateTimeOffset(Some(dto))).unwrap(), "1969-12-31 17:00:00.0000000 -07:00");
    }

    #[test]
    fn decimal_helpers() {
        assert_eq!(rescale(12345, 2, 4), 1_234_500);
        assert_eq!(rescale(12345, 4, 2), 123);
        assert_eq!(rescale(12350, 4, 2), 124);
        assert_eq!(rescale(-12350, 4, 2), -124);
        assert_eq!(decimal_string(12345678901, 4), "1234567.8901");
        assert_eq!(decimal_string(-5, 2), "-0.05");
        assert_eq!(decimal_string(0, 3), "0.000");
        assert_eq!(decimal_string(42, 0), "42");
        assert_eq!(guid_string(&uuid::Uuid::parse_str("6f9619ff-8b86-d011-b42d-00c04fc964ff").unwrap()), "6F9619FF-8B86-D011-B42D-00C04FC964FF");
        assert_eq!(hex_string(&[0xDE, 0xAD]), "0xDEAD");
    }

    #[test]
    fn batch_builder_matches_arrow_types() {
        let cols = vec![
            ColumnInfo::new("a", SqlType::Decimal { precision: 18, scale: 4 }, true, 0),
            ColumnInfo::new("a", SqlType::DateTimeOffset { scale: 7 }, true, 1),
            ColumnInfo::new("", SqlType::SqlVariant, true, 2),
            ColumnInfo::new("m", SqlType::Money, true, 3),
        ];
        let mut bb = BatchBuilder::new(&cols, 8);
        let names: Vec<_> = bb.schema().fields().iter().map(|f| f.name().clone()).collect();
        assert_eq!(names, vec!["a", "a_2", "(No column name 3)", "m"]);
        for (f, c) in bb.schema().fields().iter().zip(&cols) {
            assert_eq!(f.data_type(), &c.sql_type.arrow_type());
        }
        let mut row = TokenRow::new();
        row.push(ColumnData::Numeric(Some(Numeric::new_with_scale(12345, 2)))); // 123.45 → rescaled to 4
        row.push(ColumnData::DateTimeOffset(Some(DateTimeOffset::new(DateTime2::new(Date::new(DAYS_0001_TO_1970 as u32), Time::new(0, 7)), 60))));
        row.push(ColumnData::I32(Some(42)));
        row.push(ColumnData::Numeric(Some(Numeric::new_with_scale(9223372036854775807, 4))));
        bb.push(&row);
        let mut nulls = TokenRow::new();
        for _ in 0..4 {
            nulls.push(ColumnData::I32(None));
        }
        bb.push(&nulls);
        let batch = bb.take().unwrap();
        assert_eq!(batch.num_rows(), 2);
        assert!(bb.take().is_none());
        let dec = batch.column(0).as_primitive::<Decimal128Type>();
        assert_eq!(dec.value(0), 1_234_500);
        assert!(dec.is_null(1));
        let ts = batch.column(1).as_primitive::<TimestampNanosecondType>();
        assert_eq!(ts.value(0), 0);
        assert_eq!(batch.column(2).as_string::<i32>().value(0), "42");
        assert_eq!(batch.column(3).as_primitive::<Decimal128Type>().value(0), 9223372036854775807);
        assert_eq!(bb.conversion_failures, 0);
    }
}
