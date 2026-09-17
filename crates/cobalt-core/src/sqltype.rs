use serde::{Deserialize, Serialize};
use std::fmt;

/// SQL Server column types as the server reports them. Kept close to T-SQL so we can
/// print `nvarchar(100)` in headers, script DDL, and map to Arrow.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum SqlType {
    Bit,
    TinyInt,
    SmallInt,
    Int,
    BigInt,
    Decimal { precision: u8, scale: u8 },
    Numeric { precision: u8, scale: u8 },
    Money,
    SmallMoney,
    Float,
    Real,
    Date,
    Time { scale: u8 },
    DateTime,
    DateTime2 { scale: u8 },
    SmallDateTime,
    DateTimeOffset { scale: u8 },
    /// `len` = None means `(max)`.
    Char { len: Option<u32> },
    VarChar { len: Option<u32> },
    NChar { len: Option<u32> },
    NVarChar { len: Option<u32> },
    Text,
    NText,
    Binary { len: Option<u32> },
    VarBinary { len: Option<u32> },
    Image,
    UniqueIdentifier,
    Xml,
    SqlVariant,
    Geography,
    Geometry,
    HierarchyId,
    Json,
    Vector { dims: Option<u32> },
    Timestamp,
    Other(String),
}

impl SqlType {
    /// Parse a `sys.types` name plus length/precision/scale into a `SqlType`.
    /// `max_length` is bytes (-1 = max); for n-types the char count is half.
    pub fn from_catalog(name: &str, max_length: i32, precision: u8, scale: u8) -> Self {
        let len = |bytes: i32, per_char: i32| -> Option<u32> {
            if bytes < 0 { None } else { Some((bytes / per_char) as u32) }
        };
        match name.to_ascii_lowercase().as_str() {
            "bit" => SqlType::Bit,
            "tinyint" => SqlType::TinyInt,
            "smallint" => SqlType::SmallInt,
            "int" => SqlType::Int,
            "bigint" => SqlType::BigInt,
            "decimal" => SqlType::Decimal { precision, scale },
            "numeric" => SqlType::Numeric { precision, scale },
            "money" => SqlType::Money,
            "smallmoney" => SqlType::SmallMoney,
            "float" => SqlType::Float,
            "real" => SqlType::Real,
            "date" => SqlType::Date,
            "time" => SqlType::Time { scale },
            "datetime" => SqlType::DateTime,
            "datetime2" => SqlType::DateTime2 { scale },
            "smalldatetime" => SqlType::SmallDateTime,
            "datetimeoffset" => SqlType::DateTimeOffset { scale },
            "char" => SqlType::Char { len: len(max_length, 1) },
            "varchar" => SqlType::VarChar { len: len(max_length, 1) },
            "nchar" => SqlType::NChar { len: len(max_length, 2) },
            "nvarchar" => SqlType::NVarChar { len: len(max_length, 2) },
            "text" => SqlType::Text,
            "ntext" => SqlType::NText,
            "binary" => SqlType::Binary { len: len(max_length, 1) },
            "varbinary" => SqlType::VarBinary { len: len(max_length, 1) },
            "image" => SqlType::Image,
            "uniqueidentifier" => SqlType::UniqueIdentifier,
            "xml" => SqlType::Xml,
            "sql_variant" => SqlType::SqlVariant,
            "geography" => SqlType::Geography,
            "geometry" => SqlType::Geometry,
            "hierarchyid" => SqlType::HierarchyId,
            "json" => SqlType::Json,
            "vector" => SqlType::Vector { dims: None },
            "timestamp" | "rowversion" => SqlType::Timestamp,
            other => SqlType::Other(other.to_string()),
        }
    }

    pub fn is_numeric(&self) -> bool {
        matches!(
            self,
            SqlType::TinyInt | SqlType::SmallInt | SqlType::Int | SqlType::BigInt | SqlType::Decimal { .. } | SqlType::Numeric { .. }
                | SqlType::Money | SqlType::SmallMoney | SqlType::Float | SqlType::Real
        )
    }
    pub fn is_string(&self) -> bool {
        matches!(
            self,
            SqlType::Char { .. } | SqlType::VarChar { .. } | SqlType::NChar { .. } | SqlType::NVarChar { .. } | SqlType::Text | SqlType::NText | SqlType::Xml | SqlType::Json
        )
    }
    pub fn is_binary(&self) -> bool {
        matches!(self, SqlType::Binary { .. } | SqlType::VarBinary { .. } | SqlType::Image | SqlType::Timestamp)
    }
    pub fn is_temporal(&self) -> bool {
        matches!(
            self,
            SqlType::Date | SqlType::Time { .. } | SqlType::DateTime | SqlType::DateTime2 { .. } | SqlType::SmallDateTime | SqlType::DateTimeOffset { .. }
        )
    }
    /// Right-align in grids?
    pub fn right_align(&self) -> bool {
        self.is_numeric()
    }

    /// The Arrow type this column materializes as. Mapping is deliberately lossless-first:
    /// decimals → Decimal128, datetime2 → Timestamp(µs/ns), datetimeoffset → Timestamp(µs, UTC) with the
    /// original offset kept in a companion column by the driver when needed, uniqueidentifier → Utf8,
    /// xml/json → Utf8 (LargeUtf8 for max types), sql_variant → Utf8 (formatted).
    pub fn arrow_type(&self) -> arrow_schema::DataType {
        use arrow_schema::{DataType as D, TimeUnit as T};
        match self {
            SqlType::Bit => D::Boolean,
            SqlType::TinyInt => D::UInt8,
            SqlType::SmallInt => D::Int16,
            SqlType::Int => D::Int32,
            SqlType::BigInt => D::Int64,
            SqlType::Decimal { precision, scale } | SqlType::Numeric { precision, scale } => D::Decimal128(*precision, *scale as i8),
            SqlType::Money => D::Decimal128(19, 4),
            SqlType::SmallMoney => D::Decimal128(10, 4),
            SqlType::Float => D::Float64,
            SqlType::Real => D::Float32,
            SqlType::Date => D::Date32,
            SqlType::Time { .. } => D::Time64(T::Nanosecond),
            SqlType::DateTime | SqlType::SmallDateTime => D::Timestamp(T::Millisecond, None),
            SqlType::DateTime2 { .. } => D::Timestamp(T::Nanosecond, None),
            SqlType::DateTimeOffset { .. } => D::Timestamp(T::Nanosecond, Some("UTC".into())),
            SqlType::Char { .. } | SqlType::VarChar { len: Some(_) } | SqlType::NChar { .. } | SqlType::NVarChar { len: Some(_) } => D::Utf8,
            SqlType::VarChar { len: None } | SqlType::NVarChar { len: None } | SqlType::Text | SqlType::NText | SqlType::Xml | SqlType::Json => D::LargeUtf8,
            SqlType::Binary { .. } | SqlType::VarBinary { len: Some(_) } | SqlType::Timestamp => D::Binary,
            SqlType::VarBinary { len: None } | SqlType::Image | SqlType::Geography | SqlType::Geometry | SqlType::HierarchyId => D::LargeBinary,
            SqlType::UniqueIdentifier => D::Utf8,
            SqlType::SqlVariant => D::Utf8,
            SqlType::Vector { .. } => D::LargeUtf8,
            SqlType::Other(_) => D::LargeUtf8,
        }
    }
}

impl fmt::Display for SqlType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fn l(len: &Option<u32>) -> String {
            match len {
                Some(n) => n.to_string(),
                None => "max".into(),
            }
        }
        match self {
            SqlType::Bit => write!(f, "bit"),
            SqlType::TinyInt => write!(f, "tinyint"),
            SqlType::SmallInt => write!(f, "smallint"),
            SqlType::Int => write!(f, "int"),
            SqlType::BigInt => write!(f, "bigint"),
            SqlType::Decimal { precision, scale } => write!(f, "decimal({precision},{scale})"),
            SqlType::Numeric { precision, scale } => write!(f, "numeric({precision},{scale})"),
            SqlType::Money => write!(f, "money"),
            SqlType::SmallMoney => write!(f, "smallmoney"),
            SqlType::Float => write!(f, "float"),
            SqlType::Real => write!(f, "real"),
            SqlType::Date => write!(f, "date"),
            SqlType::Time { scale } => write!(f, "time({scale})"),
            SqlType::DateTime => write!(f, "datetime"),
            SqlType::DateTime2 { scale } => write!(f, "datetime2({scale})"),
            SqlType::SmallDateTime => write!(f, "smalldatetime"),
            SqlType::DateTimeOffset { scale } => write!(f, "datetimeoffset({scale})"),
            SqlType::Char { len } => write!(f, "char({})", l(len)),
            SqlType::VarChar { len } => write!(f, "varchar({})", l(len)),
            SqlType::NChar { len } => write!(f, "nchar({})", l(len)),
            SqlType::NVarChar { len } => write!(f, "nvarchar({})", l(len)),
            SqlType::Text => write!(f, "text"),
            SqlType::NText => write!(f, "ntext"),
            SqlType::Binary { len } => write!(f, "binary({})", l(len)),
            SqlType::VarBinary { len } => write!(f, "varbinary({})", l(len)),
            SqlType::Image => write!(f, "image"),
            SqlType::UniqueIdentifier => write!(f, "uniqueidentifier"),
            SqlType::Xml => write!(f, "xml"),
            SqlType::SqlVariant => write!(f, "sql_variant"),
            SqlType::Geography => write!(f, "geography"),
            SqlType::Geometry => write!(f, "geometry"),
            SqlType::HierarchyId => write!(f, "hierarchyid"),
            SqlType::Json => write!(f, "json"),
            SqlType::Vector { dims: Some(d) } => write!(f, "vector({d})"),
            SqlType::Vector { dims: None } => write!(f, "vector"),
            SqlType::Timestamp => write!(f, "timestamp"),
            SqlType::Other(s) => write!(f, "{s}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn catalog_parse() {
        assert_eq!(SqlType::from_catalog("nvarchar", 200, 0, 0), SqlType::NVarChar { len: Some(100) });
        assert_eq!(SqlType::from_catalog("nvarchar", -1, 0, 0), SqlType::NVarChar { len: None });
        assert_eq!(SqlType::from_catalog("decimal", 9, 18, 4).to_string(), "decimal(18,4)");
        assert_eq!(SqlType::from_catalog("varbinary", -1, 0, 0).to_string(), "varbinary(max)");
    }
}
