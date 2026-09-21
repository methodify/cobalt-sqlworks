//! GitHub-flavoured Markdown table.

use crate::{Ctx, Progress, Result};
use cobalt_core::ColumnInfo;
use std::io::Write;

/// Escape a cell for a GFM table: `|` → `\|`, line breaks → `<br>`.
pub fn escape_cell(s: &str, escape_pipes: bool) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '|' if escape_pipes => out.push_str("\\|"),
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push_str("<br>");
            }
            '\n' => out.push_str("<br>"),
            c => out.push(c),
        }
    }
    out
}

/// `| a | b |` header + `|---:|:---|` separator lines.
pub fn header_lines(columns: &[ColumnInfo], align_numbers_right: bool, escape_pipes: bool, line_ending: &str) -> String {
    let mut s = String::new();
    s.push('|');
    for c in columns {
        s.push(' ');
        s.push_str(&escape_cell(&c.name, escape_pipes));
        s.push_str(" |");
    }
    s.push_str(line_ending);
    s.push('|');
    for c in columns {
        s.push_str(if align_numbers_right && c.sql_type.right_align() { "---:|" } else { ":---|" });
    }
    s.push_str(line_ending);
    s
}

pub(crate) fn write<W: Write>(ctx: &Ctx<'_, '_>, sink: &mut W, progress: &mut dyn FnMut(Progress) -> bool) -> Result<(usize, Vec<String>)> {
    let o = &ctx.opts.markdown;
    let fmt = ctx.fmt.clone().with_null_text(&o.null_as);
    let nl = "\n";
    if o.include_headers {
        sink.write_all(header_lines(ctx.columns, o.align_numbers_right, o.escape_pipes, nl).as_bytes())?;
    }
    let rows = ctx.for_each_batch(progress, |batch| {
        let cols = ctx.format_batch(batch, &fmt);
        let mut buf = String::with_capacity(batch.num_rows() * cols.len() * 12);
        for r in 0..batch.num_rows() {
            buf.push('|');
            for col in &cols {
                buf.push(' ');
                buf.push_str(&escape_cell(&col[r], o.escape_pipes));
                buf.push_str(" |");
            }
            buf.push_str(nl);
        }
        sink.write_all(buf.as_bytes())?;
        Ok(())
    })?;
    Ok((rows, Vec::new()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn escaping() {
        assert_eq!(escape_cell("a|b", true), "a\\|b");
        assert_eq!(escape_cell("a|b", false), "a|b");
        assert_eq!(escape_cell("l1\r\nl2\nl3", true), "l1<br>l2<br>l3");
    }
}
