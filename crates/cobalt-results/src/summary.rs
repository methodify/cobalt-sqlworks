use crate::{CellValue, ResultSet};
use std::collections::HashSet;

/// Status-bar style aggregates over a set of cells.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Summary {
    pub count: usize,
    pub nulls: usize,
    pub distinct: usize,
    /// Present when at least one numeric value was seen.
    pub sum: Option<f64>,
    pub avg: Option<f64>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub numeric_count: usize,
}

impl Summary {
    pub fn is_numeric(&self) -> bool {
        self.numeric_count > 0 && self.numeric_count + self.nulls == self.count
    }
}

/// `cells` are (visible_row, col) pairs. Caps work at `max_cells` to keep the UI responsive.
pub fn summarize(rs: &ResultSet, cells: impl Iterator<Item = (usize, usize)>, max_cells: usize) -> Summary {
    let mut s = Summary::default();
    let mut distinct: HashSet<String> = HashSet::new();
    let (mut sum, mut min, mut max) = (0.0f64, f64::INFINITY, f64::NEG_INFINITY);
    for (row, col) in cells.take(max_cells) {
        s.count += 1;
        let v = rs.cell_value(row, col);
        if v.is_null() {
            s.nulls += 1;
            continue;
        }
        if let Some(f) = v.as_f64() {
            if !matches!(v, CellValue::Bool(_)) {
                s.numeric_count += 1;
                sum += f;
                min = min.min(f);
                max = max.max(f);
            }
        }
        if distinct.len() < 100_000 {
            distinct.insert(format!("{:?}", v));
        }
    }
    s.distinct = distinct.len();
    if s.numeric_count > 0 {
        s.sum = Some(sum);
        s.avg = Some(sum / s.numeric_count as f64);
        s.min = Some(min);
        s.max = Some(max);
    }
    s
}
