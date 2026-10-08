//! Column profiling and the totals row: aggregates over a result set's visible rows.

use crate::state::TotalKind;
use cobalt_results::summary::{summarize, Summary};
use cobalt_results::{CellFormatter, ResultSet};
use std::sync::Arc;

/// Rows scanned at most (keeps a profile of a huge set responsive).
pub const SCAN_CAP: usize = 200_000;
pub const HIST_BINS: usize = 12;

pub struct ColumnProfile {
    pub name: String,
    pub type_label: String,
    pub summary: Summary,
    /// Most frequent values with their counts.
    pub top: Vec<(Arc<str>, usize)>,
    /// Counts per bin between `hist_range`, for numeric columns with a spread.
    pub histogram: Vec<usize>,
    pub hist_range: Option<(f64, f64)>,
}

pub struct ColumnProfiles {
    pub generation: u64,
    pub rows_scanned: usize,
    pub columns: Vec<ColumnProfile>,
    pub open: bool,
}

pub fn profile(rs: &ResultSet, fmt: &CellFormatter) -> ColumnProfiles {
    let rows = rs.visible_count().min(SCAN_CAP);
    let mut columns = Vec::with_capacity(rs.column_count());
    for col in 0..rs.column_count() {
        let summary = summarize(rs, (0..rows).map(|r| (r, col)), SCAN_CAP);
        let (mut values, _) = rs.distinct_values(col, 10_000, fmt);
        values.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        values.truncate(5);
        let (histogram, hist_range) = match (summary.min, summary.max) {
            (Some(lo), Some(hi)) if summary.is_numeric() && hi > lo => {
                let mut bins = vec![0usize; HIST_BINS];
                for r in 0..rows {
                    if let Some(v) = rs.cell_value(r, col).as_f64() {
                        let t = ((v - lo) / (hi - lo)).clamp(0.0, 1.0);
                        let b = ((t * HIST_BINS as f64) as usize).min(HIST_BINS - 1);
                        bins[b] += 1;
                    }
                }
                (bins, Some((lo, hi)))
            }
            _ => (Vec::new(), None),
        };
        let info = &rs.columns[col];
        columns.push(ColumnProfile { name: info.name.clone(), type_label: info.sql_type.to_string(), summary, top: values, histogram, hist_range });
    }
    ColumnProfiles { generation: rs.generation(), rows_scanned: rows, columns, open: true }
}

/// One aggregate string per column for the totals row (empty for non-numeric columns on the
/// numeric aggregates).
pub fn totals(rs: &ResultSet, kind: TotalKind) -> Vec<String> {
    let rows = rs.visible_count().min(SCAN_CAP);
    (0..rs.column_count())
        .map(|col| {
            let s = summarize(rs, (0..rows).map(|r| (r, col)), SCAN_CAP);
            match kind {
                TotalKind::Count => crate::state::fmt_count((s.count - s.nulls) as u64),
                TotalKind::Distinct => crate::state::fmt_count(s.distinct as u64),
                TotalKind::Sum => s.sum.map(fmt_num).unwrap_or_default(),
                TotalKind::Avg => s.avg.map(fmt_num).unwrap_or_default(),
                TotalKind::Min => s.min.map(fmt_num).unwrap_or_default(),
                TotalKind::Max => s.max.map(fmt_num).unwrap_or_default(),
            }
        })
        .collect()
}

pub fn fmt_num(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        let s = crate::state::fmt_count(v.abs() as u64);
        if v < 0.0 {
            format!("-{s}")
        } else {
            s
        }
    } else {
        format!("{v:.4}").trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

/// A cell-text predicate from [`find_matcher`].
pub type FindMatcher = Box<dyn Fn(&str) -> bool>;

/// The matcher behind the results find bar: plain substring, or a regex (also used for whole-word
/// matching, by wrapping the escaped text in word boundaries).
pub fn find_matcher(text: &str, case_sensitive: bool, use_regex: bool, whole_word: bool) -> Result<FindMatcher, String> {
    if use_regex || whole_word {
        let mut pattern = if use_regex { text.to_string() } else { regex::escape(text) };
        if whole_word {
            pattern = format!(r"\b(?:{pattern})\b");
        }
        let re = regex::RegexBuilder::new(&pattern).case_insensitive(!case_sensitive).build().map_err(|e| e.to_string().lines().next().unwrap_or("invalid regex").to_string())?;
        Ok(Box::new(move |s: &str| re.is_match(s)))
    } else if case_sensitive {
        let needle = text.to_string();
        Ok(Box::new(move |s: &str| s.contains(&needle)))
    } else {
        let needle = text.to_lowercase();
        Ok(Box::new(move |s: &str| s.to_lowercase().contains(&needle)))
    }
}

#[cfg(test)]
mod tests {
    use super::find_matcher;
    #[test]
    fn matchers() {
        assert!(find_matcher("abc", false, false, false).unwrap()("xABCx"));
        assert!(!find_matcher("abc", true, false, false).unwrap()("xABCx"));
        assert!(find_matcher("abc", false, false, true).unwrap()("x abc x"));
        assert!(!find_matcher("abc", false, false, true).unwrap()("xabcx"));
        assert!(find_matcher("^a.c$", false, true, false).unwrap()("ABC"));
        assert!(find_matcher("(", false, true, false).is_err());
    }
}
