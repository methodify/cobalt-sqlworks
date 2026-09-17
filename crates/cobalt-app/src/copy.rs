//! Clipboard text builders over a grid selection: TSV (Excel-ready), CSV, Markdown, JSON,
//! INSERT statements, IN lists.

use crate::state::Selection;
use cobalt_core::quote_ident;
use cobalt_results::{CellFormatter, CellValue, ResultSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyKind {
    Tsv,
    TsvWithHeaders,
    HeadersOnly,
    Csv,
    Markdown,
    Json,
    Insert,
    InList,
    Cell,
}

pub struct CopyOptions<'a> {
    pub fmt: &'a CellFormatter,
    pub null_as: &'a str,
    pub table_name: &'a str,
}

fn ranges(rs: &ResultSet, sel: &Selection) -> Option<(Vec<usize>, Vec<usize>)> {
    let (r, c) = sel.resolve(rs.visible_count(), rs.column_count())?;
    Some((r.collect(), c.collect()))
}

fn full_text(rs: &ResultSet, row: usize, col: usize, fmt: &CellFormatter, null_as: &str) -> String {
    match rs.cell_value(row, col) {
        CellValue::Null => null_as.to_string(),
        CellValue::Text(t) => t,
        _ => rs.cell_text(row, col, fmt).to_string(),
    }
}

fn tsv_escape(s: &str) -> String {
    if s.contains('\t') || s.contains('\n') || s.contains('\r') || s.contains('"') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn csv_escape(s: &str) -> String {
    if s.contains(',') || s.contains('\n') || s.contains('\r') || s.contains('"') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

pub fn build(rs: &ResultSet, sel: &Selection, kind: CopyKind, o: &CopyOptions<'_>) -> Option<String> {
    let (rows, cols) = ranges(rs, sel)?;
    let names: Vec<String> = cols.iter().map(|&c| rs.columns[c].name.clone()).collect();
    let mut out = String::new();
    match kind {
        CopyKind::Cell => {
            let (r, c) = (*rows.first()?, *cols.first()?);
            out = full_text(rs, r, c, o.fmt, o.null_as);
        }
        CopyKind::Tsv | CopyKind::TsvWithHeaders | CopyKind::HeadersOnly => {
            if matches!(kind, CopyKind::TsvWithHeaders | CopyKind::HeadersOnly) {
                out.push_str(&names.iter().map(|n| tsv_escape(n)).collect::<Vec<_>>().join("\t"));
                out.push_str("\r\n");
            }
            if kind != CopyKind::HeadersOnly {
                for &r in &rows {
                    let line: Vec<String> = cols.iter().map(|&c| tsv_escape(&full_text(rs, r, c, o.fmt, o.null_as))).collect();
                    out.push_str(&line.join("\t"));
                    out.push_str("\r\n");
                }
            }
        }
        CopyKind::Csv => {
            out.push_str(&names.iter().map(|n| csv_escape(n)).collect::<Vec<_>>().join(","));
            out.push_str("\r\n");
            for &r in &rows {
                let line: Vec<String> = cols.iter().map(|&c| csv_escape(&full_text(rs, r, c, o.fmt, o.null_as))).collect();
                out.push_str(&line.join(","));
                out.push_str("\r\n");
            }
        }
        CopyKind::Markdown => {
            let esc = |s: &str| s.replace('|', "\\|").replace('\n', "<br>").replace('\r', "");
            out.push_str("| ");
            out.push_str(&names.iter().map(|n| esc(n)).collect::<Vec<_>>().join(" | "));
            out.push_str(" |\n|");
            for &c in &cols {
                out.push_str(if rs.columns[c].sql_type.right_align() { "---:|" } else { "---|" });
            }
            out.push('\n');
            for &r in &rows {
                out.push_str("| ");
                let line: Vec<String> = cols.iter().map(|&c| esc(&full_text(rs, r, c, o.fmt, o.null_as))).collect();
                out.push_str(&line.join(" | "));
                out.push_str(" |\n");
            }
        }
        CopyKind::Json => {
            let mut arr = Vec::with_capacity(rows.len());
            for &r in &rows {
                let mut obj = serde_json::Map::new();
                for (i, &c) in cols.iter().enumerate() {
                    let mut key = names[i].clone();
                    if key.is_empty() {
                        key = format!("column{}", c + 1);
                    }
                    let mut k = key.clone();
                    let mut n = 2;
                    while obj.contains_key(&k) {
                        k = format!("{key}_{n}");
                        n += 1;
                    }
                    obj.insert(k, rs.cell_value(r, c).to_json());
                }
                arr.push(serde_json::Value::Object(obj));
            }
            out = serde_json::to_string_pretty(&serde_json::Value::Array(arr)).unwrap_or_default();
        }
        CopyKind::Insert => {
            let table = if o.table_name.is_empty() { "[dbo].[Table]" } else { o.table_name };
            let col_list = names.iter().map(|n| quote_ident(n)).collect::<Vec<_>>().join(", ");
            for chunk in rows.chunks(1000) {
                out.push_str(&format!("INSERT INTO {table} ({col_list})\nVALUES\n"));
                let vals: Vec<String> = chunk
                    .iter()
                    .map(|&r| format!("  ({})", cols.iter().map(|&c| rs.cell_value(r, c).to_sql_literal()).collect::<Vec<_>>().join(", ")))
                    .collect();
                out.push_str(&vals.join(",\n"));
                out.push_str(";\n");
            }
        }
        CopyKind::InList => {
            let mut seen = std::collections::HashSet::new();
            let mut items = Vec::new();
            for &r in &rows {
                for &c in &cols {
                    let v = rs.cell_value(r, c);
                    if v.is_null() {
                        continue;
                    }
                    let lit = v.to_sql_literal();
                    if seen.insert(lit.clone()) {
                        items.push(lit);
                    }
                }
            }
            out = format!("({})", items.join(", "));
        }
    }
    Some(out)
}
