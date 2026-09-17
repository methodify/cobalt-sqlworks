use crate::SqlType;
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectKind {
    Database,
    Schema,
    Table,
    View,
    Procedure,
    ScalarFunction,
    TableFunction,
    AggregateFunction,
    Synonym,
    Sequence,
    TableType,
    Column,
    Parameter,
    Index,
    Key,
    Constraint,
    Trigger,
}

impl ObjectKind {
    pub fn label(&self) -> &'static str {
        match self {
            ObjectKind::Database => "Database",
            ObjectKind::Schema => "Schema",
            ObjectKind::Table => "Table",
            ObjectKind::View => "View",
            ObjectKind::Procedure => "Stored Procedure",
            ObjectKind::ScalarFunction => "Scalar-valued Function",
            ObjectKind::TableFunction => "Table-valued Function",
            ObjectKind::AggregateFunction => "Aggregate Function",
            ObjectKind::Synonym => "Synonym",
            ObjectKind::Sequence => "Sequence",
            ObjectKind::TableType => "User-defined Table Type",
            ObjectKind::Column => "Column",
            ObjectKind::Parameter => "Parameter",
            ObjectKind::Index => "Index",
            ObjectKind::Key => "Key",
            ObjectKind::Constraint => "Constraint",
            ObjectKind::Trigger => "Trigger",
        }
    }
    /// `sys.objects.type` codes that map to this kind.
    pub fn from_sys_type(code: &str) -> Option<Self> {
        Some(match code.trim() {
            "U" => ObjectKind::Table,
            "V" => ObjectKind::View,
            "P" | "PC" | "X" => ObjectKind::Procedure,
            "FN" | "FS" => ObjectKind::ScalarFunction,
            "IF" | "TF" | "FT" => ObjectKind::TableFunction,
            "AF" => ObjectKind::AggregateFunction,
            "SN" => ObjectKind::Synonym,
            "SO" => ObjectKind::Sequence,
            "TT" => ObjectKind::TableType,
            "TR" | "TA" => ObjectKind::Trigger,
            _ => return None,
        })
    }
}

/// A fully qualified schema object.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ObjectRef {
    pub database: String,
    pub schema: String,
    pub name: String,
    pub kind: ObjectKind,
    pub object_id: Option<i32>,
}

impl ObjectRef {
    pub fn bracketed(&self) -> String {
        format!("[{}].[{}]", bracket(&self.schema), bracket(&self.name))
    }
    pub fn qualified(&self) -> String {
        format!("{}.{}", self.schema, self.name)
    }
}

impl fmt::Display for ObjectRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.schema, self.name)
    }
}

/// Escape `]` inside a bracketed identifier.
pub fn bracket(ident: &str) -> String {
    ident.replace(']', "]]")
}

/// Quote an identifier only if it needs it.
pub fn quote_ident(ident: &str) -> String {
    let simple = !ident.is_empty()
        && ident.chars().next().map(|c| c.is_ascii_alphabetic() || c == '_').unwrap_or(false)
        && ident.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$' || c == '#');
    if simple { ident.to_string() } else { format!("[{}]", bracket(ident)) }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnInfo {
    pub name: String,
    pub sql_type: SqlType,
    pub nullable: bool,
    /// 0-based position in the result set / table.
    pub ordinal: usize,
    #[serde(default)]
    pub is_identity: bool,
    #[serde(default)]
    pub is_computed: bool,
    #[serde(default)]
    pub in_primary_key: bool,
    #[serde(default)]
    pub default_definition: Option<String>,
    #[serde(default)]
    pub collation: Option<String>,
}

impl ColumnInfo {
    pub fn new(name: impl Into<String>, sql_type: SqlType, nullable: bool, ordinal: usize) -> Self {
        Self {
            name: name.into(),
            sql_type,
            nullable,
            ordinal,
            is_identity: false,
            is_computed: false,
            in_primary_key: false,
            default_definition: None,
            collation: None,
        }
    }
    pub fn type_label(&self) -> String {
        format!("{}{}", self.sql_type, if self.nullable { ", null" } else { ", not null" })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParameterInfo {
    pub name: String,
    pub sql_type: SqlType,
    pub is_output: bool,
    pub has_default: bool,
    pub ordinal: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexInfo {
    pub name: String,
    pub is_unique: bool,
    pub is_clustered: bool,
    pub is_primary_key: bool,
    pub is_unique_constraint: bool,
    pub index_type: String,
    pub key_columns: Vec<(String, bool)>, // (name, descending)
    pub included_columns: Vec<String>,
    pub filter: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyInfo {
    pub name: String,
    pub kind: KeyKind,
    pub columns: Vec<String>,
    /// For foreign keys: referenced table and columns.
    pub references: Option<(ObjectRef, Vec<String>)>,
    pub definition: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyKind {
    PrimaryKey,
    Unique,
    ForeignKey,
    Check,
    Default,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatabaseInfo {
    pub name: String,
    pub is_system: bool,
    pub state: String,
    pub is_read_only: bool,
}

/// A snapshot of one database's objects for completion and the tree. Cached per connection.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DatabaseCatalog {
    pub database: String,
    pub schemas: Vec<String>,
    pub objects: Vec<ObjectRef>,
    /// object_id → columns, filled lazily.
    #[serde(default)]
    pub columns: std::collections::HashMap<i32, Vec<ColumnInfo>>,
    pub refreshed_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl DatabaseCatalog {
    pub fn find(&self, schema: Option<&str>, name: &str) -> Option<&ObjectRef> {
        let name_l = name.to_ascii_lowercase();
        let schema_l = schema.map(|s| s.to_ascii_lowercase());
        self.objects.iter().find(|o| {
            o.name.to_ascii_lowercase() == name_l
                && match &schema_l {
                    Some(s) => &o.schema.to_ascii_lowercase() == s,
                    None => true,
                }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quoting() {
        assert_eq!(quote_ident("orders"), "orders");
        assert_eq!(quote_ident("Order Details"), "[Order Details]");
        assert_eq!(quote_ident("a]b"), "[a]]b]");
        assert_eq!(quote_ident("1st"), "[1st]");
    }
}
