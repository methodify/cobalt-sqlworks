//! Parquet via `parquet::arrow::ArrowWriter`; the Arrow schema is written as-is (parquet-rs
//! supports every type our driver produces: Decimal128, Time64(ns), Timestamp(ns, tz), Large*).

use crate::{Compression, Ctx, Progress, Result};
use arrow::array::RecordBatch;
use arrow::datatypes::Schema;
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression as PqCompression, ZstdLevel};
use parquet::file::metadata::KeyValue;
use parquet::file::properties::{EnabledStatistics, WriterProperties};
use std::io::Write;
use std::sync::Arc;

pub(crate) const CREATED_BY: &str = "Cobalt SQL Works";
pub(crate) const SQL_TYPES_KEY: &str = "cobalt.sql_types";

/// Column SQL types as schema-level metadata so readers (and our own importer) can recover them.
pub(crate) fn schema_with_metadata(ctx: &Ctx<'_>) -> Arc<Schema> {
    let mut md = ctx.rs.schema.metadata().clone();
    md.insert(SQL_TYPES_KEY.into(), ctx.sql_types_json());
    Arc::new(Schema::new_with_metadata(ctx.rs.schema.fields().clone(), md))
}

pub(crate) fn write<W: Write + Send>(ctx: &Ctx<'_>, sink: &mut W, progress: &mut dyn FnMut(Progress) -> bool) -> Result<(usize, Vec<String>)> {
    let o = &ctx.opts.parquet;
    let compression = match o.compression {
        Compression::None => PqCompression::UNCOMPRESSED,
        Compression::Snappy => PqCompression::SNAPPY,
        Compression::Zstd => PqCompression::ZSTD(ZstdLevel::default()),
        Compression::Lz4 => PqCompression::LZ4_RAW,
    };
    let props = WriterProperties::builder()
        .set_compression(compression)
        .set_max_row_group_size(o.row_group_rows.max(1))
        .set_statistics_enabled(if o.statistics { EnabledStatistics::Page } else { EnabledStatistics::None })
        .set_created_by(CREATED_BY.to_string())
        .set_key_value_metadata(Some(vec![KeyValue::new(SQL_TYPES_KEY.to_string(), ctx.sql_types_json())]))
        .build();
    let schema = schema_with_metadata(ctx);
    let mut writer = ArrowWriter::try_new(&mut *sink, schema.clone(), Some(props))?;
    let rows = ctx.for_each_batch(progress, |batch: &RecordBatch| {
        let batch = RecordBatch::try_new(schema.clone(), batch.columns().to_vec())?;
        writer.write(&batch)?;
        Ok(())
    });
    match rows {
        Ok(rows) => {
            writer.close()?;
            Ok((rows, Vec::new()))
        }
        Err(e) => Err(e),
    }
}
