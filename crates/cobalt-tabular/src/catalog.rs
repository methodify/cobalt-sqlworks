//! The model tree from DAX: `INFO.VIEW.TABLES / COLUMNS / MEASURES / RELATIONSHIPS` in one
//! request, parsed by column name so a service missing a column still yields a catalog;
//! hierarchies from `INFO.HIERARCHIES / LEVELS / TABLES / COLUMNS` joined on ids (best effort);
//! the function list from `INFO.FUNCTIONS`; and a one-value probe.

use crate::decode::{DaxResult, DaxTable};
use crate::rest::{DaxOptions, PowerBiClient};
use crate::TabularError;
use cobalt_core::tabular::{Column, Hierarchy, Level, Measure, ModelCatalog, Relationship, Table, TabularDataType, TabularTarget};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// One row of `INFO.FUNCTIONS()`, for the completer's function list.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaxFunction {
    pub name: String,
    pub description: Option<String>,
    /// `INTERFACE_NAME` (`DATETIME`, `FILTER`, `MATH`, `INFO`, …).
    pub category: Option<String>,
    /// `PARAMETER_LIST`, as the engine formats it.
    pub parameters: Option<String>,
}

/// The four `EVALUATE`s of a catalog load. `minimal` keeps only Name / Table / DataType /
/// IsHidden / Type / Expression (and the relationship ends), for a service missing the rest.
pub fn catalog_query(minimal: bool) -> String {
    if minimal {
        return [
            r#"EVALUATE SELECTCOLUMNS(INFO.VIEW.TABLES(), "Name", [Name], "IsHidden", [IsHidden], "Expression", [Expression])"#,
            r#"EVALUATE SELECTCOLUMNS(FILTER(INFO.VIEW.COLUMNS(), [DataCategory] <> "RowNumber"), "Table", [Table], "Name", [Name], "DataType", [DataType], "IsHidden", [IsHidden], "Type", [Type], "Expression", [Expression])"#,
            r#"EVALUATE SELECTCOLUMNS(INFO.VIEW.MEASURES(), "Table", [Table], "Name", [Name], "DataType", [DataType], "IsHidden", [IsHidden], "Expression", [Expression])"#,
            r#"EVALUATE SELECTCOLUMNS(INFO.VIEW.RELATIONSHIPS(), "FromTable", [FromTable], "FromColumn", [FromColumn], "ToTable", [ToTable], "ToColumn", [ToColumn], "IsActive", [IsActive])"#,
        ]
        .join("\n");
    }
    [
        r#"EVALUATE SELECTCOLUMNS(INFO.VIEW.TABLES(), "Name", [Name], "Description", [Description], "IsHidden", [IsHidden], "StorageMode", [StorageMode], "Expression", [Expression])"#,
        r#"EVALUATE SELECTCOLUMNS(FILTER(INFO.VIEW.COLUMNS(), [DataCategory] <> "RowNumber"), "Table", [Table], "Name", [Name], "DataType", [DataType], "IsHidden", [IsHidden], "IsKey", [IsKey], "Type", [Type], "FormatString", [FormatString], "SortByColumn", [SortByColumn], "DisplayFolder", [DisplayFolder], "Description", [Description], "Expression", [Expression], "SummarizeBy", [SummarizeBy])"#,
        r#"EVALUATE SELECTCOLUMNS(INFO.VIEW.MEASURES(), "Table", [Table], "Name", [Name], "DataType", [DataType], "IsHidden", [IsHidden], "FormatString", [FormatString], "DisplayFolder", [DisplayFolder], "Description", [Description], "Expression", [Expression])"#,
        r#"EVALUATE SELECTCOLUMNS(INFO.VIEW.RELATIONSHIPS(), "FromTable", [FromTable], "FromColumn", [FromColumn], "FromCardinality", [FromCardinality], "ToTable", [ToTable], "ToColumn", [ToColumn], "ToCardinality", [ToCardinality], "IsActive", [IsActive], "CrossFilteringBehavior", [CrossFilteringBehavior])"#,
    ]
    .join("\n")
}

/// Raw id-keyed metadata for hierarchies; joined client-side.
pub const HIERARCHY_QUERY: &str = "EVALUATE INFO.HIERARCHIES()\nEVALUATE INFO.LEVELS()\nEVALUATE INFO.TABLES()\nEVALUATE INFO.COLUMNS()";

/// `INFO.FUNCTIONS` with the four columns the completer uses.
pub const FUNCTIONS_QUERY: &str = r#"EVALUATE SELECTCOLUMNS(INFO.FUNCTIONS(), "Name", [FUNCTION_NAME], "Description", [DESCRIPTION], "Category", [INTERFACE_NAME], "Parameters", [PARAMETER_LIST])"#;

/// The cheapest query there is; proves the token, the tenant settings and Build permission.
pub const PROBE_QUERY: &str = "EVALUATE {1}";

async fn run(client: &PowerBiClient, target: &TabularTarget, dax: &str) -> Result<Vec<DaxResult>, TabularError> {
    let opts = DaxOptions { timeout_secs: Some(120), ..DaxOptions::default() };
    client.execute(target, dax, &opts).await?.collect().await
}

/// The model's tables, columns, measures, relationships and (best effort) hierarchies.
pub async fn load_catalog(client: &PowerBiClient, target: &TabularTarget) -> Result<ModelCatalog, TabularError> {
    let results = match run(client, target, &catalog_query(false)).await {
        Ok(r) if !r.iter().any(|x| x.error().is_some()) => r,
        _ => run(client, target, &catalog_query(true)).await?,
    };
    let mut catalog = catalog_from_results(&results, target.model())?;
    if let Ok(h) = run(client, target, HIERARCHY_QUERY).await {
        attach_hierarchies(&mut catalog, &h);
    }
    Ok(catalog)
}

/// The functions the engine knows (name, description, category, parameter list).
pub async fn load_functions(client: &PowerBiClient, target: &TabularTarget) -> Result<Vec<DaxFunction>, TabularError> {
    let results = match run(client, target, FUNCTIONS_QUERY).await {
        Ok(r) if r.first().and_then(DaxResult::table).is_some() => r,
        _ => run(client, target, "EVALUATE INFO.FUNCTIONS()").await?,
    };
    let first = results.first().ok_or_else(|| TabularError::Decode("INFO.FUNCTIONS returned nothing".into()))?;
    let table = first.table().ok_or_else(|| first.error().unwrap_or_else(|| TabularError::Decode("no table".into())))?;
    Ok(functions_from_table(table))
}

/// `EVALUATE {1}`: Ok when the model answers, else the refusal.
pub async fn probe(client: &PowerBiClient, target: &TabularTarget) -> Result<(), TabularError> {
    let results = run(client, target, PROBE_QUERY).await?;
    match results.first() {
        Some(DaxResult::Table(_)) => Ok(()),
        Some(r) => Err(r.error().unwrap_or_else(|| TabularError::Decode("no result".into()))),
        None => Err(TabularError::Decode("the probe returned no result set".into())),
    }
}

/// `INFO.FUNCTIONS` rows (renamed or raw column names) to `DaxFunction`s, sorted by name.
pub fn functions_from_table(t: &DaxTable) -> Vec<DaxFunction> {
    let pick = |row: usize, names: &[&str]| names.iter().find_map(|n| t.text(n, row));
    let mut out: Vec<DaxFunction> = (0..t.num_rows())
        .filter_map(|r| {
            let name = pick(r, &["Name", "FUNCTION_NAME"])?;
            Some(DaxFunction { name, description: pick(r, &["Description", "DESCRIPTION"]), category: pick(r, &["Category", "INTERFACE_NAME"]), parameters: pick(r, &["Parameters", "PARAMETER_LIST"]) })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out.dedup_by(|a, b| a.name == b.name);
    out
}

/// Build the catalog from the four result sets of [`catalog_query`], by position. A missing
/// or errored section (other than tables) leaves that part empty.
pub fn catalog_from_results(results: &[DaxResult], model_name: &str) -> Result<ModelCatalog, TabularError> {
    let section = |i: usize| results.get(i).and_then(DaxResult::table);
    let tables = match results.first() {
        Some(DaxResult::Table(t)) => t,
        Some(r) => return Err(r.error().unwrap_or_else(|| TabularError::Decode("no tables".into()))),
        None => return Err(TabularError::Decode("the catalog query returned no result sets".into())),
    };
    let mut catalog = ModelCatalog { name: model_name.to_string(), ..ModelCatalog::default() };
    catalog.tables = read_tables(tables);
    if let Some(t) = section(1) {
        for (table, column) in read_columns(t) {
            table_mut(&mut catalog, &table).columns.push(column);
        }
    }
    let mut measures = 0usize;
    let mut blank = 0usize;
    if let Some(t) = section(2) {
        for (table, measure) in read_measures(t) {
            measures += 1;
            blank += usize::from(measure.expression.is_none());
            table_mut(&mut catalog, &table).measures.push(measure);
        }
    }
    catalog.expressions_hidden = measures > 0 && blank == measures;
    if let Some(t) = section(3) {
        catalog.relationships = read_relationships(t);
    }
    Ok(catalog)
}

fn table_mut<'a>(catalog: &'a mut ModelCatalog, name: &str) -> &'a mut Table {
    let idx = match catalog.tables.iter().position(|t| t.name == name) {
        Some(i) => i,
        None => {
            catalog.tables.push(Table { name: name.to_string(), ..Table::default() });
            catalog.tables.len() - 1
        }
    };
    &mut catalog.tables[idx]
}

fn read_tables(t: &DaxTable) -> Vec<Table> {
    (0..t.num_rows())
        .filter_map(|r| {
            Some(Table { name: t.text("Name", r)?, hidden: t.flag("IsHidden", r), storage_mode: t.text("StorageMode", r), description: t.text("Description", r), expression: t.text("Expression", r), ..Table::default() })
        })
        .collect()
}

fn read_columns(t: &DaxTable) -> Vec<(String, Column)> {
    (0..t.num_rows())
        .filter_map(|r| {
            let table = t.text("Table", r)?;
            let kind = t.text("Type", r).unwrap_or_default();
            if kind.eq_ignore_ascii_case("RowNumber") {
                return None;
            }
            let column = Column {
                name: t.text("Name", r)?,
                data_type: t.text("DataType", r).map(|s| TabularDataType::parse(&s)).unwrap_or_default(),
                hidden: t.flag("IsHidden", r),
                key: t.flag("IsKey", r),
                calculated: kind.eq_ignore_ascii_case("Calculated"),
                format_string: t.text("FormatString", r),
                sort_by_column: t.text("SortByColumn", r),
                display_folder: t.text("DisplayFolder", r),
                description: t.text("Description", r),
                expression: t.text("Expression", r),
                summarize_by: t.text("SummarizeBy", r),
            };
            Some((table, column))
        })
        .collect()
}

fn read_measures(t: &DaxTable) -> Vec<(String, Measure)> {
    (0..t.num_rows())
        .filter_map(|r| {
            let table = t.text("Table", r)?;
            let measure = Measure {
                name: t.text("Name", r)?,
                data_type: t.text("DataType", r).map(|s| TabularDataType::parse(&s)).unwrap_or_default(),
                hidden: t.flag("IsHidden", r),
                format_string: t.text("FormatString", r),
                display_folder: t.text("DisplayFolder", r),
                description: t.text("Description", r),
                expression: t.text("Expression", r),
            };
            Some((table, measure))
        })
        .collect()
}

fn read_relationships(t: &DaxTable) -> Vec<Relationship> {
    (0..t.num_rows())
        .filter_map(|r| {
            Some(Relationship {
                from_table: t.text("FromTable", r)?,
                from_column: t.text("FromColumn", r)?,
                to_table: t.text("ToTable", r)?,
                to_column: t.text("ToColumn", r)?,
                from_cardinality: t.text("FromCardinality", r).unwrap_or_else(|| "Many".into()),
                to_cardinality: t.text("ToCardinality", r).unwrap_or_else(|| "One".into()),
                active: t.col("IsActive").map(|_| t.flag("IsActive", r)).unwrap_or(true),
                cross_filter: t.text("CrossFilteringBehavior", r).unwrap_or_else(|| "OneDirection".into()),
            })
        })
        .collect()
}

/// Join `INFO.HIERARCHIES / LEVELS / TABLES / COLUMNS` (by position, in that order) into
/// `Table.hierarchies`. Anything missing leaves the tables as they are.
pub fn attach_hierarchies(catalog: &mut ModelCatalog, results: &[DaxResult]) {
    let section = |i: usize| results.get(i).and_then(DaxResult::table);
    let (Some(hier), Some(levels), Some(tables), Some(columns)) = (section(0), section(1), section(2), section(3)) else { return };
    let table_names: HashMap<i64, String> = (0..tables.num_rows()).filter_map(|r| Some((tables.int("ID", r)?, tables.text("Name", r)?))).collect();
    let column_names: HashMap<i64, String> = (0..columns.num_rows()).filter_map(|r| Some((columns.int("ID", r)?, columns.text("ExplicitName", r).or_else(|| columns.text("InferredName", r)).or_else(|| columns.text("Name", r))?))).collect();
    let mut by_hierarchy: HashMap<i64, Vec<Level>> = HashMap::new();
    for r in 0..levels.num_rows() {
        let (Some(hid), Some(name)) = (levels.int("HierarchyID", r), levels.text("Name", r)) else { continue };
        let column = levels.int("ColumnID", r).and_then(|c| column_names.get(&c).cloned()).unwrap_or_default();
        by_hierarchy.entry(hid).or_default().push(Level { name, column, ordinal: levels.int("Ordinal", r).unwrap_or(0) });
    }
    for r in 0..hier.num_rows() {
        let (Some(id), Some(tid), Some(name)) = (hier.int("ID", r), hier.int("TableID", r), hier.text("Name", r)) else { continue };
        let Some(table) = table_names.get(&tid) else { continue };
        let mut levels = by_hierarchy.remove(&id).unwrap_or_default();
        levels.sort_by_key(|l| l.ordinal);
        let h = Hierarchy { name, hidden: hier.flag("IsHidden", r), display_folder: hier.text("DisplayFolder", r), levels };
        table_mut(catalog, table).hierarchies.push(h);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::{ArrayRef, BooleanArray, Int64Array, RecordBatch, StringArray};
    use arrow::datatypes::{DataType, Field, Schema};
    use std::sync::Arc;

    fn table(cols: &[(&str, DataType)], rows: Vec<Vec<Option<&str>>>) -> DaxResult {
        let schema = Arc::new(Schema::new(cols.iter().map(|(n, t)| Field::new(*n, t.clone(), true)).collect::<Vec<_>>()));
        let arrays: Vec<ArrayRef> = cols
            .iter()
            .enumerate()
            .map(|(i, (_, t))| -> ArrayRef {
                let col: Vec<Option<&str>> = rows.iter().map(|r| r[i]).collect();
                match t {
                    DataType::Boolean => Arc::new(BooleanArray::from(col.iter().map(|v| v.map(|s| s == "true")).collect::<Vec<_>>())),
                    DataType::Int64 => Arc::new(Int64Array::from(col.iter().map(|v| v.and_then(|s| s.parse().ok())).collect::<Vec<Option<i64>>>())),
                    _ => Arc::new(StringArray::from(col)),
                }
            })
            .collect();
        let batch = RecordBatch::try_new(schema, arrays).unwrap();
        DaxResult::Table(DaxTable::from_batches(vec![batch]).unwrap())
    }

    fn sample(hidden_expressions: bool) -> Vec<DaxResult> {
        let s = DataType::Utf8;
        let b = DataType::Boolean;
        let expr = |e: &'static str| if hidden_expressions { None } else { Some(e) };
        vec![
            table(
                &[("Name", s.clone()), ("Description", s.clone()), ("IsHidden", b.clone()), ("StorageMode", s.clone()), ("Expression", s.clone())],
                vec![vec![Some("Sales"), None, Some("false"), Some("Import"), None], vec![Some("Date"), Some("Calendar"), Some("false"), Some("Import"), expr("CALENDARAUTO()")], vec![Some("LocalDateTable_1"), None, Some("true"), Some("Import"), None]],
            ),
            table(
                &[("Table", s.clone()), ("Name", s.clone()), ("DataType", s.clone()), ("IsHidden", b.clone()), ("IsKey", b.clone()), ("Type", s.clone()), ("FormatString", s.clone()), ("SortByColumn", s.clone()), ("DisplayFolder", s.clone()), ("Description", s.clone()), ("Expression", s.clone()), ("SummarizeBy", s.clone())],
                vec![
                    vec![Some("Sales"), Some("Amount"), Some("Decimal"), Some("false"), Some("false"), Some("Data"), Some("#,0.00"), None, None, None, None, Some("Sum")],
                    vec![Some("Sales"), Some("DateKey"), Some("Int64"), Some("true"), Some("false"), Some("Data"), None, None, None, None, None, Some("None")],
                    vec![Some("Sales"), Some("Margin"), Some("Double"), Some("false"), Some("false"), Some("Calculated"), None, None, Some("Calc"), None, expr("[Amount] * 0.3"), Some("Sum")],
                    vec![Some("Date"), Some("Date"), Some("DateTime"), Some("false"), Some("true"), Some("Data"), None, None, None, None, None, Some("None")],
                    vec![Some("Date"), Some("Month"), Some("Text"), Some("false"), Some("false"), Some("Data"), None, Some("MonthNo"), None, None, None, Some("None")],
                    vec![Some("Date"), Some("RowNumber-2662979B"), Some("Int64"), Some("true"), Some("false"), Some("RowNumber"), None, None, None, None, None, None],
                ],
            ),
            table(
                &[("Table", s.clone()), ("Name", s.clone()), ("DataType", s.clone()), ("IsHidden", b.clone()), ("FormatString", s.clone()), ("DisplayFolder", s.clone()), ("Description", s.clone()), ("Expression", s.clone())],
                vec![vec![Some("Sales"), Some("Total Sales"), Some("Variant"), Some("false"), Some("$#,0"), None, Some("Sum of amount"), expr("SUM(Sales[Amount])")], vec![Some("Sales"), Some("Orders"), Some("Int64"), Some("true"), None, Some("Counts"), None, expr("COUNTROWS(Sales)")]],
            ),
            table(
                &[("FromTable", s.clone()), ("FromColumn", s.clone()), ("FromCardinality", s.clone()), ("ToTable", s.clone()), ("ToColumn", s.clone()), ("ToCardinality", s.clone()), ("IsActive", b.clone()), ("CrossFilteringBehavior", s.clone())],
                vec![vec![Some("Sales"), Some("DateKey"), Some("Many"), Some("Date"), Some("Date"), Some("One"), Some("true"), Some("OneDirection")]],
            ),
        ]
    }

    #[test]
    fn builds_the_catalog() {
        let c = catalog_from_results(&sample(false), "Sales model").unwrap();
        assert_eq!(c.name, "Sales model");
        assert_eq!(c.tables.len(), 3);
        let sales = c.table("sales").unwrap();
        assert_eq!(sales.columns.len(), 3);
        assert_eq!(sales.columns[0].data_type, TabularDataType::Decimal);
        assert_eq!(sales.columns[0].format_string.as_deref(), Some("#,0.00"));
        assert_eq!(sales.columns[0].summarize_by.as_deref(), Some("Sum"));
        assert!(sales.columns[1].hidden);
        assert!(sales.columns[2].calculated);
        assert_eq!(sales.columns[2].expression.as_deref(), Some("[Amount] * 0.3"));
        assert_eq!(sales.measures.len(), 2);
        assert_eq!(sales.measures[0].expression.as_deref(), Some("SUM(Sales[Amount])"));
        assert_eq!(sales.measures[0].format_string.as_deref(), Some("$#,0"));
        assert!(sales.measures[1].hidden);
        let date = c.table("Date").unwrap();
        assert_eq!(date.expression.as_deref(), Some("CALENDARAUTO()"));
        assert_eq!(date.columns.len(), 2, "RowNumber filtered out");
        assert!(date.columns[0].key);
        assert_eq!(date.columns[1].sort_by_column.as_deref(), Some("MonthNo"));
        assert!(c.tables[2].hidden);
        assert_eq!(c.relationships.len(), 1);
        assert_eq!(c.relationships[0].from_cardinality, "Many");
        assert!(c.relationships[0].active);
        assert!(!c.expressions_hidden);
        assert_eq!(c.measure("total sales").unwrap().0.name, "Sales");
    }

    #[test]
    fn hidden_expressions_are_detected() {
        let c = catalog_from_results(&sample(true), "m").unwrap();
        assert!(c.expressions_hidden);
        assert!(c.all_measures().all(|(_, m)| m.expression.is_none()));
    }

    #[test]
    fn tolerates_missing_sections_and_errors() {
        let mut r = sample(false);
        r[3] = DaxResult::Error { message: "no relationships here".into(), code: None };
        let c = catalog_from_results(&r, "m").unwrap();
        assert!(c.relationships.is_empty());
        r.truncate(2);
        let c = catalog_from_results(&r, "m").unwrap();
        assert_eq!(c.measure_count(), 0);
        assert_eq!(c.column_count(), 5);
        let r = vec![DaxResult::Error { message: "nope".into(), code: Some("0x1".into()) }];
        let e = catalog_from_results(&r, "m").unwrap_err();
        assert!(matches!(e, TabularError::Query { .. }));
        assert!(catalog_from_results(&[], "m").is_err());
    }

    #[test]
    fn hierarchies_join_on_ids() {
        let mut c = catalog_from_results(&sample(false), "m").unwrap();
        let i = DataType::Int64;
        let s = DataType::Utf8;
        let h = vec![
            table(&[("ID", i.clone()), ("TableID", i.clone()), ("Name", s.clone()), ("IsHidden", DataType::Boolean), ("DisplayFolder", s.clone())], vec![vec![Some("10"), Some("2"), Some("Calendar"), Some("false"), None]]),
            table(
                &[("ID", i.clone()), ("HierarchyID", i.clone()), ("Ordinal", i.clone()), ("Name", s.clone()), ("ColumnID", i.clone())],
                vec![vec![Some("100"), Some("10"), Some("1"), Some("Month"), Some("21")], vec![Some("101"), Some("10"), Some("0"), Some("Year"), Some("22")]],
            ),
            table(&[("ID", i.clone()), ("Name", s.clone())], vec![vec![Some("1"), Some("Sales")], vec![Some("2"), Some("Date")]]),
            table(&[("ID", i.clone()), ("TableID", i.clone()), ("ExplicitName", s.clone()), ("InferredName", s.clone())], vec![vec![Some("21"), Some("2"), Some("Month"), None], vec![Some("22"), Some("2"), None, Some("Year")]]),
        ];
        attach_hierarchies(&mut c, &h);
        let date = c.table("Date").unwrap();
        assert_eq!(date.hierarchies.len(), 1);
        let hh = &date.hierarchies[0];
        assert_eq!(hh.name, "Calendar");
        assert_eq!(hh.levels.iter().map(|l| (l.name.as_str(), l.column.as_str(), l.ordinal)).collect::<Vec<_>>(), vec![("Year", "Year", 0), ("Month", "Month", 1)]);
        // a failed hierarchy request changes nothing
        let mut c2 = catalog_from_results(&sample(false), "m").unwrap();
        attach_hierarchies(&mut c2, &[DaxResult::Error { message: "x".into(), code: None }]);
        assert!(c2.tables.iter().all(|t| t.hierarchies.is_empty()));
    }

    #[test]
    fn functions_from_either_column_set() {
        let s = DataType::Utf8;
        let renamed = table(&[("Name", s.clone()), ("Description", s.clone()), ("Category", s.clone()), ("Parameters", s.clone())], vec![vec![Some("SUMX"), Some("Sums an expression"), Some("MATH"), Some("(Table, Expression)")], vec![Some("ABS"), None, Some("MATH"), Some("(Number)")]]);
        let f = functions_from_table(renamed.table().unwrap());
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].name, "ABS");
        assert_eq!(f[1], DaxFunction { name: "SUMX".into(), description: Some("Sums an expression".into()), category: Some("MATH".into()), parameters: Some("(Table, Expression)".into()) });
        let raw = table(&[("FUNCTION_NAME", s.clone()), ("DESCRIPTION", s.clone()), ("PARAMETER_LIST", s.clone()), ("INTERFACE_NAME", s.clone())], vec![vec![Some("NOW"), Some("Now"), Some("()"), Some("DATETIME")]]);
        let f = functions_from_table(raw.table().unwrap());
        assert_eq!(f[0].category.as_deref(), Some("DATETIME"));
        assert_eq!(f[0].parameters.as_deref(), Some("()"));
    }

    #[test]
    fn queries_have_four_evaluates() {
        assert_eq!(catalog_query(false).matches("EVALUATE").count(), 4);
        assert_eq!(catalog_query(true).matches("EVALUATE").count(), 4);
        assert!(catalog_query(true).contains("\"Type\", [Type]"));
        assert_eq!(HIERARCHY_QUERY.matches("EVALUATE").count(), 4);
    }
}
