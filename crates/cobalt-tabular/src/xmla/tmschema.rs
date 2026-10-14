//! The model tree from the TMSCHEMA DMVs (MS-SSAS-T), for servers without `INFO.VIEW`:
//! `$SYSTEM.TMSCHEMA_TABLES / _COLUMNS / _MEASURES / _RELATIONSHIPS / _HIERARCHIES / _LEVELS`
//! joined on their ids. Enumerations are the TOM integers; unknown values keep their number.

use crate::decode::DaxTable;
use cobalt_core::tabular::{Column, Hierarchy, Level, Measure, ModelCatalog, Relationship, Table, TabularDataType};
use std::collections::HashMap;

/// The six DMV queries, in the order [`catalog_from_dmvs`] takes them.
pub const DMV_QUERIES: [&str; 6] = [
    "SELECT * FROM $SYSTEM.TMSCHEMA_TABLES",
    "SELECT * FROM $SYSTEM.TMSCHEMA_COLUMNS",
    "SELECT * FROM $SYSTEM.TMSCHEMA_MEASURES",
    "SELECT * FROM $SYSTEM.TMSCHEMA_RELATIONSHIPS",
    "SELECT * FROM $SYSTEM.TMSCHEMA_HIERARCHIES",
    "SELECT * FROM $SYSTEM.TMSCHEMA_LEVELS",
];

/// TOM `DataType`: 2 string, 6 int64, 8 double, 9 dateTime, 10 decimal, 11 boolean, 17 binary,
/// 1 automatic, 19 unknown / variant. A name is accepted too.
pub fn data_type(v: &str) -> TabularDataType {
    match v.trim() {
        "2" => TabularDataType::String,
        "6" => TabularDataType::Int64,
        "8" => TabularDataType::Double,
        "9" => TabularDataType::DateTime,
        "10" => TabularDataType::Decimal,
        "11" => TabularDataType::Boolean,
        "17" => TabularDataType::Binary,
        "19" => TabularDataType::Variant,
        other => TabularDataType::parse(other),
    }
}

/// TOM `ColumnType`: 1 data, 2 calculated, 3 rowNumber, 4 calculatedTableColumn.
fn column_kind(v: Option<String>) -> &'static str {
    match v.as_deref().map(str::trim) {
        Some("2") | Some("Calculated") => "Calculated",
        Some("3") | Some("RowNumber") => "RowNumber",
        Some("4") => "CalculatedTableColumn",
        _ => "Data",
    }
}

/// TOM `AggregateFunction` (SummarizeBy): 1 default, 2 none, 3 sum, 4 min, 5 max, 6 count,
/// 7 average, 8 distinctCount.
fn summarize_by(v: Option<String>) -> Option<String> {
    let v = v?;
    Some(
        match v.trim() {
            "1" => "Default",
            "2" => "None",
            "3" => "Sum",
            "4" => "Min",
            "5" => "Max",
            "6" => "Count",
            "7" => "Average",
            "8" => "DistinctCount",
            other => return Some(other.to_string()),
        }
        .to_string(),
    )
}

/// TOM `RelationshipEndCardinality`: 1 none, 2 one, 3 many.
fn cardinality(v: Option<String>, default: &str) -> String {
    match v.as_deref().map(str::trim) {
        Some("1") => "None".into(),
        Some("2") => "One".into(),
        Some("3") => "Many".into(),
        Some(other) if !other.is_empty() => other.to_string(),
        _ => default.to_string(),
    }
}

/// TOM `CrossFilteringBehavior`: 1 oneDirection, 2 bothDirections, 3 automatic.
fn cross_filter(v: Option<String>) -> String {
    match v.as_deref().map(str::trim) {
        Some("2") => "BothDirections".into(),
        Some("3") => "Automatic".into(),
        Some(other) if !other.is_empty() && other != "1" => other.to_string(),
        _ => "OneDirection".into(),
    }
}

/// TOM `ModeType` on a partition is not on the table DMV; the table's `DefaultPartitionMode`
/// is not either, so storage mode stays unknown here.
pub fn catalog_from_dmvs(tables: &DaxTable, columns: Option<&DaxTable>, measures: Option<&DaxTable>, relationships: Option<&DaxTable>, hierarchies: Option<&DaxTable>, levels: Option<&DaxTable>, model_name: &str) -> ModelCatalog {
    let mut catalog = ModelCatalog { name: model_name.to_string(), ..ModelCatalog::default() };
    let mut table_names: HashMap<i64, String> = HashMap::new();
    for r in 0..tables.num_rows() {
        let Some(name) = tables.text("Name", r) else { continue };
        if let Some(id) = tables.int("ID", r) {
            table_names.insert(id, name.clone());
        }
        catalog.tables.push(Table { name, hidden: tables.flag("IsHidden", r), description: tables.text("Description", r), ..Table::default() });
    }
    let table_index = |catalog: &mut ModelCatalog, id: i64| -> Option<usize> {
        let name = table_names.get(&id)?;
        catalog.tables.iter().position(|t| &t.name == name)
    };
    let mut column_names: HashMap<i64, String> = HashMap::new();
    if let Some(c) = columns {
        let mut sort_ids: Vec<(usize, usize, i64)> = Vec::new();
        for r in 0..c.num_rows() {
            let name = c.text("ExplicitName", r).or_else(|| c.text("InferredName", r)).or_else(|| c.text("Name", r));
            let (Some(name), Some(tid)) = (name, c.int("TableID", r)) else { continue };
            if let Some(id) = c.int("ID", r) {
                column_names.insert(id, name.clone());
            }
            let kind = column_kind(c.text("Type", r));
            if kind == "RowNumber" {
                continue;
            }
            let Some(ti) = table_index(&mut catalog, tid) else { continue };
            let column = Column {
                name,
                data_type: c.text("ExplicitDataType", r).or_else(|| c.text("InferredDataType", r)).or_else(|| c.text("DataType", r)).map(|s| data_type(&s)).unwrap_or_default(),
                hidden: c.flag("IsHidden", r),
                key: c.flag("IsKey", r),
                calculated: kind == "Calculated",
                format_string: c.text("FormatString", r),
                sort_by_column: None,
                display_folder: c.text("DisplayFolder", r),
                description: c.text("Description", r),
                expression: c.text("Expression", r),
                summarize_by: summarize_by(c.text("SummarizeBy", r)),
            };
            if let Some(sid) = c.int("SortByColumnID", r) {
                sort_ids.push((ti, catalog.tables[ti].columns.len(), sid));
            }
            catalog.tables[ti].columns.push(column);
        }
        for (ti, ci, sid) in sort_ids {
            if let Some(n) = column_names.get(&sid) {
                catalog.tables[ti].columns[ci].sort_by_column = Some(n.clone());
            }
        }
    }
    let mut count = 0usize;
    let mut blank = 0usize;
    if let Some(m) = measures {
        for r in 0..m.num_rows() {
            let (Some(name), Some(tid)) = (m.text("Name", r), m.int("TableID", r)) else { continue };
            let Some(ti) = table_index(&mut catalog, tid) else { continue };
            let measure = Measure {
                name,
                data_type: m.text("DataType", r).map(|s| data_type(&s)).unwrap_or_default(),
                hidden: m.flag("IsHidden", r),
                format_string: m.text("FormatString", r),
                display_folder: m.text("DisplayFolder", r),
                description: m.text("Description", r),
                expression: m.text("Expression", r),
            };
            count += 1;
            blank += usize::from(measure.expression.is_none());
            catalog.tables[ti].measures.push(measure);
        }
    }
    catalog.expressions_hidden = count > 0 && blank == count;
    if let Some(rel) = relationships {
        for r in 0..rel.num_rows() {
            let ends = (rel.int("FromTableID", r).and_then(|id| table_names.get(&id)), rel.int("FromColumnID", r).and_then(|id| column_names.get(&id)), rel.int("ToTableID", r).and_then(|id| table_names.get(&id)), rel.int("ToColumnID", r).and_then(|id| column_names.get(&id)));
            let (Some(ft), Some(fc), Some(tt), Some(tc)) = ends else { continue };
            catalog.relationships.push(Relationship {
                from_table: ft.clone(),
                from_column: fc.clone(),
                to_table: tt.clone(),
                to_column: tc.clone(),
                from_cardinality: cardinality(rel.text("FromCardinality", r), "Many"),
                to_cardinality: cardinality(rel.text("ToCardinality", r), "One"),
                active: rel.col("IsActive").map(|_| rel.flag("IsActive", r)).unwrap_or(true),
                cross_filter: cross_filter(rel.text("CrossFilteringBehavior", r)),
            });
        }
    }
    if let (Some(h), Some(l)) = (hierarchies, levels) {
        let mut by_hierarchy: HashMap<i64, Vec<Level>> = HashMap::new();
        for r in 0..l.num_rows() {
            let (Some(hid), Some(name)) = (l.int("HierarchyID", r), l.text("Name", r)) else { continue };
            let column = l.int("ColumnID", r).and_then(|c| column_names.get(&c).cloned()).unwrap_or_default();
            by_hierarchy.entry(hid).or_default().push(Level { name, column, ordinal: l.int("Ordinal", r).unwrap_or(0) });
        }
        for r in 0..h.num_rows() {
            let (Some(id), Some(tid), Some(name)) = (h.int("ID", r), h.int("TableID", r), h.text("Name", r)) else { continue };
            let Some(ti) = table_index(&mut catalog, tid) else { continue };
            let mut levels = by_hierarchy.remove(&id).unwrap_or_default();
            levels.sort_by_key(|x| x.ordinal);
            catalog.tables[ti].hierarchies.push(Hierarchy { name, hidden: h.flag("IsHidden", r), display_folder: h.text("DisplayFolder", r), levels });
        }
    }
    catalog
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::{ArrayRef, BooleanArray, Int64Array, RecordBatch, StringArray};
    use arrow::datatypes::{DataType, Field, Schema};
    use std::sync::Arc;

    fn table(cols: &[(&str, DataType)], rows: Vec<Vec<Option<&str>>>) -> DaxTable {
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
        DaxTable::from_batches(vec![RecordBatch::try_new(schema, arrays).unwrap()]).unwrap()
    }

    #[test]
    fn joins_the_dmvs() {
        let i = DataType::Int64;
        let s = DataType::Utf8;
        let b = DataType::Boolean;
        let tables = table(&[("ID", i.clone()), ("Name", s.clone()), ("IsHidden", b.clone()), ("Description", s.clone())], vec![vec![Some("1"), Some("Sales"), Some("false"), None], vec![Some("2"), Some("Date"), Some("false"), Some("Calendar")], vec![Some("3"), Some("LocalDateTable_1"), Some("true"), None]]);
        let columns = table(
            &[("ID", i.clone()), ("TableID", i.clone()), ("ExplicitName", s.clone()), ("InferredName", s.clone()), ("ExplicitDataType", i.clone()), ("IsHidden", b.clone()), ("IsKey", b.clone()), ("Type", i.clone()), ("FormatString", s.clone()), ("SortByColumnID", i.clone()), ("DisplayFolder", s.clone()), ("Expression", s.clone()), ("SummarizeBy", i.clone())],
            vec![
                vec![Some("10"), Some("1"), Some("Amount"), None, Some("10"), Some("false"), Some("false"), Some("1"), Some("#,0.00"), None, None, None, Some("3")],
                vec![Some("11"), Some("1"), Some("DateKey"), None, Some("6"), Some("true"), Some("false"), Some("1"), None, None, None, None, Some("2")],
                vec![Some("12"), Some("1"), None, Some("Margin"), Some("8"), Some("false"), Some("false"), Some("2"), None, None, Some("Calc"), Some("[Amount] * 0.3"), Some("1")],
                vec![Some("13"), Some("1"), Some("RowNumber-123"), None, Some("6"), Some("true"), Some("false"), Some("3"), None, None, None, None, None],
                vec![Some("20"), Some("2"), Some("Date"), None, Some("9"), Some("false"), Some("true"), Some("1"), None, None, None, None, Some("2")],
                vec![Some("21"), Some("2"), Some("Month"), None, Some("2"), Some("false"), Some("false"), Some("1"), None, Some("22"), None, None, Some("2")],
                vec![Some("22"), Some("2"), Some("MonthNo"), None, Some("6"), Some("true"), Some("false"), Some("1"), None, None, None, None, Some("2")],
            ],
        );
        let measures = table(
            &[("ID", i.clone()), ("TableID", i.clone()), ("Name", s.clone()), ("Expression", s.clone()), ("FormatString", s.clone()), ("IsHidden", b.clone()), ("DataType", i.clone()), ("DisplayFolder", s.clone()), ("Description", s.clone())],
            vec![vec![Some("100"), Some("1"), Some("Total Sales"), Some("SUM(Sales[Amount])"), Some("$#,0"), Some("false"), Some("19"), None, Some("Sum of amount")], vec![Some("101"), Some("1"), Some("Orders"), Some("COUNTROWS(Sales)"), None, Some("true"), Some("6"), Some("Counts"), None]],
        );
        let rels = table(
            &[("FromTableID", i.clone()), ("FromColumnID", i.clone()), ("ToTableID", i.clone()), ("ToColumnID", i.clone()), ("FromCardinality", i.clone()), ("ToCardinality", i.clone()), ("IsActive", b.clone()), ("CrossFilteringBehavior", i.clone())],
            vec![vec![Some("1"), Some("11"), Some("2"), Some("20"), Some("3"), Some("2"), Some("true"), Some("1")], vec![Some("1"), Some("11"), Some("3"), Some("999"), Some("3"), Some("2"), Some("false"), Some("2")]],
        );
        let hier = table(&[("ID", i.clone()), ("TableID", i.clone()), ("Name", s.clone()), ("IsHidden", b.clone()), ("DisplayFolder", s.clone())], vec![vec![Some("50"), Some("2"), Some("Calendar"), Some("false"), None]]);
        let levels = table(&[("ID", i.clone()), ("HierarchyID", i.clone()), ("Ordinal", i.clone()), ("Name", s.clone()), ("ColumnID", i.clone())], vec![vec![Some("500"), Some("50"), Some("1"), Some("Month"), Some("21")], vec![Some("501"), Some("50"), Some("0"), Some("Date"), Some("20")]]);
        let c = catalog_from_dmvs(&tables, Some(&columns), Some(&measures), Some(&rels), Some(&hier), Some(&levels), "Sales model");
        assert_eq!(c.name, "Sales model");
        assert_eq!(c.tables.len(), 3);
        let sales = c.table("Sales").unwrap();
        assert_eq!(sales.columns.len(), 3, "RowNumber skipped");
        assert_eq!(sales.columns[0].data_type, TabularDataType::Decimal);
        assert_eq!(sales.columns[0].summarize_by.as_deref(), Some("Sum"));
        assert_eq!(sales.columns[0].format_string.as_deref(), Some("#,0.00"));
        assert!(sales.columns[1].hidden);
        assert_eq!(sales.columns[1].data_type, TabularDataType::Int64);
        assert_eq!(sales.columns[2].name, "Margin");
        assert!(sales.columns[2].calculated);
        assert_eq!(sales.columns[2].data_type, TabularDataType::Double);
        assert_eq!(sales.measures.len(), 2);
        assert_eq!(sales.measures[0].data_type, TabularDataType::Variant);
        assert_eq!(sales.measures[0].expression.as_deref(), Some("SUM(Sales[Amount])"));
        assert!(sales.measures[1].hidden);
        let date = c.table("Date").unwrap();
        assert!(date.columns[0].key);
        assert_eq!(date.columns[0].data_type, TabularDataType::DateTime);
        assert_eq!(date.columns[1].sort_by_column.as_deref(), Some("MonthNo"));
        assert_eq!(date.hierarchies.len(), 1);
        assert_eq!(date.hierarchies[0].levels.iter().map(|l| (l.name.as_str(), l.column.as_str())).collect::<Vec<_>>(), vec![("Date", "Date"), ("Month", "Month")]);
        assert_eq!(c.relationships.len(), 1, "a relationship to an unknown column is dropped");
        let r = &c.relationships[0];
        assert_eq!((r.from_table.as_str(), r.from_column.as_str(), r.to_table.as_str(), r.to_column.as_str()), ("Sales", "DateKey", "Date", "Date"));
        assert_eq!((r.from_cardinality.as_str(), r.to_cardinality.as_str(), r.cross_filter.as_str()), ("Many", "One", "OneDirection"));
        assert!(r.active);
        assert!(!c.expressions_hidden);
        assert!(c.tables[2].hidden);
        // tables only
        let c = catalog_from_dmvs(&tables, None, None, None, None, None, "m");
        assert_eq!(c.column_count(), 0);
        assert!(!c.expressions_hidden);
    }

    #[test]
    fn hidden_expressions() {
        let i = DataType::Int64;
        let s = DataType::Utf8;
        let tables = table(&[("ID", i.clone()), ("Name", s.clone())], vec![vec![Some("1"), Some("T")]]);
        let measures = table(&[("TableID", i.clone()), ("Name", s.clone()), ("Expression", s.clone())], vec![vec![Some("1"), Some("M"), None], vec![Some("1"), Some("N"), Some("")]]);
        let c = catalog_from_dmvs(&tables, None, Some(&measures), None, None, None, "m");
        assert!(c.expressions_hidden);
        assert_eq!(data_type("11"), TabularDataType::Boolean);
        assert_eq!(data_type("String"), TabularDataType::String);
        assert_eq!(data_type("17"), TabularDataType::Binary);
    }
}
