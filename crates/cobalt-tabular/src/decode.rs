//! The Arrow query API's response body, decoded as bytes arrive. The body is one or more
//! concatenated IPC streams (schema, LZ4-framed batches, end-of-stream marker, repeat), so the
//! framing (continuation marker, metadata length, body length, the zero-length end marker) is
//! parsed here and each stream gets its own `StreamDecoder`. Every batch leaves cast to the
//! schema `cobalt_results::ResultSet` derives from the `ColumnInfo`s, so the grid appends it
//! unchanged. A stream whose schema metadata says `IsError` is folded into one `DaxItem::Error`.

use crate::TabularError;
use arrow::array::{Array, ArrayRef, BooleanArray, Int64Array, LargeStringArray, RecordBatch, StringArray};
use arrow::buffer::Buffer;
use arrow::datatypes::{DataType, Field, Schema, SchemaRef};
use arrow::util::display::{ArrayFormatter, FormatOptions};
use arrow_ipc::reader::StreamDecoder;
use bytes::{Buf, Bytes, BytesMut};
use cobalt_core::{ColumnInfo, SqlType};
use std::collections::VecDeque;
use std::sync::Arc;

const CONTINUATION: [u8; 4] = [0xff; 4];

/// What a DAX run yields, in order: one `ResultSetStart` per `EVALUATE`, its batches, its
/// `ResultSetEnd`; an engine error replaces the set it would have been.
#[derive(Clone, Debug)]
pub enum DaxItem {
    ResultSetStart { columns: Vec<ColumnInfo> },
    /// Already cast: `batch.schema()` equals [`grid_schema`] of the set's columns.
    Batch(RecordBatch),
    ResultSetEnd { rows: u64 },
    Error { message: String, code: Option<String> },
}

/// A whole result set held in memory (catalog loads, probes, the dev CLI).
#[derive(Clone, Debug)]
pub struct DaxTable {
    pub columns: Vec<ColumnInfo>,
    pub batches: Vec<RecordBatch>,
    pub rows: u64,
}

/// One `EVALUATE`'s outcome once collected.
#[derive(Clone, Debug)]
pub enum DaxResult {
    Table(DaxTable),
    Error { message: String, code: Option<String> },
}

impl DaxResult {
    pub fn table(&self) -> Option<&DaxTable> {
        match self {
            DaxResult::Table(t) => Some(t),
            DaxResult::Error { .. } => None,
        }
    }

    /// The error rowset as a `TabularError::Query`, if this is one.
    pub fn error(&self) -> Option<TabularError> {
        match self {
            DaxResult::Error { message, code } => Some(TabularError::Query { code: code.clone(), message: message.clone() }),
            DaxResult::Table(_) => None,
        }
    }
}

fn strip_brackets(s: &str) -> &str {
    s.trim_matches(|c| c == '[' || c == ']')
}

impl DaxTable {
    /// Wrap foreign batches (same schema each) the way the decoder would: columns from the
    /// Arrow fields, every batch cast to the grid schema.
    pub fn from_batches(batches: Vec<RecordBatch>) -> Result<Self, TabularError> {
        let Some(first) = batches.first() else {
            return Ok(Self { columns: Vec::new(), batches: Vec::new(), rows: 0 });
        };
        let columns = columns_for(&first.schema());
        let schema = grid_schema(&columns);
        let batches: Vec<RecordBatch> = batches.iter().map(|b| cast_batch(b, &schema)).collect::<Result<_, _>>()?;
        let rows = batches.iter().map(|b| b.num_rows() as u64).sum();
        Ok(Self { columns, batches, rows })
    }

    /// Column index by name, case-insensitively; `[Name]` and `Name` both match.
    pub fn col(&self, name: &str) -> Option<usize> {
        let want = strip_brackets(name);
        self.columns.iter().position(|c| strip_brackets(&c.name).eq_ignore_ascii_case(want))
    }

    pub fn num_rows(&self) -> usize {
        self.rows as usize
    }

    fn locate(&self, row: usize) -> Option<(&RecordBatch, usize)> {
        let mut r = row;
        for b in &self.batches {
            if r < b.num_rows() {
                return Some((b, r));
            }
            r -= b.num_rows();
        }
        None
    }

    fn cell(&self, col: &str, row: usize) -> Option<(&ArrayRef, usize)> {
        let c = self.col(col)?;
        let (b, r) = self.locate(row)?;
        let arr = b.column(c);
        if arr.is_null(r) {
            None
        } else {
            Some((arr, r))
        }
    }

    /// The cell as text (nulls and blanks are `None`).
    pub fn text(&self, col: &str, row: usize) -> Option<String> {
        let (arr, r) = self.cell(col, row)?;
        let s = match arr.data_type() {
            DataType::LargeUtf8 => arr.as_any().downcast_ref::<LargeStringArray>().map(|a| a.value(r).to_string()),
            DataType::Utf8 => arr.as_any().downcast_ref::<StringArray>().map(|a| a.value(r).to_string()),
            _ => ArrayFormatter::try_new(arr.as_ref(), &FormatOptions::default()).ok().map(|f| f.value(r).to_string()),
        }?;
        if s.trim().is_empty() {
            None
        } else {
            Some(s)
        }
    }

    /// The cell as a flag: Arrow booleans, or the texts `true` / `1`.
    pub fn flag(&self, col: &str, row: usize) -> bool {
        let Some((arr, r)) = self.cell(col, row) else { return false };
        match arr.as_any().downcast_ref::<BooleanArray>() {
            Some(a) => a.value(r),
            None => matches!(self.text(col, row).map(|s| s.to_ascii_lowercase()).as_deref(), Some("true") | Some("1")),
        }
    }

    /// The cell as an integer (Arrow int64, or any text that parses).
    pub fn int(&self, col: &str, row: usize) -> Option<i64> {
        let (arr, r) = self.cell(col, row)?;
        match arr.as_any().downcast_ref::<Int64Array>() {
            Some(a) => Some(a.value(r)),
            None => self.text(col, row)?.trim().parse().ok(),
        }
    }
}

/// The `SqlType` the grid shows for an Arrow field of the API's response. Dictionary columns
/// map by their value type (the cast unpacks them); anything with no grid type becomes text.
pub fn sql_type_for(dt: &DataType) -> SqlType {
    use DataType as D;
    let preferred = match dt {
        D::Boolean => SqlType::Bit,
        D::Int8 | D::Int16 => SqlType::SmallInt,
        D::UInt8 => SqlType::TinyInt,
        D::Int32 | D::UInt16 => SqlType::Int,
        D::Int64 | D::UInt32 => SqlType::BigInt,
        D::UInt64 => SqlType::Decimal { precision: 20, scale: 0 },
        D::Float16 | D::Float32 | D::Float64 => SqlType::Float,
        D::Decimal128(p, s) if *s >= 0 => SqlType::Decimal { precision: *p, scale: *s as u8 },
        D::Date32 => SqlType::Date,
        // the API encodes DateTime as date64 milliseconds
        D::Date64 | D::Timestamp(..) => SqlType::DateTime2 { scale: 3 },
        D::Time32(_) | D::Time64(_) => SqlType::Time { scale: 7 },
        D::Binary | D::LargeBinary | D::FixedSizeBinary(_) | D::BinaryView => SqlType::VarBinary { len: None },
        D::Dictionary(_, value) => sql_type_for(value),
        // utf8, large_utf8, utf8_view, null, union (Variant), lists, structs…
        _ => SqlType::NVarChar { len: None },
    };
    let castable = matches!(preferred, SqlType::NVarChar { .. }) || arrow::compute::can_cast_types(dt, &preferred.arrow_type());
    if castable {
        preferred
    } else {
        SqlType::NVarChar { len: None }
    }
}

/// `ColumnInfo`s for a response schema: the field names as the engine sent them
/// (`Table[Column]` or `[Name]`), every column nullable.
pub fn columns_for(schema: &Schema) -> Vec<ColumnInfo> {
    schema.fields().iter().enumerate().map(|(i, f)| ColumnInfo::new(f.name().clone(), sql_type_for(f.data_type()), true, i)).collect()
}

/// The schema `cobalt_results::ResultSet::new` derives from these columns (same names, same
/// duplicate handling, every field nullable, no metadata).
pub fn grid_schema(columns: &[ColumnInfo]) -> SchemaRef {
    let fields: Vec<Field> = columns.iter().map(|c| Field::new(dedupe_name(columns, c), c.sql_type.arrow_type(), true)).collect();
    Arc::new(Schema::new(fields))
}

/// Mirrors `cobalt_results::dedupe_name`: a second `x` becomes `x_2`, a blank name
/// `(No column name N)`.
fn dedupe_name(columns: &[ColumnInfo], c: &ColumnInfo) -> String {
    let base = if c.name.is_empty() { format!("(No column name {})", c.ordinal + 1) } else { c.name.clone() };
    let dupes_before = columns[..c.ordinal].iter().filter(|o| o.name == c.name).count();
    if dupes_before == 0 {
        base
    } else {
        format!("{base}_{}", dupes_before + 1)
    }
}

/// One column to the grid's Arrow type: identity, else `arrow::compute::cast` (dictionaries
/// unpack, date64 → timestamp, utf8 → large_utf8), else formatted as text with nulls kept.
pub fn cast_column(col: &ArrayRef, target: &DataType) -> Result<ArrayRef, TabularError> {
    if col.data_type() == target {
        return Ok(col.clone());
    }
    // arrow's union cast extracts one child and nulls the rest; a Variant column wants every value
    let is_union = matches!(col.data_type(), DataType::Union(..));
    if !is_union && arrow::compute::can_cast_types(col.data_type(), target) {
        if let Ok(out) = arrow::compute::cast(col, target) {
            if out.data_type() == target {
                return Ok(out);
            }
        }
    }
    match target {
        DataType::Utf8 | DataType::LargeUtf8 => format_as_text(col, target),
        _ => Err(TabularError::Decode(format!("cannot convert an Arrow {} column to {}", col.data_type(), target))),
    }
}

fn format_as_text(col: &ArrayRef, target: &DataType) -> Result<ArrayRef, TabularError> {
    let fmt = ArrayFormatter::try_new(col.as_ref(), &FormatOptions::default().with_null("")).map_err(|e| TabularError::Decode(e.to_string()))?;
    let values: Vec<Option<String>> = (0..col.len()).map(|i| if col.is_null(i) { None } else { Some(fmt.value(i).to_string()) }).collect();
    Ok(if matches!(target, DataType::Utf8) { Arc::new(StringArray::from(values)) } else { Arc::new(LargeStringArray::from(values)) })
}

/// Project and cast column by column so the batch's schema is exactly `schema`.
pub fn cast_batch(batch: &RecordBatch, schema: &SchemaRef) -> Result<RecordBatch, TabularError> {
    if batch.num_columns() != schema.fields().len() {
        return Err(TabularError::Decode(format!("batch has {} columns, the result set {}", batch.num_columns(), schema.fields().len())));
    }
    let cols: Vec<ArrayRef> = batch.columns().iter().zip(schema.fields()).map(|(c, f)| cast_column(c, f.data_type())).collect::<Result<_, _>>()?;
    RecordBatch::try_new(schema.clone(), cols).map_err(|e| TabularError::Decode(e.to_string()))
}

enum OpenSet {
    Data { schema: SchemaRef, rows: u64 },
    Error { fault_code: Option<String>, fault_string: Option<String>, code: Option<String>, lines: Vec<String> },
}

enum Frame {
    /// One complete IPC message; `schema` is set when it is the stream's schema message.
    Message { bytes: Bytes, schema: Option<Schema> },
    End,
}

/// Push bytes in, pull `DaxItem`s out. Owns the unconsumed tail between chunks.
#[derive(Default)]
pub struct Decoder {
    buf: BytesMut,
    inner: StreamDecoder,
    set: Option<OpenSet>,
    queue: VecDeque<DaxItem>,
    pending_error: Option<TabularError>,
}

impl Decoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed the next chunk of the body; complete messages are decoded at once.
    pub fn push(&mut self, chunk: &[u8]) -> Result<(), TabularError> {
        self.buf.extend_from_slice(chunk);
        while let Some(frame) = self.next_frame()? {
            match frame {
                Frame::Message { bytes, schema } => self.feed(bytes, schema)?,
                Frame::End => self.close_set(),
            }
        }
        Ok(())
    }

    /// The body is over: close an unterminated set (its rows are good; the missing marker
    /// becomes an error after them) and flag trailing bytes that are not padding.
    pub fn finish(&mut self) {
        let open = self.set.is_some();
        self.close_set();
        let leftover = self.buf.iter().any(|b| *b != 0);
        if open {
            self.pending_error = Some(TabularError::Decode("the response ended before the end of the result set (connection dropped or the query timed out)".into()));
        } else if leftover {
            self.pending_error = Some(TabularError::Decode(format!("{} unexpected bytes after the last result set", self.buf.len())));
        }
        self.buf.clear();
    }

    /// The next decoded item, if any is ready.
    pub fn pop(&mut self) -> Option<DaxItem> {
        self.queue.pop_front()
    }

    /// The error `finish` found, once the queued items are drained.
    pub fn take_error(&mut self) -> Option<TabularError> {
        self.pending_error.take()
    }

    fn next_frame(&mut self) -> Result<Option<Frame>, TabularError> {
        let b = &self.buf[..];
        if b.len() < 4 {
            return Ok(None);
        }
        let prefix = if b[..4] == CONTINUATION { 4 } else { 0 };
        if b.len() < prefix + 4 {
            return Ok(None);
        }
        let len = u32::from_le_bytes([b[prefix], b[prefix + 1], b[prefix + 2], b[prefix + 3]]) as usize;
        if len == 0 {
            self.buf.advance(prefix + 4);
            return Ok(Some(Frame::End));
        }
        let meta_end = prefix + 4 + len;
        if b.len() < meta_end {
            return Ok(None);
        }
        let message = arrow_ipc::root_as_message(&b[prefix + 4..meta_end]).map_err(|e| TabularError::Decode(format!("bad Arrow message header: {e}")))?;
        let body = usize::try_from(message.bodyLength()).map_err(|_| TabularError::Decode("negative Arrow body length".into()))?;
        let total = meta_end + body;
        if b.len() < total {
            return Ok(None);
        }
        // read the schema here: the decoder only acts on a body-less message once more bytes
        // follow it, and a schema-only stream (an empty error rowset, `schemaOnly`) has none
        let schema = message.header_as_schema().map(arrow_ipc::convert::fb_to_schema);
        Ok(Some(Frame::Message { bytes: self.buf.split_to(total).freeze(), schema }))
    }

    fn feed(&mut self, bytes: Bytes, schema: Option<Schema>) -> Result<(), TabularError> {
        if self.set.is_none() {
            if let Some(schema) = schema {
                self.open_set(&Arc::new(schema));
            }
        }
        let mut buffer = Buffer::from(bytes);
        let batch = self.inner.decode(&mut buffer).map_err(|e| TabularError::Decode(e.to_string()))?;
        if self.set.is_none() {
            if let Some(schema) = self.inner.schema() {
                self.open_set(&schema);
            }
        }
        if let Some(batch) = batch {
            self.take_batch(batch)?;
        }
        Ok(())
    }

    fn open_set(&mut self, schema: &SchemaRef) {
        let meta = schema.metadata();
        let is_error = meta.get("IsError").map(|v| v.eq_ignore_ascii_case("true")).unwrap_or(false) || meta.contains_key("FaultCode") || meta.contains_key("FaultString");
        if is_error {
            self.set = Some(OpenSet::Error { fault_code: meta.get("FaultCode").cloned(), fault_string: meta.get("FaultString").cloned(), code: None, lines: Vec::new() });
            return;
        }
        let columns = columns_for(schema);
        let grid = grid_schema(&columns);
        self.set = Some(OpenSet::Data { schema: grid, rows: 0 });
        self.queue.push_back(DaxItem::ResultSetStart { columns });
    }

    fn take_batch(&mut self, batch: RecordBatch) -> Result<(), TabularError> {
        match &mut self.set {
            Some(OpenSet::Data { schema, rows }) => {
                let cast = cast_batch(&batch, schema)?;
                *rows += cast.num_rows() as u64;
                self.queue.push_back(DaxItem::Batch(cast));
                Ok(())
            }
            Some(OpenSet::Error { code, lines, .. }) => {
                read_error_rows(&batch, code, lines);
                Ok(())
            }
            None => Err(TabularError::Decode("a record batch arrived before its schema".into())),
        }
    }

    fn close_set(&mut self) {
        match self.set.take() {
            Some(OpenSet::Data { rows, .. }) => self.queue.push_back(DaxItem::ResultSetEnd { rows }),
            Some(OpenSet::Error { fault_code, fault_string, code, lines }) => {
                let message = if lines.is_empty() { fault_string.unwrap_or_else(|| "the query failed (no error text in the response)".into()) } else { lines.join("\n") };
                self.queue.push_back(DaxItem::Error { message, code: code.or(fault_code) });
            }
            None => {}
        }
        self.inner = StreamDecoder::new();
    }
}

/// Fold an error rowset's rows into lines: `ErrorMessage` (plus `ErrorDescription` when it
/// says more) and any source-location columns as `(name: value)`.
fn read_error_rows(batch: &RecordBatch, code: &mut Option<String>, lines: &mut Vec<String>) {
    let schema = batch.schema();
    let text = |name: &str, row: usize| -> Option<String> {
        let i = schema.fields().iter().position(|f| strip_brackets(f.name()).eq_ignore_ascii_case(name))?;
        cell_text(batch.column(i), row)
    };
    let known = ["ErrorCode", "ErrorMessage", "ErrorDescription"];
    for row in 0..batch.num_rows() {
        if code.is_none() {
            *code = text("ErrorCode", row);
        }
        let message = text("ErrorMessage", row);
        let description = text("ErrorDescription", row);
        let mut line = match (&message, &description) {
            (Some(m), Some(d)) if d != m && !m.contains(d.as_str()) => format!("{m} {d}"),
            (Some(m), _) => m.clone(),
            (None, Some(d)) => d.clone(),
            (None, None) => String::new(),
        };
        for f in schema.fields() {
            let name = strip_brackets(f.name());
            if known.iter().any(|k| k.eq_ignore_ascii_case(name)) {
                continue;
            }
            if let Some(v) = text(name, row) {
                line.push_str(&format!(" ({name}: {v})"));
            }
        }
        if !line.trim().is_empty() {
            lines.push(line.trim().to_string());
        }
    }
}

fn cell_text(arr: &ArrayRef, row: usize) -> Option<String> {
    if arr.is_null(row) {
        return None;
    }
    let f = ArrayFormatter::try_new(arr.as_ref(), &FormatOptions::default()).ok()?;
    let s = f.value(row).to_string();
    if s.trim().is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Decode a complete body held in memory (tests, files): every item in order.
pub fn decode_all(chunks: impl IntoIterator<Item = Bytes>) -> Result<Vec<DaxItem>, TabularError> {
    let mut d = Decoder::new();
    let mut out = Vec::new();
    for c in chunks {
        d.push(&c)?;
        while let Some(i) = d.pop() {
            out.push(i);
        }
    }
    d.finish();
    while let Some(i) = d.pop() {
        out.push(i);
    }
    match d.take_error() {
        Some(e) => Err(e),
        None => Ok(out),
    }
}

/// Group items into result sets and errors.
pub fn collect_items(items: Vec<DaxItem>) -> Vec<DaxResult> {
    let mut out = Vec::new();
    let mut open: Option<DaxTable> = None;
    for item in items {
        match item {
            DaxItem::ResultSetStart { columns } => open = Some(DaxTable { columns, batches: Vec::new(), rows: 0 }),
            DaxItem::Batch(b) => {
                if let Some(t) = open.as_mut() {
                    t.rows += b.num_rows() as u64;
                    t.batches.push(b);
                }
            }
            DaxItem::ResultSetEnd { rows } => {
                if let Some(mut t) = open.take() {
                    t.rows = rows;
                    out.push(DaxResult::Table(t));
                }
            }
            DaxItem::Error { message, code } => out.push(DaxResult::Error { message, code }),
        }
    }
    if let Some(t) = open.take() {
        out.push(DaxResult::Table(t));
    }
    out
}

#[cfg(test)]
pub(crate) mod testing {
    //! IPC streams built in memory, the way the service would send them.
    use super::*;
    use arrow::array::{Date64Array, Decimal128Array, DictionaryArray, Float64Array, Int32Array};
    use arrow::datatypes::{Int32Type, TimeUnit};
    use arrow_ipc::writer::{IpcWriteOptions, StreamWriter};
    use arrow_ipc::CompressionType;
    use std::collections::HashMap;

    pub fn write_stream(batches: &[RecordBatch], schema: &Schema, lz4: bool) -> Vec<u8> {
        let opts = if lz4 { IpcWriteOptions::default().try_with_compression(Some(CompressionType::LZ4_FRAME)).unwrap() } else { IpcWriteOptions::default() };
        let mut out = Vec::new();
        let mut w = StreamWriter::try_new_with_options(&mut out, schema, opts).unwrap();
        for b in batches {
            w.write(b).unwrap();
        }
        w.finish().unwrap();
        drop(w);
        out
    }

    /// A dictionary utf8, a date64, a decimal128(19,4), a bool, an int64, a double.
    pub fn sample_schema() -> Schema {
        Schema::new(vec![
            Field::new("Product[Category]", DataType::Dictionary(Box::new(DataType::Int32), Box::new(DataType::Utf8)), true),
            Field::new("Sales[Date]", DataType::Date64, true),
            Field::new("[Amount]", DataType::Decimal128(19, 4), true),
            Field::new("Product[Active]", DataType::Boolean, true),
            Field::new("Sales[Qty]", DataType::Int64, true),
            Field::new("[Ratio]", DataType::Float64, true),
        ])
    }

    /// Two batches of four rows.
    pub fn sample_batches(schema: &Schema) -> Vec<RecordBatch> {
        let schema = Arc::new(schema.clone());
        let mk = |offset: i64| -> RecordBatch {
            let cats: DictionaryArray<Int32Type> = vec![Some("Bikes"), None, Some("Bikes"), Some("Clothing")].into_iter().collect();
            let dates = Date64Array::from(vec![Some(1_700_000_000_000 + offset), None, Some(0), Some(86_400_000)]);
            let amounts = Decimal128Array::from(vec![Some(1234_5000), None, Some(-1), Some(0)]).with_precision_and_scale(19, 4).unwrap();
            let active = BooleanArray::from(vec![Some(true), None, Some(false), Some(true)]);
            let qty = Int64Array::from(vec![Some(1 + offset), None, Some(3), Some(4)]);
            let ratio = Float64Array::from(vec![Some(0.5), None, Some(1.5), Some(2.0)]);
            RecordBatch::try_new(schema.clone(), vec![Arc::new(cats), Arc::new(dates), Arc::new(amounts), Arc::new(active), Arc::new(qty), Arc::new(ratio)]).unwrap()
        };
        vec![mk(0), mk(100)]
    }

    pub fn expected_grid_schema() -> SchemaRef {
        Arc::new(Schema::new(vec![
            Field::new("Product[Category]", DataType::LargeUtf8, true),
            Field::new("Sales[Date]", DataType::Timestamp(TimeUnit::Microsecond, None), true),
            Field::new("[Amount]", DataType::Decimal128(19, 4), true),
            Field::new("Product[Active]", DataType::Boolean, true),
            Field::new("Sales[Qty]", DataType::Int64, true),
            Field::new("[Ratio]", DataType::Float64, true),
        ]))
    }

    /// An error rowset as the service sends one: `IsError` metadata, one row.
    pub fn error_stream() -> Vec<u8> {
        let meta: HashMap<String, String> = [("IsError", "true"), ("FaultCode", "0xC1140001"), ("FaultString", "Query (1, 10) The syntax for 'FOO' is incorrect.")].into_iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let schema = Schema::new_with_metadata(
            vec![Field::new("ErrorCode", DataType::Utf8, true), Field::new("ErrorMessage", DataType::Utf8, true), Field::new("ErrorDescription", DataType::Utf8, true), Field::new("LineNumber", DataType::Int32, true)],
            meta,
        );
        let batch = RecordBatch::try_new(
            Arc::new(schema.clone()),
            vec![
                Arc::new(StringArray::from(vec![Some("0xC1140001")])),
                Arc::new(StringArray::from(vec![Some("Query (1, 10) The syntax for 'FOO' is incorrect.")])),
                Arc::new(StringArray::from(vec![Some("The syntax for 'FOO' is incorrect.")])),
                Arc::new(Int32Array::from(vec![Some(1)])),
            ],
        )
        .unwrap();
        write_stream(&[batch], &schema, false)
    }

    pub fn chunked(bytes: &[u8], size: usize) -> Vec<Bytes> {
        bytes.chunks(size).map(Bytes::copy_from_slice).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;
    use arrow::array::{Int32Array, NullArray, TimestampMicrosecondArray, UnionArray};
    use arrow::buffer::ScalarBuffer;
    use arrow::datatypes::{UnionFields, UnionMode};

    /// set 1 (2 batches) + error rowset + set 2 (1 batch).
    fn body(lz4: bool) -> Vec<u8> {
        let schema = sample_schema();
        let mut body = write_stream(&sample_batches(&schema), &schema, lz4);
        body.extend(error_stream());
        body.extend(write_stream(&sample_batches(&schema)[..1], &schema, lz4));
        body
    }

    fn check_items(items: &[DaxItem]) {
        assert_eq!(items.len(), 8, "{items:?}");
        let DaxItem::ResultSetStart { columns } = &items[0] else { panic!("{:?}", items[0]) };
        let types: Vec<SqlType> = columns.iter().map(|c| c.sql_type.clone()).collect();
        assert_eq!(types, vec![SqlType::NVarChar { len: None }, SqlType::DateTime2 { scale: 3 }, SqlType::Decimal { precision: 19, scale: 4 }, SqlType::Bit, SqlType::BigInt, SqlType::Float]);
        assert_eq!(columns[0].name, "Product[Category]");
        assert!(columns.iter().all(|c| c.nullable));
        assert_eq!(columns.iter().map(|c| c.ordinal).collect::<Vec<_>>(), vec![0, 1, 2, 3, 4, 5]);
        for i in [1, 2, 6] {
            let DaxItem::Batch(b) = &items[i] else { panic!("{:?}", items[i]) };
            assert_eq!(b.schema(), expected_grid_schema());
            assert_eq!(b.num_rows(), 4);
            let cats = b.column(0).as_any().downcast_ref::<LargeStringArray>().unwrap();
            assert_eq!(cats.value(0), "Bikes");
            assert!(cats.is_null(1));
            let dates = b.column(1).as_any().downcast_ref::<TimestampMicrosecondArray>().unwrap();
            assert_eq!(dates.value(3), 86_400_000_000);
            assert!(dates.is_null(1));
        }
        assert!(matches!(items[3], DaxItem::ResultSetEnd { rows: 8 }));
        let DaxItem::Error { message, code } = &items[4] else { panic!("{:?}", items[4]) };
        assert_eq!(code.as_deref(), Some("0xC1140001"));
        assert_eq!(message, "Query (1, 10) The syntax for 'FOO' is incorrect. (LineNumber: 1)");
        assert!(matches!(items[5], DaxItem::ResultSetStart { .. }));
        assert!(matches!(items[7], DaxItem::ResultSetEnd { rows: 4 }));
    }

    #[test]
    fn concatenated_streams_plain_and_lz4() {
        for lz4 in [false, true] {
            let items = decode_all(vec![Bytes::from(body(lz4))]).unwrap();
            check_items(&items);
        }
    }

    #[test]
    fn small_chunks_split_messages() {
        let b = body(true);
        check_items(&decode_all(chunked(&b, 7)).unwrap());
        check_items(&decode_all(chunked(&b, 1)).unwrap());
        check_items(&decode_all(chunked(&b, 4096)).unwrap());
    }

    #[test]
    fn error_only_stream() {
        let items = decode_all(vec![Bytes::from(error_stream())]).unwrap();
        assert_eq!(items.len(), 1);
        assert!(matches!(&items[0], DaxItem::Error { .. }));
        let r = collect_items(items);
        assert!(r[0].error().unwrap().to_string().contains("syntax"));
    }

    #[test]
    fn error_stream_without_rows_uses_fault_string() {
        let meta: std::collections::HashMap<String, String> = [("IsError".to_string(), "true".to_string()), ("FaultString".to_string(), "boom".to_string())].into_iter().collect();
        let schema = Schema::new_with_metadata(vec![Field::new("ErrorMessage", DataType::Utf8, true)], meta);
        let items = decode_all(vec![Bytes::from(write_stream(&[], &schema, false))]).unwrap();
        let DaxItem::Error { message, code } = &items[0] else { panic!("{:?}", items[0]) };
        assert_eq!(message, "boom");
        assert!(code.is_none());
    }

    #[test]
    fn schema_only_stream_opens_and_closes_a_set() {
        let schema = sample_schema();
        let mut body = write_stream(&[], &schema, true);
        body.extend(write_stream(&sample_batches(&schema)[..1], &schema, true));
        let items = decode_all(chunked(&body, 7)).unwrap();
        assert!(matches!(&items[0], DaxItem::ResultSetStart { columns } if columns.len() == 6), "{items:?}");
        assert!(matches!(items[1], DaxItem::ResultSetEnd { rows: 0 }), "{items:?}");
        assert!(matches!(items[2], DaxItem::ResultSetStart { .. }));
        assert!(matches!(items[3], DaxItem::Batch(_)));
        assert!(matches!(items[4], DaxItem::ResultSetEnd { rows: 4 }));
        assert_eq!(items.len(), 5);
    }

    #[test]
    fn truncated_body_keeps_rows_then_errors() {
        let b = body(false);
        let cut = &b[..b.len() - 20];
        let err = decode_all(vec![Bytes::copy_from_slice(cut)]).unwrap_err();
        assert!(err.to_string().contains("ended before"), "{err}");
        let mut d = Decoder::new();
        d.push(cut).unwrap();
        d.finish();
        let mut n = 0;
        while d.pop().is_some() {
            n += 1;
        }
        assert!(n >= 6, "rows before the cut are delivered: {n}");
        assert!(d.take_error().is_some());
    }

    #[test]
    fn trailing_zero_padding_is_ignored() {
        let mut b = body(false);
        b.extend([0u8; 8]);
        assert_eq!(decode_all(vec![Bytes::from(b)]).unwrap().len(), 8);
    }

    #[test]
    fn empty_body_is_no_items() {
        assert!(decode_all(Vec::<Bytes>::new()).unwrap().is_empty());
    }

    #[test]
    fn union_and_unknown_types_become_text() {
        let fields = UnionFields::try_new(vec![0, 1], vec![Field::new("i", DataType::Int32, false), Field::new("s", DataType::Utf8, false)]).unwrap();
        let type_ids = ScalarBuffer::<i8>::from(vec![0, 1, 0]);
        let offsets = ScalarBuffer::<i32>::from(vec![0, 0, 1]);
        let union = UnionArray::try_new(fields.clone(), type_ids, Some(offsets), vec![Arc::new(Int32Array::from(vec![7, 9])) as ArrayRef, Arc::new(StringArray::from(vec!["x"])) as ArrayRef]).unwrap();
        let schema = Schema::new(vec![Field::new("[V]", DataType::Union(fields, UnionMode::Dense), true), Field::new("[N]", DataType::Null, true)]);
        let batch = RecordBatch::try_new(Arc::new(schema.clone()), vec![Arc::new(union), Arc::new(NullArray::new(3))]).unwrap();
        let t = DaxTable::from_batches(vec![batch]).unwrap();
        assert_eq!(t.columns[0].sql_type, SqlType::NVarChar { len: None });
        assert_eq!(t.columns[1].sql_type, SqlType::NVarChar { len: None });
        assert_eq!(t.batches[0].schema(), grid_schema(&t.columns));
        assert_eq!(t.text("V", 0).as_deref(), Some("{i=7}"));
        assert_eq!(t.text("V", 1).as_deref(), Some("{s=x}"));
        assert_eq!(t.text("N", 0), None);
    }

    #[test]
    fn duplicate_and_blank_names_match_the_grid() {
        let schema = Schema::new(vec![Field::new("x", DataType::Int32, true), Field::new("x", DataType::Int32, true), Field::new("", DataType::Utf8, true)]);
        let cols = columns_for(&schema);
        let g = grid_schema(&cols);
        assert_eq!(g.fields().iter().map(|f| f.name().as_str()).collect::<Vec<_>>(), vec!["x", "x_2", "(No column name 3)"]);
        assert_eq!(cols[1].name, "x");
    }

    #[test]
    fn table_cells() {
        let schema = sample_schema();
        let t = DaxTable::from_batches(sample_batches(&schema)).unwrap();
        assert_eq!(t.num_rows(), 8);
        assert_eq!(t.text("Product[Category]", 3).as_deref(), Some("Clothing"));
        assert_eq!(t.text("Product[Category]", 1), None);
        assert!(t.flag("Product[Active]", 0));
        assert!(!t.flag("Product[Active]", 1));
        assert_eq!(t.int("Sales[Qty]", 4), Some(101));
        assert_eq!(t.text("[Amount]", 0).as_deref(), Some("1234.5000"));
        assert_eq!(t.col("nope"), None);
    }
}
