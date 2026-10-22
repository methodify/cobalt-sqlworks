//! The SQL emitter for `ExploreSpec` over a local table: the twin of `cobalt-tabular`'s DAX
//! emitter, so the Explore builder (wells, filters, Top N, subtotals, the matrix) serves any
//! result set or file. One local table per spec (its name is the `FieldRef::Column.table`).

use cobalt_core::chart::Agg;
use cobalt_core::explore::{DateUnit, ExploreSpec, FieldRef, FilterOp, FilterSpec, SortBy, SortSpec, ValueRef};
use cobalt_core::tabular::{ModelCatalog, TabularDataType};

/// The generated query, with the result column names the matrix builder reads.
#[derive(Clone, Debug, PartialEq)]
pub struct SqlQuery {
    pub text: String,
    /// Result column names of the grouping fields, in spec order (rows then columns).
    pub group_columns: Vec<String>,
    /// Result column names of the values, in spec order.
    pub value_columns: Vec<String>,
    /// Per grouping field, the name of its subtotal flag column (`1` on subtotal rows), when
    /// `subtotals` is on; otherwise empty.
    pub subtotal_columns: Vec<String>,
}

/// Which SQL the emitter writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SqlDialect {
    /// The local engine (DataFusion): `"quoted"`, `LIMIT`, `ILIKE`, `now()`.
    DataFusion,
    /// SQL Server, Azure SQL, Fabric: `[quoted]`, `TOP`, `LIKE`, `DATEADD`.
    TSql,
}

fn q(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn qd(name: &str, d: SqlDialect) -> String {
    match d {
        SqlDialect::DataFusion => q(name),
        SqlDialect::TSql => format!("[{}]", name.replace(']', "]]")),
    }
}

fn lit(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// A literal typed by the column: numbers raw, dates as `DATE '…'`, text quoted.
fn typed(dt: TabularDataType, raw: &str, d: SqlDialect) -> Result<String, String> {
    let t = raw.trim();
    let tsql = d == SqlDialect::TSql;
    match dt {
        TabularDataType::Int64 | TabularDataType::Double | TabularDataType::Decimal => {
            if t.parse::<f64>().is_ok() { Ok(t.to_string()) } else { Err(format!("{raw:?} is not a number")) }
        }
        TabularDataType::Boolean => match t.to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" => Ok(if tsql { "1".into() } else { "TRUE".into() }),
            "false" | "0" | "no" => Ok(if tsql { "0".into() } else { "FALSE".into() }),
            _ => Err(format!("{raw:?} is not true or false")),
        },
        TabularDataType::DateTime => {
            let d = t.get(..10).unwrap_or(t);
            if d.len() == 10 && d.as_bytes()[4] == b'-' && d.as_bytes()[7] == b'-' {
                if tsql {
                    Ok(lit(t))
                } else if t.len() > 10 {
                    Ok(format!("TIMESTAMP {}", lit(t)))
                } else {
                    Ok(format!("DATE {}", lit(d)))
                }
            } else {
                Err(format!("{raw:?} is not a date (yyyy-mm-dd)"))
            }
        }
        _ => Ok(lit(t)),
    }
}

fn column_type(catalog: &ModelCatalog, table: &str, column: &str) -> TabularDataType {
    catalog.table(table).and_then(|t| t.columns.iter().find(|c| c.name.eq_ignore_ascii_case(column))).map(|c| c.data_type).unwrap_or_default()
}

fn filter_sql(f: &FilterSpec, catalog: &ModelCatalog, d: SqlDialect) -> Result<String, String> {
    let col = qd(&f.column, d);
    let dt = column_type(catalog, &f.table, &f.column);
    let tsql = d == SqlDialect::TSql;
    let text_col = if tsql { format!("CAST({col} AS NVARCHAR(4000))") } else { format!("CAST({col} AS VARCHAR)") };
    let like = if tsql { "LIKE" } else { "ILIKE" };
    let escape = if tsql { " ESCAPE '\\'" } else { "" };
    Ok(match &f.op {
        FilterOp::In { values } => {
            if values.is_empty() {
                return Ok(String::new());
            }
            let vs: Result<Vec<String>, String> = values.iter().map(|v| typed(dt, v, d)).collect();
            format!("{col} IN ({})", vs?.join(", "))
        }
        FilterOp::NotIn { values } => {
            if values.is_empty() {
                return Ok(String::new());
            }
            let vs: Result<Vec<String>, String> = values.iter().map(|v| typed(dt, v, d)).collect();
            format!("({col} IS NULL OR {col} NOT IN ({}))", vs?.join(", "))
        }
        FilterOp::Between { lo, hi } => {
            let mut parts = Vec::new();
            if let Some(lo) = lo.as_ref().filter(|s| !s.trim().is_empty()) {
                parts.push(format!("{col} >= {}", typed(dt, lo, d)?));
            }
            if let Some(hi) = hi.as_ref().filter(|s| !s.trim().is_empty()) {
                parts.push(format!("{col} <= {}", typed(dt, hi, d)?));
            }
            parts.join(" AND ")
        }
        FilterOp::Contains { text } => format!("{text_col} {like} {}{escape}", lit(&format!("%{}%", text.replace('%', "\\%").replace('_', "\\_")))),
        FilterOp::StartsWith { text } => format!("{text_col} {like} {}{escape}", lit(&format!("{}%", text.replace('%', "\\%").replace('_', "\\_")))),
        FilterOp::IsBlank => format!("{col} IS NULL"),
        FilterOp::NotBlank => format!("{col} IS NOT NULL"),
        FilterOp::RelativeDate { unit, last } => {
            if tsql {
                let part = match unit {
                    DateUnit::Days => "day",
                    DateUnit::Months => "month",
                    DateUnit::Years => "year",
                };
                format!("{col} >= DATEADD({part}, -{last}, SYSDATETIME())")
            } else {
                let interval = match unit {
                    DateUnit::Days => format!("{last} days"),
                    DateUnit::Months => format!("{last} months"),
                    DateUnit::Years => format!("{last} years"),
                };
                format!("{col} >= now() - INTERVAL {}", lit(&interval))
            }
        }
    })
}

fn value_sql(v: &ValueRef, d: SqlDialect) -> Result<String, String> {
    match &v.field {
        FieldRef::Measure { name } => Err(format!("{name}: a table has no measures; add a column with an aggregation")),
        FieldRef::Column { column, .. } => {
            let c = qd(column, d);
            Ok(match v.agg {
                Agg::None | Agg::Sum => format!("sum({c})"),
                Agg::Count => format!("count({c})"),
                Agg::CountDistinct => format!("count(DISTINCT {c})"),
                Agg::Avg => format!("avg({c})"),
                Agg::Min => format!("min({c})"),
                Agg::Max => format!("max({c})"),
            })
        }
    }
}

/// Emit the query for the local engine. The table is the one the fields name; every field
/// must name the same table.
pub fn to_sql(spec: &ExploreSpec, catalog: &ModelCatalog) -> Result<SqlQuery, String> {
    to_sql_in(spec, catalog, SqlDialect::DataFusion, None)
}

/// Emit the query in `dialect`; `from` is the FROM target already quoted for that dialect
/// (`None` = the fields' table, quoted).
pub fn to_sql_in(spec: &ExploreSpec, catalog: &ModelCatalog, d: SqlDialect, from: Option<&str>) -> Result<SqlQuery, String> {
    let q = |name: &str| qd(name, d);
    let groups: Vec<&FieldRef> = spec.rows.iter().chain(spec.columns.iter()).collect();
    if groups.is_empty() && spec.values.is_empty() {
        return Err("add a field to Rows, Columns or Values".into());
    }
    let mut table: Option<String> = None;
    let mut group_columns = Vec::new();
    let mut select = Vec::new();
    let mut group_by = Vec::new();
    for g in &groups {
        let FieldRef::Column { table: t, column } = g else { return Err("measures cannot group".into()) };
        if let Some(prev) = &table {
            if !prev.eq_ignore_ascii_case(t) {
                return Err(format!("fields from two tables ({prev}, {t}); Explore over a local table uses one"));
            }
        } else {
            table = Some(t.clone());
        }
        select.push(format!("{} AS {}", q(column), q(column)));
        group_by.push(q(column));
        group_columns.push(column.clone());
    }
    let mut value_columns = Vec::new();
    for v in &spec.values {
        if let FieldRef::Column { table: t, .. } = &v.field {
            if let Some(prev) = &table {
                if !prev.eq_ignore_ascii_case(t) {
                    return Err(format!("fields from two tables ({prev}, {t}); Explore over a local table uses one"));
                }
            } else {
                table = Some(t.clone());
            }
        }
        let name = v.name();
        select.push(format!("{} AS {}", value_sql(v, d)?, q(&name)));
        value_columns.push(name);
    }
    let Some(table) = table else { return Err("add a column to Rows, Columns or Values".into()) };
    let mut subtotal_columns = Vec::new();
    if spec.subtotals && !groups.is_empty() {
        for (i, gcol) in group_by.iter().enumerate() {
            let flag = format!("IsSubtotal {}", i + 1);
            select.push(format!("GROUPING({gcol}) AS {}", q(&flag)));
            subtotal_columns.push(flag);
        }
    }
    let top = match (d, spec.top_n) {
        (SqlDialect::TSql, Some(n)) => format!("TOP ({n}) "),
        _ => String::new(),
    };
    let from_sql = from.map(str::to_string).unwrap_or_else(|| q(&table));
    let mut text = format!("SELECT {top}{}\nFROM {from_sql}", select.join(",\n       "));
    let wheres: Result<Vec<String>, String> = spec.filters.iter().map(|f| filter_sql(f, catalog, d)).collect();
    let wheres: Vec<String> = wheres?.into_iter().filter(|w| !w.is_empty()).collect();
    if !wheres.is_empty() {
        text.push_str(&format!("\nWHERE {}", wheres.join("\n  AND ")));
    }
    if !group_by.is_empty() {
        if spec.subtotals {
            text.push_str(&format!("\nGROUP BY ROLLUP ({})", group_by.join(", ")));
        } else {
            text.push_str(&format!("\nGROUP BY {}", group_by.join(", ")));
        }
    }
    // order: the explicit sort, else the groups in order (subtotal rows after their group)
    let mut order: Vec<String> = Vec::new();
    match &spec.sort {
        Some(SortSpec { by, desc }) => {
            let dir = if *desc { "DESC" } else { "ASC" };
            match by {
                SortBy::Field { field } => {
                    let FieldRef::Column { column, .. } = field else { return Err("sort by a column".into()) };
                    if !group_columns.iter().any(|g| g == column) {
                        return Err(format!("sort column {column} is not in Rows or Columns"));
                    }
                    if spec.subtotals {
                        if let Some(i) = group_columns.iter().position(|g| g == column) {
                            order.push(format!("{} ASC", q(&format!("IsSubtotal {}", i + 1))));
                        }
                    }
                    order.push(format!("{} {dir}{nulls}", q(column), nulls = if d == SqlDialect::TSql { "" } else { " NULLS LAST" }));
                }
                SortBy::Value { index } => {
                    let Some(name) = value_columns.get(*index) else { return Err("the sorted value is gone".into()) };
                    order.push(format!("{} {dir}{nulls}", q(name), nulls = if d == SqlDialect::TSql { "" } else { " NULLS LAST" }));
                }
            }
        }
        None => {
            for (i, g) in group_columns.iter().enumerate() {
                if spec.subtotals {
                    order.push(format!("{} ASC", q(&format!("IsSubtotal {}", i + 1))));
                }
                order.push(format!("{} ASC{nulls}", q(g), nulls = if d == SqlDialect::TSql { "" } else { " NULLS LAST" }));
            }
        }
    }
    if !order.is_empty() {
        text.push_str(&format!("\nORDER BY {}", order.join(", ")));
    }
    if let (SqlDialect::DataFusion, Some(n)) = (d, spec.top_n) {
        text.push_str(&format!("\nLIMIT {n}"));
    }
    Ok(SqlQuery { text, group_columns, value_columns, subtotal_columns })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cobalt_core::explore::ExploreSource;
    use cobalt_core::tabular::{Column, Table};

    fn catalog() -> ModelCatalog {
        let col = |n: &str, dt: TabularDataType| Column { name: n.into(), data_type: dt, hidden: false, key: false, calculated: false, format_string: None, sort_by_column: None, display_folder: None, description: None, expression: None, summarize_by: None };
        ModelCatalog { name: "local".into(), description: None, tables: vec![Table { name: "orders".into(), hidden: false, storage_mode: None, description: None, expression: None, columns: vec![col("customer", TabularDataType::String), col("amount", TabularDataType::Double), col("ordered_on", TabularDataType::DateTime)], measures: vec![], hierarchies: vec![] }], relationships: vec![], calculation_groups: vec![], expressions_hidden: false }
    }

    fn spec() -> ExploreSpec {
        let mut s = ExploreSpec::new(ExploreSource::SemanticModel { target: cobalt_core::tabular::TabularTarget::Http { url: "x".into(), model: "m".into() } });
        s.rows.push(FieldRef::Column { table: "orders".into(), column: "customer".into() });
        s.values.push(ValueRef { field: FieldRef::Column { table: "orders".into(), column: "amount".into() }, agg: Agg::Sum, label: None });
        s
    }

    #[test]
    fn group_and_sum() {
        let q = to_sql(&spec(), &catalog()).unwrap();
        assert_eq!(q.text, "SELECT \"customer\" AS \"customer\",\n       sum(\"amount\") AS \"Sum of amount\"\nFROM \"orders\"\nGROUP BY \"customer\"\nORDER BY \"customer\" ASC NULLS LAST");
        assert_eq!(q.group_columns, vec!["customer"]);
        assert_eq!(q.value_columns, vec!["Sum of amount"]);
    }

    #[test]
    fn filters_sort_top_subtotals() {
        let mut s = spec();
        s.filters.push(FilterSpec { table: "orders".into(), column: "customer".into(), op: FilterOp::In { values: vec!["Ann".into(), "O'Neil".into()] } });
        s.filters.push(FilterSpec { table: "orders".into(), column: "amount".into(), op: FilterOp::Between { lo: Some("10".into()), hi: None } });
        s.filters.push(FilterSpec { table: "orders".into(), column: "ordered_on".into(), op: FilterOp::RelativeDate { unit: DateUnit::Days, last: 30 } });
        s.sort = Some(SortSpec { by: SortBy::Value { index: 0 }, desc: true });
        s.top_n = Some(5);
        s.subtotals = true;
        let q = to_sql(&s, &catalog()).unwrap();
        assert!(q.text.contains("WHERE \"customer\" IN ('Ann', 'O''Neil')\n  AND \"amount\" >= 10\n  AND \"ordered_on\" >= now() - INTERVAL '30 days'"));
        assert!(q.text.contains("GROUP BY ROLLUP (\"customer\")"));
        assert!(q.text.contains("GROUPING(\"customer\") AS \"IsSubtotal 1\""));
        assert!(q.text.ends_with("ORDER BY \"Sum of amount\" DESC NULLS LAST\nLIMIT 5"));
        assert_eq!(q.subtotal_columns, vec!["IsSubtotal 1"]);
    }

    #[test]
    fn tsql_dialect() {
        let mut spec = ExploreSpec::new(ExploreSource::Local { table: "orders".into() });
        spec.rows.push(FieldRef::Column { table: "orders".into(), column: "customer".into() });
        spec.values.push(ValueRef { field: FieldRef::Column { table: "orders".into(), column: "amount".into() }, agg: Agg::Sum, label: None });
        spec.filters.push(FilterSpec { table: "orders".into(), column: "customer".into(), op: FilterOp::Contains { text: "an".into() } });
        spec.filters.push(FilterSpec { table: "orders".into(), column: "ordered_on".into(), op: FilterOp::RelativeDate { unit: DateUnit::Months, last: 3 } });
        spec.filters.push(FilterSpec { table: "orders".into(), column: "ordered_on".into(), op: FilterOp::Between { lo: Some("2024-01-01".into()), hi: None } });
        spec.sort = Some(SortSpec { by: SortBy::Value { index: 0 }, desc: true });
        spec.top_n = Some(5);
        spec.subtotals = true;
        let q = to_sql_in(&spec, &catalog(), SqlDialect::TSql, Some("[Shop].[dbo].[orders]")).unwrap();
        assert_eq!(q.text, "SELECT TOP (5) [customer] AS [customer],\n       sum([amount]) AS [Sum of amount],\n       GROUPING([customer]) AS [IsSubtotal 1]\nFROM [Shop].[dbo].[orders]\nWHERE CAST([customer] AS NVARCHAR(4000)) LIKE '%an%' ESCAPE '\\'\n  AND [ordered_on] >= DATEADD(month, -3, SYSDATETIME())\n  AND [ordered_on] >= '2024-01-01'\nGROUP BY ROLLUP ([customer])\nORDER BY [Sum of amount] DESC");
    }

    #[test]
    fn refuses_measures_and_two_tables() {
        let mut s = spec();
        s.values.push(ValueRef::measure("Total"));
        assert!(to_sql(&s, &catalog()).unwrap_err().contains("no measures"));
        let mut s = spec();
        s.columns.push(FieldRef::Column { table: "other".into(), column: "x".into() });
        assert!(to_sql(&s, &catalog()).unwrap_err().contains("two tables"));
    }
}
