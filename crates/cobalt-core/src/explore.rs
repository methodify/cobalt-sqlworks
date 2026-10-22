//! Explore: a pivot-table-style query spec (rows, columns, values, filters, sort, top N,
//! subtotals) that is engine-agnostic. The DAX emitter lives in `cobalt-tabular`; a SQL emitter
//! can follow. Saved as `.cobalt-explore.json` and carried by dashboard tiles.
//! Design: `docs/design/semantic_models.md` §3.1 (5) and §3.2.

use crate::chart::Agg;
use crate::tabular::TabularTarget;
use serde::{Deserialize, Serialize};

pub const EXPLORE_EXT: &str = ".cobalt-explore.json";

/// Where the fields come from.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExploreSource {
    SemanticModel { target: TabularTarget },
    /// A local table of the in-process engine (a result set, a file).
    Local { table: String },
}

impl ExploreSource {
    pub fn local_table(&self) -> Option<&str> {
        match self {
            ExploreSource::Local { table } => Some(table),
            _ => None,
        }
    }
}

/// A field of the source: a model column or a measure.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FieldRef {
    Column { table: String, column: String },
    Measure { name: String },
}

impl FieldRef {
    pub fn label(&self) -> String {
        match self {
            FieldRef::Column { column, .. } => column.clone(),
            FieldRef::Measure { name } => name.clone(),
        }
    }
    pub fn is_measure(&self) -> bool {
        matches!(self, FieldRef::Measure { .. })
    }
    /// `Table[Column]` or `[Measure]`, for labels and result column names.
    pub fn qualified(&self) -> String {
        match self {
            FieldRef::Column { table, column } => format!("{table}[{column}]"),
            FieldRef::Measure { name } => format!("[{name}]"),
        }
    }
}

/// A value well entry: a measure as is, or a column with an aggregation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValueRef {
    pub field: FieldRef,
    /// Ignored for measures (`Agg::None`).
    #[serde(default)]
    pub agg: Agg,
    /// The result column's name; `None` = derived ("Sum of Amount", the measure's name).
    #[serde(default)]
    pub label: Option<String>,
}

impl ValueRef {
    pub fn measure(name: impl Into<String>) -> Self {
        Self { field: FieldRef::Measure { name: name.into() }, agg: Agg::None, label: None }
    }
    pub fn column(table: impl Into<String>, column: impl Into<String>, agg: Agg) -> Self {
        Self { field: FieldRef::Column { table: table.into(), column: column.into() }, agg, label: None }
    }
    /// The result column's name.
    pub fn name(&self) -> String {
        if let Some(l) = &self.label {
            return l.clone();
        }
        match &self.field {
            FieldRef::Measure { name } => name.clone(),
            FieldRef::Column { column, .. } => match self.agg {
                Agg::None | Agg::Sum => format!("Sum of {column}"),
                Agg::Count => format!("Count of {column}"),
                Agg::CountDistinct => format!("Distinct {column}"),
                Agg::Avg => format!("Average {column}"),
                Agg::Min => format!("Min {column}"),
                Agg::Max => format!("Max {column}"),
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DateUnit {
    Days,
    Months,
    Years,
}

/// A filter on a column. Values are the column's text form; the emitter types them with the
/// catalog (text quoted, numbers raw, dates as `DATE(y, m, d)`).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum FilterOp {
    /// One of these values (`TREATAS`).
    In { values: Vec<String> },
    NotIn { values: Vec<String> },
    /// Inclusive range; either bound may be open.
    Between { lo: Option<String>, hi: Option<String> },
    Contains { text: String },
    StartsWith { text: String },
    IsBlank,
    NotBlank,
    /// The last `n` units up to today (date columns).
    RelativeDate { unit: DateUnit, last: u32 },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FilterSpec {
    pub table: String,
    pub column: String,
    pub op: FilterOp,
}

impl FilterSpec {
    pub fn summary(&self) -> String {
        let v = match &self.op {
            FilterOp::In { values } => match values.len() {
                0 => "is any".to_string(),
                1 => format!("= {}", values[0]),
                n => format!("in {n} values"),
            },
            FilterOp::NotIn { values } => format!("not in {} values", values.len()),
            FilterOp::Between { lo, hi } => match (lo, hi) {
                (Some(a), Some(b)) => format!("{a} … {b}"),
                (Some(a), None) => format!("≥ {a}"),
                (None, Some(b)) => format!("≤ {b}"),
                (None, None) => "any".to_string(),
            },
            FilterOp::Contains { text } => format!("contains {text}"),
            FilterOp::StartsWith { text } => format!("starts with {text}"),
            FilterOp::IsBlank => "is blank".to_string(),
            FilterOp::NotBlank => "is not blank".to_string(),
            FilterOp::RelativeDate { unit, last } => format!("last {last} {}", match unit {
                DateUnit::Days => "days",
                DateUnit::Months => "months",
                DateUnit::Years => "years",
            }),
        };
        format!("{} {v}", self.column)
    }
}

/// What the result is sorted by.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "by", rename_all = "snake_case")]
pub enum SortBy {
    /// A grouping field (rows or columns well).
    Field { field: FieldRef },
    /// A value well entry by index.
    Value { index: usize },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SortSpec {
    pub by: SortBy,
    #[serde(default)]
    pub desc: bool,
}

/// The pivot spec: what to group by (rows down, columns across), what to show, filters, order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExploreSpec {
    pub version: u32,
    pub title: String,
    pub source: Option<ExploreSource>,
    pub rows: Vec<FieldRef>,
    pub columns: Vec<FieldRef>,
    pub values: Vec<ValueRef>,
    pub filters: Vec<FilterSpec>,
    pub sort: Option<SortSpec>,
    pub top_n: Option<u64>,
    pub subtotals: bool,
}

impl Default for ExploreSpec {
    fn default() -> Self {
        Self { version: 1, title: String::new(), source: None, rows: Vec::new(), columns: Vec::new(), values: Vec::new(), filters: Vec::new(), sort: None, top_n: None, subtotals: false }
    }
}

impl ExploreSpec {
    pub fn new(source: ExploreSource) -> Self {
        Self { source: Some(source), ..Default::default() }
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let s: Self = serde_json::from_str(text).map_err(|e| e.to_string())?;
        if s.version > 1 {
            return Err(format!("this file needs a newer Cobalt (explore version {})", s.version));
        }
        Ok(s)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    pub fn is_explore_path(p: &std::path::Path) -> bool {
        p.to_string_lossy().to_ascii_lowercase().ends_with(EXPLORE_EXT)
    }

    /// Every grouping field, rows first then columns.
    pub fn group_fields(&self) -> impl Iterator<Item = &FieldRef> {
        self.rows.iter().chain(self.columns.iter())
    }

    /// Nothing to ask for yet.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty() && self.columns.is_empty() && self.values.is_empty()
    }

    /// The field is in a well already.
    pub fn contains(&self, f: &FieldRef) -> bool {
        self.group_fields().any(|x| x == f) || self.values.iter().any(|v| &v.field == f)
    }

    pub fn remove(&mut self, f: &FieldRef) {
        self.rows.retain(|x| x != f);
        self.columns.retain(|x| x != f);
        self.values.retain(|v| &v.field != f);
        if let Some(SortSpec { by: SortBy::Field { field }, .. }) = &self.sort {
            if field == f {
                self.sort = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_names() {
        let mut s = ExploreSpec::new(ExploreSource::SemanticModel { target: TabularTarget::Http { url: "u".into(), model: "m".into() } });
        s.rows.push(FieldRef::Column { table: "Date".into(), column: "Year".into() });
        s.values.push(ValueRef::measure("Sales Amount"));
        s.values.push(ValueRef::column("Sales", "Quantity", Agg::Avg));
        s.filters.push(FilterSpec { table: "Product".into(), column: "Category".into(), op: FilterOp::In { values: vec!["A".into(), "B".into()] } });
        let back = ExploreSpec::parse(&s.to_json()).unwrap();
        assert_eq!(back, s);
        assert_eq!(s.values[1].name(), "Average Quantity");
        assert_eq!(s.filters[0].summary(), "Category in 2 values");
        assert!(s.contains(&FieldRef::Measure { name: "Sales Amount".into() }));
        s.remove(&FieldRef::Column { table: "Date".into(), column: "Year".into() });
        assert!(s.rows.is_empty());
    }
}
