//! XML: element style `<rows><row><col>v</col></row></rows>` or attribute style `<row col="v"/>`.

use crate::{dedupe_names, Ctx, Progress, Result};
use quick_xml::events::{BytesDecl, BytesEnd, BytesStart, BytesText, Event};
use quick_xml::Writer;
use std::io::Write;

pub(crate) const XSI_NS: &str = "http://www.w3.org/2001/XMLSchema-instance";

/// Make a column name a valid XML `Name`: invalid characters become `_`, a leading digit (or
/// empty name) gets a `_` prefix. `[a b]` → `a_b`.
pub fn xml_name(name: &str) -> String {
    let trimmed = name.trim().trim_start_matches('[').trim_end_matches(']');
    let mut out = String::with_capacity(trimmed.len() + 1);
    for (i, ch) in trimmed.chars().enumerate() {
        let ok = if i == 0 { ch.is_alphabetic() || ch == '_' } else { ch.is_alphanumeric() || ch == '_' || ch == '-' || ch == '.' };
        if ok {
            out.push(ch);
        } else if i == 0 && ch.is_ascii_digit() {
            out.push('_');
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() || out.eq_ignore_ascii_case("xml") {
        out.insert(0, '_');
    }
    out
}

pub(crate) fn write<W: Write>(ctx: &Ctx<'_>, sink: &mut W, progress: &mut dyn FnMut(Progress) -> bool) -> Result<(usize, Vec<String>)> {
    let o = &ctx.opts.xml;
    let names = dedupe_names(ctx.rs.columns.iter().map(|c| c.name.as_str()));
    let names: Vec<String> = dedupe_names(names.iter().map(|n| xml_name(n)).collect::<Vec<_>>().iter().map(String::as_str));
    let root = xml_name(&o.root_element);
    let row_el = xml_name(&o.row_element);
    let nl: &[u8] = if o.formatted { b"\n" } else { b"" };
    let indent = |n: usize| -> Vec<u8> { if o.formatted { vec![b' '; n * 2] } else { Vec::new() } };

    // Prologue.
    {
        let mut w = Writer::new(Vec::new());
        w.write_event(Event::Decl(BytesDecl::new("1.0", Some("utf-8"), None)))?;
        let mut buf = w.into_inner();
        buf.extend_from_slice(nl);
        if o.include_schema_comment {
            let cols: Vec<String> = ctx.rs.columns.iter().map(|c| format!("{} {}", c.name, c.sql_type)).collect();
            let mut w = Writer::new(Vec::new());
            w.write_event(Event::Comment(BytesText::new(&format!(" columns: {} ", cols.join(", ").replace("--", "- -")))))?;
            buf.extend_from_slice(&w.into_inner());
            buf.extend_from_slice(nl);
        }
        let mut w = Writer::new(Vec::new());
        let mut start = BytesStart::new(root.as_str());
        if !o.attribute_style {
            start.push_attribute(("xmlns:xsi", XSI_NS));
        }
        w.write_event(Event::Start(start))?;
        buf.extend_from_slice(&w.into_inner());
        buf.extend_from_slice(nl);
        sink.write_all(&buf)?;
    }

    let rows = ctx.for_each_batch(progress, |batch| {
        let cols = ctx.format_batch(batch, &ctx.fmt);
        let mut w = Writer::new(Vec::with_capacity(batch.num_rows() * names.len() * 24));
        for r in 0..batch.num_rows() {
            w.get_mut().extend_from_slice(&indent(1));
            if o.attribute_style {
                let mut start = BytesStart::new(row_el.as_str());
                for (c, name) in names.iter().enumerate() {
                    if !batch.column(c).is_null(r) {
                        start.push_attribute((name.as_str(), &*cols[c][r]));
                    }
                }
                w.write_event(Event::Empty(start))?;
            } else {
                w.write_event(Event::Start(BytesStart::new(row_el.as_str())))?;
                for (c, name) in names.iter().enumerate() {
                    w.get_mut().extend_from_slice(nl);
                    w.get_mut().extend_from_slice(&indent(2));
                    if batch.column(c).is_null(r) {
                        let mut e = BytesStart::new(name.as_str());
                        e.push_attribute(("xsi:nil", "true"));
                        w.write_event(Event::Empty(e))?;
                    } else {
                        w.write_event(Event::Start(BytesStart::new(name.as_str())))?;
                        w.write_event(Event::Text(BytesText::new(&cols[c][r])))?;
                        w.write_event(Event::End(BytesEnd::new(name.as_str())))?;
                    }
                }
                w.get_mut().extend_from_slice(nl);
                w.get_mut().extend_from_slice(&indent(1));
                w.write_event(Event::End(BytesEnd::new(row_el.as_str())))?;
            }
            w.get_mut().extend_from_slice(nl);
        }
        sink.write_all(&w.into_inner())?;
        Ok(())
    })?;

    let mut w = Writer::new(Vec::new());
    w.write_event(Event::End(BytesEnd::new(root.as_str())))?;
    let mut buf = w.into_inner();
    buf.extend_from_slice(nl);
    sink.write_all(&buf)?;
    Ok((rows, Vec::new()))
}

#[cfg(test)]
mod tests {
    use super::xml_name;
    #[test]
    fn names() {
        assert_eq!(xml_name("[a b]"), "a_b");
        assert_eq!(xml_name("1st"), "_1st");
        assert_eq!(xml_name(""), "_");
        assert_eq!(xml_name("ok.name-1"), "ok.name-1");
        assert_eq!(xml_name("a/b"), "a_b");
        assert_eq!(xml_name("xml"), "_xml");
    }
}
