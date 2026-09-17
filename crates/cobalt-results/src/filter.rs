use crate::{CellFormatter, CellValue};
use arrow::array::RecordBatch;
use cobalt_core::ColumnInfo;
use std::collections::HashSet;

/// One column filter. Several filters are ANDed.
#[derive(Clone, Debug, PartialEq)]
pub struct ColumnFilter {
    pub column: usize,
    pub op: FilterOp,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FilterOp {
    /// Keep rows whose display text is in the set (the header checklist).
    In(HashSet<String>),
    /// Case-insensitive substring on display text.
    Contains(String),
    NotContains(String),
    Equals(String),
    NotEquals(String),
    StartsWith(String),
    EndsWith(String),
    /// Numeric/temporal comparisons parse `value` against the typed cell.
    Gt(String),
    Gte(String),
    Lt(String),
    Lte(String),
    Between(String, String),
    IsNull,
    NotNull,
}

impl FilterOp {
    pub fn label(&self) -> String {
        match self {
            FilterOp::In(s) => format!("in {} values", s.len()),
            FilterOp::Contains(v) => format!("contains \"{v}\""),
            FilterOp::NotContains(v) => format!("not contains \"{v}\""),
            FilterOp::Equals(v) => format!("= {v}"),
            FilterOp::NotEquals(v) => format!("≠ {v}"),
            FilterOp::StartsWith(v) => format!("starts with \"{v}\""),
            FilterOp::EndsWith(v) => format!("ends with \"{v}\""),
            FilterOp::Gt(v) => format!("> {v}"),
            FilterOp::Gte(v) => format!("≥ {v}"),
            FilterOp::Lt(v) => format!("< {v}"),
            FilterOp::Lte(v) => format!("≤ {v}"),
            FilterOp::Between(a, b) => format!("between {a} and {b}"),
            FilterOp::IsNull => "is NULL".into(),
            FilterOp::NotNull => "is not NULL".into(),
        }
    }
}

/// Evaluate all filters over one batch → keep mask.
pub fn evaluate(batch: &RecordBatch, columns: &[ColumnInfo], filters: &[ColumnFilter], fmt: &CellFormatter) -> Vec<bool> {
    let n = batch.num_rows();
    let mut keep = vec![true; n];
    for f in filters {
        let arr = batch.column(f.column);
        let text = matches!(
            f.op,
            FilterOp::In(_) | FilterOp::Contains(_) | FilterOp::NotContains(_) | FilterOp::Equals(_) | FilterOp::NotEquals(_) | FilterOp::StartsWith(_) | FilterOp::EndsWith(_)
        )
        .then(|| fmt.format_column(arr, &columns[f.column]));
        for i in 0..n {
            if !keep[i] {
                continue;
            }
            let null = arr.is_null(i);
            let ok = match &f.op {
                FilterOp::IsNull => null,
                FilterOp::NotNull => !null,
                FilterOp::In(set) => set.contains(&*text.as_ref().unwrap()[i]),
                FilterOp::Contains(v) => !null && text.as_ref().unwrap()[i].to_lowercase().contains(&v.to_lowercase()),
                FilterOp::NotContains(v) => null || !text.as_ref().unwrap()[i].to_lowercase().contains(&v.to_lowercase()),
                FilterOp::Equals(v) => !null && text.as_ref().unwrap()[i].eq_ignore_ascii_case(v),
                FilterOp::NotEquals(v) => null || !text.as_ref().unwrap()[i].eq_ignore_ascii_case(v),
                FilterOp::StartsWith(v) => !null && text.as_ref().unwrap()[i].to_lowercase().starts_with(&v.to_lowercase()),
                FilterOp::EndsWith(v) => !null && text.as_ref().unwrap()[i].to_lowercase().ends_with(&v.to_lowercase()),
                FilterOp::Gt(v) | FilterOp::Gte(v) | FilterOp::Lt(v) | FilterOp::Lte(v) => {
                    !null && compare(&CellValue::from_array(arr, i), v).map(|o| match &f.op {
                        FilterOp::Gt(_) => o.is_gt(),
                        FilterOp::Gte(_) => o.is_ge(),
                        FilterOp::Lt(_) => o.is_lt(),
                        _ => o.is_le(),
                    }).unwrap_or(false)
                }
                FilterOp::Between(a, b) => {
                    !null && {
                        let cv = CellValue::from_array(arr, i);
                        compare(&cv, a).map(|o| o.is_ge()).unwrap_or(false) && compare(&cv, b).map(|o| o.is_le()).unwrap_or(false)
                    }
                }
            };
            keep[i] = ok;
        }
    }
    keep
}

/// Compare a typed cell against user text: numbers numerically, dates/times chronologically, else as text.
fn compare(cell: &CellValue, text: &str) -> Option<std::cmp::Ordering> {
    let text = text.trim();
    match cell {
        CellValue::Int(_) | CellValue::Float(_) | CellValue::Decimal(_, _) | CellValue::Bool(_) => {
            let a = cell.as_f64()?;
            let b: f64 = text.replace(',', "").parse().ok()?;
            a.partial_cmp(&b)
        }
        CellValue::Date(d) => {
            let b = chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d").ok()?;
            Some(d.cmp(&b))
        }
        CellValue::DateTime(dt) => {
            let b = parse_datetime(text)?;
            Some(dt.cmp(&b))
        }
        CellValue::DateTimeTz(dt) => {
            let b = parse_datetime(text)?;
            Some(dt.naive_utc().cmp(&b))
        }
        CellValue::Time(t) => {
            let b = chrono::NaiveTime::parse_from_str(text, "%H:%M:%S%.f").or_else(|_| chrono::NaiveTime::parse_from_str(text, "%H:%M:%S")).or_else(|_| chrono::NaiveTime::parse_from_str(text, "%H:%M")).ok()?;
            Some(t.cmp(&b))
        }
        CellValue::Text(s) => Some(s.to_lowercase().cmp(&text.to_lowercase())),
        _ => None,
    }
}

fn parse_datetime(text: &str) -> Option<chrono::NaiveDateTime> {
    for f in ["%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S", "%Y-%m-%dT%H:%M:%S", "%Y-%m-%d %H:%M"] {
        if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(text, f) {
            return Some(dt);
        }
    }
    chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d").ok().map(|d| d.and_hms_opt(0, 0, 0).unwrap())
}
