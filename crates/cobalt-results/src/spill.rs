use crate::Result;
use arrow::array::RecordBatch;
use arrow::ipc::reader::FileReader;
use arrow::ipc::writer::FileWriter;
use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug)]
pub struct SpilledChunk {
    pub path: PathBuf,
    pub rows: usize,
}

impl SpilledChunk {
    pub fn remove(&self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub fn spill(dir: &Path, batch: &RecordBatch) -> Result<SpilledChunk> {
    std::fs::create_dir_all(dir)?;
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let path = dir.join(format!("cobalt-spill-{}-{n}.arrow", std::process::id()));
    let file = BufWriter::new(File::create(&path)?);
    let mut w = FileWriter::try_new(file, batch.schema().as_ref())?;
    w.write(batch)?;
    w.finish()?;
    Ok(SpilledChunk { path, rows: batch.num_rows() })
}

pub fn reload(chunk: &SpilledChunk) -> Result<RecordBatch> {
    let file = File::open(&chunk.path)?;
    let mut reader = FileReader::try_new(file, None)?;
    let schema = reader.schema();
    let batches: Vec<RecordBatch> = reader.by_ref().collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(arrow::compute::concat_batches(&schema, &batches)?)
}

/// Tiny LRU of reloaded chunks so scrolling near a spilled region doesn't hit disk every frame.
pub struct ReloadCache {
    cap: usize,
    entries: Vec<(usize, RecordBatch)>,
}

impl ReloadCache {
    pub fn new(cap: usize) -> Self {
        Self { cap, entries: Vec::new() }
    }
    pub fn get(&mut self, chunk: usize) -> Option<RecordBatch> {
        let pos = self.entries.iter().position(|(c, _)| *c == chunk)?;
        let e = self.entries.remove(pos);
        let b = e.1.clone();
        self.entries.push(e);
        Some(b)
    }
    pub fn put(&mut self, chunk: usize, batch: RecordBatch) {
        self.entries.retain(|(c, _)| *c != chunk);
        if self.entries.len() >= self.cap {
            self.entries.remove(0);
        }
        self.entries.push((chunk, batch));
    }
}
