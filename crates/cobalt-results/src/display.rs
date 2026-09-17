use crate::value::{hex, CellValue};
use arrow::array::{Array, ArrayRef, AsArray};
use arrow::datatypes::{DataType, Decimal128Type};
use arrow::util::display::{ArrayFormatter, FormatOptions};
use cobalt_core::{ColumnInfo, ResultsSettings};
use parking_lot::Mutex;
use std::sync::Arc;

/// Settings-aware cell formatting. Cheap to clone; carries a generation so caches invalidate
/// when the user changes NULL text, date format, etc.
#[derive(Clone, Debug)]
pub struct CellFormatter {
    pub null_text: Arc<str>,
    pub bit_as_number: bool,
    pub datetime_format: Arc<str>,
    /// Truncate long text in the grid to this many chars (0 = never). Viewer always gets the full value.
    pub max_chars: usize,
    generation: u64,
}

impl Default for CellFormatter {
    fn default() -> Self {
        Self::from_settings(&ResultsSettings::default())
    }
}

impl CellFormatter {
    pub fn from_settings(s: &ResultsSettings) -> Self {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        use std::hash::{Hash, Hasher};
        s.null_text.hash(&mut h);
        s.bit_as_number.hash(&mut h);
        s.datetime_format.hash(&mut h);
        Self {
            null_text: Arc::from(s.null_text.as_str()),
            bit_as_number: s.bit_as_number,
            datetime_format: Arc::from(s.datetime_format.as_str()),
            max_chars: 4096,
            generation: h.finish(),
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Builder: change the text truncation limit (0 = never truncate). Exporters use 0.
    pub fn with_max_chars(mut self, max_chars: usize) -> Self {
        self.max_chars = max_chars;
        self.bump_generation();
        self
    }

    /// Builder: change the chrono format used for datetime/datetimeoffset columns.
    pub fn with_datetime_format(mut self, format: &str) -> Self {
        self.datetime_format = Arc::from(format);
        self.bump_generation();
        self
    }

    /// Builder: change the text used for NULL cells.
    pub fn with_null_text(mut self, null_text: &str) -> Self {
        self.null_text = Arc::from(null_text);
        self.bump_generation();
        self
    }

    /// Builder: render `bit` as `1`/`0` (true) or `true`/`false` (false).
    pub fn with_bit_as_number(mut self, bit_as_number: bool) -> Self {
        self.bit_as_number = bit_as_number;
        self.bump_generation();
        self
    }

    /// Recompute the cache generation from the current fields so display caches keyed on the
    /// old generation are not reused for a differently configured formatter.
    fn bump_generation(&mut self) {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.null_text.hash(&mut h);
        self.bit_as_number.hash(&mut h);
        self.datetime_format.hash(&mut h);
        self.max_chars.hash(&mut h);
        self.generation = h.finish();
    }

    /// Format every value of one array. This is the hot path; it runs once per (chunk, column)
    /// and the result is cached.
    pub fn format_column(&self, arr: &ArrayRef, col: &ColumnInfo) -> Arc<Vec<Arc<str>>> {
        let n = arr.len();
        let mut out: Vec<Arc<str>> = Vec::with_capacity(n);
        let null = self.null_text.clone();
        match arr.data_type() {
            DataType::Boolean => {
                let a = arr.as_boolean();
                let (t, f): (Arc<str>, Arc<str>) = if self.bit_as_number { (Arc::from("1"), Arc::from("0")) } else { (Arc::from("true"), Arc::from("false")) };
                for i in 0..n {
                    out.push(if a.is_null(i) { null.clone() } else if a.value(i) { t.clone() } else { f.clone() });
                }
            }
            DataType::Binary | DataType::LargeBinary => {
                for i in 0..n {
                    if arr.is_null(i) {
                        out.push(null.clone());
                    } else {
                        let bytes = match arr.data_type() {
                            DataType::Binary => arr.as_binary::<i32>().value(i),
                            _ => arr.as_binary::<i64>().value(i),
                        };
                        let shown = if self.max_chars > 0 && bytes.len() * 2 > self.max_chars { &bytes[..self.max_chars / 2] } else { bytes };
                        let mut s = String::with_capacity(2 + shown.len() * 2);
                        s.push_str("0x");
                        s.push_str(&hex(shown));
                        if shown.len() < bytes.len() {
                            s.push('…');
                        }
                        out.push(Arc::from(s));
                    }
                }
            }
            DataType::Decimal128(_, scale) => {
                let a = arr.as_primitive::<Decimal128Type>();
                for i in 0..n {
                    out.push(if a.is_null(i) { null.clone() } else { Arc::from(CellValue::decimal_string(a.value(i), *scale)) });
                }
            }
            DataType::Utf8 | DataType::LargeUtf8 => {
                for i in 0..n {
                    if arr.is_null(i) {
                        out.push(null.clone());
                    } else {
                        let s = match arr.data_type() {
                            DataType::Utf8 => arr.as_string::<i32>().value(i),
                            _ => arr.as_string::<i64>().value(i),
                        };
                        out.push(self.truncate(s));
                    }
                }
            }
            DataType::Timestamp(_, _) | DataType::Date32 | DataType::Time64(_) => {
                let opts = FormatOptions::default()
                    .with_null(&self.null_text)
                    .with_timestamp_format(Some(&self.datetime_format))
                    .with_timestamp_tz_format(Some(&self.datetime_format))
                    .with_date_format(Some("%Y-%m-%d"))
                    .with_time_format(Some("%H:%M:%S%.f"));
                match ArrayFormatter::try_new(arr.as_ref(), &opts) {
                    Ok(f) => {
                        for i in 0..n {
                            let s = f.value(i).to_string();
                            // Only trim trailing zeros of a *fractional* part; "2024-01-10" and
                            // "12:30:00" must keep their zeros.
                            let trimmed = match s.rfind('.') {
                                Some(dot) if s[dot + 1..].bytes().all(|b| b.is_ascii_digit()) => s.trim_end_matches('0').trim_end_matches('.'),
                                _ => s.as_str(),
                            };
                            out.push(Arc::from(trimmed));
                        }
                    }
                    Err(_) => out.resize(n, Arc::from("?")),
                }
                // note: trimming trailing zeros keeps "13:45:30.1234567" but turns "13:45:30.000" → "13:45:30"
                let _ = &col;
            }
            _ => {
                let opts = FormatOptions::default().with_null(&self.null_text);
                match ArrayFormatter::try_new(arr.as_ref(), &opts) {
                    Ok(f) => {
                        for i in 0..n {
                            out.push(Arc::from(f.value(i).to_string()));
                        }
                    }
                    Err(_) => out.resize(n, Arc::from("?")),
                }
            }
        }
        Arc::new(out)
    }

    fn truncate(&self, s: &str) -> Arc<str> {
        if self.max_chars > 0 && s.len() > self.max_chars {
            let cut = s.char_indices().nth(self.max_chars).map(|(i, _)| i).unwrap_or(s.len());
            Arc::from(format!("{}…", &s[..cut]))
        } else {
            Arc::from(s)
        }
    }
}

/// LRU of formatted columns keyed by (chunk, column, formatter generation).
pub struct DisplayCache {
    cap: usize,
    entries: Mutex<Vec<(usize, usize, u64, Arc<Vec<Arc<str>>>)>>,
}

impl DisplayCache {
    pub fn new(cap: usize) -> Self {
        Self { cap, entries: Mutex::new(Vec::new()) }
    }
    pub fn get(&self, chunk: usize, col: usize, gen: u64, row: usize) -> Option<Arc<str>> {
        let mut e = self.entries.lock();
        let pos = e.iter().position(|(c, k, g, _)| *c == chunk && *k == col && *g == gen)?;
        let item = e.remove(pos);
        let v = item.3.get(row).cloned();
        e.push(item);
        v
    }
    pub fn put(&self, chunk: usize, col: usize, gen: u64, values: Arc<Vec<Arc<str>>>) {
        let mut e = self.entries.lock();
        e.retain(|(c, k, _, _)| !(*c == chunk && *k == col));
        if e.len() >= self.cap {
            e.remove(0);
        }
        e.push((chunk, col, gen, values));
    }
    pub fn clear(&self) {
        self.entries.lock().clear();
    }
}
