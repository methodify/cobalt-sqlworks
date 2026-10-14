//! The data behind a chart: one pass over the result set's view (what the grid shows, sorted
//! and filtered), the X values as categories, numbers or times, one `f64` series per Y column,
//! folded per X when the spec aggregates, and the visible rows behind each X so a click on the
//! chart can select them in the grid. Pure data; the drawing lives in the app.

use crate::display::CellFormatter;
use crate::ResultSet;
use arrow::array::{Array, ArrayRef, Float64Array};
use arrow::datatypes::{DataType, TimeUnit};
use cobalt_core::{Agg, ChartKind, ChartSpec, SqlType};
use std::collections::HashMap;
use std::sync::Arc;

/// What the X axis holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XKind {
    /// Row numbers (no X column).
    RowNumber,
    Category,
    Number,
    /// Milliseconds since the epoch (dates, times of day as ms into the day, timestamps).
    Time,
    /// Histogram bins of a numeric or time column, in order; `x_labels` carry the ranges.
    Bin,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Series {
    pub name: String,
    /// One value per X; NaN = no value.
    pub values: Vec<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChartData {
    pub x_kind: XKind,
    pub x_name: String,
    /// Numeric X (number, time or row number) per point; empty for categories.
    pub x_values: Vec<f64>,
    /// Category labels per point; empty for numeric X.
    pub x_labels: Vec<String>,
    pub series: Vec<Series>,
    /// Visible rows behind each point (every row for a category; the one row otherwise).
    pub rows: Vec<Vec<u32>>,
    /// Rows the pass looked at.
    pub total_rows: usize,
    /// Distinct X values before top-N folding.
    pub distinct_x: usize,
    /// Whether the last point is the "other" bucket of a top-N fold.
    pub has_other: bool,
    /// Histogram: the bin edges (points + 1 of them) and whether the count came from the data.
    pub bin_edges: Vec<f64>,
    pub bins_auto: bool,
    pub generation: u64,
    /// Why no chart could be built, in words.
    pub error: Option<String>,
}

impl ChartData {
    pub fn empty(generation: u64, error: impl Into<String>) -> Self {
        Self { x_kind: XKind::RowNumber, x_name: String::new(), x_values: Vec::new(), x_labels: Vec::new(), series: Vec::new(), rows: Vec::new(), total_rows: 0, distinct_x: 0, has_other: false, bin_edges: Vec::new(), bins_auto: false, generation, error: Some(error.into()) }
    }
    pub fn points(&self) -> usize {
        self.x_labels.len().max(self.x_values.len())
    }
    /// The X label of a point for a tooltip or axis.
    pub fn x_label(&self, i: usize) -> String {
        if let Some(l) = self.x_labels.get(i) {
            return l.clone();
        }
        match (self.x_kind, self.x_values.get(i)) {
            (XKind::Time, Some(v)) => fmt_time(*v),
            (XKind::RowNumber, Some(v)) => format!("row {}", *v as i64 + 1),
            (_, Some(v)) => fmt_num(*v),
            _ => String::new(),
        }
    }
}

/// Columns that can be a Y series (numbers) and that make a natural X (anything).
pub fn numeric_columns(rs: &ResultSet) -> Vec<usize> {
    rs.columns.iter().enumerate().filter(|(_, c)| is_numeric(&c.sql_type)).map(|(i, _)| i).collect()
}

pub fn is_numeric(t: &SqlType) -> bool {
    matches!(t, SqlType::TinyInt | SqlType::SmallInt | SqlType::Int | SqlType::BigInt | SqlType::Real | SqlType::Float | SqlType::Decimal { .. } | SqlType::Numeric { .. } | SqlType::Money | SqlType::SmallMoney | SqlType::Bit)
}

fn is_time(t: &SqlType) -> bool {
    matches!(t, SqlType::Date | SqlType::Time { .. } | SqlType::DateTime | SqlType::DateTime2 { .. } | SqlType::SmallDateTime | SqlType::DateTimeOffset { .. })
}

/// A default spec for a result set: the first text column (or none) as X, the selected or
/// the first numeric column(s) as Y; count by the first text column when nothing is numeric.
pub fn default_spec(rs: &ResultSet, selected_cols: &[usize]) -> ChartSpec {
    let cols = &rs.columns;
    let numeric = numeric_columns(rs);
    let text_first = cols.iter().position(|c| !is_numeric(&c.sql_type));
    let sel_numeric: Vec<usize> = selected_cols.iter().copied().filter(|c| numeric.contains(c)).collect();
    let sel_x = selected_cols.iter().copied().find(|c| !numeric.contains(c));
    let y: Vec<usize> = if !sel_numeric.is_empty() { sel_numeric } else { numeric.iter().copied().take(1).collect() };
    let x = sel_x.or(text_first);
    let mut spec = ChartSpec { kind: ChartKind::Bar, x: x.map(|i| cols[i].name.clone()), y: y.iter().map(|i| cols[*i].name.clone()).collect(), ..Default::default() };
    if spec.y.is_empty() {
        spec.agg = Agg::Count;
    }
    if x.is_none() && !spec.y.is_empty() {
        spec.kind = ChartKind::Line;
        spec.agg = Agg::None;
    }
    spec
}

fn col_index(rs: &ResultSet, name: &str) -> Option<usize> {
    rs.columns.iter().position(|c| c.name == name).or_else(|| rs.columns.iter().position(|c| c.name.eq_ignore_ascii_case(name)))
}

/// The series values of a column as f64 (NaN for NULL), whatever its numeric Arrow type.
pub(crate) fn as_f64(col: &ArrayRef) -> Option<Float64Array> {
    match col.data_type() {
        DataType::Float64 => col.as_any().downcast_ref::<Float64Array>().cloned(),
        DataType::Boolean | DataType::Int8 | DataType::Int16 | DataType::Int32 | DataType::Int64 | DataType::UInt8 | DataType::UInt16 | DataType::UInt32 | DataType::UInt64 | DataType::Float32 | DataType::Float16 | DataType::Decimal128(_, _) | DataType::Decimal256(_, _) => {
            arrow::compute::cast(col, &DataType::Float64).ok().and_then(|a| a.as_any().downcast_ref::<Float64Array>().cloned())
        }
        _ => None,
    }
}

/// A time-like column as milliseconds (dates at midnight, times of day as ms into the day).
fn as_time_ms(col: &ArrayRef) -> Option<Float64Array> {
    let (as_i64, divisor): (ArrayRef, f64) = match col.data_type() {
        DataType::Date32 | DataType::Date64 | DataType::Timestamp(_, _) => {
            let ts = arrow::compute::cast(col, &DataType::Timestamp(TimeUnit::Millisecond, None)).ok()?;
            (arrow::compute::cast(&ts, &DataType::Int64).ok()?, 1.0)
        }
        DataType::Time32(_) | DataType::Time64(_) => {
            let t = arrow::compute::cast(col, &DataType::Time64(TimeUnit::Microsecond)).ok()?;
            (arrow::compute::cast(&t, &DataType::Int64).ok()?, 1000.0)
        }
        _ => return None,
    };
    let f = arrow::compute::cast(&as_i64, &DataType::Float64).ok()?;
    let f = f.as_any().downcast_ref::<Float64Array>()?;
    if divisor == 1.0 {
        return Some(f.clone());
    }
    Some(Float64Array::from_iter(f.iter().map(|v| v.map(|x| x / divisor))))
}

/// Build the chart data for `spec` over the current view. Never panics on a bad spec: the
/// result carries `error` instead.
pub fn chart_data(rs: &ResultSet, spec: &ChartSpec, fmt: &CellFormatter) -> ChartData {
    let generation = rs.generation();
    let cols = &rs.columns;
    let x_idx = match &spec.x {
        Some(n) => match col_index(rs, n) {
            Some(i) => Some(i),
            None => return ChartData::empty(generation, format!("no column named {n}")),
        },
        None => None,
    };
    let mut y_idx: Vec<usize> = Vec::new();
    for n in &spec.y {
        match col_index(rs, n) {
            Some(i) if is_numeric(&cols[i].sql_type) => y_idx.push(i),
            Some(_) => return ChartData::empty(generation, format!("{n} is not a numeric column")),
            None => return ChartData::empty(generation, format!("no column named {n}")),
        }
    }
    if y_idx.is_empty() && spec.agg != Agg::Count && spec.kind != ChartKind::Histogram {
        return ChartData::empty(generation, "pick at least one numeric column for Y (or count rows)");
    }
    let x_kind = match x_idx {
        None => XKind::RowNumber,
        Some(i) if is_numeric(&cols[i].sql_type) => XKind::Number,
        Some(i) if is_time(&cols[i].sql_type) => XKind::Time,
        Some(_) => XKind::Category,
    };
    // a histogram bins a numeric or time X: the edges come from a first pass over X
    let histogram = spec.kind == ChartKind::Histogram;
    let mut bin_edges: Vec<f64> = Vec::new();
    let mut bins_auto = false;
    let x_time = x_kind == XKind::Time;
    if histogram {
        let Some(xi) = x_idx.filter(|_| matches!(x_kind, XKind::Number | XKind::Time)) else {
            return ChartData::empty(generation, "a histogram needs a numeric or date column for X");
        };
        let mut xs: Vec<f64> = Vec::new();
        for batch in rs.view_batches(65_536) {
            let Ok(batch) = batch else { continue };
            let col = if x_time { as_time_ms(batch.column(xi)) } else { as_f64(batch.column(xi)) };
            if let Some(col) = col {
                xs.extend(col.iter().flatten().filter(|v| v.is_finite()));
            }
        }
        if xs.is_empty() {
            return ChartData::empty(generation, "no values to bin");
        }
        let (bins, auto) = match spec.bins {
            Some(b) => (b.clamp(1, 10_000), false),
            None => (auto_bins(&mut xs), true),
        };
        bins_auto = auto;
        let lo = xs.iter().copied().fold(f64::INFINITY, f64::min);
        let hi = xs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let (lo, hi) = if hi > lo { (lo, hi) } else { (lo - 0.5, lo + 0.5) };
        let width = (hi - lo) / bins as f64;
        bin_edges = (0..=bins).map(|i| lo + width * i as f64).collect();
    }
    let x_kind = if histogram { XKind::Bin } else { x_kind };
    let agg = if (spec.kind == ChartKind::Pie || histogram) && spec.agg == Agg::None { if y_idx.is_empty() { Agg::Count } else { Agg::Sum } } else { spec.agg };
    let n_series = if y_idx.is_empty() { 1 } else { y_idx.len() };
    let series_names: Vec<String> = if y_idx.is_empty() { vec!["count".to_string()] } else { y_idx.iter().map(|i| cols[*i].name.clone()).collect() };
    // accumulators per X: index into the point vectors
    let mut x_order: Vec<String> = Vec::new(); // category labels (Category) in first-seen order
    let mut x_num: Vec<f64> = Vec::new(); // numeric / time / row X per point (unaggregated or aggregated)
    let mut cat_index: HashMap<String, usize> = HashMap::new();
    let mut num_index: HashMap<u64, usize> = HashMap::new(); // f64 bits → point (aggregated numeric X)
    let mut sums: Vec<Vec<f64>> = vec![Vec::new(); n_series];
    let mut counts: Vec<Vec<u64>> = vec![Vec::new(); n_series];
    let mut mins: Vec<Vec<f64>> = vec![Vec::new(); n_series];
    let mut maxs: Vec<Vec<f64>> = vec![Vec::new(); n_series];
    let mut distinct: Vec<Vec<std::collections::HashSet<u64>>> = vec![Vec::new(); n_series];
    let mut rows: Vec<Vec<u32>> = Vec::new();
    let mut total_rows = 0usize;
    let mut visible_row: u32 = 0;
    let want_distinct = agg == Agg::CountDistinct;
    let ensure_point = |p: usize, sums: &mut Vec<Vec<f64>>, counts: &mut Vec<Vec<u64>>, mins: &mut Vec<Vec<f64>>, maxs: &mut Vec<Vec<f64>>, distinct: &mut Vec<Vec<std::collections::HashSet<u64>>>, rows: &mut Vec<Vec<u32>>| {
        while rows.len() <= p {
            rows.push(Vec::new());
            for s in 0..n_series {
                sums[s].push(0.0);
                counts[s].push(0);
                mins[s].push(f64::INFINITY);
                maxs[s].push(f64::NEG_INFINITY);
                if want_distinct {
                    distinct[s].push(Default::default());
                }
            }
        }
    };
    if histogram {
        // every bin is a point, empty ones included, in edge order
        let nb = bin_edges.len().saturating_sub(1);
        if nb > 0 {
            ensure_point(nb - 1, &mut sums, &mut counts, &mut mins, &mut maxs, &mut distinct, &mut rows);
        }
        for i in 0..nb {
            let (a, b) = (bin_edges[i], bin_edges[i + 1]);
            x_order.push(if x_time { format!("{} – {}", fmt_time(a), fmt_time(b)) } else { format!("{} – {}", fmt_num(a), fmt_num(b)) });
        }
    }
    let bin_of = |v: f64| -> Option<usize> {
        let nb = bin_edges.len().saturating_sub(1);
        if nb == 0 || !v.is_finite() {
            return None;
        }
        let lo = bin_edges[0];
        let hi = bin_edges[nb];
        if v < lo || v > hi {
            return None;
        }
        Some((((v - lo) / (hi - lo)) * nb as f64).floor().min(nb as f64 - 1.0) as usize)
    };
    for batch in rs.view_batches(65_536) {
        let Ok(batch) = batch else { continue };
        let n = batch.num_rows();
        let ys: Vec<Option<Float64Array>> = y_idx.iter().map(|i| as_f64(batch.column(*i))).collect();
        let x_num_col: Option<Float64Array> = match (x_kind, x_idx) {
            (XKind::Number, Some(i)) => as_f64(batch.column(i)),
            (XKind::Time, Some(i)) => as_time_ms(batch.column(i)),
            (XKind::Bin, Some(i)) => if x_time { as_time_ms(batch.column(i)) } else { as_f64(batch.column(i)) },
            _ => None,
        };
        let x_text_col: Option<Arc<Vec<Arc<str>>>> = match (x_kind, x_idx) {
            (XKind::Category, Some(i)) => Some(fmt.format_column(batch.column(i), &cols[i])),
            _ => None,
        };
        for r in 0..n {
            total_rows += 1;
            let vr = visible_row;
            visible_row += 1;
            // the point this row belongs to
            let p = match x_kind {
                XKind::Bin => {
                    let v = x_num_col.as_ref().map(|c| if c.is_null(r) { f64::NAN } else { c.value(r) }).unwrap_or(f64::NAN);
                    match bin_of(v) {
                        Some(p) => p,
                        None => continue,
                    }
                }
                XKind::Category => {
                    let label = x_text_col.as_ref().and_then(|c| c.get(r).map(|s| s.to_string())).unwrap_or_default();
                    match cat_index.get(&label) {
                        Some(&p) => p,
                        None => {
                            let p = x_order.len();
                            cat_index.insert(label.clone(), p);
                            x_order.push(label);
                            p
                        }
                    }
                }
                XKind::Number | XKind::Time => {
                    let v = x_num_col.as_ref().map(|c| if c.is_null(r) { f64::NAN } else { c.value(r) }).unwrap_or(f64::NAN);
                    if agg == Agg::None {
                        let p = x_num.len();
                        x_num.push(v);
                        p
                    } else {
                        let key = v.to_bits();
                        match num_index.get(&key) {
                            Some(&p) => p,
                            None => {
                                let p = x_num.len();
                                num_index.insert(key, p);
                                x_num.push(v);
                                p
                            }
                        }
                    }
                }
                XKind::RowNumber => {
                    let p = x_num.len();
                    x_num.push(vr as f64);
                    p
                }
            };
            ensure_point(p, &mut sums, &mut counts, &mut mins, &mut maxs, &mut distinct, &mut rows);
            if rows[p].len() < 1_000_000 {
                rows[p].push(vr);
            }
            if y_idx.is_empty() {
                counts[0][p] += 1;
                continue;
            }
            for (s, y) in ys.iter().enumerate() {
                let Some(y) = y else { continue };
                if y.is_null(r) {
                    continue;
                }
                let v = y.value(r);
                if v.is_nan() {
                    continue;
                }
                sums[s][p] += v;
                counts[s][p] += 1;
                if v < mins[s][p] {
                    mins[s][p] = v;
                }
                if v > maxs[s][p] {
                    maxs[s][p] = v;
                }
                if want_distinct {
                    distinct[s][p].insert(v.to_bits());
                }
            }
        }
    }
    let points = rows.len();
    let distinct_x = points;
    let value_of = |s: usize, p: usize| -> f64 {
        if y_idx.is_empty() {
            return counts[0][p] as f64;
        }
        match agg {
            Agg::None | Agg::Sum => {
                if counts[s][p] == 0 { f64::NAN } else { sums[s][p] }
            }
            Agg::Count => counts[s][p] as f64,
            Agg::Avg => {
                if counts[s][p] == 0 { f64::NAN } else { sums[s][p] / counts[s][p] as f64 }
            }
            Agg::Min => {
                if counts[s][p] == 0 { f64::NAN } else { mins[s][p] }
            }
            Agg::Max => {
                if counts[s][p] == 0 { f64::NAN } else { maxs[s][p] }
            }
            Agg::CountDistinct => distinct[s][p].len() as f64,
        }
    };
    let mut series: Vec<Series> = (0..n_series).map(|s| Series { name: series_names[s].clone(), values: (0..points).map(|p| value_of(s, p)).collect() }).collect();
    let mut x_values = x_num;
    let mut x_labels = x_order;

    // numeric and time X in value order (a category keeps the view's order)
    if matches!(x_kind, XKind::Number | XKind::Time) && agg != Agg::None {
        let mut order: Vec<usize> = (0..points).collect();
        order.sort_by(|a, b| x_values[*a].partial_cmp(&x_values[*b]).unwrap_or(std::cmp::Ordering::Equal));
        x_values = order.iter().map(|i| x_values[*i]).collect();
        rows = order.iter().map(|i| std::mem::take(&mut rows[*i])).collect();
        for s in series.iter_mut() {
            s.values = order.iter().map(|i| s.values[*i]).collect();
        }
    } else if matches!(x_kind, XKind::Number | XKind::Time) && agg == Agg::None && spec.kind != ChartKind::Scatter {
        // a line over raw rows reads best in X order
        let mut order: Vec<usize> = (0..points).collect();
        order.sort_by(|a, b| x_values[*a].partial_cmp(&x_values[*b]).unwrap_or(std::cmp::Ordering::Equal));
        x_values = order.iter().map(|i| x_values[*i]).collect();
        rows = order.iter().map(|i| std::mem::take(&mut rows[*i])).collect();
        for s in series.iter_mut() {
            s.values = order.iter().map(|i| s.values[*i]).collect();
        }
    }
    // top-N on categories: the N largest by the first series, the rest folded into "other"
    let mut has_other = false;
    if let (XKind::Category, Some(n)) = (x_kind, spec.top_n) {
        if n > 0 && points > n {
            let mut order: Vec<usize> = (0..points).collect();
            order.sort_by(|a, b| {
                let va = series[0].values[*a];
                let vb = series[0].values[*b];
                vb.partial_cmp(&va).unwrap_or(std::cmp::Ordering::Equal)
            });
            let keep: Vec<usize> = order[..n].to_vec();
            let rest: Vec<usize> = order[n..].to_vec();
            let foldable = matches!(agg, Agg::Sum | Agg::Count) || y_idx.is_empty();
            let new_labels: Vec<String> = keep.iter().map(|i| x_labels[*i].clone()).chain(foldable.then(|| "other".to_string())).collect();
            let mut new_rows: Vec<Vec<u32>> = keep.iter().map(|i| std::mem::take(&mut rows[*i])).collect();
            if foldable {
                let mut other_rows: Vec<u32> = Vec::new();
                for i in &rest {
                    other_rows.extend(std::mem::take(&mut rows[*i]));
                }
                new_rows.push(other_rows);
                has_other = true;
            }
            for s in series.iter_mut() {
                let mut v: Vec<f64> = keep.iter().map(|i| s.values[*i]).collect();
                if foldable {
                    v.push(rest.iter().map(|i| s.values[*i]).filter(|x| !x.is_nan()).sum());
                }
                s.values = v;
            }
            x_labels = new_labels;
            rows = new_rows;
        }
    }
    let x_name = match x_idx {
        Some(i) => cols[i].name.clone(),
        None => "row".to_string(),
    };
    ChartData { x_kind, x_name, x_values, x_labels, series, rows, total_rows, distinct_x, has_other, bin_edges, bins_auto, generation, error: None }
}

/// Bin count from the data: Freedman–Diaconis (2·IQR·n^-1/3) when the IQR is positive, else
/// Sturges (log2 n + 1); between 5 and 200. Reorders `xs`.
pub fn auto_bins(xs: &mut [f64]) -> usize {
    let n = xs.len();
    if n < 2 {
        return 1;
    }
    let q = |xs: &mut [f64], f: f64| -> f64 {
        let k = ((n - 1) as f64 * f).round() as usize;
        let (_, v, _) = xs.select_nth_unstable_by(k, |a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        *v
    };
    let q1 = q(xs, 0.25);
    let q3 = q(xs, 0.75);
    let lo = xs.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = xs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let iqr = q3 - q1;
    let bins = if iqr > 0.0 && hi > lo {
        let width = 2.0 * iqr / (n as f64).cbrt();
        ((hi - lo) / width).ceil() as usize
    } else {
        ((n as f64).log2() + 1.0).ceil() as usize
    };
    bins.clamp(5, 200)
}

pub fn fmt_num(v: f64) -> String {
    if v.is_nan() {
        return "–".into();
    }
    if v.fract() == 0.0 && v.abs() < 1e15 {
        let n = v as i64;
        let s = n.abs().to_string();
        let mut out = String::new();
        for (i, c) in s.chars().enumerate() {
            if i > 0 && (s.len() - i) % 3 == 0 {
                out.push(',');
            }
            out.push(c);
        }
        return if n < 0 { format!("-{out}") } else { out };
    }
    let s = format!("{v:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

pub fn fmt_time(ms: f64) -> String {
    if ms.is_nan() {
        return "–".into();
    }
    let secs = (ms / 1000.0).floor() as i64;
    let nanos = ((ms - secs as f64 * 1000.0) * 1_000_000.0) as u32;
    match chrono::DateTime::from_timestamp(secs, nanos) {
        Some(dt) => {
            let n = dt.naive_utc();
            if n.format("%H:%M:%S").to_string() == "00:00:00" {
                n.format("%Y-%m-%d").to_string()
            } else {
                n.format("%Y-%m-%d %H:%M:%S").to_string()
            }
        }
        None => fmt_num(ms),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::MemoryBudget;
    use crate::RunState;
    use arrow::array::{Float64Array as F64, Int32Array, RecordBatch, StringArray as Str};
    use cobalt_core::ColumnInfo;

    fn sample() -> Arc<ResultSet> {
        let cols = vec![ColumnInfo::new("region", SqlType::NVarChar { len: Some(20) }, true, 0), ColumnInfo::new("amount", SqlType::Decimal { precision: 18, scale: 2 }, true, 1), ColumnInfo::new("qty", SqlType::Int, false, 2)];
        let rs = ResultSet::new(0, cols, Arc::new(MemoryBudget::unlimited()), std::env::temp_dir());
        // two chunks; amounts: east 10+30+5, west 20+40, north 7; a NULL region
        let regions = [Some("east"), Some("west"), Some("east"), None, Some("north"), Some("west"), Some("east")];
        let amounts = [Some(10.0), Some(20.0), Some(30.0), Some(99.0), Some(7.0), Some(40.0), Some(5.0)];
        let qty = [1, 2, 3, 4, 5, 6, 7];
        for (lo, hi) in [(0, 4), (4, 7)] {
            let b = RecordBatch::try_new(
                rs.schema.clone(),
                vec![
                    Arc::new(Str::from(regions[lo..hi].to_vec())) as ArrayRef,
                    Arc::new(arrow::compute::cast(&(Arc::new(F64::from(amounts[lo..hi].to_vec())) as ArrayRef), rs.schema.field(1).data_type()).unwrap()),
                    Arc::new(Int32Array::from(qty[lo..hi].to_vec())),
                ],
            )
            .unwrap();
            rs.append(b).unwrap();
        }
        rs.set_state(RunState::Complete);
        rs
    }

    #[test]
    fn sums_per_category_in_view_order_with_rows() {
        let rs = sample();
        let fmt = CellFormatter::default();
        let spec = ChartSpec { kind: ChartKind::Bar, x: Some("region".into()), y: vec!["amount".into(), "qty".into()], agg: Agg::Sum, ..Default::default() };
        let d = chart_data(&rs, &spec, &fmt);
        assert!(d.error.is_none(), "{:?}", d.error);
        assert_eq!(d.x_kind, XKind::Category);
        assert_eq!(d.x_labels, vec!["east", "west", "NULL", "north"]);
        assert_eq!(d.series.len(), 2);
        assert_eq!(d.series[0].values, vec![45.0, 60.0, 99.0, 7.0]);
        assert_eq!(d.series[1].values, vec![11.0, 8.0, 4.0, 5.0]);
        assert_eq!(d.rows[0], vec![0, 2, 6]);
        assert_eq!(d.total_rows, 7);
        // count without Y
        let spec = ChartSpec { kind: ChartKind::Bar, x: Some("region".into()), y: vec![], agg: Agg::Count, ..Default::default() };
        let d = chart_data(&rs, &spec, &fmt);
        assert_eq!(d.series[0].name, "count");
        assert_eq!(d.series[0].values, vec![3.0, 2.0, 1.0, 1.0]);
        // average, min, max, count distinct
        for (agg, want) in [(Agg::Avg, 15.0), (Agg::Min, 5.0), (Agg::Max, 30.0), (Agg::CountDistinct, 3.0)] {
            let spec = ChartSpec { kind: ChartKind::Bar, x: Some("region".into()), y: vec!["amount".into()], agg, ..Default::default() };
            assert_eq!(chart_data(&rs, &spec, &fmt).series[0].values[0], want, "{agg:?}");
        }
    }

    #[test]
    fn top_n_folds_the_rest_and_pie_sums() {
        let rs = sample();
        let fmt = CellFormatter::default();
        let spec = ChartSpec { kind: ChartKind::Pie, x: Some("region".into()), y: vec!["amount".into()], agg: Agg::None, top_n: Some(2), ..Default::default() };
        let d = chart_data(&rs, &spec, &fmt);
        assert_eq!(d.x_labels, vec!["NULL", "west", "other"]);
        assert_eq!(d.series[0].values, vec![99.0, 60.0, 52.0]);
        assert!(d.has_other);
        assert_eq!(d.rows[2].len(), 4);
        assert_eq!(d.distinct_x, 4);
    }

    #[test]
    fn raw_rows_and_numeric_x_in_order() {
        let rs = sample();
        let fmt = CellFormatter::default();
        // no X: row numbers, every row
        let spec = ChartSpec { kind: ChartKind::Line, x: None, y: vec!["amount".into()], agg: Agg::None, ..Default::default() };
        let d = chart_data(&rs, &spec, &fmt);
        assert_eq!(d.x_kind, XKind::RowNumber);
        assert_eq!(d.x_values, vec![0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
        assert_eq!(d.series[0].values[3], 99.0);
        assert_eq!(d.x_label(2), "row 3");
        // numeric X aggregated: sorted by X
        let spec = ChartSpec { kind: ChartKind::Line, x: Some("qty".into()), y: vec!["amount".into()], agg: Agg::Sum, ..Default::default() };
        let d = chart_data(&rs, &spec, &fmt);
        assert_eq!(d.x_kind, XKind::Number);
        assert_eq!(d.x_values, vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0]);
        // through a sorted view the rows are visible rows
        rs.apply_view(crate::ViewSpec { filters: vec![], sort: vec![crate::SortKey { column: 1, descending: true }] }).unwrap();
        let spec = ChartSpec { kind: ChartKind::Bar, x: Some("region".into()), y: vec!["amount".into()], agg: Agg::Sum, ..Default::default() };
        let d = chart_data(&rs, &spec, &fmt);
        assert_eq!(d.x_labels, vec!["NULL", "west", "east", "north"]);
        assert_eq!(d.rows[0], vec![0]);
        // errors in words
        let spec = ChartSpec { kind: ChartKind::Bar, x: Some("nope".into()), y: vec!["amount".into()], ..Default::default() };
        assert!(chart_data(&rs, &spec, &fmt).error.unwrap().contains("nope"));
        let spec = ChartSpec { kind: ChartKind::Bar, x: Some("region".into()), y: vec!["region".into()], ..Default::default() };
        assert!(chart_data(&rs, &spec, &fmt).error.unwrap().contains("not a numeric"));
    }

    #[test]
    fn histogram_bins_every_value_with_ranges() {
        let rs = sample();
        let fmt = CellFormatter::default();
        // amounts 5, 7, 10, 20, 30, 40, 99 → 4 bins of 23.5 from 5 to 99
        let spec = ChartSpec { kind: ChartKind::Histogram, x: Some("amount".into()), y: vec![], bins: Some(4), ..Default::default() };
        let d = chart_data(&rs, &spec, &fmt);
        assert!(d.error.is_none(), "{:?}", d.error);
        assert_eq!(d.x_kind, XKind::Bin);
        assert_eq!(d.bin_edges.len(), 5);
        assert_eq!(d.x_labels[0], "5 – 28.5");
        assert_eq!(d.series[0].name, "count");
        assert_eq!(d.series[0].values, vec![4.0, 2.0, 0.0, 1.0]);
        assert_eq!(d.series[0].values.iter().sum::<f64>(), 7.0);
        assert_eq!(d.rows[3], vec![3]);
        assert!(!d.bins_auto);
        // a sum per bin, and the automatic bin count
        let spec = ChartSpec { kind: ChartKind::Histogram, x: Some("amount".into()), y: vec!["qty".into()], bins: Some(2), ..Default::default() };
        let d = chart_data(&rs, &spec, &fmt);
        assert_eq!(d.series[0].values, vec![1.0 + 2.0 + 3.0 + 5.0 + 6.0 + 7.0, 4.0]);
        let spec = ChartSpec { kind: ChartKind::Histogram, x: Some("amount".into()), ..Default::default() };
        let d = chart_data(&rs, &spec, &fmt);
        assert!(d.bins_auto && d.points() >= 5);
        assert_eq!(d.series[0].values.iter().sum::<f64>(), 7.0);
        // not for text
        let spec = ChartSpec { kind: ChartKind::Histogram, x: Some("region".into()), ..Default::default() };
        assert!(chart_data(&rs, &spec, &fmt).error.unwrap().contains("numeric or date"));
        let mut xs: Vec<f64> = (0..1000).map(|i| i as f64).collect();
        let b = auto_bins(&mut xs);
        assert!((5..=200).contains(&b), "{b}");
    }

    #[test]
    fn default_spec_and_formats() {
        let rs = sample();
        let s = default_spec(&rs, &[]);
        assert_eq!(s.x.as_deref(), Some("region"));
        assert_eq!(s.y, vec!["amount"]);
        let s = default_spec(&rs, &[2]);
        assert_eq!(s.y, vec!["qty"]);
        assert_eq!(fmt_num(1234567.0), "1,234,567");
        assert_eq!(fmt_num(-1234.5), "-1234.5");
        assert_eq!(fmt_num(2.0), "2");
        assert_eq!(fmt_time(0.0), "1970-01-01");
        assert_eq!(fmt_time(3_600_000.0), "1970-01-01 01:00:00");
        assert_eq!(ChartKind::parse("Pie"), Some(ChartKind::Pie));
        assert_eq!(Agg::parse("count distinct"), Some(Agg::CountDistinct));
    }
}
