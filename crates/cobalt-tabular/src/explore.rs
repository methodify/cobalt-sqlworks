//! The Explore pivot builder's DAX emitter: an engine-agnostic `ExploreSpec` (rows, columns,
//! values, filters, sort, top N, subtotals) becomes the query Power BI's own visuals would send,
//! typed and validated against the model's catalog. Design: `docs/design/semantic_models.md`
//! §3.1 (5) and §3.2.
//!
//! Shapes:
//! - grouping fields: `SUMMARIZECOLUMNS(groups…, filters…, "Name", expr, …)`, each group
//!   wrapped in `ROLLUPADDISSUBTOTAL('T'[C], "IsSubtotal <n>")` when subtotals are on;
//! - values only: `ROW("Name", expr, …)`, filters folded into `CALCULATE(expr, __F1, …)`;
//! - filters: one `VAR __F<n>` table expression each, in a `DEFINE` block;
//! - top N: `TOPN(n, <table>, <order expr>, 0|1)` around the table;
//! - `ORDER BY` after the `EVALUATE` (see [`to_dax`] for the subtotal convention).

use cobalt_core::chart::Agg;
use cobalt_core::explore::{DateUnit, ExploreSpec, FieldRef, FilterOp, FilterSpec, SortBy, SortSpec, ValueRef};
use cobalt_core::tabular::{dax_column_ref, dax_measure_ref, dax_table_ref, Column, ModelCatalog, TabularDataType};

/// A generated query and the names the service gives its result columns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DaxQuery {
    pub text: String,
    /// Result column names of the grouping fields, in spec order (rows then columns), as the
    /// service names them: `Table[Column]`.
    pub group_columns: Vec<String>,
    /// Result column names of the values, in spec order: `[Name]` (`ValueRef::name()`).
    pub value_columns: Vec<String>,
    /// Per grouping field, the name of its subtotal flag column (`[IsSubtotal <n>]`), when
    /// `subtotals` is on; otherwise empty.
    pub subtotal_columns: Vec<String>,
}

const INDENT: &str = "    ";

/// The spec as DAX, validated against the catalog: every table, column and measure must exist
/// (measures by name anywhere in the model), filter literals must fit the column's data type,
/// the sort target must be a grouping field or a value index, Rows / Columns take columns only.
///
/// `ORDER BY` convention: without subtotals it names the sort target alone. With subtotals the
/// order is, per grouping field in spec order, `[IsSubtotal n] ASC, 'T'[C] <dir>` (the user's
/// direction when the sort targets that field, else `ASC`), and a sort by value goes last. So
/// detail rows come before their subtotal row within a group, the grand total is the last row,
/// and a value sort orders the detail rows inside the innermost group.
///
/// Top N orders by the sort target; with no sort, by the first value descending (or the first
/// grouping field ascending when there are no values). Formatting: four-space indentation, a
/// `DEFINE` block only when there are filters, no trailing newline.
pub fn to_dax(spec: &ExploreSpec, catalog: &ModelCatalog) -> Result<DaxQuery, String> {
    if spec.is_empty() {
        return Err("add a field to Rows, Columns or Values first".into());
    }

    // Grouping fields: columns only, every one in the catalog.
    let mut groups: Vec<String> = Vec::new();
    let mut group_columns = Vec::new();
    for f in spec.group_fields() {
        match f {
            FieldRef::Column { table, column } => {
                resolve_column(catalog, table, column)?;
                groups.push(dax_column_ref(table, column));
                group_columns.push(f.qualified());
            }
            FieldRef::Measure { name } => return Err(format!("Rows and Columns take columns, not measures: {}", dax_measure_ref(name))),
        }
    }

    // Values: name + the expression before any CALCULATE wrapping.
    let mut values: Vec<(String, Value)> = Vec::new();
    for v in &spec.values {
        values.push((v.name(), value_expr(v, catalog)?));
    }
    let value_columns: Vec<String> = values.iter().map(|(n, _)| format!("[{}]", n.replace(']', "]]"))).collect();

    // Filters: one table expression each.
    let mut filter_vars: Vec<String> = Vec::new();
    for (i, f) in spec.filters.iter().enumerate() {
        let col = resolve_column(catalog, &f.table, &f.column)?;
        filter_vars.push(format!("VAR __F{} = {}", i + 1, filter_expr(f, col)?));
    }
    let filter_names: Vec<String> = (1..=spec.filters.len()).map(|i| format!("__F{i}")).collect();

    // Sort target as an ORDER BY / TOPN expression.
    let sort = spec.sort.as_ref().map(|s| sort_target(s, spec, &value_columns)).transpose()?;

    let mut out = String::new();
    if !filter_vars.is_empty() {
        out.push_str("DEFINE\n");
        for v in &filter_vars {
            out.push_str(INDENT);
            out.push_str(v);
            out.push('\n');
        }
    }
    out.push_str("EVALUATE\n");

    if groups.is_empty() {
        // Values only: a single row.
        let mut lines = vec!["ROW(".to_string()];
        for (i, (name, v)) in values.iter().enumerate() {
            let expr = if filter_names.is_empty() { v.plain() } else { format!("CALCULATE({}, {})", v.core, filter_names.join(", ")) };
            lines.push(format!("{INDENT}{}, {expr}{}", dax_string(name), comma(i, values.len())));
        }
        lines.push(")".into());
        push_indented(&mut out, &lines, 1);
        return Ok(DaxQuery { text: out, group_columns, value_columns, subtotal_columns: Vec::new() });
    }

    let subtotal_columns: Vec<String> = if spec.subtotals { (1..=groups.len()).map(|i| format!("[IsSubtotal {i}]")).collect() } else { Vec::new() };
    let mut args: Vec<String> = Vec::new();
    for (i, g) in groups.iter().enumerate() {
        if spec.subtotals {
            args.push(format!("ROLLUPADDISSUBTOTAL({g}, \"IsSubtotal {}\")", i + 1));
        } else {
            args.push(g.clone());
        }
    }
    args.extend(filter_names.iter().cloned());
    for (name, v) in &values {
        args.push(format!("{}, {}", dax_string(name), v.plain()));
    }
    let mut table = vec!["SUMMARIZECOLUMNS(".to_string()];
    let n = args.len();
    for (i, a) in args.into_iter().enumerate() {
        table.push(format!("{INDENT}{a}{}", comma(i, n)));
    }
    table.push(")".into());

    let table = match spec.top_n {
        Some(0) => return Err("Top N needs a number greater than zero".into()),
        Some(n) => {
            let (expr, desc) = match &sort {
                Some(s) => s.clone(),
                None if !value_columns.is_empty() => (value_columns[0].clone(), true),
                None => (groups[0].clone(), false),
            };
            let mut t = vec!["TOPN(".to_string(), format!("{INDENT}{n},")];
            for (i, l) in table.iter().enumerate() {
                let last = i + 1 == table.len();
                t.push(format!("{INDENT}{l}{}", if last { "," } else { "" }));
            }
            t.push(format!("{INDENT}{expr}, {}", if desc { 0 } else { 1 }));
            t.push(")".into());
            t
        }
        None => table,
    };
    push_indented(&mut out, &table, 1);

    let order = order_by(spec, &groups, &sort);
    if !order.is_empty() {
        out.push_str("\nORDER BY ");
        out.push_str(&order.join(", "));
    }

    Ok(DaxQuery { text: out, group_columns, value_columns, subtotal_columns })
}

/// One line for titles and tiles: "Rows: Category, Year · Values: Sales Amount, Average
/// Quantity · 2 filters".
pub fn describe(spec: &ExploreSpec) -> String {
    let mut parts = Vec::new();
    let labels = |fs: &[FieldRef]| fs.iter().map(FieldRef::label).collect::<Vec<_>>().join(", ");
    if !spec.rows.is_empty() {
        parts.push(format!("Rows: {}", labels(&spec.rows)));
    }
    if !spec.columns.is_empty() {
        parts.push(format!("Columns: {}", labels(&spec.columns)));
    }
    if !spec.values.is_empty() {
        parts.push(format!("Values: {}", spec.values.iter().map(ValueRef::name).collect::<Vec<_>>().join(", ")));
    }
    match spec.filters.len() {
        0 => {}
        1 => parts.push("1 filter".into()),
        n => parts.push(format!("{n} filters")),
    }
    if let Some(n) = spec.top_n {
        parts.push(format!("Top {n}"));
    }
    if parts.is_empty() {
        return "Nothing selected".into();
    }
    parts.join(" · ")
}

/// A value's expression: `core` is what goes inside `CALCULATE`; measures stay bare unless
/// filters are folded in (the `ROW` form).
struct Value {
    core: String,
    is_measure: bool,
}

impl Value {
    fn plain(&self) -> String {
        if self.is_measure {
            self.core.clone()
        } else {
            format!("CALCULATE({})", self.core)
        }
    }
}

fn value_expr(v: &ValueRef, catalog: &ModelCatalog) -> Result<Value, String> {
    match &v.field {
        FieldRef::Measure { name } => {
            catalog.measure(name).ok_or_else(|| format!("unknown measure {}", dax_measure_ref(name)))?;
            Ok(Value { core: dax_measure_ref(name), is_measure: true })
        }
        FieldRef::Column { table, column } => {
            let col = resolve_column(catalog, table, column)?;
            let r = dax_column_ref(table, column);
            let f = match v.agg {
                Agg::Sum => "SUM",
                Agg::Count => "COUNT",
                Agg::CountDistinct => "DISTINCTCOUNT",
                Agg::Avg => "AVERAGE",
                Agg::Min => "MIN",
                Agg::Max => "MAX",
                Agg::None if col.data_type.is_numeric() => "SUM",
                Agg::None => "COUNT",
            };
            Ok(Value { core: format!("{f}({r})"), is_measure: false })
        }
    }
}

fn resolve_column<'a>(catalog: &'a ModelCatalog, table: &str, column: &str) -> Result<&'a Column, String> {
    let t = catalog.table(table).ok_or_else(|| format!("unknown table {}", dax_table_ref(table)))?;
    t.columns.iter().find(|c| c.name.eq_ignore_ascii_case(column)).ok_or_else(|| format!("unknown column {}", dax_column_ref(table, column)))
}

fn filter_expr(f: &FilterSpec, col: &Column) -> Result<String, String> {
    let c = dax_column_ref(&f.table, &f.column);
    let lit = |raw: &str| literal(col, &c, raw);
    let list = |values: &[String]| values.iter().map(|v| lit(v)).collect::<Result<Vec<_>, _>>().map(|v| v.join(", "));
    Ok(match &f.op {
        FilterOp::In { values } | FilterOp::NotIn { values } if values.is_empty() => format!("ALL({c})"),
        FilterOp::In { values } => format!("TREATAS({{{}}}, {c})", list(values)?),
        FilterOp::NotIn { values } => format!("FILTER(ALL({c}), NOT {c} IN {{{}}})", list(values)?),
        FilterOp::Between { lo, hi } => {
            let mut conds = Vec::new();
            if let Some(lo) = lo {
                conds.push(format!("{c} >= {}", lit(lo)?));
            }
            if let Some(hi) = hi {
                conds.push(format!("{c} <= {}", lit(hi)?));
            }
            if conds.is_empty() {
                format!("ALL({c})")
            } else {
                format!("FILTER(ALL({c}), {})", conds.join(" && "))
            }
        }
        FilterOp::Contains { text } => format!("FILTER(ALL({c}), CONTAINSSTRING({c}, {}))", dax_string(text)),
        FilterOp::StartsWith { text } => {
            let s = dax_string(text);
            format!("FILTER(ALL({c}), LEFT({c}, LEN({s})) = {s})")
        }
        FilterOp::IsBlank => format!("FILTER(ALL({c}), ISBLANK({c}))"),
        FilterOp::NotBlank => format!("FILTER(ALL({c}), NOT ISBLANK({c}))"),
        FilterOp::RelativeDate { unit, last } => {
            let start = match unit {
                DateUnit::Days => format!("TODAY() - {last}"),
                DateUnit::Months => format!("EDATE(TODAY(), -{last})"),
                DateUnit::Years => format!("EDATE(TODAY(), -{})", 12 * u64::from(*last)),
            };
            format!("FILTER(ALL({c}), {c} >= {start} && {c} <= TODAY())")
        }
    })
}

/// A filter value typed by the column: text quoted, numbers raw (validated), booleans as
/// `TRUE()` / `FALSE()`, dates as `DATE(y, m, d)` from an ISO `YYYY-MM-DD` prefix. Variant and
/// unknown columns take a number when the value is one, else text.
fn literal(col: &Column, loc: &str, raw: &str) -> Result<String, String> {
    let s = raw.trim();
    match col.data_type {
        TabularDataType::String => Ok(dax_string(raw)),
        TabularDataType::Int64 | TabularDataType::Double | TabularDataType::Decimal => number(s).ok_or_else(|| format!("filter on {loc}: '{raw}' is not a number")),
        TabularDataType::Boolean => match s.to_ascii_lowercase().as_str() {
            "true" | "1" => Ok("TRUE()".into()),
            "false" | "0" => Ok("FALSE()".into()),
            _ => Err(format!("filter on {loc}: '{raw}' is not true or false")),
        },
        TabularDataType::DateTime => date(s).ok_or_else(|| format!("filter on {loc}: '{raw}' is not a date (YYYY-MM-DD)")),
        TabularDataType::Binary => Err(format!("filter on {loc}: binary columns cannot be filtered")),
        TabularDataType::Variant | TabularDataType::Unknown => Ok(number(s).unwrap_or_else(|| dax_string(raw))),
    }
}

fn number(s: &str) -> Option<String> {
    let finite = s.parse::<f64>().map(f64::is_finite).unwrap_or(false);
    let plain = s.chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E'));
    (finite && plain).then(|| s.to_string())
}

fn date(s: &str) -> Option<String> {
    let head = s.split(['T', ' ']).next()?;
    let mut it = head.split('-');
    let y: u32 = it.next()?.parse().ok()?;
    let m: u32 = it.next()?.parse().ok()?;
    let d: u32 = it.next()?.parse().ok()?;
    if it.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some(format!("DATE({y}, {m}, {d})"))
}

/// A DAX string literal: `"` doubled.
fn dax_string(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// `(expression, descending)` for the spec's sort.
fn sort_target(s: &SortSpec, spec: &ExploreSpec, value_columns: &[String]) -> Result<(String, bool), String> {
    let expr = match &s.by {
        SortBy::Field { field } => {
            if !spec.group_fields().any(|g| g == field) {
                return Err(format!("sort field {} is not in Rows or Columns", field.qualified()));
            }
            match field {
                FieldRef::Column { table, column } => dax_column_ref(table, column),
                FieldRef::Measure { name } => dax_measure_ref(name),
            }
        }
        SortBy::Value { index } => value_columns.get(*index).cloned().ok_or_else(|| format!("sort value #{} does not exist", index + 1))?,
    };
    Ok((expr, s.desc))
}

/// The `ORDER BY` terms (see [`to_dax`] for the subtotal convention).
fn order_by(spec: &ExploreSpec, groups: &[String], sort: &Option<(String, bool)>) -> Vec<String> {
    let dir = |desc: bool| if desc { "DESC" } else { "ASC" };
    if !spec.subtotals {
        return sort.iter().map(|(e, d)| format!("{e} {}", dir(*d))).collect();
    }
    let mut terms = Vec::new();
    for (i, g) in groups.iter().enumerate() {
        terms.push(format!("[IsSubtotal {}] ASC", i + 1));
        let desc = matches!(sort, Some((e, d)) if e == g && *d);
        terms.push(format!("{g} {}", dir(desc)));
    }
    if let Some((e, d)) = sort {
        if !groups.contains(e) {
            terms.push(format!("{e} {}", dir(*d)));
        }
    }
    terms
}

fn comma(i: usize, n: usize) -> &'static str {
    if i + 1 < n {
        ","
    } else {
        ""
    }
}

fn push_indented(out: &mut String, lines: &[String], level: usize) {
    for (i, l) in lines.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        for _ in 0..level {
            out.push_str(INDENT);
        }
        out.push_str(l);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cobalt_core::tabular::{Measure, Table};

    fn col(name: &str, t: TabularDataType) -> Column {
        Column { name: name.into(), data_type: t, ..Default::default() }
    }

    fn catalog() -> ModelCatalog {
        use TabularDataType::*;
        ModelCatalog {
            name: "Sales".into(),
            tables: vec![
                Table { name: "Product".into(), columns: vec![col("Category", String), col("Color", String), col("ListPrice", Decimal)], ..Default::default() },
                Table { name: "Date".into(), columns: vec![col("Year", Int64), col("Date", DateTime)], ..Default::default() },
                Table {
                    name: "Sales".into(),
                    columns: vec![col("Quantity", Int64), col("Amount", Double), col("Discontinued", Boolean), col("OrderNumber", String)],
                    measures: vec![Measure { name: "Sales Amount".into(), ..Default::default() }, Measure { name: "Total Quantity".into(), ..Default::default() }],
                    ..Default::default()
                },
            ],
            ..Default::default()
        }
    }

    fn c(table: &str, column: &str) -> FieldRef {
        FieldRef::Column { table: table.into(), column: column.into() }
    }

    fn filter(table: &str, column: &str, op: FilterOp) -> FilterSpec {
        FilterSpec { table: table.into(), column: column.into(), op }
    }

    fn base() -> ExploreSpec {
        let mut s = ExploreSpec::default();
        s.rows.push(c("Product", "Category"));
        s.columns.push(c("Date", "Year"));
        s.values.push(ValueRef::measure("Sales Amount"));
        s.values.push(ValueRef::column("Sales", "Quantity", Agg::Sum));
        s
    }

    #[test]
    fn rows_columns_values_exact() {
        let mut s = base();
        s.filters.push(filter("Product", "Category", FilterOp::In { values: vec!["Accessories".into(), "Clothing".into()] }));
        s.filters.push(filter("Date", "Year", FilterOp::Between { lo: Some("2024".into()), hi: None }));
        s.sort = Some(SortSpec { by: SortBy::Value { index: 0 }, desc: true });
        let q = to_dax(&s, &catalog()).unwrap();
        assert_eq!(
            q.text,
            "DEFINE\n\
             \x20   VAR __F1 = TREATAS({\"Accessories\", \"Clothing\"}, 'Product'[Category])\n\
             \x20   VAR __F2 = FILTER(ALL('Date'[Year]), 'Date'[Year] >= 2024)\n\
             EVALUATE\n\
             \x20   SUMMARIZECOLUMNS(\n\
             \x20       'Product'[Category],\n\
             \x20       'Date'[Year],\n\
             \x20       __F1,\n\
             \x20       __F2,\n\
             \x20       \"Sales Amount\", [Sales Amount],\n\
             \x20       \"Sum of Quantity\", CALCULATE(SUM('Sales'[Quantity]))\n\
             \x20   )\n\
             ORDER BY [Sales Amount] DESC"
        );
        assert_eq!(q.group_columns, vec!["Product[Category]", "Date[Year]"]);
        assert_eq!(q.value_columns, vec!["[Sales Amount]", "[Sum of Quantity]"]);
        assert!(q.subtotal_columns.is_empty());
    }

    #[test]
    fn column_aggregations() {
        let mut s = ExploreSpec::default();
        s.rows.push(c("Product", "Category"));
        for agg in Agg::ALL {
            s.values.push(ValueRef::column("Sales", "Amount", agg));
        }
        s.values.push(ValueRef::column("Sales", "OrderNumber", Agg::None));
        let q = to_dax(&s, &catalog()).unwrap();
        for f in [
            "CALCULATE(SUM('Sales'[Amount]))",
            "CALCULATE(COUNT('Sales'[Amount]))",
            "CALCULATE(AVERAGE('Sales'[Amount]))",
            "CALCULATE(MIN('Sales'[Amount]))",
            "CALCULATE(MAX('Sales'[Amount]))",
            "CALCULATE(DISTINCTCOUNT('Sales'[Amount]))",
        ] {
            assert!(q.text.contains(f), "{f} missing in\n{}", q.text);
        }
        // Agg::None: SUM on a numeric column, COUNT on text.
        assert_eq!(q.text.matches("CALCULATE(SUM('Sales'[Amount]))").count(), 2);
        assert!(q.text.contains("\"Sum of OrderNumber\", CALCULATE(COUNT('Sales'[OrderNumber]))"));
        assert!(!q.text.contains("ORDER BY"));
        assert!(!q.text.starts_with("DEFINE"));
    }

    #[test]
    fn measures_only_row_exact() {
        let mut s = ExploreSpec::default();
        s.values.push(ValueRef::measure("Sales Amount"));
        s.values.push(ValueRef::column("Sales", "Quantity", Agg::Avg));
        let q = to_dax(&s, &catalog()).unwrap();
        assert_eq!(q.text, "EVALUATE\n    ROW(\n        \"Sales Amount\", [Sales Amount],\n        \"Average Quantity\", CALCULATE(AVERAGE('Sales'[Quantity]))\n    )");
        assert!(q.group_columns.is_empty());
        assert_eq!(q.value_columns, vec!["[Sales Amount]", "[Average Quantity]"]);

        s.filters.push(filter("Date", "Year", FilterOp::In { values: vec!["2025".into()] }));
        s.sort = Some(SortSpec { by: SortBy::Value { index: 0 }, desc: true });
        s.top_n = Some(5);
        let q = to_dax(&s, &catalog()).unwrap();
        assert!(q.text.starts_with("DEFINE\n    VAR __F1 = TREATAS({2025}, 'Date'[Year])\nEVALUATE\n    ROW("));
        assert!(q.text.contains("\"Sales Amount\", CALCULATE([Sales Amount], __F1)"));
        assert!(q.text.contains("\"Average Quantity\", CALCULATE(AVERAGE('Sales'[Quantity]), __F1)"));
        assert!(!q.text.contains("ORDER BY") && !q.text.contains("TOPN"));
    }

    #[test]
    fn filters_of_every_op() {
        let mut s = ExploreSpec::default();
        s.rows.push(c("Product", "Category"));
        s.filters.push(filter("Product", "Category", FilterOp::In { values: vec!["Bikes".into(), "Say \"hi\"".into()] }));
        s.filters.push(filter("Date", "Year", FilterOp::NotIn { values: vec!["2023".into(), "2024".into()] }));
        s.filters.push(filter("Product", "ListPrice", FilterOp::Between { lo: Some("10.5".into()), hi: Some("99".into()) }));
        s.filters.push(filter("Date", "Date", FilterOp::Between { lo: None, hi: Some("2025-03-31T00:00:00".into()) }));
        s.filters.push(filter("Sales", "Discontinued", FilterOp::In { values: vec!["true".into(), "0".into()] }));
        s.filters.push(filter("Product", "Color", FilterOp::Contains { text: "Red".into() }));
        s.filters.push(filter("Product", "Color", FilterOp::StartsWith { text: "Bl".into() }));
        s.filters.push(filter("Product", "Color", FilterOp::IsBlank));
        s.filters.push(filter("Product", "Color", FilterOp::NotBlank));
        s.filters.push(filter("Date", "Date", FilterOp::RelativeDate { unit: DateUnit::Days, last: 7 }));
        s.filters.push(filter("Date", "Date", FilterOp::RelativeDate { unit: DateUnit::Months, last: 3 }));
        s.filters.push(filter("Date", "Date", FilterOp::RelativeDate { unit: DateUnit::Years, last: 2 }));
        s.filters.push(filter("Product", "Category", FilterOp::In { values: vec![] }));
        let q = to_dax(&s, &catalog()).unwrap();
        let expect = [
            "VAR __F1 = TREATAS({\"Bikes\", \"Say \"\"hi\"\"\"}, 'Product'[Category])",
            "VAR __F2 = FILTER(ALL('Date'[Year]), NOT 'Date'[Year] IN {2023, 2024})",
            "VAR __F3 = FILTER(ALL('Product'[ListPrice]), 'Product'[ListPrice] >= 10.5 && 'Product'[ListPrice] <= 99)",
            "VAR __F4 = FILTER(ALL('Date'[Date]), 'Date'[Date] <= DATE(2025, 3, 31))",
            "VAR __F5 = TREATAS({TRUE(), FALSE()}, 'Sales'[Discontinued])",
            "VAR __F6 = FILTER(ALL('Product'[Color]), CONTAINSSTRING('Product'[Color], \"Red\"))",
            "VAR __F7 = FILTER(ALL('Product'[Color]), LEFT('Product'[Color], LEN(\"Bl\")) = \"Bl\")",
            "VAR __F8 = FILTER(ALL('Product'[Color]), ISBLANK('Product'[Color]))",
            "VAR __F9 = FILTER(ALL('Product'[Color]), NOT ISBLANK('Product'[Color]))",
            "VAR __F10 = FILTER(ALL('Date'[Date]), 'Date'[Date] >= TODAY() - 7 && 'Date'[Date] <= TODAY())",
            "VAR __F11 = FILTER(ALL('Date'[Date]), 'Date'[Date] >= EDATE(TODAY(), -3) && 'Date'[Date] <= TODAY())",
            "VAR __F12 = FILTER(ALL('Date'[Date]), 'Date'[Date] >= EDATE(TODAY(), -24) && 'Date'[Date] <= TODAY())",
            "VAR __F13 = ALL('Product'[Category])",
        ];
        for e in expect {
            assert!(q.text.contains(e), "{e} missing in\n{}", q.text);
        }
        assert!(q.text.contains("        __F1,\n        __F2,\n") && q.text.contains("        __F13\n    )"));
    }

    #[test]
    fn subtotals_naming_and_order() {
        let mut s = base();
        s.subtotals = true;
        s.sort = Some(SortSpec { by: SortBy::Value { index: 1 }, desc: true });
        let q = to_dax(&s, &catalog()).unwrap();
        assert!(q.text.contains("ROLLUPADDISSUBTOTAL('Product'[Category], \"IsSubtotal 1\"),\n        ROLLUPADDISSUBTOTAL('Date'[Year], \"IsSubtotal 2\"),"));
        assert!(q.text.ends_with("ORDER BY [IsSubtotal 1] ASC, 'Product'[Category] ASC, [IsSubtotal 2] ASC, 'Date'[Year] ASC, [Sum of Quantity] DESC"));
        assert_eq!(q.subtotal_columns, vec!["[IsSubtotal 1]", "[IsSubtotal 2]"]);

        s.sort = Some(SortSpec { by: SortBy::Field { field: c("Date", "Year") }, desc: true });
        let q = to_dax(&s, &catalog()).unwrap();
        assert!(q.text.ends_with("ORDER BY [IsSubtotal 1] ASC, 'Product'[Category] ASC, [IsSubtotal 2] ASC, 'Date'[Year] DESC"));

        s.sort = None;
        let q = to_dax(&s, &catalog()).unwrap();
        assert!(q.text.ends_with("ORDER BY [IsSubtotal 1] ASC, 'Product'[Category] ASC, [IsSubtotal 2] ASC, 'Date'[Year] ASC"));
    }

    #[test]
    fn top_n_with_and_without_sort() {
        let mut s = base();
        s.top_n = Some(10);
        let q = to_dax(&s, &catalog()).unwrap();
        assert_eq!(
            q.text,
            "EVALUATE\n    TOPN(\n        10,\n        SUMMARIZECOLUMNS(\n            'Product'[Category],\n            'Date'[Year],\n            \"Sales Amount\", [Sales Amount],\n            \"Sum of Quantity\", CALCULATE(SUM('Sales'[Quantity]))\n        ),\n        [Sales Amount], 0\n    )"
        );

        s.sort = Some(SortSpec { by: SortBy::Field { field: c("Date", "Year") }, desc: false });
        let q = to_dax(&s, &catalog()).unwrap();
        assert!(q.text.contains("        'Date'[Year], 1\n    )\nORDER BY 'Date'[Year] ASC"));

        s.values.clear();
        s.sort = None;
        let q = to_dax(&s, &catalog()).unwrap();
        assert!(q.text.contains("            'Date'[Year]\n        ),\n        'Product'[Category], 1\n    )"));

        s.top_n = Some(0);
        assert!(to_dax(&s, &catalog()).unwrap_err().contains("Top N"));
    }

    #[test]
    fn sort_by_field_and_value() {
        let mut s = base();
        s.sort = Some(SortSpec { by: SortBy::Field { field: c("Product", "Category") }, desc: false });
        assert!(to_dax(&s, &catalog()).unwrap().text.ends_with("\nORDER BY 'Product'[Category] ASC"));
        s.sort = Some(SortSpec { by: SortBy::Value { index: 1 }, desc: true });
        assert!(to_dax(&s, &catalog()).unwrap().text.ends_with("\nORDER BY [Sum of Quantity] DESC"));
        s.sort = Some(SortSpec { by: SortBy::Value { index: 7 }, desc: true });
        assert_eq!(to_dax(&s, &catalog()).unwrap_err(), "sort value #8 does not exist");
        s.sort = Some(SortSpec { by: SortBy::Field { field: c("Product", "Color") }, desc: true });
        assert_eq!(to_dax(&s, &catalog()).unwrap_err(), "sort field Product[Color] is not in Rows or Columns");
    }

    #[test]
    fn distinct_combinations_without_values() {
        let mut s = ExploreSpec::default();
        s.rows.push(c("Product", "Category"));
        s.rows.push(c("Product", "Color"));
        let q = to_dax(&s, &catalog()).unwrap();
        assert_eq!(q.text, "EVALUATE\n    SUMMARIZECOLUMNS(\n        'Product'[Category],\n        'Product'[Color]\n    )");
        assert!(q.value_columns.is_empty());
    }

    #[test]
    fn errors() {
        let cat = catalog();
        let mut s = ExploreSpec::default();
        s.rows.push(c("Nope", "Category"));
        assert_eq!(to_dax(&s, &cat).unwrap_err(), "unknown table 'Nope'");
        s.rows = vec![c("Product", "Nope")];
        assert_eq!(to_dax(&s, &cat).unwrap_err(), "unknown column 'Product'[Nope]");
        s.rows = vec![FieldRef::Measure { name: "Sales Amount".into() }];
        assert_eq!(to_dax(&s, &cat).unwrap_err(), "Rows and Columns take columns, not measures: [Sales Amount]");
        s.rows = vec![c("Product", "Category")];
        s.values.push(ValueRef::measure("Nope"));
        assert_eq!(to_dax(&s, &cat).unwrap_err(), "unknown measure [Nope]");
        s.values = vec![ValueRef::column("Sales", "Nope", Agg::Sum)];
        assert_eq!(to_dax(&s, &cat).unwrap_err(), "unknown column 'Sales'[Nope]");
        s.values.clear();
        s.filters.push(filter("Date", "Year", FilterOp::In { values: vec!["abc".into()] }));
        assert_eq!(to_dax(&s, &cat).unwrap_err(), "filter on 'Date'[Year]: 'abc' is not a number");
        s.filters = vec![filter("Date", "Date", FilterOp::Between { lo: Some("yesterday".into()), hi: None })];
        assert_eq!(to_dax(&s, &cat).unwrap_err(), "filter on 'Date'[Date]: 'yesterday' is not a date (YYYY-MM-DD)");
        s.filters = vec![filter("Sales", "Discontinued", FilterOp::In { values: vec!["maybe".into()] })];
        assert_eq!(to_dax(&s, &cat).unwrap_err(), "filter on 'Sales'[Discontinued]: 'maybe' is not true or false");
        s.filters = vec![filter("Sales", "Nope", FilterOp::IsBlank)];
        assert_eq!(to_dax(&s, &cat).unwrap_err(), "unknown column 'Sales'[Nope]");
        assert_eq!(to_dax(&ExploreSpec::default(), &cat).unwrap_err(), "add a field to Rows, Columns or Values first");
    }

    #[test]
    fn quoting() {
        let mut cat = catalog();
        cat.tables.push(Table { name: "O'Brien".into(), columns: vec![col("a]b", TabularDataType::String)], ..Default::default() });
        let mut s = ExploreSpec::default();
        s.rows.push(c("O'Brien", "a]b"));
        s.values.push(ValueRef { field: FieldRef::Measure { name: "Sales Amount".into() }, agg: Agg::None, label: Some("Say \"x\"".into()) });
        let q = to_dax(&s, &cat).unwrap();
        assert!(q.text.contains("'O''Brien'[a]]b],\n        \"Say \"\"x\"\"\", [Sales Amount]"));
        assert_eq!(q.group_columns, vec!["O'Brien[a]b]"]);
        assert_eq!(q.value_columns, vec!["[Say \"x\"]"]);
    }

    #[test]
    fn describe_line() {
        let mut s = base();
        s.rows.push(c("Date", "Year"));
        s.columns.clear();
        s.values[1] = ValueRef::column("Sales", "Quantity", Agg::Avg);
        s.filters.push(filter("Product", "Category", FilterOp::IsBlank));
        s.filters.push(filter("Date", "Year", FilterOp::NotBlank));
        assert_eq!(describe(&s), "Rows: Category, Year · Values: Sales Amount, Average Quantity · 2 filters");
        s.columns.push(c("Product", "Color"));
        s.filters.pop();
        s.top_n = Some(10);
        assert_eq!(describe(&s), "Rows: Category, Year · Columns: Color · Values: Sales Amount, Average Quantity · 1 filter · Top 10");
        assert_eq!(describe(&ExploreSpec::default()), "Nothing selected");
    }
}
