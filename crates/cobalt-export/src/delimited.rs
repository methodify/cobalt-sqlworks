//! CSV / TSV: RFC 4180 quoting through the `csv` crate, configurable delimiter, quote style,
//! line ending, BOM, NULL text, and UTF-8 / UTF-16LE encoding.

use crate::{Ctx, Encoding, LineEnding, Progress, Result};
use csv::{QuoteStyle, Terminator, WriterBuilder};
use std::io::Write;

pub(crate) fn write<W: Write>(ctx: &Ctx<'_, '_>, sink: &mut W, progress: &mut dyn FnMut(Progress) -> bool, delimiter: u8) -> Result<(usize, Vec<String>)> {
    let o = &ctx.opts.csv;
    let fmt = ctx.fmt.clone().with_null_text(&o.null_as);
    let mut builder = WriterBuilder::new();
    builder
        .delimiter(delimiter)
        .has_headers(false)
        .quote_style(if o.quote_all { QuoteStyle::Always } else { QuoteStyle::Necessary })
        .terminator(match o.line_ending {
            LineEnding::Crlf => Terminator::CRLF,
            LineEnding::Lf => Terminator::Any(b'\n'),
        });
    let encoding = o.encoding;

    match encoding {
        Encoding::Utf8 if o.bom => sink.write_all(&[0xEF, 0xBB, 0xBF])?,
        Encoding::Utf16Le => sink.write_all(&[0xFF, 0xFE])?,
        _ => {}
    }

    let emit = |buf: Vec<u8>, sink: &mut W| -> Result<()> {
        match encoding {
            Encoding::Utf8 => sink.write_all(&buf)?,
            Encoding::Utf16Le => {
                let text = String::from_utf8(buf).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
                let mut out = Vec::with_capacity(text.len() * 2);
                for unit in text.encode_utf16() {
                    out.extend_from_slice(&unit.to_le_bytes());
                }
                sink.write_all(&out)?;
            }
        }
        Ok(())
    };

    if o.include_headers {
        let mut w = builder.from_writer(Vec::new());
        w.write_record(ctx.columns.iter().map(|c| c.name.as_str()))?;
        emit(w.into_inner().map_err(|e| e.into_error())?, sink)?;
    }

    let rows = ctx.for_each_batch(progress, |batch| {
        let cols = ctx.format_batch(batch, &fmt);
        let mut w = builder.from_writer(Vec::with_capacity(batch.num_rows() * cols.len() * 12));
        for r in 0..batch.num_rows() {
            for col in &cols {
                w.write_field(col[r].as_bytes())?;
            }
            w.write_record(None::<&[u8]>)?;
        }
        emit(w.into_inner().map_err(|e| e.into_error())?, sink)
    })?;
    Ok((rows, Vec::new()))
}
