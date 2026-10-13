//! Semantic models (Power BI / Fabric) and Analysis Services: the target a DAX tab queries and
//! the model's catalog (tables, columns, measures, hierarchies, relationships) as plain data.
//! Design: `docs/design/semantic_models.md`. No IO here: `cobalt-tabular` fills these from the
//! Arrow query API (`INFO.VIEW.*`) or XMLA rowsets; the editor's DAX completer reads them.

use serde::{Deserialize, Serialize};

/// Where a DAX query goes.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TabularTarget {
    /// A Power BI / Fabric semantic model on a capacity, queried through the Arrow query API
    /// (`executeDaxQueries`) and, later, XMLA.
    PowerBi {
        workspace_id: String,
        workspace_name: String,
        /// The semantic model's item id (the Power BI dataset id).
        dataset_id: String,
        model_name: String,
    },
    /// Azure Analysis Services: `asazure://<region>.asazure.windows.net/<server>`.
    AzureAs { region: String, server: String, model: String },
    /// Analysis Services over HTTP (`msmdpump.dll`) or any XMLA URL.
    Http { url: String, model: String },
}

impl TabularTarget {
    /// The model's display name.
    pub fn model(&self) -> &str {
        match self {
            TabularTarget::PowerBi { model_name, .. } => model_name,
            TabularTarget::AzureAs { model, .. } | TabularTarget::Http { model, .. } => model,
        }
    }

    /// What the status bar shows next to the model: the workspace, the server, or the URL host.
    pub fn host_label(&self) -> String {
        match self {
            TabularTarget::PowerBi { workspace_name, .. } => workspace_name.clone(),
            TabularTarget::AzureAs { region, server, .. } => format!("{server} ({region})"),
            TabularTarget::Http { url, .. } => url.split("://").nth(1).unwrap_or(url).split('/').next().unwrap_or(url).to_string(),
        }
    }

    /// The `powerbi://` / `asazure://` connection string other tools accept (Excel, SSMS,
    /// Tabular Editor, DAX Studio).
    pub fn connection_string(&self) -> String {
        match self {
            TabularTarget::PowerBi { workspace_name, model_name, .. } => {
                format!("Data Source=powerbi://api.powerbi.com/v1.0/myorg/{};Initial Catalog={model_name}", url_encode(workspace_name))
            }
            TabularTarget::AzureAs { region, server, model } => format!("Data Source=asazure://{region}.asazure.windows.net/{server};Initial Catalog={model}"),
            TabularTarget::Http { url, model } => format!("Data Source={url};Initial Catalog={model}"),
        }
    }

    /// Stable identity for pins, snapshots and tiles.
    pub fn key(&self) -> String {
        match self {
            TabularTarget::PowerBi { dataset_id, .. } => format!("pbi:{dataset_id}"),
            TabularTarget::AzureAs { region, server, model } => format!("asazure:{region}/{server}/{model}"),
            TabularTarget::Http { url, model } => format!("http:{url}#{model}"),
        }
    }
}

fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// A tabular model's data types (the TOM `DataType` names as `INFO.VIEW.COLUMNS()[DataType]`
/// reports them).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TabularDataType {
    String,
    Int64,
    Double,
    Decimal,
    DateTime,
    Boolean,
    Binary,
    Variant,
    #[default]
    Unknown,
}

impl TabularDataType {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "string" | "text" => Self::String,
            "int64" | "integer" | "whole number" | "int" => Self::Int64,
            "double" | "decimal number" | "float" => Self::Double,
            "decimal" | "currency" | "fixed decimal number" => Self::Decimal,
            "datetime" | "date" | "date/time" | "time" => Self::DateTime,
            "boolean" | "true/false" | "bool" => Self::Boolean,
            "binary" => Self::Binary,
            "variant" => Self::Variant,
            _ => Self::Unknown,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::String => "text",
            Self::Int64 => "whole number",
            Self::Double => "decimal number",
            Self::Decimal => "fixed decimal",
            Self::DateTime => "date/time",
            Self::Boolean => "true/false",
            Self::Binary => "binary",
            Self::Variant => "variant",
            Self::Unknown => "unknown",
        }
    }

    pub fn is_numeric(self) -> bool {
        matches!(self, Self::Int64 | Self::Double | Self::Decimal)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Column {
    pub name: String,
    pub data_type: TabularDataType,
    pub hidden: bool,
    pub key: bool,
    /// `Data`, `Calculated` or `RowNumber` (the last is filtered out of catalogs).
    pub calculated: bool,
    pub format_string: Option<String>,
    pub sort_by_column: Option<String>,
    pub display_folder: Option<String>,
    pub description: Option<String>,
    /// Calculated columns only; blank when the service hides expressions from this user.
    pub expression: Option<String>,
    /// TOM `SummarizeBy` (`Sum`, `Count`, `None`, `Default`, …) — the builder's default aggregation.
    pub summarize_by: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Measure {
    pub name: String,
    pub data_type: TabularDataType,
    pub hidden: bool,
    pub format_string: Option<String>,
    pub display_folder: Option<String>,
    pub description: Option<String>,
    /// Blank when the service hides expressions from this user (Build-only access).
    pub expression: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Level {
    pub name: String,
    pub column: String,
    pub ordinal: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Hierarchy {
    pub name: String,
    pub hidden: bool,
    pub display_folder: Option<String>,
    pub levels: Vec<Level>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Table {
    pub name: String,
    pub hidden: bool,
    /// `Import`, `DirectQuery`, `DirectLake`, `Dual`, … as the model reports it.
    pub storage_mode: Option<String>,
    pub description: Option<String>,
    /// Calculated tables only.
    pub expression: Option<String>,
    pub columns: Vec<Column>,
    pub measures: Vec<Measure>,
    pub hierarchies: Vec<Hierarchy>,
}

impl Table {
    pub fn visible_columns(&self) -> impl Iterator<Item = &Column> {
        self.columns.iter().filter(|c| !c.hidden)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Relationship {
    pub from_table: String,
    pub from_column: String,
    pub to_table: String,
    pub to_column: String,
    /// `One` / `Many` on each side.
    pub from_cardinality: String,
    pub to_cardinality: String,
    pub active: bool,
    /// `OneDirection` / `BothDirections`.
    pub cross_filter: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CalculationGroup {
    pub table: String,
    pub items: Vec<String>,
}

/// The model as the explorer, the completer and the builder see it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelCatalog {
    pub name: String,
    pub description: Option<String>,
    pub tables: Vec<Table>,
    pub relationships: Vec<Relationship>,
    pub calculation_groups: Vec<CalculationGroup>,
    /// True when measure / calculated column expressions came back blank, i.e. the service hid
    /// them from this user (Build-only access).
    pub expressions_hidden: bool,
}

impl ModelCatalog {
    pub fn table(&self, name: &str) -> Option<&Table> {
        self.tables.iter().find(|t| t.name.eq_ignore_ascii_case(name))
    }

    pub fn measure(&self, name: &str) -> Option<(&Table, &Measure)> {
        self.tables.iter().find_map(|t| t.measures.iter().find(|m| m.name.eq_ignore_ascii_case(name)).map(|m| (t, m)))
    }

    pub fn all_measures(&self) -> impl Iterator<Item = (&Table, &Measure)> {
        self.tables.iter().flat_map(|t| t.measures.iter().map(move |m| (t, m)))
    }

    pub fn column_count(&self) -> usize {
        self.tables.iter().map(|t| t.columns.len()).sum()
    }

    pub fn measure_count(&self) -> usize {
        self.tables.iter().map(|t| t.measures.len()).sum()
    }
}

/// DAX quoting: `'Table'[Column]` and `[Measure]`.
pub fn dax_table_ref(table: &str) -> String {
    format!("'{}'", table.replace('\'', "''"))
}

pub fn dax_column_ref(table: &str, column: &str) -> String {
    format!("{}[{}]", dax_table_ref(table), column.replace(']', "]]"))
}

pub fn dax_measure_ref(measure: &str) -> String {
    format!("[{}]", measure.replace(']', "]]"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refs_and_strings() {
        assert_eq!(dax_column_ref("Date", "Year"), "'Date'[Year]");
        assert_eq!(dax_column_ref("O'Brien", "a]b"), "'O''Brien'[a]]b]");
        assert_eq!(dax_measure_ref("Sales Amount"), "[Sales Amount]");
        let t = TabularTarget::PowerBi { workspace_id: "w".into(), workspace_name: "Fabric test".into(), dataset_id: "d".into(), model_name: "Sales".into() };
        assert_eq!(t.connection_string(), "Data Source=powerbi://api.powerbi.com/v1.0/myorg/Fabric%20test;Initial Catalog=Sales");
        assert_eq!(t.key(), "pbi:d");
        assert_eq!(TabularDataType::parse("Int64"), TabularDataType::Int64);
        assert_eq!(TabularDataType::parse("Decimal Number"), TabularDataType::Double);
    }
}
