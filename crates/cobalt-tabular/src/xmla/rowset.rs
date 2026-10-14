//! The XMLA response body: `soap:Envelope` with a `Session` header, `DiscoverResponse` /
//! `ExecuteResponse` → `return` → one `root` per result set (`xsd:schema` with the `row`
//! complex type, then `row` elements), or a `soap:Fault`. One streaming pass with quick-xml
//! over the body already in memory; element names are `_xHHHH_`-decoded unless `sql:field`
//! names the column. Rows become Arrow batches of [`CHUNK_ROWS`] in the grid's schema, as the
//! same `DaxItem` sequence the Arrow query API yields.

use super::soap::{NS_EMPTY, NS_EXCEPTION};
use crate::decode::{grid_schema, DaxItem};
use crate::TabularError;
use arrow::array::{ArrayRef, BooleanArray, Decimal128Array, Float64Array, Int32Array, Int64Array, LargeBinaryArray, LargeStringArray, RecordBatch, TimestampMicrosecondArray};
use base64::Engine;
use cobalt_core::{ColumnInfo, SqlType};
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use std::collections::HashMap;
use std::sync::Arc;

/// Rows per Arrow batch.
pub const CHUNK_ROWS: usize = 10_000;

/// A `soap:Fault`, or the error elements of an exception `root` when the caller wants one error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fault {
    pub code: Option<String>,
    pub message: String,
}

impl Fault {
    /// The server forgot the session (expired, moved to another node, never existed): the
    /// caller starts a new one and retries once.
    pub fn is_session_lost(&self) -> bool {
        let t = self.message.to_lowercase();
        t.contains("session") && ["not valid", "does not exist", "expired", "moved", "invalid", "not found"].iter().any(|w| t.contains(w))
    }
}

/// What one body held.
#[derive(Debug, Default)]
pub struct Parsed {
    pub session_id: Option<String>,
    /// The result sets, in order, as the grid consumes them; errors inside the body are
    /// `DaxItem::Error`s in their place.
    pub items: Vec<DaxItem>,
    /// Set when the body is a `soap:Fault`.
    pub fault: Option<Fault>,
    /// `root` elements seen (an answer with no rows still has one, in the `empty` namespace).
    pub roots: usize,
}

/// One column of the `row` complex type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColSpec {
    pub name: String,
    /// `xsd:` type without the prefix (`string`, `int`, `long`, `double`, `decimal`, `boolean`,
    /// `dateTime`, `base64Binary`, …).
    pub xsd_type: String,
    pub nullable: bool,
}

/// Decode the `_xHHHH_` escapes SQLXML uses in element names (`Order_x0020_Details` →
/// `Order Details`); UTF-16 surrogate pairs join.
pub fn decode_name(s: &str) -> String {
    if !s.contains("_x") {
        return s.to_string();
    }
    let mut units: Vec<u16> = Vec::with_capacity(s.len());
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'_' && i + 7 <= b.len() && b[i + 1] == b'x' && b[i + 6] == b'_' {
            if let Ok(u) = u16::from_str_radix(&s[i + 2..i + 6], 16) {
                units.push(u);
                i += 7;
                continue;
            }
        }
        // copy one UTF-8 char as UTF-16
        let ch = s[i..].chars().next().unwrap_or('?');
        let mut buf = [0u16; 2];
        units.extend_from_slice(ch.encode_utf16(&mut buf));
        i += ch.len_utf8();
    }
    String::from_utf16_lossy(&units)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RootKind {
    Rowset,
    Empty,
    Exception,
}

#[derive(Default)]
struct RowState {
    depth: usize,
    cells: Vec<Option<String>>,
    /// The column of the open cell element, `None` outside a cell (or inside an unknown one).
    cell: Option<usize>,
    cell_depth: usize,
    nil: bool,
    had_error: bool,
}

struct RootState {
    depth: usize,
    kind: RootKind,
    columns: Vec<ColSpec>,
    index: HashMap<String, usize>,
    cells: Vec<Vec<Option<String>>>,
    rows: u64,
    in_schema: Option<usize>,
    in_row_type: Option<usize>,
    row: Option<RowState>,
    errors: Vec<(String, Option<String>)>,
}

impl RootState {
    fn new(depth: usize, kind: RootKind) -> Self {
        Self { depth, kind, columns: Vec::new(), index: HashMap::new(), cells: Vec::new(), rows: 0, in_schema: None, in_row_type: None, row: None, errors: Vec::new() }
    }
}

struct ErrorState {
    depth: usize,
    description: Option<String>,
    code: Option<String>,
    child: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Capture {
    None,
    Cell,
    FaultCode,
    FaultString,
    ErrorChild,
}

struct FaultState {
    depth: usize,
    code: Option<String>,
    string: Option<String>,
    errors: Vec<(String, Option<String>)>,
}

struct Parser {
    depth: usize,
    row_cap: Option<u64>,
    header: Option<usize>,
    session_id: Option<String>,
    fault: Option<FaultState>,
    error: Option<ErrorState>,
    root: Option<RootState>,
    capture: Capture,
    text: String,
    out: Parsed,
}

fn attr<'a>(e: &'a BytesStart<'a>, local: &str) -> Option<String> {
    e.attributes().flatten().find(|a| a.key.local_name().as_ref() == local).map(|a| quick_xml::escape::unescape(&a.value).map(|v| v.into_owned()).unwrap_or_else(|_| a.value.to_string()))
}

fn is_true(v: Option<String>) -> bool {
    matches!(v.as_deref().map(str::trim), Some("true") | Some("1"))
}

impl Parser {
    fn start(&mut self, e: &BytesStart<'_>, is_empty: bool) {
        let name = e.local_name();
        let name = name.as_ref();
        let depth = self.depth;
        // SOAP header: the Session id
        if self.header.is_some() {
            if name == "Session" {
                if let Some(id) = attr(e, "SessionId") {
                    self.session_id = Some(id);
                }
            }
            return;
        }
        if name == "Header" && depth == 1 {
            self.header = Some(depth);
            return;
        }
        // AS error elements, inside a fault's detail, an exception root, or a row
        if let Some(err) = self.error.as_mut() {
            err.child = Some(name.to_string());
            self.text.clear();
            self.capture = Capture::ErrorChild;
            return;
        }
        if name == "Error" {
            let description = attr(e, "Description");
            let code = attr(e, "ErrorCode");
            if is_empty {
                self.push_error(description, code);
            } else {
                self.error = Some(ErrorState { depth, description, code, child: None });
            }
            if let Some(row) = self.root.as_mut().and_then(|r| r.row.as_mut()) {
                row.had_error = true;
            }
            return;
        }
        if name == "Fault" {
            self.fault = Some(FaultState { depth, code: None, string: None, errors: Vec::new() });
            return;
        }
        if let Some(f) = self.fault.as_ref() {
            if depth == f.depth + 1 {
                match name {
                    "faultcode" => {
                        self.text.clear();
                        self.capture = Capture::FaultCode;
                    }
                    "faultstring" => {
                        self.text.clear();
                        self.capture = Capture::FaultString;
                    }
                    _ => {}
                }
            }
            return;
        }
        if name == "root" && self.root.is_none() {
            let kind = match attr(e, "xmlns").as_deref() {
                Some(NS_EMPTY) => RootKind::Empty,
                Some(NS_EXCEPTION) => RootKind::Exception,
                _ => RootKind::Rowset,
            };
            self.out.roots += 1;
            if is_empty {
                self.finish_root(RootState::new(depth, kind));
            } else {
                self.root = Some(RootState::new(depth, kind));
            }
            return;
        }
        let Some(root) = self.root.as_mut() else { return };
        if let Some(row) = root.row.as_mut() {
            if depth == row.depth + 1 {
                match root.index.get(name) {
                    Some(&i) => {
                        let nil = is_true(attr(e, "nil"));
                        if is_empty {
                            row.cells[i] = if nil { None } else { Some(String::new()) };
                        } else {
                            row.cell = Some(i);
                            row.cell_depth = depth;
                            row.nil = nil;
                            self.text.clear();
                            self.capture = Capture::Cell;
                        }
                    }
                    None => row.cell = None,
                }
            }
            return;
        }
        if root.in_schema.is_some() {
            if root.in_row_type.is_some() {
                if name == "element" {
                    let field = attr(e, "field");
                    let elem = attr(e, "name").unwrap_or_default();
                    let col_name = field.unwrap_or_else(|| decode_name(&elem));
                    let xsd_type = attr(e, "type").map(|t| t.rsplit(':').next().unwrap_or("").to_string()).unwrap_or_default();
                    let nullable = attr(e, "minOccurs").map(|m| m.trim() == "0").unwrap_or(false);
                    root.index.insert(elem, root.columns.len());
                    root.columns.push(ColSpec { name: col_name, xsd_type, nullable });
                }
            } else if name == "complexType" && attr(e, "name").as_deref() == Some("row") && !is_empty {
                root.in_row_type = Some(depth);
            }
            return;
        }
        if name == "schema" && !is_empty {
            root.in_schema = Some(depth);
            return;
        }
        if name == "row" && depth == root.depth + 1 {
            if root.cells.is_empty() {
                root.cells = vec![Vec::new(); root.columns.len()];
            }
            let cells = vec![None; root.columns.len()];
            if is_empty {
                root.push_row(cells, self.row_cap);
            } else {
                root.row = Some(RowState { depth, cells, ..RowState::default() });
            }
        }
    }

    fn end(&mut self) {
        let depth = self.depth;
        if self.header == Some(depth) {
            self.header = None;
            return;
        }
        if self.header.is_some() {
            return;
        }
        if let Some(err) = self.error.as_mut() {
            if depth == err.depth {
                let err = self.error.take().expect("error state");
                self.push_error(err.description, err.code);
            } else if err.child.is_some() && depth == err.depth + 1 {
                let child = err.child.take().unwrap_or_default();
                let text = std::mem::take(&mut self.text);
                self.capture = Capture::None;
                let text = text.trim().to_string();
                if !text.is_empty() {
                    match child.as_str() {
                        "Description" | "Message" => err.description = Some(text),
                        "ErrorCode" => err.code = Some(text),
                        _ => {}
                    }
                }
            }
            return;
        }
        if let Some(f) = self.fault.as_mut() {
            if depth == f.depth {
                let f = self.fault.take().expect("fault state");
                let code = f.errors.iter().find_map(|(_, c)| c.clone()).or(f.code);
                let lines: Vec<String> = f.errors.iter().map(|(m, _)| m.clone()).filter(|m| !m.trim().is_empty()).collect();
                let message = if lines.is_empty() { f.string.unwrap_or_else(|| "the server returned a SOAP fault with no text".into()) } else { lines.join("\n") };
                self.out.fault = Some(Fault { code, message });
            } else if depth == f.depth + 1 {
                match self.capture {
                    Capture::FaultCode => f.code = Some(std::mem::take(&mut self.text).trim().to_string()).filter(|s| !s.is_empty()),
                    Capture::FaultString => f.string = Some(std::mem::take(&mut self.text).trim().to_string()).filter(|s| !s.is_empty()),
                    _ => {}
                }
                self.capture = Capture::None;
            }
            return;
        }
        let Some(root) = self.root.as_mut() else { return };
        if depth == root.depth {
            let root = self.root.take().expect("root state");
            self.finish_root(root);
            return;
        }
        if let Some(row) = root.row.as_mut() {
            if depth == row.depth {
                let row = root.row.take().expect("row state");
                if !row.had_error {
                    root.push_row(row.cells, self.row_cap);
                }
            } else if row.cell.is_some() && depth == row.cell_depth {
                let i = row.cell.take().expect("cell");
                let text = std::mem::take(&mut self.text);
                self.capture = Capture::None;
                row.cells[i] = if row.nil { None } else { Some(text) };
            }
            return;
        }
        if root.in_row_type == Some(depth) {
            root.in_row_type = None;
        } else if root.in_schema == Some(depth) {
            root.in_schema = None;
        }
    }

    fn push_error(&mut self, description: Option<String>, code: Option<String>) {
        let message = description.unwrap_or_default();
        if let Some(root) = self.root.as_mut() {
            root.errors.push((message, code));
        } else if let Some(f) = self.fault.as_mut() {
            f.errors.push((message, code));
        } else {
            self.out.items.push(DaxItem::Error { message: if message.is_empty() { "the server reported an error with no text".into() } else { message }, code });
        }
    }

    fn finish_root(&mut self, root: RootState) {
        let errors = root.errors.clone();
        match root.kind {
            RootKind::Empty => {}
            RootKind::Exception => {
                self.out.items.push(fold_errors(&errors));
                return;
            }
            RootKind::Rowset => {
                if !root.columns.is_empty() {
                    let columns: Vec<ColSpec> = root.columns;
                    let mut cells = root.cells;
                    if cells.is_empty() {
                        cells = vec![Vec::new(); columns.len()];
                    }
                    let (infos, arrays) = build_columns(&columns, cells);
                    self.out.items.push(DaxItem::ResultSetStart { columns: infos.clone() });
                    let schema = grid_schema(&infos);
                    let total = root.rows as usize;
                    let mut offset = 0usize;
                    while offset < total {
                        let len = (total - offset).min(CHUNK_ROWS);
                        let cols: Vec<ArrayRef> = arrays.iter().map(|a| a.slice(offset, len)).collect();
                        match RecordBatch::try_new(schema.clone(), cols) {
                            Ok(b) => self.out.items.push(DaxItem::Batch(b)),
                            Err(e) => {
                                self.out.items.push(DaxItem::Error { message: format!("could not build an Arrow batch from the rowset: {e}"), code: None });
                                break;
                            }
                        }
                        offset += len;
                    }
                    self.out.items.push(DaxItem::ResultSetEnd { rows: root.rows });
                }
            }
        }
        if !errors.is_empty() {
            self.out.items.push(fold_errors(&errors));
        }
    }
}

impl RootState {
    fn push_row(&mut self, cells: Vec<Option<String>>, cap: Option<u64>) {
        if cap.is_some_and(|c| self.rows >= c) {
            return;
        }
        if self.cells.is_empty() {
            self.cells = vec![Vec::new(); self.columns.len()];
        }
        for (col, v) in self.cells.iter_mut().zip(cells) {
            col.push(v);
        }
        self.rows += 1;
    }
}

fn fold_errors(errors: &[(String, Option<String>)]) -> DaxItem {
    let code = errors.iter().find_map(|(_, c)| c.clone());
    let lines: Vec<&str> = errors.iter().map(|(m, _)| m.trim()).filter(|m| !m.is_empty()).collect();
    let message = if lines.is_empty() { "the query failed (no error text in the response)".to_string() } else { lines.join("\n") };
    DaxItem::Error { message, code }
}

/// Parse one response body. `row_cap` keeps at most that many rows per result set (the
/// server sends them all; the count reported is what was kept).
pub fn parse(body: &str, row_cap: Option<u64>) -> Result<Parsed, TabularError> {
    // Power BI's clusters end the body with a NUL byte after </soap:Envelope>
    let body = body.trim_end_matches(|c: char| c == '\0' || c.is_whitespace());
    let mut reader = Reader::from_str(body);
    reader.config_mut().trim_text(false);
    let mut p = Parser { depth: 0, row_cap, header: None, session_id: None, fault: None, error: None, root: None, capture: Capture::None, text: String::new(), out: Parsed::default() };
    loop {
        let ev = reader.read_event().map_err(|e| TabularError::Decode(format!("malformed XML at byte {}: {e}", reader.error_position())))?;
        match ev {
            Event::Start(e) => {
                p.start(&e, false);
                p.depth += 1;
            }
            Event::Empty(e) => p.start(&e, true),
            Event::End(_) => {
                p.depth = p.depth.saturating_sub(1);
                p.end();
            }
            Event::Text(t) => {
                if p.capture != Capture::None {
                    p.text.push_str(&t.xml10_content());
                }
            }
            Event::CData(c) => {
                if p.capture != Capture::None {
                    p.text.push_str(&c.xml10_content());
                }
            }
            Event::GeneralRef(r) => {
                if p.capture != Capture::None {
                    match r.resolve_char_ref() {
                        Ok(Some(c)) => p.text.push(c),
                        _ => match r.as_ref() {
                            "amp" => p.text.push('&'),
                            "lt" => p.text.push('<'),
                            "gt" => p.text.push('>'),
                            "quot" => p.text.push('"'),
                            "apos" => p.text.push('\''),
                            other => {
                                p.text.push('&');
                                p.text.push_str(other);
                                p.text.push(';');
                            }
                        },
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if let Some(root) = p.root.take() {
        // the body ended inside a result set: keep what arrived, then say so
        p.finish_root(root);
        p.out.items.push(DaxItem::Error { message: "the response ended before the end of the result set (connection dropped or the query timed out)".into(), code: None });
    }
    let mut out = p.out;
    out.session_id = p.session_id;
    Ok(out)
}

// ───────────────────────────────── column conversion ─────────────────────────────────

/// The grid type for an `xsd:` type name, before the values are seen.
pub fn sql_type_for_xsd(xsd_type: &str) -> SqlType {
    match xsd_type {
        "int" | "short" | "unsignedShort" | "byte" | "unsignedByte" => SqlType::Int,
        "long" | "unsignedInt" | "integer" | "unsignedLong" => SqlType::BigInt,
        "double" | "float" => SqlType::Float,
        "decimal" => SqlType::Decimal { precision: 19, scale: 4 },
        "boolean" => SqlType::Bit,
        "dateTime" | "date" => SqlType::DateTime2 { scale: 3 },
        "base64Binary" | "hexBinary" => SqlType::VarBinary { len: None },
        _ => SqlType::NVarChar { len: None },
    }
}

/// Every column to an Arrow array of the grid's type; a column whose values do not parse as
/// its declared type falls back to text, so nothing is lost.
fn build_columns(specs: &[ColSpec], cells: Vec<Vec<Option<String>>>) -> (Vec<ColumnInfo>, Vec<ArrayRef>) {
    let mut infos = Vec::with_capacity(specs.len());
    let mut arrays = Vec::with_capacity(specs.len());
    for (i, (spec, values)) in specs.iter().zip(cells).enumerate() {
        let (ty, arr) = convert(spec, values);
        infos.push(ColumnInfo::new(spec.name.clone(), ty, true, i));
        arrays.push(arr);
    }
    (infos, arrays)
}

fn text_array(values: Vec<Option<String>>) -> ArrayRef {
    Arc::new(LargeStringArray::from(values))
}

fn convert(spec: &ColSpec, values: Vec<Option<String>>) -> (SqlType, ArrayRef) {
    let ty = sql_type_for_xsd(&spec.xsd_type);
    let fallback = SqlType::NVarChar { len: None };
    let parsed: Option<ArrayRef> = match ty {
        SqlType::Int => values.iter().map(|v| v.as_deref().map(|s| s.trim().parse::<i32>()).transpose()).collect::<Result<Vec<Option<i32>>, _>>().ok().map(|v| Arc::new(Int32Array::from(v)) as ArrayRef),
        SqlType::BigInt => values.iter().map(|v| v.as_deref().map(|s| s.trim().parse::<i64>()).transpose()).collect::<Result<Vec<Option<i64>>, _>>().ok().map(|v| Arc::new(Int64Array::from(v)) as ArrayRef),
        SqlType::Float => values.iter().map(|v| v.as_deref().map(parse_double).transpose()).collect::<Result<Vec<Option<f64>>, _>>().ok().map(|v| Arc::new(Float64Array::from(v)) as ArrayRef),
        SqlType::Bit => values.iter().map(|v| v.as_deref().map(|s| match s.trim() { "true" | "1" => Ok(true), "false" | "0" => Err(()).or(Ok(false)), _ => Err(()) }).transpose()).collect::<Result<Vec<Option<bool>>, ()>>().ok().map(|v| Arc::new(BooleanArray::from(v)) as ArrayRef),
        SqlType::DateTime2 { .. } => values.iter().map(|v| v.as_deref().map(|s| parse_datetime_micros(s).ok_or(())).transpose()).collect::<Result<Vec<Option<i64>>, ()>>().ok().map(|v| Arc::new(TimestampMicrosecondArray::from(v)) as ArrayRef),
        SqlType::VarBinary { .. } => values.iter().map(|v| v.as_deref().map(|s| base64::engine::general_purpose::STANDARD.decode(s.trim())).transpose()).collect::<Result<Vec<Option<Vec<u8>>>, _>>().ok().map(|v| Arc::new(LargeBinaryArray::from_iter(v)) as ArrayRef),
        SqlType::Decimal { .. } => {
            return match convert_decimal(&values) {
                Some((p, s, arr)) => (SqlType::Decimal { precision: p, scale: s }, arr),
                None => match values.iter().map(|v| v.as_deref().map(parse_double).transpose()).collect::<Result<Vec<Option<f64>>, _>>() {
                    Ok(v) => (SqlType::Float, Arc::new(Float64Array::from(v)) as ArrayRef),
                    Err(_) => (fallback, text_array(values)),
                },
            };
        }
        _ => None,
    };
    match parsed {
        Some(arr) => (ty, arr),
        None => (fallback, text_array(values)),
    }
}

fn parse_double(s: &str) -> Result<f64, ()> {
    match s.trim() {
        "INF" | "+INF" => Ok(f64::INFINITY),
        "-INF" => Ok(f64::NEG_INFINITY),
        "NaN" => Ok(f64::NAN),
        t => t.parse::<f64>().map_err(|_| ()),
    }
}

/// `xsd:decimal` values to a `Decimal128` whose scale is the widest fraction seen (precision at
/// least 19); `None` when a value is not a decimal literal or does not fit 38 digits.
fn convert_decimal(values: &[Option<String>]) -> Option<(u8, u8, ArrayRef)> {
    let mut scale = 0usize;
    let mut int_digits = 1usize;
    let mut parts: Vec<Option<(bool, String, String)>> = Vec::with_capacity(values.len());
    for v in values {
        let Some(s) = v.as_deref().map(str::trim) else {
            parts.push(None);
            continue;
        };
        let (neg, s) = match s.strip_prefix('-') {
            Some(r) => (true, r),
            None => (false, s.strip_prefix('+').unwrap_or(s)),
        };
        let (int, frac) = s.split_once('.').unwrap_or((s, ""));
        let int = int.trim_start_matches('0');
        let frac = frac.trim_end_matches('0');
        if (int.is_empty() && frac.is_empty() && s.is_empty()) || !int.bytes().all(|b| b.is_ascii_digit()) || !frac.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        scale = scale.max(frac.len());
        int_digits = int_digits.max(int.len());
        parts.push(Some((neg, int.to_string(), frac.to_string())));
    }
    if int_digits + scale > 38 || scale > 38 {
        return None;
    }
    let precision = (int_digits + scale).clamp(19, 38);
    let mut out: Vec<Option<i128>> = Vec::with_capacity(parts.len());
    for p in parts {
        match p {
            None => out.push(None),
            Some((neg, int, frac)) => {
                let mut digits = int;
                digits.push_str(&frac);
                for _ in frac.len()..scale {
                    digits.push('0');
                }
                let v: i128 = if digits.is_empty() { 0 } else { digits.parse().ok()? };
                out.push(Some(if neg { -v } else { v }));
            }
        }
    }
    let arr = Decimal128Array::from(out).with_precision_and_scale(precision as u8, scale as i8).ok()?;
    Some((precision as u8, scale as u8, Arc::new(arr)))
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * i64::from(mp) + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// `YYYY-MM-DD[THH:MM[:SS[.fraction]]][Z|±HH:MM]` to microseconds since the epoch (an offset
/// is applied, so the value is UTC); `None` when the text is not a date.
pub fn parse_datetime_micros(s: &str) -> Option<i64> {
    let s = s.trim();
    let (neg, s) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s),
    };
    let year_len = s.bytes().position(|b| b == b'-')?;
    if year_len < 4 {
        return None;
    }
    let year: i64 = s[..year_len].parse().ok()?;
    let year = if neg { -year } else { year };
    let rest = &s[year_len + 1..];
    if rest.len() < 5 || rest.as_bytes()[2] != b'-' {
        return None;
    }
    let month: u32 = rest[..2].parse().ok()?;
    let day: u32 = rest[3..5].parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let mut rest = &rest[5..];
    let (mut h, mut mi, mut sec, mut micros) = (0u32, 0u32, 0u32, 0i64);
    if let Some(t) = rest.strip_prefix('T').or_else(|| rest.strip_prefix(' ')) {
        if t.len() < 5 || t.as_bytes()[2] != b':' {
            return None;
        }
        h = t[..2].parse().ok()?;
        mi = t[3..5].parse().ok()?;
        rest = &t[5..];
        if let Some(r) = rest.strip_prefix(':') {
            if r.len() < 2 {
                return None;
            }
            sec = r[..2].parse().ok()?;
            rest = &r[2..];
            if let Some(f) = rest.strip_prefix('.') {
                let n = f.bytes().take_while(u8::is_ascii_digit).count();
                let frac = &f[..n];
                let mut scaled = 0i64;
                for (i, c) in frac.bytes().enumerate().take(6) {
                    scaled += i64::from(c - b'0') * 10i64.pow(5 - i as u32);
                }
                micros = scaled;
                rest = &f[n..];
            }
        }
        if h > 24 || mi > 59 || sec > 60 {
            return None;
        }
    }
    let mut offset_secs = 0i64;
    match rest {
        "" | "Z" => {}
        r if r.starts_with('+') || r.starts_with('-') => {
            let sign = if r.starts_with('-') { -1 } else { 1 };
            let r = &r[1..];
            if r.len() < 5 || r.as_bytes()[2] != b':' {
                return None;
            }
            let oh: i64 = r[..2].parse().ok()?;
            let om: i64 = r[3..5].parse().ok()?;
            offset_secs = sign * (oh * 3600 + om * 60);
        }
        _ => return None,
    }
    let days = days_from_civil(year, month, day);
    let secs = days * 86_400 + i64::from(h) * 3600 + i64::from(mi) * 60 + i64::from(sec) - offset_secs;
    Some(secs * 1_000_000 + micros)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::collect_items;
    use arrow::array::Array;

    const ENV_OPEN: &str = "<?xml version=\"1.0\"?><soap:Envelope xmlns:soap=\"http://schemas.xmlsoap.org/soap/envelope/\">";

    fn rowset_root(schema_elements: &str, rows: &str) -> String {
        format!(
            "<root xmlns=\"urn:schemas-microsoft-com:xml-analysis:rowset\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xmlns:xsd=\"http://www.w3.org/2001/XMLSchema\">\
<xsd:schema targetNamespace=\"urn:schemas-microsoft-com:xml-analysis:rowset\" xmlns:sql=\"urn:schemas-microsoft-com:xml-sql\" elementFormDefault=\"qualified\">\
<xsd:element name=\"root\"><xsd:complexType><xsd:sequence minOccurs=\"0\" maxOccurs=\"unbounded\"><xsd:element name=\"row\" type=\"row\"/></xsd:sequence></xsd:complexType></xsd:element>\
<xsd:simpleType name=\"uuid\"><xsd:restriction base=\"xsd:string\"><xsd:pattern value=\"[0-9a-zA-Z]{{8}}\"/></xsd:restriction></xsd:simpleType>\
<xsd:complexType name=\"row\"><xsd:choice minOccurs=\"0\" maxOccurs=\"unbounded\">{schema_elements}</xsd:choice></xsd:complexType>\
</xsd:schema>{rows}</root>"
        )
    }

    fn execute_response(roots: &str) -> String {
        format!("{ENV_OPEN}<soap:Body><ExecuteResponse xmlns=\"urn:schemas-microsoft-com:xml-analysis\"><return>{roots}</return></ExecuteResponse></soap:Body></soap:Envelope>")
    }

    const MIXED_SCHEMA: &str = "<xsd:element sql:field=\"Sales[Product]\" name=\"Sales_x005B_Product_x005D_\" type=\"xsd:string\" minOccurs=\"0\"/>\
<xsd:element sql:field=\"Sales[Qty]\" name=\"Sales_x005B_Qty_x005D_\" type=\"xsd:int\" minOccurs=\"0\"/>\
<xsd:element sql:field=\"Sales[Weight]\" name=\"Sales_x005B_Weight_x005D_\" type=\"xsd:double\" minOccurs=\"0\"/>\
<xsd:element sql:field=\"Sales[Amount]\" name=\"Sales_x005B_Amount_x005D_\" type=\"xsd:decimal\" minOccurs=\"0\"/>\
<xsd:element sql:field=\"Sales[When]\" name=\"Sales_x005B_When_x005D_\" type=\"xsd:dateTime\" minOccurs=\"0\"/>\
<xsd:element sql:field=\"Sales[Flag]\" name=\"Sales_x005B_Flag_x005D_\" type=\"xsd:boolean\" minOccurs=\"0\"/>\
<xsd:element name=\"Order_x0020_Count\" type=\"xsd:long\" minOccurs=\"0\"/>";

    const MIXED_ROWS: &str = "<row><Sales_x005B_Product_x005D_>Bike &amp; &lt;Helmet&gt;</Sales_x005B_Product_x005D_><Sales_x005B_Qty_x005D_>3</Sales_x005B_Qty_x005D_><Sales_x005B_Weight_x005D_>1.5</Sales_x005B_Weight_x005D_><Sales_x005B_Amount_x005D_>12.3456</Sales_x005B_Amount_x005D_><Sales_x005B_When_x005D_>2024-03-05T10:20:30</Sales_x005B_When_x005D_><Sales_x005B_Flag_x005D_>true</Sales_x005B_Flag_x005D_><Order_x0020_Count>7</Order_x0020_Count></row>\
<row><Sales_x005B_Product_x005D_ xsi:nil=\"true\"/><Sales_x005B_Weight_x005D_>-INF</Sales_x005B_Weight_x005D_><Sales_x005B_Amount_x005D_>-5</Sales_x005B_Amount_x005D_><Sales_x005B_When_x005D_>2024-03-06</Sales_x005B_When_x005D_><Sales_x005B_Flag_x005D_>false</Sales_x005B_Flag_x005D_></row>\
<row><Sales_x005B_Product_x005D_></Sales_x005B_Product_x005D_><Sales_x005B_Qty_x005D_>-1</Sales_x005B_Qty_x005D_><Sales_x005B_Weight_x005D_>2</Sales_x005B_Weight_x005D_><Sales_x005B_Amount_x005D_>0</Sales_x005B_Amount_x005D_><Sales_x005B_When_x005D_>2024-03-07T00:00:00.1234567Z</Sales_x005B_When_x005D_><Order_x0020_Count>9</Order_x0020_Count></row>";

    #[test]
    fn mixed_rowset() {
        let body = execute_response(&rowset_root(MIXED_SCHEMA, MIXED_ROWS));
        let p = parse(&body, None).unwrap();
        assert!(p.fault.is_none());
        assert_eq!(p.roots, 1);
        let r = collect_items(p.items);
        assert_eq!(r.len(), 1);
        let t = r[0].table().unwrap();
        assert_eq!(t.rows, 3);
        let names: Vec<&str> = t.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["Sales[Product]", "Sales[Qty]", "Sales[Weight]", "Sales[Amount]", "Sales[When]", "Sales[Flag]", "Order Count"]);
        let types: Vec<SqlType> = t.columns.iter().map(|c| c.sql_type.clone()).collect();
        assert_eq!(types, vec![SqlType::NVarChar { len: None }, SqlType::Int, SqlType::Float, SqlType::Decimal { precision: 19, scale: 4 }, SqlType::DateTime2 { scale: 3 }, SqlType::Bit, SqlType::BigInt]);
        assert_eq!(t.batches.len(), 1);
        let b = &t.batches[0];
        assert_eq!(b.schema(), grid_schema(&t.columns));
        assert_eq!(t.text("Sales[Product]", 0).as_deref(), Some("Bike & <Helmet>"));
        assert!(b.column(0).is_null(1), "xsi:nil is null");
        assert!(!b.column(0).is_null(2), "an empty element is an empty string");
        assert_eq!(t.text("Sales[Product]", 2), None);
        assert!(b.column(1).is_null(1), "a missing element is null");
        assert_eq!(t.int("Sales[Qty]", 2), Some(-1));
        let w = b.column(2).as_any().downcast_ref::<Float64Array>().unwrap();
        assert_eq!(w.value(1), f64::NEG_INFINITY);
        let a = b.column(3).as_any().downcast_ref::<Decimal128Array>().unwrap();
        assert_eq!(a.value(0), 123456);
        assert_eq!(a.value(1), -50000);
        let d = b.column(4).as_any().downcast_ref::<TimestampMicrosecondArray>().unwrap();
        assert_eq!(d.value(0), parse_datetime_micros("2024-03-05T10:20:30").unwrap());
        assert_eq!(d.value(1), parse_datetime_micros("2024-03-06T00:00:00").unwrap());
        assert_eq!(d.value(2) % 1_000_000, 123_456);
        assert!(t.flag("Sales[Flag]", 0));
        assert!(!t.flag("Sales[Flag]", 1));
        assert!(b.column(5).is_null(2));
        assert_eq!(t.int("Order Count", 2), Some(9));
    }

    #[test]
    fn two_roots_then_empty_and_chunks() {
        let small = "<xsd:element sql:field=\"[Value]\" name=\"_x005B_Value_x005D_\" type=\"xsd:long\" minOccurs=\"0\"/>";
        let mut rows = String::new();
        for i in 0..CHUNK_ROWS + 5 {
            rows.push_str(&format!("<row><_x005B_Value_x005D_>{i}</_x005B_Value_x005D_></row>"));
        }
        let first = rowset_root(small, &rows);
        let second = rowset_root("<xsd:element name=\"Name\" type=\"xsd:string\" minOccurs=\"0\"/>", "<row><Name>a</Name></row>");
        let empty = "<root xmlns=\"urn:schemas-microsoft-com:xml-analysis:empty\"/>";
        let body = execute_response(&format!("{first}{second}{empty}"));
        let p = parse(&body, None).unwrap();
        assert_eq!(p.roots, 3);
        let kinds: Vec<&str> = p.items.iter().map(|i| match i { DaxItem::ResultSetStart { .. } => "start", DaxItem::Batch(_) => "batch", DaxItem::ResultSetEnd { .. } => "end", DaxItem::Error { .. } => "error" }).collect();
        assert_eq!(kinds, vec!["start", "batch", "batch", "end", "start", "batch", "end"]);
        let r = collect_items(p.items);
        assert_eq!(r[0].table().unwrap().rows, (CHUNK_ROWS + 5) as u64);
        assert_eq!(r[0].table().unwrap().batches[0].num_rows(), CHUNK_ROWS);
        assert_eq!(r[0].table().unwrap().int("[Value]", CHUNK_ROWS + 4), Some((CHUNK_ROWS + 4) as i64));
        assert_eq!(r[1].table().unwrap().text("Name", 0).as_deref(), Some("a"));
        // the row cap keeps the first n
        let p = parse(&body, Some(3)).unwrap();
        let r = collect_items(p.items);
        assert_eq!(r[0].table().unwrap().rows, 3);
        assert_eq!(r[1].table().unwrap().rows, 1);
    }

    #[test]
    fn empty_rowset_has_columns_and_no_rows() {
        let body = execute_response(&rowset_root("<xsd:element name=\"Name\" type=\"xsd:string\" minOccurs=\"0\"/>", ""));
        let r = collect_items(parse(&body, None).unwrap().items);
        assert_eq!(r.len(), 1);
        let t = r[0].table().unwrap();
        assert_eq!(t.rows, 0);
        assert_eq!(t.columns.len(), 1);
        assert!(t.batches.is_empty());
    }

    #[test]
    fn soap_fault_with_as_error() {
        let body = format!("{ENV_OPEN}<soap:Body><soap:Fault><faultcode>XMLAnalysisError.0xc10e0002</faultcode><faultstring>Query (1, 1) The syntax for &apos;EVALUAT&apos; is incorrect.</faultstring><detail><Error ErrorCode=\"3238658050\" Description=\"Query (1, 1) The syntax for 'EVALUAT' is incorrect. (DAX(EVALUAT {{1}}))\" Source=\"Microsoft Analysis Services\" HelpFile=\"\"/></detail></soap:Fault></soap:Body></soap:Envelope>");
        let p = parse(&body, None).unwrap();
        let f = p.fault.unwrap();
        assert_eq!(f.code.as_deref(), Some("3238658050"));
        assert!(f.message.starts_with("Query (1, 1) The syntax for 'EVALUAT' is incorrect. (DAX("));
        assert!(!f.is_session_lost());
        assert!(p.items.is_empty());
        // without a detail: faultstring and faultcode
        let body = format!("{ENV_OPEN}<soap:Body><soap:Fault><faultcode>soap:Server</faultcode><faultstring>The session with ID &apos;abc&apos; does not exist.</faultstring></soap:Fault></soap:Body></soap:Envelope>");
        let f = parse(&body, None).unwrap().fault.unwrap();
        assert_eq!(f.code.as_deref(), Some("soap:Server"));
        assert_eq!(f.message, "The session with ID 'abc' does not exist.");
        assert!(f.is_session_lost());
        // child-element form
        let body = format!("{ENV_OPEN}<soap:Body><soap:Fault><faultstring>x</faultstring><detail><Error><ErrorCode>7</ErrorCode><Description>Session expired</Description></Error></detail></soap:Fault></soap:Body></soap:Envelope>");
        let f = parse(&body, None).unwrap().fault.unwrap();
        assert_eq!(f, Fault { code: Some("7".into()), message: "Session expired".into() });
        assert!(f.is_session_lost());
    }

    #[test]
    fn paas_session_answer_with_trailing_nul_and_big_fault() {
        // the BeginSession answer: Session header, empty root, a NUL after the envelope
        let body = format!("{ENV_OPEN}<soap:Header><Session xmlns=\"urn:schemas-microsoft-com:xml-analysis\" SessionId=\"D4F84F15-7C2B-4A1E-9E0D-123456789ABC\"/></soap:Header><soap:Body><ExecuteResponse xmlns=\"urn:schemas-microsoft-com:xml-analysis\"><return><root xmlns=\"urn:schemas-microsoft-com:xml-analysis:empty\"/></return></ExecuteResponse></soap:Body></soap:Envelope>\u{0}\n");
        let p = parse(&body, None).unwrap();
        assert_eq!(p.session_id.as_deref(), Some("D4F84F15-7C2B-4A1E-9E0D-123456789ABC"));
        assert_eq!(p.roots, 1);
        assert!(p.items.is_empty() && p.fault.is_none());
        // a fault the way the engine writes it: prefixed names, namespaces on every element, a
        // Location block under Error, a huge Description, and the probe's query text inside
        let long = "x".repeat(50_000);
        let body = format!(
            "{ENV_OPEN}<soap:Body><soap:Fault xmlns:soap=\"http://schemas.xmlsoap.org/soap/envelope/\"><faultcode>XMLAnalysisError.0xc10e0002</faultcode><faultstring>Query (3, 1) The syntax for &apos;EVALUAT&apos; is incorrect.</faultstring><detail><Error ErrorCode=\"3238920194\" Description=\"Query (3, 1) The syntax for 'EVALUAT' is incorrect. (DAX(// probe\nEVALUAT {{1}} {long}))\" Source=\"Microsoft Analysis Services\" HelpFile=\"\" xmlns=\"urn:schemas-microsoft-com:xml-analysis\"><Location xmlns=\"http://schemas.microsoft.com/analysisservices/2003/engine/2/2\"><Start><Line>3</Line><Column>1</Column></Start><End><Line>3</Line><Column>7</Column></End><LineOffset>0</LineOffset><TextLength>7</TextLength></Location></Error></detail></soap:Fault></soap:Body></soap:Envelope>\u{0}"
        );
        let p = parse(&body, None).unwrap();
        let f = p.fault.unwrap();
        assert_eq!(f.code.as_deref(), Some("3238920194"));
        assert!(f.message.starts_with("Query (3, 1) The syntax for 'EVALUAT' is incorrect. (DAX(// probe\nEVALUAT {1} xxxx"), "{}", &f.message[..80]);
        assert!(f.message.len() > 50_000);
        assert!(!f.is_session_lost());
    }

    #[test]
    fn exception_root_and_row_error() {
        let exc = "<root xmlns=\"urn:schemas-microsoft-com:xml-analysis:exception\"><Exception/><Messages><Error ErrorCode=\"3238002695\" Description=\"Internal error: An unexpected exception occurred.\" Source=\"Microsoft Analysis Services\" HelpFile=\"\"/></Messages></root>";
        let body = execute_response(exc);
        let p = parse(&body, None).unwrap();
        assert!(p.fault.is_none());
        assert_eq!(p.items.len(), 1);
        assert!(matches!(&p.items[0], DaxItem::Error { message, code } if message == "Internal error: An unexpected exception occurred." && code.as_deref() == Some("3238002695")));
        // a rowset with an error element inside a row: the good rows stay, the error follows
        let rows = "<row><Name>a</Name></row><row><Error ErrorCode=\"1\" Description=\"row 2 broke\"/></row><row><Name>c</Name></row>";
        let body = execute_response(&rowset_root("<xsd:element name=\"Name\" type=\"xsd:string\" minOccurs=\"0\"/>", rows));
        let r = collect_items(parse(&body, None).unwrap().items);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].table().unwrap().rows, 2);
        assert_eq!(r[1].error().unwrap().to_string(), "row 2 broke (1)");
    }

    #[test]
    fn session_header_and_discover_catalogs() {
        let body = format!(
            "{ENV_OPEN}<soap:Header><Session xmlns=\"urn:schemas-microsoft-com:xml-analysis\" SessionId=\"7D4E7B0E-1C9A-4F3B-9C2D-0A1B2C3D4E5F\"/></soap:Header><soap:Body><DiscoverResponse xmlns=\"urn:schemas-microsoft-com:xml-analysis\"><return>{}</return></DiscoverResponse></soap:Body></soap:Envelope>",
            rowset_root(
                "<xsd:element sql:field=\"CATALOG_NAME\" name=\"CATALOG_NAME\" type=\"xsd:string\"/><xsd:element sql:field=\"DESCRIPTION\" name=\"DESCRIPTION\" type=\"xsd:string\" minOccurs=\"0\"/>",
                "<row><CATALOG_NAME>Sales</CATALOG_NAME><DESCRIPTION/></row><row><CATALOG_NAME>HR</CATALOG_NAME></row>"
            )
        );
        let p = parse(&body, None).unwrap();
        assert_eq!(p.session_id.as_deref(), Some("7D4E7B0E-1C9A-4F3B-9C2D-0A1B2C3D4E5F"));
        let r = collect_items(p.items);
        let t = r[0].table().unwrap();
        assert_eq!(t.rows, 2);
        assert!(t.columns.iter().all(|c| c.nullable), "every grid column is nullable");
        assert_eq!(t.text("CATALOG_NAME", 1).as_deref(), Some("HR"));
        assert_eq!(t.text("DESCRIPTION", 0), None);
    }

    #[test]
    fn bad_values_fall_back_to_text_and_decimals_widen() {
        let schema = "<xsd:element name=\"N\" type=\"xsd:int\" minOccurs=\"0\"/><xsd:element name=\"D\" type=\"xsd:decimal\" minOccurs=\"0\"/><xsd:element name=\"B\" type=\"xsd:base64Binary\" minOccurs=\"0\"/><xsd:element name=\"T\" type=\"xsd:time\" minOccurs=\"0\"/>";
        let rows = "<row><N>12</N><D>1234567890123456.123456</D><B>AQID</B><T>10:00:00</T></row><row><N>not a number</N><D>2</D></row>";
        let body = execute_response(&rowset_root(schema, rows));
        let r = collect_items(parse(&body, None).unwrap().items);
        let t = r[0].table().unwrap();
        assert_eq!(t.columns[0].sql_type, SqlType::NVarChar { len: None });
        assert_eq!(t.text("N", 1).as_deref(), Some("not a number"));
        assert_eq!(t.columns[1].sql_type, SqlType::Decimal { precision: 22, scale: 6 });
        assert_eq!(t.columns[2].sql_type, SqlType::VarBinary { len: None });
        let bin = t.batches[0].column(2).as_any().downcast_ref::<LargeBinaryArray>().unwrap();
        assert_eq!(bin.value(0), &[1u8, 2, 3]);
        assert_eq!(t.columns[3].sql_type, SqlType::NVarChar { len: None });
        // too wide for decimal128: double
        let body = execute_response(&rowset_root("<xsd:element name=\"D\" type=\"xsd:decimal\" minOccurs=\"0\"/>", "<row><D>1234567890123456789012345678901234567890.5</D></row>"));
        let r = collect_items(parse(&body, None).unwrap().items);
        assert_eq!(r[0].table().unwrap().columns[0].sql_type, SqlType::Float);
    }

    #[test]
    fn truncated_body_keeps_rows_and_reports() {
        let body = execute_response(&rowset_root("<xsd:element name=\"Name\" type=\"xsd:string\" minOccurs=\"0\"/>", "<row><Name>a</Name></row><row><Name>b</Name>"));
        // quick-xml reports an unclosed tag as EOF inside; our parser then flags the open set
        match parse(&body, None) {
            Ok(p) => {
                let r = collect_items(p.items);
                assert!(r.iter().any(|x| x.error().is_some()));
            }
            Err(TabularError::Decode(_)) => {}
            Err(e) => panic!("{e}"),
        }
    }

    #[test]
    fn name_decoding() {
        assert_eq!(decode_name("Order_x0020_Details"), "Order Details");
        assert_eq!(decode_name("Sales_x005B_Amount_x005D_"), "Sales[Amount]");
        assert_eq!(decode_name("plain"), "plain");
        assert_eq!(decode_name("_x0041_"), "A");
        assert_eq!(decode_name("a_xZZZZ_b"), "a_xZZZZ_b");
        assert_eq!(decode_name("_xD83D__xDE00_"), "\u{1F600}");
        assert_eq!(decode_name("ünï_x0020_x"), "ünï x");
    }

    #[test]
    fn datetimes() {
        assert_eq!(parse_datetime_micros("1970-01-01T00:00:00"), Some(0));
        assert_eq!(parse_datetime_micros("1970-01-02"), Some(86_400_000_000));
        assert_eq!(parse_datetime_micros("1969-12-31T23:59:59"), Some(-1_000_000));
        assert_eq!(parse_datetime_micros("2000-03-01T00:00:00Z"), Some(951_868_800_000_000));
        assert_eq!(parse_datetime_micros("2000-03-01T01:00:00+01:00"), Some(951_868_800_000_000));
        assert_eq!(parse_datetime_micros("2024-02-29T12:30:45.5"), Some(parse_datetime_micros("2024-02-29T12:30:45").unwrap() + 500_000));
        assert_eq!(parse_datetime_micros("0001-01-01T00:00:00"), Some(-62_135_596_800_000_000));
        assert_eq!(parse_datetime_micros("2024-13-01"), None);
        assert_eq!(parse_datetime_micros("yesterday"), None);
    }

    #[test]
    fn xsd_types() {
        assert_eq!(sql_type_for_xsd("string"), SqlType::NVarChar { len: None });
        assert_eq!(sql_type_for_xsd("short"), SqlType::Int);
        assert_eq!(sql_type_for_xsd("unsignedInt"), SqlType::BigInt);
        assert_eq!(sql_type_for_xsd("float"), SqlType::Float);
        assert_eq!(sql_type_for_xsd("date"), SqlType::DateTime2 { scale: 3 });
        assert_eq!(sql_type_for_xsd("anyType"), SqlType::NVarChar { len: None });
    }
}
