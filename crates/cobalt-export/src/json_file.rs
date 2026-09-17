//! JSON (array of objects, streamed) and JSON Lines.

use crate::{Ctx, Progress, Result};
use arrow::array::RecordBatch;
use cobalt_results::CellValue;
use serde_json::{Map, Value};
use std::io::Write;

pub(crate) fn write<W: Write>(ctx: &Ctx<'_>, sink: &mut W, progress: &mut dyn FnMut(Progress) -> bool, lines: bool) -> Result<(usize, Vec<String>)> {
    let o = &ctx.opts.json;
    let names = ctx.names();
    let temporal: Vec<bool> = ctx.rs.columns.iter().map(|c| c.sql_type.is_temporal()).collect();
    let pretty = o.pretty && !lines;
    let mut first = true;
    if !lines {
        sink.write_all(if pretty { b"[\n" } else { b"[" })?;
    }
    let rows = ctx.for_each_batch(progress, |batch: &RecordBatch| {
        // Display text for temporal columns when the caller doesn't want ISO.
        let display: Vec<Option<std::sync::Arc<Vec<std::sync::Arc<str>>>>> = (0..batch.num_columns())
            .map(|c| if !o.dates_as_iso && temporal[c] { Some(ctx.fmt.format_column(batch.column(c), &ctx.rs.columns[c])) } else { None })
            .collect();
        let mut buf = Vec::with_capacity(batch.num_rows() * names.len() * 16);
        for r in 0..batch.num_rows() {
            let mut obj = Map::with_capacity(names.len());
            for (c, name) in names.iter().enumerate() {
                let v = CellValue::from_array(batch.column(c), r);
                let json = match (&v, &display[c]) {
                    (CellValue::Null, _) => {
                        if !o.null_as_null {
                            continue;
                        }
                        Value::Null
                    }
                    (_, Some(text)) => Value::String(text[r].to_string()),
                    _ => v.to_json(),
                };
                obj.insert(name.clone(), json);
            }
            if lines {
                serde_json::to_writer(&mut buf, &obj)?;
                buf.push(b'\n');
            } else {
                if !first {
                    buf.extend_from_slice(if pretty { b",\n" } else { b"," });
                }
                if pretty {
                    let s = serde_json::to_string_pretty(&obj)?;
                    buf.extend_from_slice(b"  ");
                    buf.extend_from_slice(s.replace('\n', "\n  ").as_bytes());
                } else {
                    serde_json::to_writer(&mut buf, &obj)?;
                }
            }
            first = false;
        }
        sink.write_all(&buf)?;
        Ok(())
    })?;
    if !lines {
        sink.write_all(if pretty { b"\n]\n" } else { b"]" })?;
    }
    Ok((rows, Vec::new()))
}
