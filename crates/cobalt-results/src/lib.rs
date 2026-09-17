//! Arrow-backed result sets with disk spill, sort/filter views, and a display cache.
//!
//! A [`ResultSet`] is appended to by the session thread (one `RecordBatch` at a time) and read
//! by the UI thread through a *view* (optional filter + sort producing an index of global row
//! ids). Chunks beyond the shared [`MemoryBudget`] are spilled to Arrow IPC files and reloaded
//! on demand through a small LRU. Formatting is cached per (chunk, column) so the grid can ask
//! for thousands of cells per frame without allocating.

pub mod budget;
pub mod display;
pub mod filter;
pub mod spill;
pub mod summary;
pub mod value;

pub use budget::MemoryBudget;
pub use display::{CellFormatter, DisplayCache};
pub use filter::{ColumnFilter, FilterOp};
pub use summary::{Summary, summarize};
pub use value::CellValue;

use arrow::array::RecordBatch;
use arrow::datatypes::{Schema, SchemaRef};
use arrow::row::{RowConverter, SortField};
use cobalt_core::ColumnInfo;
use parking_lot::{Mutex, RwLock};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
pub enum ResultError {
    #[error("arrow error: {0}")]
    Arrow(#[from] arrow::error::ArrowError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}
pub type Result<T> = std::result::Result<T, ResultError>;

/// Lifecycle of the fetch behind a result set.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum RunState {
    /// Rows still arriving.
    Streaming,
    /// Row cap reached; the server-side cursor is open and waiting for "fetch more".
    Paused,
    Complete,
    Cancelled,
    Error { message: String },
}

impl RunState {
    pub fn is_live(&self) -> bool {
        matches!(self, RunState::Streaming | RunState::Paused)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SortKey {
    pub column: usize,
    pub descending: bool,
}

/// The current filter/sort applied on top of the raw rows.
#[derive(Clone, Debug, Default)]
pub struct ViewSpec {
    pub filters: Vec<ColumnFilter>,
    pub sort: Vec<SortKey>,
}

impl ViewSpec {
    pub fn is_identity(&self) -> bool {
        self.filters.is_empty() && self.sort.is_empty()
    }
}

/// Where a chunk currently lives.
enum Chunk {
    Mem(RecordBatch),
    Spilled(spill::SpilledChunk),
}

impl Chunk {
    fn rows(&self) -> usize {
        match self {
            Chunk::Mem(b) => b.num_rows(),
            Chunk::Spilled(s) => s.rows,
        }
    }
}

struct Inner {
    chunks: Vec<Chunk>,
    /// `row_offsets[i]` = global row id of the first row in chunk `i`; last element = total rows.
    row_offsets: Vec<usize>,
    bytes_in_mem: usize,
    state: RunState,
    /// Visible global row ids, or `None` when the view is the identity.
    view_index: Option<Arc<Vec<u32>>>,
    view_spec: ViewSpec,
    /// Incremented whenever the raw rows or the view change; the UI uses it to invalidate caches.
    generation: u64,
}

pub struct ResultSet {
    /// 0-based position of this result set within its run.
    pub index: usize,
    pub columns: Vec<ColumnInfo>,
    pub schema: SchemaRef,
    inner: RwLock<Inner>,
    budget: Arc<MemoryBudget>,
    spill_dir: std::path::PathBuf,
    reload_cache: Mutex<spill::ReloadCache>,
    display: DisplayCache,
    total_rows: AtomicUsize,
    /// Rows affected as reported by the server (for DML result-less statements this set is not created;
    /// here it's the final DONE count when known).
    pub rows_affected: AtomicUsize,
}

impl ResultSet {
    pub fn new(index: usize, columns: Vec<ColumnInfo>, budget: Arc<MemoryBudget>, spill_dir: std::path::PathBuf) -> Arc<Self> {
        let fields: Vec<arrow::datatypes::Field> = columns
            .iter()
            .map(|c| arrow::datatypes::Field::new(dedupe_name(&columns, c), c.sql_type.arrow_type(), true))
            .collect();
        let schema = Arc::new(Schema::new(fields));
        Arc::new(Self {
            index,
            columns,
            schema,
            inner: RwLock::new(Inner {
                chunks: Vec::new(),
                row_offsets: vec![0],
                bytes_in_mem: 0,
                state: RunState::Streaming,
                view_index: None,
                view_spec: ViewSpec::default(),
                generation: 0,
            }),
            budget,
            spill_dir,
            reload_cache: Mutex::new(spill::ReloadCache::new(8)),
            display: DisplayCache::new(96),
            total_rows: AtomicUsize::new(0),
            rows_affected: AtomicUsize::new(0),
        })
    }

    /// Build a result set from a fixed schema and batches (tests, plan/showplan wrappers, local queries).
    pub fn from_batches(index: usize, columns: Vec<ColumnInfo>, batches: Vec<RecordBatch>) -> Arc<Self> {
        let rs = Self::new(index, columns, Arc::new(MemoryBudget::unlimited()), std::env::temp_dir());
        for b in batches {
            rs.append(b).expect("append");
        }
        rs.set_state(RunState::Complete);
        rs
    }

    // ---------- writer side ----------

    /// Append a batch. Batches must match `schema` (the driver guarantees it).
    pub fn append(&self, batch: RecordBatch) -> Result<()> {
        if batch.num_rows() == 0 {
            return Ok(());
        }
        let bytes = batch.get_array_memory_size();
        let mut inner = self.inner.write();
        let start = *inner.row_offsets.last().unwrap();
        inner.row_offsets.push(start + batch.num_rows());
        inner.chunks.push(Chunk::Mem(batch));
        inner.bytes_in_mem += bytes;
        inner.generation += 1;
        self.total_rows.store(start + inner.chunks.last().unwrap().rows(), Ordering::Release);
        self.budget.add(bytes);
        // Spill oldest in-memory chunks while over budget (never the one we just added unless it alone exceeds).
        while self.budget.over_limit() && inner.bytes_in_mem > 0 {
            let Some(pos) = inner.chunks.iter().position(|c| matches!(c, Chunk::Mem(_))) else { break };
            if pos == inner.chunks.len() - 1 && inner.chunks.len() > 1 {
                break;
            }
            let Chunk::Mem(b) = &inner.chunks[pos] else { unreachable!() };
            let b = b.clone();
            drop(inner);
            let spilled = spill::spill(&self.spill_dir, &b)?;
            inner = self.inner.write();
            let freed = b.get_array_memory_size();
            inner.chunks[pos] = Chunk::Spilled(spilled);
            inner.bytes_in_mem = inner.bytes_in_mem.saturating_sub(freed);
            self.budget.sub(freed);
            tracing::debug!(chunk = pos, freed, "spilled chunk to disk");
        }
        Ok(())
    }

    pub fn set_state(&self, state: RunState) {
        let mut inner = self.inner.write();
        inner.state = state;
        inner.generation += 1;
    }

    pub fn set_rows_affected(&self, n: u64) {
        self.rows_affected.store(n as usize, Ordering::Relaxed);
    }

    // ---------- reader side ----------

    pub fn state(&self) -> RunState {
        self.inner.read().state.clone()
    }

    pub fn generation(&self) -> u64 {
        self.inner.read().generation
    }

    /// Raw rows fetched so far.
    pub fn row_count(&self) -> usize {
        self.total_rows.load(Ordering::Acquire)
    }

    /// Rows after filtering.
    pub fn visible_count(&self) -> usize {
        let inner = self.inner.read();
        match &inner.view_index {
            Some(v) => v.len(),
            None => *inner.row_offsets.last().unwrap(),
        }
    }

    pub fn column_count(&self) -> usize {
        self.columns.len()
    }

    pub fn chunk_count(&self) -> usize {
        self.inner.read().chunks.len()
    }

    pub fn bytes_in_memory(&self) -> usize {
        self.inner.read().bytes_in_mem
    }

    pub fn spilled_chunks(&self) -> usize {
        self.inner.read().chunks.iter().filter(|c| matches!(c, Chunk::Spilled(_))).count()
    }

    pub fn view_spec(&self) -> ViewSpec {
        self.inner.read().view_spec.clone()
    }

    /// Map a visible row index to the global row id.
    pub fn global_row(&self, visible_row: usize) -> Option<usize> {
        let inner = self.inner.read();
        match &inner.view_index {
            Some(v) => v.get(visible_row).map(|&g| g as usize),
            None => (visible_row < *inner.row_offsets.last().unwrap()).then_some(visible_row),
        }
    }

    /// Locate chunk index and row-within-chunk for a global row.
    fn locate(offsets: &[usize], global: usize) -> Option<(usize, usize)> {
        if global >= *offsets.last()? {
            return None;
        }
        let chunk = match offsets.binary_search(&global) {
            Ok(i) => i,
            Err(i) => i - 1,
        };
        Some((chunk, global - offsets[chunk]))
    }

    /// Get chunk `i` as an in-memory batch (reloading from spill if needed).
    pub fn chunk(&self, i: usize) -> Option<RecordBatch> {
        let inner = self.inner.read();
        match inner.chunks.get(i)? {
            Chunk::Mem(b) => Some(b.clone()),
            Chunk::Spilled(s) => {
                let s = s.clone();
                drop(inner);
                let mut cache = self.reload_cache.lock();
                if let Some(b) = cache.get(i) {
                    return Some(b);
                }
                match spill::reload(&s) {
                    Ok(b) => {
                        cache.put(i, b.clone());
                        Some(b)
                    }
                    Err(e) => {
                        tracing::error!(error = %e, chunk = i, "failed to reload spilled chunk");
                        None
                    }
                }
            }
        }
    }

    /// Display text for a visible cell. Cheap after the first call per (chunk, column).
    pub fn cell_text(&self, visible_row: usize, col: usize, fmt: &CellFormatter) -> Arc<str> {
        let Some(global) = self.global_row(visible_row) else { return Arc::from("") };
        self.cell_text_global(global, col, fmt)
    }

    pub fn cell_text_global(&self, global: usize, col: usize, fmt: &CellFormatter) -> Arc<str> {
        let offsets = self.inner.read().row_offsets.clone();
        let Some((ci, r)) = Self::locate(&offsets, global) else { return Arc::from("") };
        if let Some(text) = self.display.get(ci, col, fmt.generation(), r) {
            return text;
        }
        let Some(batch) = self.chunk(ci) else { return Arc::from("") };
        let formatted = fmt.format_column(batch.column(col), &self.columns[col]);
        let text = formatted.get(r).cloned().unwrap_or_else(|| Arc::from(""));
        self.display.put(ci, col, fmt.generation(), formatted);
        text
    }

    /// Typed value for a visible cell (viewer, copy-as-JSON, summary).
    pub fn cell_value(&self, visible_row: usize, col: usize) -> CellValue {
        let Some(global) = self.global_row(visible_row) else { return CellValue::Null };
        self.cell_value_global(global, col)
    }

    pub fn cell_value_global(&self, global: usize, col: usize) -> CellValue {
        let offsets = self.inner.read().row_offsets.clone();
        let Some((ci, r)) = Self::locate(&offsets, global) else { return CellValue::Null };
        let Some(batch) = self.chunk(ci) else { return CellValue::Null };
        CellValue::from_array(batch.column(col), r)
    }

    pub fn is_null(&self, visible_row: usize, col: usize) -> bool {
        matches!(self.cell_value(visible_row, col), CellValue::Null)
    }

    // ---------- views ----------

    /// Recompute the view. Runs on the calling thread; call from a background task for large sets.
    pub fn apply_view(&self, spec: ViewSpec) -> Result<()> {
        if spec.is_identity() {
            let mut inner = self.inner.write();
            inner.view_index = None;
            inner.view_spec = spec;
            inner.generation += 1;
            return Ok(());
        }
        let (nchunks, offsets) = {
            let inner = self.inner.read();
            (inner.chunks.len(), inner.row_offsets.clone())
        };
        let fmt = CellFormatter::default();
        // 1. filter → candidate global rows
        let mut candidates: Vec<u32> = Vec::new();
        for ci in 0..nchunks {
            let Some(batch) = self.chunk(ci) else { continue };
            let base = offsets[ci] as u32;
            if spec.filters.is_empty() {
                candidates.extend(base..base + batch.num_rows() as u32);
            } else {
                let mask = filter::evaluate(&batch, &self.columns, &spec.filters, &fmt);
                candidates.extend(mask.iter().enumerate().filter(|(_, &m)| m).map(|(i, _)| base + i as u32));
            }
        }
        // 2. sort candidates using byte-comparable rows
        if !spec.sort.is_empty() {
            let sort_cols: Vec<usize> = spec.sort.iter().map(|k| k.column).collect();
            let fields: Vec<SortField> = spec
                .sort
                .iter()
                .map(|k| {
                    let mut opts = arrow::compute::SortOptions::default();
                    opts.descending = k.descending;
                    opts.nulls_first = !k.descending;
                    SortField::new_with_options(self.schema.field(k.column).data_type().clone(), opts)
                })
                .collect();
            let converter = RowConverter::new(fields)?;
            // Build rows chunk by chunk; keep them alive for comparison.
            let mut chunk_rows: Vec<Option<arrow::row::Rows>> = Vec::with_capacity(nchunks);
            for ci in 0..nchunks {
                let Some(batch) = self.chunk(ci) else { chunk_rows.push(None); continue };
                let cols: Vec<_> = sort_cols.iter().map(|&c| batch.column(c).clone()).collect();
                chunk_rows.push(Some(converter.convert_columns(&cols)?));
            }
            let key = |g: u32| -> Option<arrow::row::Row<'_>> {
                let (ci, r) = Self::locate(&offsets, g as usize)?;
                chunk_rows[ci].as_ref().map(|rows| rows.row(r))
            };
            candidates.sort_by(|&a, &b| match (key(a), key(b)) {
                (Some(x), Some(y)) => x.cmp(&y).then(a.cmp(&b)),
                _ => a.cmp(&b),
            });
        }
        let mut inner = self.inner.write();
        inner.view_index = Some(Arc::new(candidates));
        inner.view_spec = spec;
        inner.generation += 1;
        Ok(())
    }

    /// Distinct display values of a column across the *raw* rows (for the header filter popup),
    /// with counts, capped at `limit` distinct values. Returns (values, truncated).
    pub fn distinct_values(&self, col: usize, limit: usize, fmt: &CellFormatter) -> (Vec<(Arc<str>, usize)>, bool) {
        let n = self.chunk_count();
        let mut counts: indexmap::IndexMap<Arc<str>, usize> = indexmap::IndexMap::new();
        let mut truncated = false;
        'outer: for ci in 0..n {
            let Some(batch) = self.chunk(ci) else { continue };
            let formatted = fmt.format_column(batch.column(col), &self.columns[col]);
            for v in formatted.iter() {
                if let Some(c) = counts.get_mut(v) {
                    *c += 1;
                } else if counts.len() < limit {
                    counts.insert(v.clone(), 1);
                } else {
                    truncated = true;
                    break 'outer;
                }
            }
        }
        let mut out: Vec<(Arc<str>, usize)> = counts.into_iter().collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        (out, truncated)
    }

    // ---------- export side ----------

    /// Iterate raw chunks in fetch order (no view applied). Streams from spill.
    pub fn raw_batches(&self) -> impl Iterator<Item = RecordBatch> + '_ {
        (0..self.chunk_count()).filter_map(move |i| self.chunk(i))
    }

    /// Batches of the *visible* rows in view order, `batch_rows` at a time. When the view is the
    /// identity this is `raw_batches`; otherwise rows are gathered by index (materializes each output batch only).
    pub fn view_batches(&self, batch_rows: usize) -> Box<dyn Iterator<Item = Result<RecordBatch>> + '_> {
        let (view, offsets) = {
            let inner = self.inner.read();
            (inner.view_index.clone(), inner.row_offsets.clone())
        };
        match view {
            None => Box::new(self.raw_batches().map(Ok)),
            Some(index) => {
                let schema = self.schema.clone();
                let ncols = self.columns.len();
                Box::new((0..index.len()).step_by(batch_rows.max(1)).map(move |start| {
                    let end = (start + batch_rows).min(index.len());
                    let slice = &index[start..end];
                    // group by chunk to take from each once
                    let mut per_chunk: std::collections::BTreeMap<usize, (Vec<u32>, Vec<usize>)> = Default::default();
                    for (out_pos, &g) in slice.iter().enumerate() {
                        let (ci, r) = Self::locate(&offsets, g as usize).ok_or_else(|| ResultError::Other("row out of range".into()))?;
                        let e = per_chunk.entry(ci).or_default();
                        e.0.push(r as u32);
                        e.1.push(out_pos);
                    }
                    // take rows per chunk, then interleave into output order
                    let mut pieces: Vec<(Vec<usize>, RecordBatch)> = Vec::new();
                    for (ci, (rows, positions)) in per_chunk {
                        let batch = self.chunk(ci).ok_or_else(|| ResultError::Other("chunk unavailable".into()))?;
                        let idx = arrow::array::UInt32Array::from(rows);
                        let cols = (0..ncols).map(|c| arrow::compute::take(batch.column(c), &idx, None)).collect::<std::result::Result<Vec<_>, _>>()?;
                        pieces.push((positions, RecordBatch::try_new(schema.clone(), cols)?));
                    }
                    if pieces.len() == 1 && pieces[0].0.windows(2).all(|w| w[1] == w[0] + 1) {
                        return Ok(pieces.pop().unwrap().1);
                    }
                    // interleave
                    let mut indices: Vec<(usize, usize)> = vec![(0, 0); slice.len()];
                    for (pi, (positions, _)) in pieces.iter().enumerate() {
                        for (ri, &p) in positions.iter().enumerate() {
                            indices[p] = (pi, ri);
                        }
                    }
                    let cols = (0..ncols)
                        .map(|c| {
                            let arrays: Vec<&dyn arrow::array::Array> = pieces.iter().map(|(_, b)| b.column(c).as_ref()).collect();
                            arrow::compute::interleave(&arrays, &indices)
                        })
                        .collect::<std::result::Result<Vec<_>, _>>()?;
                    Ok(RecordBatch::try_new(schema.clone(), cols)?)
                }))
            }
        }
    }

    /// Gather arbitrary *visible* rows and a column projection into one batch, in the order
    /// given (copy/clipboard builders over a rectangular selection). `cols` are column indexes.
    pub fn gather_rows(&self, visible_rows: &[usize], cols: &[usize]) -> Result<RecordBatch> {
        let (view, offsets) = {
            let inner = self.inner.read();
            (inner.view_index.clone(), inner.row_offsets.clone())
        };
        let fields: Vec<arrow::datatypes::Field> = cols.iter().map(|&c| self.schema.field(c).clone()).collect();
        let schema = Arc::new(Schema::new(fields));
        let mut per_chunk: std::collections::BTreeMap<usize, (Vec<u32>, Vec<usize>)> = Default::default();
        for (out_pos, &vr) in visible_rows.iter().enumerate() {
            let g = match &view {
                Some(v) => *v.get(vr).ok_or_else(|| ResultError::Other("row out of range".into()))? as usize,
                None => vr,
            };
            let (ci, r) = Self::locate(&offsets, g).ok_or_else(|| ResultError::Other("row out of range".into()))?;
            let e = per_chunk.entry(ci).or_default();
            e.0.push(r as u32);
            e.1.push(out_pos);
        }
        let mut pieces: Vec<(Vec<usize>, RecordBatch)> = Vec::new();
        for (ci, (rows, positions)) in per_chunk {
            let batch = self.chunk(ci).ok_or_else(|| ResultError::Other("chunk unavailable".into()))?;
            let idx = arrow::array::UInt32Array::from(rows);
            let arrays = cols.iter().map(|&c| arrow::compute::take(batch.column(c), &idx, None)).collect::<std::result::Result<Vec<_>, _>>()?;
            pieces.push((positions, RecordBatch::try_new(schema.clone(), arrays)?));
        }
        if pieces.is_empty() {
            return Ok(RecordBatch::new_empty(schema));
        }
        if pieces.len() == 1 && pieces[0].0.windows(2).all(|w| w[1] == w[0] + 1) {
            return Ok(pieces.pop().unwrap().1);
        }
        let mut indices: Vec<(usize, usize)> = vec![(0, 0); visible_rows.len()];
        for (pi, (positions, _)) in pieces.iter().enumerate() {
            for (ri, &p) in positions.iter().enumerate() {
                indices[p] = (pi, ri);
            }
        }
        let arrays = (0..cols.len())
            .map(|c| {
                let arrays: Vec<&dyn arrow::array::Array> = pieces.iter().map(|(_, b)| b.column(c).as_ref()).collect();
                arrow::compute::interleave(&arrays, &indices)
            })
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(RecordBatch::try_new(schema, arrays)?)
    }

    /// All visible rows as one batch (small sets only — copy, charts).
    pub fn view_to_single_batch(&self) -> Result<RecordBatch> {
        let batches: Vec<RecordBatch> = self.view_batches(65_536).collect::<Result<Vec<_>>>()?;
        Ok(arrow::compute::concat_batches(&self.schema, &batches)?)
    }
}

impl std::fmt::Debug for ResultSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResultSet").field("index", &self.index).field("columns", &self.columns.len()).field("rows", &self.row_count()).field("state", &self.state()).finish()
    }
}

impl Drop for ResultSet {
    fn drop(&mut self) {
        let inner = self.inner.get_mut();
        self.budget.sub(inner.bytes_in_mem);
        for c in inner.chunks.drain(..) {
            if let Chunk::Spilled(s) = c {
                s.remove();
            }
        }
    }
}

/// SQL allows duplicate/empty column names; Arrow schemas don't like them. Make them unique
/// for the schema while keeping `ColumnInfo.name` as the server sent it.
fn dedupe_name(columns: &[ColumnInfo], c: &ColumnInfo) -> String {
    let base = if c.name.is_empty() { format!("(No column name {})", c.ordinal + 1) } else { c.name.clone() };
    let dupes_before = columns[..c.ordinal].iter().filter(|o| o.name == c.name).count();
    if dupes_before == 0 { base } else { format!("{base}_{}", dupes_before + 1) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::{Int32Array, StringArray};
    use cobalt_core::SqlType;

    fn sample() -> Arc<ResultSet> {
        let cols = vec![ColumnInfo::new("id", SqlType::Int, false, 0), ColumnInfo::new("name", SqlType::NVarChar { len: Some(50) }, true, 1)];
        let rs = ResultSet::new(0, cols, Arc::new(MemoryBudget::unlimited()), std::env::temp_dir());
        for chunk in 0..3 {
            let ids: Vec<i32> = (chunk * 4..chunk * 4 + 4).collect();
            let names: Vec<Option<String>> = ids.iter().map(|i| if i % 3 == 0 { None } else { Some(format!("n{i}")) }).collect();
            let b = RecordBatch::try_new(rs.schema.clone(), vec![Arc::new(Int32Array::from(ids)), Arc::new(StringArray::from(names))]).unwrap();
            rs.append(b).unwrap();
        }
        rs.set_state(RunState::Complete);
        rs
    }

    #[test]
    fn rows_and_cells() {
        let rs = sample();
        assert_eq!(rs.row_count(), 12);
        let fmt = CellFormatter::default();
        assert_eq!(&*rs.cell_text(5, 0, &fmt), "5");
        assert_eq!(&*rs.cell_text(5, 1, &fmt), "n5");
        assert_eq!(&*rs.cell_text(6, 1, &fmt), "NULL");
        assert!(rs.is_null(6, 1));
    }

    #[test]
    fn sort_and_filter() {
        let rs = sample();
        rs.apply_view(ViewSpec { filters: vec![], sort: vec![SortKey { column: 0, descending: true }] }).unwrap();
        let fmt = CellFormatter::default();
        assert_eq!(&*rs.cell_text(0, 0, &fmt), "11");
        rs.apply_view(ViewSpec { filters: vec![ColumnFilter { column: 1, op: FilterOp::NotNull }], sort: vec![SortKey { column: 0, descending: false }] }).unwrap();
        assert_eq!(rs.visible_count(), 8);
        assert_eq!(&*rs.cell_text(0, 0, &fmt), "1");
        let b = rs.view_to_single_batch().unwrap();
        assert_eq!(b.num_rows(), 8);
    }

    #[test]
    fn gather_rows_projects_and_orders() {
        let rs = sample();
        // Rows spanning two chunks, out of order, with a column projection.
        let b = rs.gather_rows(&[5, 1, 9], &[1, 0]).unwrap();
        assert_eq!(b.num_rows(), 3);
        assert_eq!(b.schema().field(0).name(), "name");
        let names: Vec<CellValue> = (0..3).map(|r| CellValue::from_array(b.column(0), r)).collect();
        assert_eq!(names, vec![CellValue::Text("n5".into()), CellValue::Text("n1".into()), CellValue::Null]);
        assert_eq!(CellValue::from_array(b.column(1), 2), CellValue::Int(9));
        // Through a sorted view the indexes are visible rows.
        rs.apply_view(ViewSpec { filters: vec![], sort: vec![SortKey { column: 0, descending: true }] }).unwrap();
        let b = rs.gather_rows(&[0, 1], &[0]).unwrap();
        assert_eq!(CellValue::from_array(b.column(0), 0), CellValue::Int(11));
        assert_eq!(CellValue::from_array(b.column(0), 1), CellValue::Int(10));
        assert!(rs.gather_rows(&[99], &[0]).is_err());
        assert_eq!(rs.gather_rows(&[], &[0]).unwrap().num_rows(), 0);
    }

    #[test]
    fn spill_roundtrip() {
        let cols = vec![ColumnInfo::new("id", SqlType::Int, false, 0)];
        let budget = Arc::new(MemoryBudget::new(64)); // tiny → everything but the newest spills
        let dir = tempfile::tempdir().unwrap();
        let rs = ResultSet::new(0, cols, budget, dir.path().to_path_buf());
        for chunk in 0..5 {
            let b = RecordBatch::try_new(rs.schema.clone(), vec![Arc::new(Int32Array::from((chunk * 1000..chunk * 1000 + 1000).collect::<Vec<i32>>()))]).unwrap();
            rs.append(b).unwrap();
        }
        assert!(rs.spilled_chunks() >= 3, "spilled {}", rs.spilled_chunks());
        let fmt = CellFormatter::default();
        assert_eq!(&*rs.cell_text(1500, 0, &fmt), "1500");
        assert_eq!(&*rs.cell_text(4999, 0, &fmt), "4999");
        rs.apply_view(ViewSpec { filters: vec![], sort: vec![SortKey { column: 0, descending: true }] }).unwrap();
        assert_eq!(&*rs.cell_text(0, 0, &fmt), "4999");
    }
}
