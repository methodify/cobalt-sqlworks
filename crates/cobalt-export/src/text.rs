//! Clipboard builders over a rectangular selection of the *visible* grid.
//!
//! All builders gather the selection into one Arrow batch (`ResultSet::gather_rows`) and format
//! whole columns at once, so a 100k-cell copy is a handful of allocations per column rather than
//! per cell. Nothing here truncates text.

use crate::markdown::{escape_cell, header_lines};
use crate::{export_formatter, Result};
use arrow::array::{Array, RecordBatch};
use cobalt_core::{quote_ident, ColumnInfo};
use cobalt_results::{CellFormatter, CellValue, ResultSet};
use std::sync::Arc;

/// A rectangular selection: visible row indexes and column indexes, both sorted ascending.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub rows: Vec<usize>,
    pub cols: Vec<usize>,
}

impl Selection {
    pub fn new(mut rows: Vec<usize>, mut cols: Vec<usize>) -> Self {
        rows.sort_unstable();
        rows.dedup();
        cols.sort_unstable();
        cols.dedup();
        Self { rows, cols }
    }

    /// Every visible row and every column.
    pub fn all(rs: &ResultSet) -> Self {
        Self { rows: (0..rs.visible_count()).collect(), cols: (0..rs.column_count()).collect() }
    }

    /// Inclusive rectangle.
    pub fn rect(row0: usize, row1: usize, col0: usize, col1: usize) -> Self {
        let (r0, r1) = (row0.min(row1), row0.max(row1));
        let (c0, c1) = (col0.min(col1), col0.max(col1));
        Self { rows: (r0..=r1).collect(), cols: (c0..=c1).collect() }
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty() || self.cols.is_empty()
    }

    pub fn cell_count(&self) -> usize {
        self.rows.len() * self.cols.len()
    }
}

/// The gathered selection plus its formatted text, ready for any builder.
pub struct Gathered {
    pub batch: RecordBatch,
    pub columns: Vec<ColumnInfo>,
    pub text: Vec<Arc<Vec<Arc<str>>>>,
}

impl Gathered {
    pub fn rows(&self) -> usize {
        self.batch.num_rows()
    }
    pub fn cols(&self) -> usize {
        self.columns.len()
    }
    pub fn cell(&self, r: usize, c: usize) -> &str {
        &self.text[c][r]
    }
    pub fn is_null(&self, r: usize, c: usize) -> bool {
        self.batch.column(c).is_null(r)
    }
}

/// Gather + format a selection with `fmt` (never truncated, ISO datetimes, `null_as` for NULL).
pub fn gather(rs: &ResultSet, sel: &Selection, fmt: &CellFormatter, null_as: &str) -> Result<Gathered> {
    let batch = rs.gather_rows(&sel.rows, &sel.cols)?;
    let columns: Vec<ColumnInfo> = sel.cols.iter().map(|&c| rs.columns[c].clone()).collect();
    let fmt = export_formatter(fmt).with_null_text(null_as);
    let text = (0..batch.num_columns()).map(|c| fmt.format_column(batch.column(c), &columns[c])).collect();
    Ok(Gathered { batch, columns, text })
}

fn header_names(g: &Gathered) -> impl Iterator<Item = &str> {
    g.columns.iter().map(|c| c.name.as_str())
}

/// Quote a cell for TSV if it contains a tab, CR/LF or quote (Excel-paste convention).
fn tsv_quote(s: &str) -> std::borrow::Cow<'_, str> {
    if s.contains(['\t', '\n', '\r', '"']) {
        std::borrow::Cow::Owned(format!("\"{}\"", s.replace('"', "\"\"")))
    } else {
        std::borrow::Cow::Borrowed(s)
    }
}

/// RFC 4180 quoting: quote when the cell contains the delimiter, quote, CR or LF.
fn csv_quote<'a>(s: &'a str, delimiter: char) -> std::borrow::Cow<'a, str> {
    if s.contains([delimiter, '"', '\n', '\r']) {
        std::borrow::Cow::Owned(format!("\"{}\"", s.replace('"', "\"\"")))
    } else {
        std::borrow::Cow::Borrowed(s)
    }
}

fn delimited(g: &Gathered, headers: bool, delimiter: char, line_ending: &str, quote: fn(&str, char) -> std::borrow::Cow<'_, str>) -> String {
    let mut out = String::with_capacity(g.rows() * g.cols() * 10 + 64);
    if headers {
        for (i, name) in header_names(g).enumerate() {
            if i > 0 {
                out.push(delimiter);
            }
            out.push_str(&quote(name, delimiter));
        }
        out.push_str(line_ending);
    }
    for r in 0..g.rows() {
        for c in 0..g.cols() {
            if c > 0 {
                out.push(delimiter);
            }
            out.push_str(&quote(g.cell(r, c), delimiter));
        }
        out.push_str(line_ending);
    }
    out
}

/// Tab-separated, CRLF, Excel-paste ready (cells with tab/newline/quote are quoted).
pub fn to_tsv(rs: &ResultSet, sel: &Selection, headers: bool, fmt: &CellFormatter, null_as: &str) -> Result<String> {
    let g = gather(rs, sel, fmt, null_as)?;
    Ok(delimited(&g, headers, '\t', "\r\n", |s, _| tsv_quote(s)))
}

/// Comma-separated, RFC 4180 quoting, CRLF.
pub fn to_csv(rs: &ResultSet, sel: &Selection, headers: bool, fmt: &CellFormatter, null_as: &str) -> Result<String> {
    let g = gather(rs, sel, fmt, null_as)?;
    Ok(delimited(&g, headers, ',', "\r\n", csv_quote))
}

/// GFM table; numeric columns right-aligned; pipes and newlines escaped.
pub fn to_markdown(rs: &ResultSet, sel: &Selection, headers: bool, fmt: &CellFormatter, null_as: &str) -> Result<String> {
    let g = gather(rs, sel, fmt, null_as)?;
    let mut out = String::with_capacity(g.rows() * g.cols() * 10 + 64);
    if headers {
        out.push_str(&header_lines(&g.columns, true, true, "\n"));
    }
    for r in 0..g.rows() {
        out.push('|');
        for c in 0..g.cols() {
            out.push(' ');
            out.push_str(&escape_cell(g.cell(r, c), true));
            out.push_str(" |");
        }
        out.push('\n');
    }
    Ok(out)
}

/// Pretty-printed JSON array of objects (typed values, nulls as `null`, dates ISO).
pub fn to_json(rs: &ResultSet, sel: &Selection) -> Result<String> {
    let batch = rs.gather_rows(&sel.rows, &sel.cols)?;
    let names: Vec<String> = sel.cols.iter().map(|&c| rs.schema.field(c).name().clone()).collect();
    let mut rows: Vec<serde_json::Value> = Vec::with_capacity(batch.num_rows());
    for r in 0..batch.num_rows() {
        let mut obj = serde_json::Map::with_capacity(names.len());
        for (c, name) in names.iter().enumerate() {
            obj.insert(name.clone(), CellValue::from_array(batch.column(c), r).to_json());
        }
        rows.push(serde_json::Value::Object(obj));
    }
    Ok(serde_json::to_string_pretty(&serde_json::Value::Array(rows))?)
}

/// `INSERT INTO table (cols) VALUES (...), (...);` with `batch_size` rows per statement
/// (0 = one statement). Literals via [`CellValue::to_sql_literal`].
pub fn to_insert_statements(rs: &ResultSet, sel: &Selection, table: &str, batch_size: usize) -> Result<String> {
    let batch = rs.gather_rows(&sel.rows, &sel.cols)?;
    let cols: Vec<String> = sel.cols.iter().map(|&c| quote_ident(&rs.columns[c].name)).collect();
    let head = format!("INSERT INTO {table} ({}) VALUES", cols.join(", "));
    let per = if batch_size == 0 { usize::MAX } else { batch_size };
    let mut out = String::with_capacity(batch.num_rows() * cols.len() * 12 + 64);
    let n = batch.num_rows();
    let mut r = 0;
    while r < n {
        let end = r.saturating_add(per).min(n);
        out.push_str(&head);
        out.push('\n');
        for (i, row) in (r..end).enumerate() {
            out.push('(');
            for c in 0..cols.len() {
                if c > 0 {
                    out.push_str(", ");
                }
                out.push_str(&CellValue::from_array(batch.column(c), row).to_sql_literal());
            }
            out.push(')');
            out.push_str(if i + 1 == end - r { ";\n" } else { ",\n" });
        }
        r = end;
        if r < n {
            out.push('\n');
        }
    }
    Ok(out)
}

/// `(lit, lit, …)` of the distinct non-NULL literals of the selected cells, in first-seen order
/// (row-major). Strings are `N'…'` quoted.
pub fn to_in_list(rs: &ResultSet, sel: &Selection) -> Result<String> {
    let batch = rs.gather_rows(&sel.rows, &sel.cols)?;
    let mut seen: std::collections::HashSet<String> = Default::default();
    let mut items: Vec<String> = Vec::new();
    for r in 0..batch.num_rows() {
        for c in 0..batch.num_columns() {
            let v = CellValue::from_array(batch.column(c), r);
            if v.is_null() {
                continue;
            }
            let lit = v.to_sql_literal();
            if seen.insert(lit.clone()) {
                items.push(lit);
            }
        }
    }
    Ok(format!("({})", items.join(", ")))
}

/// Cells separated by a single space, rows by `\n` (the "Plain text" copy).
pub fn to_plain(rs: &ResultSet, sel: &Selection, fmt: &CellFormatter, null_as: &str) -> Result<String> {
    let g = gather(rs, sel, fmt, null_as)?;
    let mut out = String::with_capacity(g.rows() * g.cols() * 10);
    for r in 0..g.rows() {
        for c in 0..g.cols() {
            if c > 0 {
                out.push(' ');
            }
            out.push_str(g.cell(r, c));
        }
        if r + 1 < g.rows() {
            out.push('\n');
        }
    }
    Ok(out)
}

/// Column names of the selection joined by `delimiter` (Ctrl+Shift+H).
pub fn headers_line(rs: &ResultSet, sel: &Selection, delimiter: &str) -> String {
    sel.cols.iter().filter_map(|&c| rs.columns.get(c)).map(|c| c.name.as_str()).collect::<Vec<_>>().join(delimiter)
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::{Int32Array, StringArray, TimestampMillisecondArray};
    use arrow::datatypes::{DataType, Field, Schema, TimeUnit};
    use cobalt_core::SqlType;

    fn sample() -> Arc<ResultSet> {
        let cols = vec![
            ColumnInfo::new("id", SqlType::Int, false, 0),
            ColumnInfo::new("Name Col", SqlType::NVarChar { len: Some(50) }, true, 1),
            ColumnInfo::new("when", SqlType::DateTime, true, 2),
        ];
        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Int32, true),
            Field::new("Name Col", DataType::Utf8, true),
            Field::new("when", DataType::Timestamp(TimeUnit::Millisecond, None), true),
        ]));
        let b1 = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(Int32Array::from(vec![Some(1), Some(2)])),
                Arc::new(StringArray::from(vec![Some("a|b"), None])),
                Arc::new(TimestampMillisecondArray::from(vec![Some(1_704_164_645_123), None])),
            ],
        )
        .unwrap();
        let b2 = RecordBatch::try_new(
            schema,
            vec![
                Arc::new(Int32Array::from(vec![Some(1)])),
                Arc::new(StringArray::from(vec![Some("tab\there \"q\"")])),
                Arc::new(TimestampMillisecondArray::from(vec![Some(0)])),
            ],
        )
        .unwrap();
        ResultSet::from_batches(0, cols, vec![b1, b2])
    }

    #[test]
    fn tsv_quoting_and_headers() {
        let rs = sample();
        let fmt = CellFormatter::default();
        let s = to_tsv(&rs, &Selection::all(&rs), true, &fmt, "NULL").unwrap();
        assert_eq!(s, "id\tName Col\twhen\r\n1\ta|b\t2024-01-02 03:04:05.123\r\n2\tNULL\tNULL\r\n1\t\"tab\there \"\"q\"\"\"\t1970-01-01 00:00:00\r\n");
        let s = to_tsv(&rs, &Selection::all(&rs), false, &fmt, "").unwrap();
        assert!(s.starts_with("1\ta|b\t"));
        assert!(s.contains("2\t\t\r\n"));
    }

    #[test]
    fn csv_quoting() {
        let rs = sample();
        let fmt = CellFormatter::default();
        let s = to_csv(&rs, &Selection::rect(2, 2, 0, 1), false, &fmt, "").unwrap();
        assert_eq!(s, "1,\"tab\there \"\"q\"\"\"\r\n");
        let s = to_csv(&rs, &Selection::rect(0, 0, 1, 1), true, &fmt, "").unwrap();
        assert_eq!(s, "Name Col\r\na|b\r\n");
    }

    #[test]
    fn markdown_escaping_and_alignment() {
        let rs = sample();
        let fmt = CellFormatter::default();
        let s = to_markdown(&rs, &Selection::rect(0, 1, 0, 1), true, &fmt, "NULL").unwrap();
        assert_eq!(s, "| id | Name Col |\n|---:|:---|\n| 1 | a\\|b |\n| 2 | NULL |\n");
    }

    #[test]
    fn json_array() {
        let rs = sample();
        let s = to_json(&rs, &Selection::rect(0, 1, 0, 2)).unwrap();
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v[0]["id"], 1);
        assert_eq!(v[0]["Name Col"], "a|b");
        assert_eq!(v[0]["when"], "2024-01-02T03:04:05.123");
        assert!(v[1]["Name Col"].is_null());
        assert_eq!(v.as_array().unwrap().len(), 2);
    }

    #[test]
    fn insert_batching() {
        let rs = sample();
        let s = to_insert_statements(&rs, &Selection::all(&rs), "[dbo].[t]", 2).unwrap();
        let expected = "INSERT INTO [dbo].[t] (id, [Name Col], when) VALUES\n(1, N'a|b', '2024-01-02T03:04:05.123'),\n(2, NULL, NULL);\n\nINSERT INTO [dbo].[t] (id, [Name Col], when) VALUES\n(1, N'tab\there \"q\"', '1970-01-01T00:00:00');\n";
        assert_eq!(s, expected);
        let one = to_insert_statements(&rs, &Selection::all(&rs), "t", 0).unwrap();
        assert_eq!(one.matches("INSERT INTO").count(), 1);
        assert!(one.ends_with(");\n"));
    }

    #[test]
    fn in_list_distinct() {
        let rs = sample();
        let s = to_in_list(&rs, &Selection::rect(0, 2, 0, 0)).unwrap();
        assert_eq!(s, "(1, 2)");
        let s = to_in_list(&rs, &Selection::rect(0, 2, 1, 1)).unwrap();
        assert_eq!(s, "(N'a|b', N'tab\there \"q\"')");
        let s = to_in_list(&rs, &Selection::new(vec![1], vec![1])).unwrap();
        assert_eq!(s, "()");
    }

    #[test]
    fn plain_and_headers() {
        let rs = sample();
        let fmt = CellFormatter::default();
        assert_eq!(to_plain(&rs, &Selection::rect(0, 1, 0, 1), &fmt, "NULL").unwrap(), "1 a|b\n2 NULL");
        assert_eq!(headers_line(&rs, &Selection::all(&rs), "\t"), "id\tName Col\twhen");
        assert_eq!(headers_line(&rs, &Selection::new(vec![0], vec![2, 0]), ", "), "id, when");
    }

    #[test]
    fn selection_respects_view_order() {
        let rs = sample();
        rs.apply_view(cobalt_results::ViewSpec { filters: vec![], sort: vec![cobalt_results::SortKey { column: 0, descending: true }] }).unwrap();
        let fmt = CellFormatter::default();
        let s = to_plain(&rs, &Selection::rect(0, 2, 0, 0), &fmt, "").unwrap();
        assert_eq!(s, "2\n1\n1");
    }

    #[test]
    fn big_selection_is_fast() {
        let cols = vec![ColumnInfo::new("id", SqlType::Int, false, 0), ColumnInfo::new("s", SqlType::VarChar { len: Some(20) }, true, 1)];
        let schema = Arc::new(Schema::new(vec![Field::new("id", DataType::Int32, true), Field::new("s", DataType::Utf8, true)]));
        let mut batches = Vec::new();
        for b in 0..5 {
            let ids: Vec<i32> = (b * 10_000..(b + 1) * 10_000).collect();
            let ss: Vec<String> = ids.iter().map(|i| format!("row {i} with, comma")).collect();
            batches.push(RecordBatch::try_new(schema.clone(), vec![Arc::new(Int32Array::from(ids)), Arc::new(StringArray::from(ss))]).unwrap());
        }
        let rs = ResultSet::from_batches(0, cols, batches);
        let fmt = CellFormatter::default();
        let t = std::time::Instant::now();
        let s = to_tsv(&rs, &Selection::all(&rs), true, &fmt, "").unwrap();
        let e = t.elapsed();
        assert_eq!(s.lines().count(), 50_001);
        assert!(e < std::time::Duration::from_secs(5), "took {e:?}");
    }
}
