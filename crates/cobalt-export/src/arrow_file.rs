//! Arrow IPC file (Feather v2) via `arrow::ipc::writer::FileWriter`, optional LZ4/ZSTD.

use crate::{Ctx, IpcCompression, Progress, Result};
use arrow::array::RecordBatch;
use arrow::ipc::writer::{FileWriter, IpcWriteOptions};
use arrow::ipc::CompressionType;
use std::io::Write;

pub(crate) fn write<W: Write>(ctx: &Ctx<'_, '_>, sink: &mut W, progress: &mut dyn FnMut(Progress) -> bool) -> Result<(usize, Vec<String>)> {
    let compression = ctx.opts.arrow.compression.map(|c| match c {
        IpcCompression::Lz4 => CompressionType::LZ4_FRAME,
        IpcCompression::Zstd => CompressionType::ZSTD,
    });
    let options = IpcWriteOptions::default().try_with_compression(compression)?;
    let schema = crate::parquet_file::schema_with_metadata(ctx);
    let mut writer = FileWriter::try_new_with_options(&mut *sink, &schema, options)?;
    writer.write_metadata(crate::parquet_file::SQL_TYPES_KEY, ctx.sql_types_json());
    writer.write_metadata("created_by", crate::parquet_file::CREATED_BY);
    let rows = ctx.for_each_batch(progress, |batch: &RecordBatch| {
        let batch = RecordBatch::try_new(schema.clone(), batch.columns().to_vec())?;
        writer.write(&batch)?;
        Ok(())
    });
    match rows {
        Ok(rows) => {
            writer.finish()?;
            Ok((rows, Vec::new()))
        }
        Err(e) => Err(e),
    }
}
