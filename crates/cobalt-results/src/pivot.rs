//! A local pivot (matrix) over a result set's view: row keys down, column keys across, the
//! value columns in the cells. The flat input is the already-aggregated shape the Explore
//! builder's query produces (one row per combination of the row fields × column fields, plus
//! optional boolean "IsSubtotal n" flags from DAX `ROLLUPADDISSUBTOTAL`), so this never sums:
//! a duplicate flat row for the same (row key, column key) simply replaces the cell. One pass
//! over `view_batches`, like `chart`; pure data, the drawing and the wells live in the app.

use crate::chart::as_f64;
use crate::display::CellFormatter;
use crate::ResultSet;
use arrow::array::{Array, ArrayRef, BooleanArray, Float64Array, RecordBatch, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use std::collections::HashMap;
use std::sync::Arc;

/// Most row keys a pivot keeps before it reports `truncated`.
pub const MAX_ROW_KEYS: usize = 10_000;
/// Most column keys a pivot keeps before it reports `truncated`.
pub const MAX_COL_KEYS: usize = 500;
/// The label of a level that a subtotal flag marks as rolled up.
pub const TOTAL_LABEL: &str = "Total";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PivotSpec {
    /// Column indices of the row-key fields (outer to inner).
    pub rows: Vec<usize>,
    /// Column indices of the column-key fields (outer to inner).
    pub cols: Vec<usize>,
    /// Column indices of the value columns.
    pub values: Vec<usize>,
    /// Per row field, the index of its boolean subtotal flag column (true on the rows that are
    /// that field's subtotal, where the field itself is blank), when the query produced one.
    pub row_subtotal_flags: Vec<Option<usize>>,
    pub col_subtotal_flags: Vec<Option<usize>>,
}

impl PivotSpec {
    fn all_indices(&self) -> impl Iterator<Item = usize> + '_ {
        self.rows.iter().chain(&self.cols).chain(&self.values).copied().chain(self.row_subtotal_flags.iter().chain(&self.col_subtotal_flags).flatten().copied())
    }
}

/// One row (or column) header of the matrix: a label per level, outer to inner.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PivotKey {
    pub labels: Vec<Arc<str>>,
    /// How many trailing levels are "(total)" (0 = a detail key).
    pub subtotal_level: usize,
}

impl PivotKey {
    pub fn is_subtotal(&self) -> bool {
        self.subtotal_level > 0
    }
    /// The labels joined by `sep` (the export column name, a tooltip).
    pub fn joined(&self, sep: &str) -> String {
        self.labels.iter().map(|l| &**l).collect::<Vec<_>>().join(sep)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PivotCell {
    /// The value as the grid would show it.
    pub text: Arc<str>,
    /// The value as a number when its column is numeric and the cell is not NULL.
    pub num: Option<f64>,
}

#[derive(Clone, Debug)]
pub struct PivotTable {
    pub spec: PivotSpec,
    /// In first-seen order of the view; subtotal keys stay where the query put them.
    pub row_keys: Vec<PivotKey>,
    /// In first-seen order of the view. Exactly one (with no labels) when the spec has no column fields.
    pub col_keys: Vec<PivotKey>,
    /// Names of the row-key source columns (the row header captions and export column names).
    pub row_key_names: Vec<String>,
    pub value_names: Vec<String>,
    /// Dense: `cells[row * (col_keys.len() * values.len()) + col * values.len() + v]`; `None` where
    /// no flat row had that combination.
    pub cells: Vec<Option<PivotCell>>,
    /// Flat rows read.
    pub rows_in: usize,
    pub generation: u64,
    /// Hit `MAX_ROW_KEYS` or `MAX_COL_KEYS`; the rows after that were ignored.
    pub truncated: bool,
    /// Why no matrix could be built (a spec index outside the result's columns), in words.
    pub error: Option<String>,
}

impl PivotTable {
    fn empty(spec: &PivotSpec, generation: u64, error: Option<String>) -> Self {
        Self { spec: spec.clone(), row_keys: Vec::new(), col_keys: Vec::new(), row_key_names: Vec::new(), value_names: Vec::new(), cells: Vec::new(), rows_in: 0, generation, truncated: false, error }
    }

    /// Matrix cells across: one per (column key × value).
    pub fn column_count(&self) -> usize {
        self.col_keys.len() * self.spec.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.row_keys.is_empty() || self.column_count() == 0
    }

    /// The cell at row key `r`, column key `c`, value `v`; `None` when out of range or never filled.
    pub fn cell(&self, r: usize, c: usize, v: usize) -> Option<&PivotCell> {
        let nvals = self.spec.values.len();
        if r >= self.row_keys.len() || c >= self.col_keys.len() || v >= nvals {
            return None;
        }
        self.cells.get(r * self.col_keys.len() * nvals + c * nvals + v).and_then(|c| c.as_ref())
    }

    /// The name of matrix column `(c, v)` as the export shows it: the column-key labels joined by
    /// " / ", then " · " and the value name; just the value name when the spec has no column fields.
    pub fn column_name(&self, c: usize, v: usize) -> String {
        let value = self.value_names.get(v).map(String::as_str).unwrap_or("");
        match self.col_keys.get(c) {
            Some(k) if !k.labels.is_empty() => format!("{} · {value}", k.joined(" / ")),
            _ => value.to_string(),
        }
    }

    /// The flat export shape for Copy / Export: the row-key columns as utf8 (named after their
    /// source columns), then one utf8 column per (column key × value) with the cell texts; a cell
    /// no flat row filled is NULL.
    pub fn to_record_batch(&self) -> RecordBatch {
        let nrows = self.row_keys.len();
        let mut fields: Vec<Field> = Vec::new();
        let mut arrays: Vec<ArrayRef> = Vec::new();
        for (level, name) in self.row_key_names.iter().enumerate() {
            fields.push(Field::new(name, DataType::Utf8, true));
            arrays.push(Arc::new(StringArray::from_iter((0..nrows).map(|r| self.row_keys[r].labels.get(level).map(|l| l.to_string())))));
        }
        for c in 0..self.col_keys.len() {
            for v in 0..self.spec.values.len() {
                fields.push(Field::new(self.column_name(c, v), DataType::Utf8, true));
                arrays.push(Arc::new(StringArray::from_iter((0..nrows).map(|r| self.cell(r, c, v).map(|cell| cell.text.to_string())))));
            }
        }
        let schema = Arc::new(Schema::new(fields));
        RecordBatch::try_new(schema.clone(), arrays).unwrap_or_else(|_| RecordBatch::new_empty(schema))
    }
}

/// Resolve a spec by column names (exact first, then case-insensitive). `None` when any name,
/// including a `Some` flag, is not a column of `rs`. Flag slices shorter than their field slices
/// are padded with `None`.
pub fn pivot_spec_by_name(rs: &ResultSet, rows: &[&str], cols: &[&str], values: &[&str], row_flags: &[Option<&str>], col_flags: &[Option<&str>]) -> Option<PivotSpec> {
    let resolve = |names: &[&str]| names.iter().map(|n| col_index(rs, n)).collect::<Option<Vec<usize>>>();
    let resolve_flags = |flags: &[Option<&str>], len: usize| {
        let mut out: Vec<Option<usize>> = Vec::with_capacity(len);
        for i in 0..len {
            out.push(match flags.get(i).copied().flatten() {
                Some(n) => Some(col_index(rs, n)?),
                None => None,
            });
        }
        Some(out)
    };
    let rows_ix = resolve(rows)?;
    let cols_ix = resolve(cols)?;
    let values_ix = resolve(values)?;
    let row_subtotal_flags = resolve_flags(row_flags, rows_ix.len())?;
    let col_subtotal_flags = resolve_flags(col_flags, cols_ix.len())?;
    Some(PivotSpec { rows: rows_ix, cols: cols_ix, values: values_ix, row_subtotal_flags, col_subtotal_flags })
}

fn col_index(rs: &ResultSet, name: &str) -> Option<usize> {
    rs.columns.iter().position(|c| c.name == name).or_else(|| rs.columns.iter().position(|c| c.name.eq_ignore_ascii_case(name)))
}

/// The subtotal flag column of a batch as booleans (a bit column as-is; an integer 0/1 column
/// from a SQL emitter is cast). `None` when the column cannot be read as a flag.
fn as_flags(col: &ArrayRef) -> Option<BooleanArray> {
    match col.data_type() {
        DataType::Boolean => col.as_any().downcast_ref::<BooleanArray>().cloned(),
        DataType::Int8 | DataType::Int16 | DataType::Int32 | DataType::Int64 | DataType::UInt8 | DataType::UInt16 | DataType::UInt32 | DataType::UInt64 | DataType::Float32 | DataType::Float64 => {
            arrow::compute::cast(col, &DataType::Boolean).ok().and_then(|a| a.as_any().downcast_ref::<BooleanArray>().cloned())
        }
        _ => None,
    }
}

/// One batch's worth of the columns that make a key: the formatted labels per level, the raw
/// array (for NULL checks) and the subtotal flags per level.
struct KeyColumns {
    labels: Vec<Arc<Vec<Arc<str>>>>,
    arrays: Vec<ArrayRef>,
    flags: Vec<Option<BooleanArray>>,
}

impl KeyColumns {
    fn new(batch: &RecordBatch, rs: &ResultSet, fields: &[usize], flag_cols: &[Option<usize>], fmt: &CellFormatter) -> Self {
        let labels = fields.iter().map(|&i| fmt.format_column(batch.column(i), &rs.columns[i])).collect();
        let arrays = fields.iter().map(|&i| batch.column(i).clone()).collect();
        let flags = (0..fields.len()).map(|level| flag_cols.get(level).copied().flatten().and_then(|f| as_flags(batch.column(f)))).collect();
        Self { labels, arrays, flags }
    }

    /// The key of row `r`: NULL → "", a flagged level → "Total"; `subtotal_level` counts the
    /// flagged levels from the inner end.
    fn key(&self, r: usize, blank: &Arc<str>, total: &Arc<str>) -> PivotKey {
        let n = self.labels.len();
        let mut labels: Vec<Arc<str>> = Vec::with_capacity(n);
        let mut flagged: Vec<bool> = Vec::with_capacity(n);
        for level in 0..n {
            let is_total = self.flags[level].as_ref().is_some_and(|f| !f.is_null(r) && f.value(r));
            flagged.push(is_total);
            labels.push(if is_total {
                total.clone()
            } else if self.arrays[level].is_null(r) {
                blank.clone()
            } else {
                self.labels[level].get(r).cloned().unwrap_or_else(|| blank.clone())
            });
        }
        let subtotal_level = flagged.iter().rev().take_while(|&&f| f).count();
        PivotKey { labels, subtotal_level }
    }
}

/// Build the matrix for `spec` over the current view (what the grid shows, sorted and filtered).
/// Never panics on a bad spec: the result carries `error` instead.
pub fn pivot(rs: &ResultSet, spec: &PivotSpec, fmt: &CellFormatter) -> PivotTable {
    let generation = rs.generation();
    let ncols = rs.columns.len();
    if let Some(bad) = spec.all_indices().find(|&i| i >= ncols) {
        return PivotTable::empty(spec, generation, Some(format!("column {bad} is outside the result's {ncols} columns")));
    }
    let nvals = spec.values.len();
    let blank: Arc<str> = Arc::from("");
    let total: Arc<str> = Arc::from(TOTAL_LABEL);
    let mut row_keys: Vec<PivotKey> = Vec::new();
    let mut col_keys: Vec<PivotKey> = Vec::new();
    let mut row_index: HashMap<PivotKey, usize> = HashMap::new();
    let mut col_index: HashMap<PivotKey, usize> = HashMap::new();
    // sparse while the key sets grow; laid out densely at the end
    let mut sparse: HashMap<(usize, usize), Vec<Option<PivotCell>>> = HashMap::new();
    let mut rows_in = 0usize;
    let mut truncated = false;
    'batches: for batch in rs.view_batches(65_536) {
        let Ok(batch) = batch else { continue };
        let rk = KeyColumns::new(&batch, rs, &spec.rows, &spec.row_subtotal_flags, fmt);
        let ck = KeyColumns::new(&batch, rs, &spec.cols, &spec.col_subtotal_flags, fmt);
        let texts: Vec<Arc<Vec<Arc<str>>>> = spec.values.iter().map(|&i| fmt.format_column(batch.column(i), &rs.columns[i])).collect();
        let nums: Vec<Option<Float64Array>> = spec.values.iter().map(|&i| as_f64(batch.column(i))).collect();
        for r in 0..batch.num_rows() {
            let rkey = rk.key(r, &blank, &total);
            let ri = match row_index.get(&rkey) {
                Some(&i) => i,
                None => {
                    if row_keys.len() >= MAX_ROW_KEYS {
                        truncated = true;
                        break 'batches;
                    }
                    let i = row_keys.len();
                    row_index.insert(rkey.clone(), i);
                    row_keys.push(rkey);
                    i
                }
            };
            let ckey = ck.key(r, &blank, &total);
            let ci = match col_index.get(&ckey) {
                Some(&i) => i,
                None => {
                    if col_keys.len() >= MAX_COL_KEYS {
                        truncated = true;
                        break 'batches;
                    }
                    let i = col_keys.len();
                    col_index.insert(ckey.clone(), i);
                    col_keys.push(ckey);
                    i
                }
            };
            rows_in += 1;
            let cells: Vec<Option<PivotCell>> = (0..nvals)
                .map(|v| {
                    let text = texts[v].get(r).cloned().unwrap_or_else(|| blank.clone());
                    let num = match &nums[v] {
                        Some(a) if !a.is_null(r) => Some(a.value(r)),
                        _ => None,
                    };
                    Some(PivotCell { text, num })
                })
                .collect();
            // the query already aggregated: a repeat of the same combination replaces the cell
            sparse.insert((ri, ci), cells);
        }
    }
    let stride = col_keys.len() * nvals;
    let mut cells: Vec<Option<PivotCell>> = vec![None; row_keys.len() * stride];
    for ((ri, ci), vals) in sparse {
        let base = ri * stride + ci * nvals;
        for (v, cell) in vals.into_iter().enumerate() {
            cells[base + v] = cell;
        }
    }
    PivotTable {
        spec: spec.clone(),
        row_keys,
        col_keys,
        row_key_names: spec.rows.iter().map(|&i| rs.columns[i].name.clone()).collect(),
        value_names: spec.values.iter().map(|&i| rs.columns[i].name.clone()).collect(),
        cells,
        rows_in,
        generation,
        truncated,
        error: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SortKey, ViewSpec};
    use arrow::array::{BooleanArray as Bools, Float64Array as F64, Int32Array, StringArray as Str};
    use cobalt_core::{ColumnInfo, SqlType};

    fn decimal(schema: &Schema, col: usize, v: Vec<Option<f64>>) -> ArrayRef {
        arrow::compute::cast(&(Arc::new(F64::from(v)) as ArrayRef), schema.field(col).data_type()).unwrap()
    }

    /// Category × Year with Sales (decimal) and Qty (int); (C, 2025) is missing; two batches.
    fn sample() -> Arc<ResultSet> {
        let cols = vec![
            ColumnInfo::new("Category", SqlType::NVarChar { len: Some(20) }, true, 0),
            ColumnInfo::new("Year", SqlType::Int, true, 1),
            ColumnInfo::new("Sales", SqlType::Decimal { precision: 18, scale: 2 }, true, 2),
            ColumnInfo::new("Qty", SqlType::Int, true, 3),
        ];
        let rs = ResultSet::from_batches(0, cols.clone(), vec![]);
        let cats = [Some("A"), Some("A"), Some("B"), Some("B"), Some("C")];
        let years = [Some(2024), Some(2025), Some(2024), Some(2025), Some(2024)];
        let sales = [Some(10.5), Some(11.0), Some(20.0), None, Some(30.25)];
        let qty = [Some(1), Some(2), Some(3), Some(4), Some(5)];
        let rs2 = ResultSet::from_batches(0, cols, vec![]);
        for (lo, hi) in [(0, 3), (3, 5)] {
            let b = RecordBatch::try_new(
                rs.schema.clone(),
                vec![Arc::new(Str::from(cats[lo..hi].to_vec())) as ArrayRef, Arc::new(Int32Array::from(years[lo..hi].to_vec())), decimal(&rs.schema, 2, sales[lo..hi].to_vec()), Arc::new(Int32Array::from(qty[lo..hi].to_vec()))],
            )
            .unwrap();
            rs2.append(b).unwrap();
        }
        rs2
    }

    #[test]
    fn matrix_keys_in_view_order_with_cells() {
        let rs = sample();
        let fmt = CellFormatter::default();
        let spec = pivot_spec_by_name(&rs, &["category"], &["Year"], &["Sales", "Qty"], &[], &[]).unwrap();
        assert_eq!(spec, PivotSpec { rows: vec![0], cols: vec![1], values: vec![2, 3], row_subtotal_flags: vec![None], col_subtotal_flags: vec![None] });
        let t = pivot(&rs, &spec, &fmt);
        assert!(t.error.is_none(), "{:?}", t.error);
        assert!(!t.is_empty());
        assert!(!t.truncated);
        assert_eq!(t.rows_in, 5);
        assert_eq!(t.generation, rs.generation());
        let labels = |keys: &[PivotKey]| keys.iter().map(|k| k.joined("/")).collect::<Vec<_>>();
        assert_eq!(labels(&t.row_keys), vec!["A", "B", "C"]);
        assert_eq!(labels(&t.col_keys), vec!["2024", "2025"]);
        assert!(t.row_keys.iter().all(|k| !k.is_subtotal()));
        assert_eq!(t.column_count(), 4);
        assert_eq!(t.value_names, vec!["Sales", "Qty"]);
        assert_eq!(t.row_key_names, vec!["Category"]);
        assert_eq!(t.cell(0, 0, 0).unwrap().num, Some(10.5));
        assert_eq!(&*t.cell(0, 0, 0).unwrap().text, &*rs.cell_text(0, 2, &fmt));
        assert_eq!(t.cell(0, 1, 1).unwrap().num, Some(2.0));
        assert_eq!(&*t.cell(0, 1, 1).unwrap().text, "2");
        assert_eq!(t.cell(2, 0, 0).unwrap().num, Some(30.25));
        // a present row with a NULL value: a cell with the formatter's text and no number
        let b_2025_sales = t.cell(1, 1, 0).unwrap();
        assert_eq!(b_2025_sales.num, None);
        assert_eq!(&*b_2025_sales.text, "NULL");
        // the missing combination
        assert!(t.cell(2, 1, 0).is_none());
        assert!(t.cell(2, 1, 1).is_none());
        // out of range
        assert!(t.cell(3, 0, 0).is_none());
        assert!(t.cell(0, 2, 0).is_none());
        assert!(t.cell(0, 0, 2).is_none());
        // through a sorted view the keys follow the view
        rs.apply_view(ViewSpec { filters: vec![], sort: vec![SortKey { column: 0, descending: true }, SortKey { column: 1, descending: true }] }).unwrap();
        let t = pivot(&rs, &spec, &fmt);
        assert_eq!(labels(&t.row_keys), vec!["C", "B", "A"]);
        assert_eq!(labels(&t.col_keys), vec!["2024", "2025"]);
        assert_eq!(t.cell(2, 0, 0).unwrap().num, Some(10.5));
    }

    #[test]
    fn subtotal_flags_label_total_and_count_levels() {
        let cols = vec![
            ColumnInfo::new("Category", SqlType::NVarChar { len: Some(20) }, true, 0),
            ColumnInfo::new("Year", SqlType::Int, true, 1),
            ColumnInfo::new("IsSubtotal 1", SqlType::Bit, false, 2),
            ColumnInfo::new("IsSubtotal 2", SqlType::Bit, false, 3),
            ColumnInfo::new("Sales", SqlType::Float, true, 4),
        ];
        let rs = ResultSet::from_batches(0, cols, vec![]);
        // detail rows, a per-category subtotal (Year blank, flag 2), a grand total (both flags)
        let cats = vec![Some("A"), Some("A"), Some("A"), Some("B"), Some("B"), None];
        let years = vec![Some(2024), Some(2025), None, Some(2024), None, None];
        let f1 = vec![false, false, false, false, false, true];
        let f2 = vec![false, false, true, false, true, true];
        let sales = vec![1.0, 2.0, 3.0, 4.0, 4.0, 7.0];
        let b = RecordBatch::try_new(rs.schema.clone(), vec![Arc::new(Str::from(cats)) as ArrayRef, Arc::new(Int32Array::from(years)), Arc::new(Bools::from(f1)), Arc::new(Bools::from(f2)), Arc::new(F64::from(sales))]).unwrap();
        rs.append(b).unwrap();
        let fmt = CellFormatter::default();
        let spec = pivot_spec_by_name(&rs, &["Category", "Year"], &[], &["Sales"], &[Some("IsSubtotal 1"), Some("IsSubtotal 2")], &[]).unwrap();
        assert_eq!(spec.row_subtotal_flags, vec![Some(2), Some(3)]);
        let t = pivot(&rs, &spec, &fmt);
        assert!(t.error.is_none());
        assert_eq!(t.rows_in, 6);
        // no column fields: one unlabelled column key
        assert_eq!(t.col_keys, vec![PivotKey { labels: vec![], subtotal_level: 0 }]);
        assert_eq!(t.column_count(), 1);
        let keys: Vec<(String, usize)> = t.row_keys.iter().map(|k| (k.joined(" / "), k.subtotal_level)).collect();
        assert_eq!(keys, vec![("A / 2024".into(), 0), ("A / 2025".into(), 0), ("A / Total".into(), 1), ("B / 2024".into(), 0), ("B / Total".into(), 1), ("Total / Total".into(), 2)]);
        assert_eq!(t.cell(2, 0, 0).unwrap().num, Some(3.0));
        assert_eq!(t.cell(5, 0, 0).unwrap().num, Some(7.0));
        assert_eq!(t.column_name(0, 0), "Sales");
        // without the flags the same rows read as blank labels, no subtotal levels
        let plain = PivotSpec { row_subtotal_flags: vec![], ..spec.clone() };
        let t = pivot(&rs, &plain, &fmt);
        assert_eq!(t.row_keys[2], PivotKey { labels: vec![Arc::from("A"), Arc::from("")], subtotal_level: 0 });
        assert_eq!(t.row_keys[5].joined("|"), "|");
        // a blank level exports as "" (only an unfilled cell is NULL)
        let batch = t.to_record_batch();
        assert_eq!(batch.schema_ref().field(1).name(), "Year");
        let years = batch.column(1).as_any().downcast_ref::<StringArray>().unwrap().clone();
        assert!(!years.is_null(2));
        assert_eq!(years.value(2), "");
        assert_eq!(years.value(0), "2024");
    }

    #[test]
    fn values_only_matrix_and_export_shape() {
        let rs = sample();
        let fmt = CellFormatter::default();
        // values only: one column per value, named after the value
        let spec = pivot_spec_by_name(&rs, &["Category", "Year"], &[], &["Sales", "Qty"], &[], &[]).unwrap();
        let t = pivot(&rs, &spec, &fmt);
        assert_eq!(t.col_keys.len(), 1);
        assert_eq!(t.column_count(), 2);
        assert_eq!(t.row_keys.len(), 5);
        assert_eq!(t.cell(4, 0, 1).unwrap().num, Some(5.0));
        let b = t.to_record_batch();
        assert_eq!(b.num_rows(), 5);
        let names: Vec<&str> = b.schema_ref().fields().iter().map(|f| f.name().as_str()).collect();
        assert_eq!(names, vec!["Category", "Year", "Sales", "Qty"]);
        // the full matrix export: row keys as text, then "<col labels> · <value>" per cell column
        let spec = pivot_spec_by_name(&rs, &["Category"], &["Year"], &["Sales", "Qty"], &[], &[]).unwrap();
        let t = pivot(&rs, &spec, &fmt);
        let b = t.to_record_batch();
        assert_eq!(b.num_rows(), 3);
        assert_eq!(b.num_columns(), 5);
        let names: Vec<&str> = b.schema_ref().fields().iter().map(|f| f.name().as_str()).collect();
        assert_eq!(names, vec!["Category", "2024 · Sales", "2024 · Qty", "2025 · Sales", "2025 · Qty"]);
        assert!(b.schema_ref().fields().iter().all(|f| f.data_type() == &DataType::Utf8));
        let col = |i: usize| b.column(i).as_any().downcast_ref::<StringArray>().unwrap().clone();
        assert_eq!(col(0).value(2), "C");
        assert_eq!(col(2).value(0), "1");
        assert_eq!(col(4).value(1), "4");
        // (C, 2025) never happened: NULL in the export; (B, 2025) Sales was NULL in the data: the text
        assert!(col(3).is_null(2));
        assert!(col(4).is_null(2));
        assert_eq!(col(3).value(1), "NULL");
        // an empty-spec table still exports (zero columns, as many rows as keys)
        let t = pivot(&rs, &PivotSpec::default(), &fmt);
        assert_eq!(t.row_keys.len(), 1);
        assert!(t.is_empty());
        assert_eq!(t.to_record_batch().num_columns(), 0);
    }

    #[test]
    fn duplicates_keep_the_last_row() {
        let cols = vec![ColumnInfo::new("k", SqlType::Int, false, 0), ColumnInfo::new("v", SqlType::Int, false, 1)];
        let rs = ResultSet::from_batches(0, cols, vec![]);
        let b = RecordBatch::try_new(rs.schema.clone(), vec![Arc::new(Int32Array::from(vec![1, 2, 1])) as ArrayRef, Arc::new(Int32Array::from(vec![10, 20, 30]))]).unwrap();
        rs.append(b).unwrap();
        let t = pivot(&rs, &PivotSpec { rows: vec![0], values: vec![1], ..Default::default() }, &CellFormatter::default());
        assert_eq!(t.row_keys.len(), 2);
        assert_eq!(t.rows_in, 3);
        assert_eq!(t.cell(0, 0, 0).unwrap().num, Some(30.0));
        assert_eq!(t.cell(1, 0, 0).unwrap().num, Some(20.0));
    }

    #[test]
    fn key_limits_truncate() {
        let cols = vec![ColumnInfo::new("r", SqlType::Int, false, 0), ColumnInfo::new("c", SqlType::Int, false, 1), ColumnInfo::new("v", SqlType::Int, false, 2)];
        let fmt = CellFormatter::default();
        // 12,000 row keys, one column key
        let rs = ResultSet::from_batches(0, cols.clone(), vec![]);
        let n = 12_000;
        let b = RecordBatch::try_new(rs.schema.clone(), vec![Arc::new(Int32Array::from((0..n).collect::<Vec<i32>>())) as ArrayRef, Arc::new(Int32Array::from(vec![1; n as usize])), Arc::new(Int32Array::from((0..n).collect::<Vec<i32>>()))]).unwrap();
        rs.append(b).unwrap();
        let t = pivot(&rs, &PivotSpec { rows: vec![0], cols: vec![1], values: vec![2], ..Default::default() }, &fmt);
        assert!(t.truncated);
        assert_eq!(t.row_keys.len(), MAX_ROW_KEYS);
        assert_eq!(t.rows_in, MAX_ROW_KEYS);
        assert_eq!(t.cells.len(), MAX_ROW_KEYS);
        assert_eq!(t.cell(MAX_ROW_KEYS - 1, 0, 0).unwrap().num, Some((MAX_ROW_KEYS - 1) as f64));
        // one row key, 600 column keys
        let rs = ResultSet::from_batches(0, cols, vec![]);
        let n = 600;
        let b = RecordBatch::try_new(rs.schema.clone(), vec![Arc::new(Int32Array::from(vec![1; n as usize])) as ArrayRef, Arc::new(Int32Array::from((0..n).collect::<Vec<i32>>())), Arc::new(Int32Array::from((0..n).collect::<Vec<i32>>()))]).unwrap();
        rs.append(b).unwrap();
        let t = pivot(&rs, &PivotSpec { rows: vec![0], cols: vec![1], values: vec![2], ..Default::default() }, &fmt);
        assert!(t.truncated);
        assert_eq!(t.col_keys.len(), MAX_COL_KEYS);
        assert_eq!(t.column_count(), MAX_COL_KEYS);
        assert_eq!(t.row_keys.len(), 1);
        // a bad spec never panics
        let t = pivot(&rs, &PivotSpec { rows: vec![9], ..Default::default() }, &fmt);
        assert!(t.error.is_some());
        assert!(t.is_empty());
    }

    #[test]
    fn spec_by_name_resolves_case_insensitively_and_rejects_unknowns() {
        let rs = sample();
        let s = pivot_spec_by_name(&rs, &["CATEGORY"], &["year"], &["sales"], &[None], &[]).unwrap();
        assert_eq!((s.rows, s.cols, s.values), (vec![0], vec![1], vec![2]));
        assert!(pivot_spec_by_name(&rs, &["Nope"], &[], &["Sales"], &[], &[]).is_none());
        assert!(pivot_spec_by_name(&rs, &["Category"], &[], &["Sales"], &[Some("IsSubtotal 1")], &[]).is_none());
        assert!(pivot_spec_by_name(&rs, &[], &[], &[], &[], &[]).is_some());
    }
}
