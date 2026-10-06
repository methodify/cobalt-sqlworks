//! Export a notebook with its outputs as a standalone HTML page or a Markdown document.

use crate::model::*;
use pulldown_cmark::{html, Options, Parser};

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

pub fn markdown_to_html(md: &str) -> String {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TASKLISTS);
    let parser = Parser::new_ext(md, opts);
    let mut out = String::new();
    html::push_html(&mut out, parser);
    out
}

const CSS: &str = r#"
body { font-family: -apple-system, "Segoe UI", Roboto, Helvetica, Arial, sans-serif; max-width: 1100px; margin: 2em auto; padding: 0 1em; color: #1f2328; line-height: 1.5; }
.cell { margin: 1.2em 0; }
.code { background: #f6f8fa; border: 1px solid #d0d7de; border-radius: 6px; padding: .6em .8em; font-family: Consolas, "Cascadia Mono", Menlo, monospace; font-size: 13px; white-space: pre; overflow-x: auto; }
.count { color: #6e7781; font-family: Consolas, Menlo, monospace; font-size: 12px; margin-bottom: .2em; }
.output { margin-top: .5em; }
.output pre { background: #fff; border-left: 3px solid #d0d7de; padding: .4em .8em; font-size: 13px; overflow-x: auto; }
.output.error pre { border-left-color: #cf222e; color: #cf222e; }
.output table { border-collapse: collapse; font-size: 13px; }
.output th, .output td { border: 1px solid #d0d7de; padding: 2px 8px; text-align: left; white-space: nowrap; }
.output th { background: #f6f8fa; }
.rows { color: #6e7781; font-size: 12px; }
.md table { border-collapse: collapse; }
.md th, .md td { border: 1px solid #d0d7de; padding: 2px 8px; }
"#;

fn output_html(o: &Output) -> String {
    match o {
        Output::Stream { name, text } => format!("<div class=\"output{}\"><pre>{}</pre></div>\n", if name == "stderr" { " error" } else { "" }, escape(text)),
        Output::Error { ename, evalue, traceback } => {
            let body = if traceback.is_empty() { format!("{ename}: {evalue}") } else { strip_ansi(&traceback.join("\n")) };
            format!("<div class=\"output error\"><pre>{}</pre></div>\n", escape(&body))
        }
        Output::ExecuteResult { data, .. } | Output::DisplayData { data, .. } => {
            let rows = o.table_rows().map(|r| format!("<div class=\"rows\">{r} rows</div>")).unwrap_or_default();
            if let Some(h) = data.html() {
                format!("<div class=\"output\">{h}{rows}</div>\n")
            } else if let Some(m) = data.markdown() {
                format!("<div class=\"output md\">{}{rows}</div>\n", markdown_to_html(m))
            } else if let Some(png) = data.text(PNG_MIME) {
                format!("<div class=\"output\"><img src=\"data:image/png;base64,{}\"></div>\n", png.trim())
            } else {
                format!("<div class=\"output\"><pre>{}</pre></div>\n", escape(&o.preview_text()))
            }
        }
    }
}

pub fn to_html(nb: &Notebook, title: &str) -> String {
    let mut out = String::new();
    out.push_str("<!doctype html>\n<html><head><meta charset=\"utf-8\">\n<title>");
    out.push_str(&escape(title));
    out.push_str("</title>\n<style>");
    out.push_str(CSS);
    out.push_str("</style></head>\n<body>\n");
    for cell in &nb.cells {
        match cell.kind {
            CellKind::Markdown => {
                out.push_str("<div class=\"cell md\">\n");
                out.push_str(&markdown_to_html(&cell.source));
                out.push_str("</div>\n");
            }
            CellKind::Raw => {
                out.push_str("<div class=\"cell\"><pre>");
                out.push_str(&escape(&cell.source));
                out.push_str("</pre></div>\n");
            }
            CellKind::Code => {
                out.push_str("<div class=\"cell\">\n");
                let lang = nb.cell_language(cell);
                let count = cell.execution_count.map(|n| format!("[{n}]")).unwrap_or_else(|| "[ ]".into());
                out.push_str(&format!("<div class=\"count\">{count} {}</div>\n", escape(lang.label())));
                out.push_str("<div class=\"code\">");
                out.push_str(&escape(&cell.source));
                out.push_str("</div>\n");
                for o in &cell.outputs {
                    out.push_str(&output_html(o));
                }
                out.push_str("</div>\n");
            }
        }
    }
    out.push_str("</body></html>\n");
    out
}

fn fence_lang(l: &CellLanguage) -> &str {
    match l {
        CellLanguage::Sql => "sql",
        CellLanguage::Python => "python",
        CellLanguage::Scala => "scala",
        CellLanguage::R => "r",
        CellLanguage::Other(_) => "",
    }
}

pub fn to_markdown(nb: &Notebook) -> String {
    let mut out = String::new();
    for cell in &nb.cells {
        match cell.kind {
            CellKind::Markdown => {
                out.push_str(cell.source.trim_end());
                out.push_str("\n\n");
            }
            CellKind::Raw => {
                out.push_str("```\n");
                out.push_str(cell.source.trim_end());
                out.push_str("\n```\n\n");
            }
            CellKind::Code => {
                let lang = nb.cell_language(cell);
                out.push_str(&format!("```{}\n", fence_lang(&lang)));
                out.push_str(cell.source.trim_end());
                out.push_str("\n```\n\n");
                for o in &cell.outputs {
                    match o {
                        Output::ExecuteResult { data, .. } | Output::DisplayData { data, .. } if data.markdown().is_some() => {
                            out.push_str(data.markdown().unwrap().trim_end());
                            if let Some(r) = o.table_rows() {
                                out.push_str(&format!("\n\n*{r} rows*"));
                            }
                            out.push_str("\n\n");
                        }
                        Output::Error { .. } => {
                            out.push_str("```text\n");
                            out.push_str(o.preview_text().trim_end());
                            out.push_str("\n```\n\n");
                        }
                        _ => {
                            let t = o.preview_text();
                            if !t.trim().is_empty() {
                                out.push_str("```text\n");
                                out.push_str(t.trim_end());
                                out.push_str("\n```\n\n");
                            }
                        }
                    }
                }
            }
        }
    }
    out
}

impl Notebook {
    pub fn to_html(&self, title: &str) -> String {
        to_html(self, title)
    }
    pub fn to_markdown(&self) -> String {
        to_markdown(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_and_markdown() {
        let mut nb = Notebook::new(CellLanguage::Sql);
        nb.cells.push(Cell::markdown("# Title\n\nSome *text*"));
        let mut c = Cell::code("SELECT 1 AS <one>");
        c.execution_count = Some(2);
        c.outputs.push(Output::table(Some(2), b"x", "<table><tr><th>one</th></tr></table>".into(), "| one |\n|---|\n| 1 |".into(), "one\n1".into(), 1, false));
        c.outputs.push(Output::error("Err", "boom", vec![]));
        nb.cells.push(c);
        let html = to_html(&nb, "t");
        assert!(html.contains("<h1>Title</h1>"));
        assert!(html.contains("SELECT 1 AS &lt;one&gt;"));
        assert!(html.contains("<table><tr><th>one</th></tr></table>"));
        assert!(html.contains("Err: boom"));
        let md = to_markdown(&nb);
        assert!(md.starts_with("# Title\n\nSome *text*\n\n```sql\nSELECT 1 AS <one>\n```\n\n| one |"));
        assert!(md.contains("*1 rows*"));
        assert!(md.contains("```text\nErr: boom\n```"));
    }
}
