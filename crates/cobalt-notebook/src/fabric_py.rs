//! The Fabric Git form of a notebook (`notebook-content.py`): a flat sequence of blocks introduced
//! by marker lines. Mirrors the parser in local-spark-mcp (`notebook.py`), which follows the
//! empirically derived format (there is no published spec).
//!
//! ```text
//! # Fabric notebook source
//!
//! # METADATA ********************      "# META "-prefixed JSON (file level first, then after each code cell)
//! # CELL ********************          code cell, source verbatim ("# MAGIC "-prefixed when a %%magic switches language)
//! # PARAMETERS CELL ********************
//! # MARKDOWN ********************      every line prefixed "# "
//! ```

use crate::model::*;
use serde_json::{Map, Value};

pub const HEADER: &str = "# Fabric notebook source";
const STARS: &str = "********************";
const META_PREFIX: &str = "# META ";
const MAGIC_PREFIX: &str = "# MAGIC ";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Block {
    Metadata,
    Cell,
    Parameters,
    Markdown,
}

fn marker(line: &str) -> Option<Block> {
    let rest = line.strip_prefix("# ")?;
    let (name, stars) = rest.rsplit_once(' ')?;
    if stars.is_empty() || !stars.chars().all(|c| c == '*') {
        return None;
    }
    Some(match name {
        "METADATA" => Block::Metadata,
        "CELL" => Block::Cell,
        "PARAMETERS CELL" => Block::Parameters,
        "MARKDOWN" => Block::Markdown,
        _ => return None,
    })
}

fn parse_meta(lines: &[&str], line_no: usize, warnings: &mut Vec<String>) -> Map<String, Value> {
    let mut payload: Vec<&str> = Vec::new();
    for raw in lines {
        if let Some(p) = raw.strip_prefix(META_PREFIX) {
            payload.push(p);
        } else if raw.trim() == "# META" {
            payload.push("");
        } else if raw.trim().is_empty() {
            continue;
        } else {
            warnings.push(format!("line {line_no}: non-META line inside a METADATA block: {}", raw.chars().take(60).collect::<String>()));
        }
    }
    if payload.is_empty() {
        return Map::new();
    }
    match serde_json::from_str::<Value>(&payload.join("\n")) {
        Ok(Value::Object(m)) => m,
        Ok(_) => {
            warnings.push(format!("line {line_no}: METADATA is not a JSON object"));
            Map::new()
        }
        Err(e) => {
            warnings.push(format!("line {line_no}: METADATA JSON does not parse ({e})"));
            Map::new()
        }
    }
}

fn decode_code(content: &[&str]) -> String {
    let nonblank: Vec<&&str> = content.iter().filter(|l| !l.trim().is_empty()).collect();
    let magic = !nonblank.is_empty() && nonblank.iter().all(|l| l.starts_with("# MAGIC"));
    let lines: Vec<String> = content
        .iter()
        .map(|l| {
            if magic {
                if let Some(rest) = l.strip_prefix(MAGIC_PREFIX) {
                    rest.to_string()
                } else if l.trim() == "# MAGIC" {
                    String::new()
                } else {
                    l.to_string()
                }
            } else {
                l.to_string()
            }
        })
        .collect();
    trim_blank_edges(lines).join("\n")
}

fn decode_markdown(content: &[&str]) -> String {
    let lines: Vec<String> = content
        .iter()
        .map(|l| {
            if let Some(rest) = l.strip_prefix("# ") {
                rest.to_string()
            } else if *l == "#" {
                String::new()
            } else {
                l.to_string()
            }
        })
        .collect();
    trim_blank_edges(lines).join("\n")
}

fn trim_blank_edges(mut lines: Vec<String>) -> Vec<String> {
    while lines.first().map(|l| l.trim().is_empty()).unwrap_or(false) {
        lines.remove(0);
    }
    while lines.last().map(|l| l.trim().is_empty()).unwrap_or(false) {
        lines.pop();
    }
    lines
}

/// Parse the Git form. Never refuses a file: problems come back as warnings.
pub fn parse(text: &str) -> (Notebook, Vec<String>) {
    let text = text.replace("\r\n", "\n");
    let lines: Vec<&str> = text.split('\n').collect();
    let mut warnings = Vec::new();
    if lines.first().copied() != Some(HEADER) {
        warnings.push(format!("line 1: expected {HEADER:?}"));
    }
    // split into (block, start line index, content lines)
    let mut blocks: Vec<(Block, usize, Vec<&str>)> = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        if let Some(b) = marker(l) {
            blocks.push((b, i + 1, Vec::new()));
        } else if let Some(cur) = blocks.last_mut() {
            cur.2.push(l);
        }
    }
    let mut nb = Notebook::new(CellLanguage::Python);
    let mut seen_file_meta = false;
    let mut last_code: Option<usize> = None;
    for (block, line_no, content) in blocks {
        match block {
            Block::Metadata => {
                let meta = parse_meta(&content, line_no, &mut warnings);
                match last_code.take() {
                    Some(ci) => {
                        let cell = &mut nb.cells[ci];
                        // {"language": "python", "language_group": "synapse_pyspark", ...} → microsoft.*
                        let mut ms = Map::new();
                        for (k, v) in meta {
                            if k == "language" || k == "language_group" {
                                ms.insert(k, v);
                            } else {
                                cell.metadata.insert(k, v);
                            }
                        }
                        if !ms.is_empty() {
                            cell.metadata.insert("microsoft".into(), Value::Object(ms));
                        }
                    }
                    None if !seen_file_meta => {
                        seen_file_meta = true;
                        for (k, v) in meta {
                            nb.metadata.insert(k, v);
                        }
                    }
                    None => warnings.push(format!("line {line_no}: METADATA block that follows no code cell")),
                }
            }
            Block::Cell | Block::Parameters => {
                let mut cell = Cell::code(decode_code(&content));
                if block == Block::Parameters {
                    cell.set_parameters(true);
                }
                nb.cells.push(cell);
                last_code = Some(nb.cells.len() - 1);
            }
            Block::Markdown => {
                nb.cells.push(Cell::markdown(decode_markdown(&content)));
                last_code = None;
            }
        }
    }
    if !nb.metadata.contains_key("microsoft") {
        nb.metadata.insert("microsoft".into(), serde_json::json!({"language": "python", "language_group": "synapse_pyspark"}));
    }
    (nb, warnings)
}

fn meta_block(v: &Value) -> String {
    let text = serde_json::to_string_pretty(v).unwrap_or_default();
    let mut out = String::new();
    out.push_str(&format!("# METADATA {STARS}\n\n"));
    for l in text.lines() {
        if l.is_empty() {
            out.push_str("# META\n");
        } else {
            out.push_str(META_PREFIX);
            out.push_str(l);
            out.push('\n');
        }
    }
    out.push('\n');
    out
}

/// Write the Git form. File-level metadata keeps `kernel_info`, `dependencies` and anything
/// Fabric put there besides the Jupyter keys.
pub fn to_string(nb: &Notebook) -> String {
    let mut out = String::new();
    out.push_str(HEADER);
    out.push_str("\n\n");
    let mut file_meta = Map::new();
    for (k, v) in &nb.metadata {
        if matches!(k.as_str(), "kernelspec" | "language_info" | "microsoft" | "nteract" | "widgets") {
            continue;
        }
        file_meta.insert(k.clone(), v.clone());
    }
    if !file_meta.contains_key("kernel_info") {
        file_meta.insert("kernel_info".into(), serde_json::json!({"name": "synapse_pyspark"}));
    }
    if !file_meta.contains_key("dependencies") {
        file_meta.insert("dependencies".into(), Value::Object(Map::new()));
    }
    out.push_str(&meta_block(&Value::Object(file_meta)));
    let group = nb.language_group().unwrap_or_else(|| "synapse_pyspark".into());
    for cell in &nb.cells {
        match cell.kind {
            CellKind::Markdown | CellKind::Raw => {
                out.push_str(&format!("# MARKDOWN {STARS}\n\n"));
                for l in cell.source.lines() {
                    if l.is_empty() {
                        out.push_str("#\n");
                    } else {
                        out.push_str("# ");
                        out.push_str(l);
                        out.push('\n');
                    }
                }
                out.push('\n');
            }
            CellKind::Code => {
                out.push_str(&format!("# {} {STARS}\n\n", if cell.is_parameters() { "PARAMETERS CELL" } else { "CELL" }));
                let lang = nb.cell_language(cell);
                let (magic, _) = cell.split_magic();
                let needs_magic_prefix = magic.is_some() || lang != CellLanguage::Python;
                let source = if needs_magic_prefix && magic.is_none() {
                    // a non-Python cell without its magic line gets one so Fabric runs it right
                    let m = match &lang {
                        CellLanguage::Sql => "%%sql",
                        CellLanguage::Scala => "%%spark",
                        CellLanguage::R => "%%sparkr",
                        _ => "%%pyspark",
                    };
                    format!("{m}\n{}", cell.source)
                } else {
                    cell.source.clone()
                };
                for l in source.lines() {
                    if needs_magic_prefix {
                        if l.is_empty() {
                            out.push_str("# MAGIC\n");
                        } else {
                            out.push_str(MAGIC_PREFIX);
                            out.push_str(l);
                            out.push('\n');
                        }
                    } else {
                        out.push_str(l);
                        out.push('\n');
                    }
                }
                out.push('\n');
                let mut meta = Map::new();
                meta.insert("language".into(), Value::String(lang.fabric_name().to_string()));
                meta.insert("language_group".into(), Value::String(group.clone()));
                for (k, v) in &cell.metadata {
                    if k == "microsoft" {
                        continue;
                    }
                    if k == "tags" {
                        // the PARAMETERS CELL marker carries that tag
                        let rest: Vec<Value> = v.as_array().map(|a| a.iter().filter(|t| t.as_str() != Some("parameters")).cloned().collect()).unwrap_or_default();
                        if !rest.is_empty() {
                            meta.insert(k.clone(), Value::Array(rest));
                        }
                        continue;
                    }
                    meta.insert(k.clone(), v.clone());
                }
                out.push_str(&meta_block(&Value::Object(meta)));
            }
        }
    }
    let end = out.trim_end().len();
    out.truncate(end);
    out.push('\n');
    out
}

impl Notebook {
    pub fn from_fabric_py(text: &str) -> (Self, Vec<String>) {
        parse(text)
    }
    pub fn to_fabric_py(&self) -> String {
        to_string(self)
    }
    pub fn looks_like_fabric_py(text: &str) -> bool {
        text.trim_start_matches('\u{feff}').starts_with(HEADER)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "# Fabric notebook source\n\n# METADATA ********************\n\n# META {\n# META   \"kernel_info\": {\n# META     \"name\": \"synapse_pyspark\"\n# META   },\n# META   \"dependencies\": {\n# META     \"lakehouse\": {\n# META       \"default_lakehouse\": \"lh\",\n# META       \"default_lakehouse_name\": \"test\",\n# META       \"default_lakehouse_workspace_id\": \"ws\"\n# META     }\n# META   }\n# META }\n\n# MARKDOWN ********************\n\n# # Sales\n#\n# Daily totals.\n\n# PARAMETERS CELL ********************\n\nday = \"2026-01-01\"\n\n# METADATA ********************\n\n# META {\n# META   \"language\": \"python\",\n# META   \"language_group\": \"synapse_pyspark\"\n# META }\n\n# CELL ********************\n\n# MAGIC %%sql\n# MAGIC SELECT COUNT(*) FROM test.sales\n# MAGIC\n# MAGIC -- end\n\n# METADATA ********************\n\n# META {\n# META   \"language\": \"sparksql\",\n# META   \"language_group\": \"synapse_pyspark\"\n# META }\n\n# CELL ********************\n\ndf = spark.sql(\"SELECT 1\")\ndisplay(df)\n\n# METADATA ********************\n\n# META {\n# META   \"language\": \"python\",\n# META   \"language_group\": \"synapse_pyspark\"\n# META }\n";

    #[test]
    fn parses_git_form() {
        let (nb, warnings) = parse(SAMPLE);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(nb.cells.len(), 4);
        assert_eq!(nb.cells[0].kind, CellKind::Markdown);
        assert_eq!(nb.cells[0].source, "# Sales\n\nDaily totals.");
        assert!(nb.cells[1].is_parameters());
        assert_eq!(nb.cells[1].source, "day = \"2026-01-01\"");
        assert_eq!(nb.cell_language(&nb.cells[2]), CellLanguage::Sql);
        assert_eq!(nb.cells[2].source, "%%sql\nSELECT COUNT(*) FROM test.sales\n\n-- end");
        assert_eq!(nb.cells[2].split_magic().1, "SELECT COUNT(*) FROM test.sales\n\n-- end");
        assert_eq!(nb.cell_language(&nb.cells[3]), CellLanguage::Python);
        assert_eq!(nb.default_lakehouse().unwrap().name.as_deref(), Some("test"));
    }

    #[test]
    fn roundtrips_git_form() {
        let (nb, _) = parse(SAMPLE);
        let text = to_string(&nb);
        assert_eq!(text, SAMPLE);
        let (again, w) = parse(&text);
        assert!(w.is_empty());
        assert_eq!(again.cells.iter().map(|c| (&c.source, c.kind)).collect::<Vec<_>>(), nb.cells.iter().map(|c| (&c.source, c.kind)).collect::<Vec<_>>());
    }

    #[test]
    fn warns_on_bad_header_and_meta() {
        let (nb, warnings) = parse("# CELL ********************\n\nx = 1\n\n# METADATA ********************\n\n# META {not json\n");
        assert_eq!(nb.cells.len(), 1);
        assert!(warnings.iter().any(|w| w.contains("expected")));
        assert!(warnings.iter().any(|w| w.contains("does not parse")));
    }

    #[test]
    fn sql_cell_without_magic_gets_one() {
        let mut nb = Notebook::new(CellLanguage::Python);
        let mut c = Cell::code("SELECT 1");
        c.set_language_override(Some(&CellLanguage::Sql), None);
        nb.cells.push(c);
        let text = to_string(&nb);
        assert!(text.contains("# MAGIC %%sql\n# MAGIC SELECT 1\n"));
        let (again, _) = parse(&text);
        assert_eq!(again.cell_language(&again.cells[0]), CellLanguage::Sql);
    }
}
