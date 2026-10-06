//! nbformat 4 read/write. Unknown keys in notebook, cell and output metadata are kept verbatim.

use crate::model::*;
use crate::{NotebookError, Result};
use serde_json::{Map, Value};

/// nbformat "multiline string": a string or an array of line strings.
fn multiline(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().filter_map(Value::as_str).collect::<String>(),
        _ => String::new(),
    }
}

/// Split into nbformat's line array (each line keeps its newline).
fn to_lines(s: &str) -> Value {
    if s.is_empty() {
        return Value::Array(Vec::new());
    }
    Value::Array(s.split_inclusive('\n').map(|l| Value::String(l.to_string())).collect())
}

fn is_json_mime(mime: &str) -> bool {
    mime == "application/json" || mime.ends_with("+json")
}

fn parse_bundle(v: Option<&Value>) -> MimeBundle {
    let mut b = MimeBundle::default();
    if let Some(Value::Object(m)) = v {
        for (k, v) in m {
            if is_json_mime(k) && !v.is_string() && !v.is_array() {
                b.entries.insert(k.clone(), v.clone());
            } else if is_json_mime(k) && v.is_array() && v.as_array().map(|a| a.iter().all(Value::is_string)).unwrap_or(false) {
                // some writers line-split JSON mimes too; keep the parsed object when it parses
                let text = multiline(v);
                b.entries.insert(k.clone(), serde_json::from_str(&text).unwrap_or(Value::String(text)));
            } else if v.is_string() || v.is_array() {
                b.entries.insert(k.clone(), Value::String(multiline(v)));
            } else {
                b.entries.insert(k.clone(), v.clone());
            }
        }
    }
    b
}

fn bundle_json(b: &MimeBundle) -> Value {
    let mut m = Map::new();
    for (k, v) in &b.entries {
        match v {
            Value::String(s) if !is_json_mime(k) => {
                // binary payloads stay one string (Jupyter writes image/png that way)
                if k.starts_with("image/") || k == ARROW_MIME {
                    m.insert(k.clone(), Value::String(s.clone()));
                } else {
                    m.insert(k.clone(), to_lines(s));
                }
            }
            other => {
                m.insert(k.clone(), other.clone());
            }
        }
    }
    Value::Object(m)
}

fn obj(v: Option<&Value>) -> Map<String, Value> {
    match v {
        Some(Value::Object(m)) => m.clone(),
        _ => Map::new(),
    }
}

fn parse_output(v: &Value) -> Option<Output> {
    let t = v.get("output_type")?.as_str()?;
    Some(match t {
        "stream" => Output::Stream { name: v.get("name").and_then(Value::as_str).unwrap_or("stdout").to_string(), text: v.get("text").map(multiline).unwrap_or_default() },
        "execute_result" => Output::ExecuteResult { execution_count: v.get("execution_count").and_then(Value::as_u64), data: parse_bundle(v.get("data")), metadata: obj(v.get("metadata")) },
        "display_data" => Output::DisplayData { data: parse_bundle(v.get("data")), metadata: obj(v.get("metadata")) },
        "error" => Output::Error {
            ename: v.get("ename").and_then(Value::as_str).unwrap_or_default().to_string(),
            evalue: v.get("evalue").and_then(Value::as_str).unwrap_or_default().to_string(),
            traceback: v.get("traceback").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default(),
        },
        _ => return None,
    })
}

fn output_json(o: &Output) -> Value {
    match o {
        Output::Stream { name, text } => serde_json::json!({"output_type": "stream", "name": name, "text": to_lines(text)}),
        Output::ExecuteResult { execution_count, data, metadata } => {
            serde_json::json!({"output_type": "execute_result", "execution_count": execution_count, "data": bundle_json(data), "metadata": Value::Object(metadata.clone())})
        }
        Output::DisplayData { data, metadata } => serde_json::json!({"output_type": "display_data", "data": bundle_json(data), "metadata": Value::Object(metadata.clone())}),
        Output::Error { ename, evalue, traceback } => serde_json::json!({"output_type": "error", "ename": ename, "evalue": evalue, "traceback": traceback}),
    }
}

pub fn parse(text: &str) -> Result<Notebook> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let v: Value = serde_json::from_str(text)?;
    let Value::Object(root) = v else { return Err(NotebookError::Format("top level is not an object".into())) };
    let nbformat = root.get("nbformat").and_then(Value::as_u64).ok_or_else(|| NotebookError::Format("no nbformat key".into()))?;
    if nbformat != 4 {
        return Err(NotebookError::Format(format!("nbformat {nbformat} is not supported (only 4)")));
    }
    let nbformat_minor = root.get("nbformat_minor").and_then(Value::as_u64).unwrap_or(5);
    let metadata = obj(root.get("metadata"));
    let mut cells = Vec::new();
    for (i, c) in root.get("cells").and_then(Value::as_array).into_iter().flatten().enumerate() {
        let kind = CellKind::parse(c.get("cell_type").and_then(Value::as_str).unwrap_or("code"));
        let mut metadata = obj(c.get("metadata"));
        // ADS keeps its cell id in metadata; nbformat 4.5 has a top-level id
        let id = c
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| metadata.get("azdata_cell_guid").and_then(Value::as_str).map(str::to_string))
            .unwrap_or_else(|| format!("cell-{i}-{}", new_cell_id()));
        if kind != CellKind::Code {
            metadata.remove("execution");
        }
        let outputs = c.get("outputs").and_then(Value::as_array).map(|a| a.iter().filter_map(parse_output).collect()).unwrap_or_default();
        cells.push(Cell { id, kind, source: c.get("source").map(multiline).unwrap_or_default(), metadata, outputs, execution_count: c.get("execution_count").and_then(Value::as_u64) });
    }
    Ok(Notebook { metadata, cells, nbformat, nbformat_minor: nbformat_minor.max(5) })
}

pub fn to_value(nb: &Notebook) -> Value {
    let cells: Vec<Value> = nb
        .cells
        .iter()
        .map(|c| {
            let mut m = Map::new();
            m.insert("cell_type".into(), Value::String(c.kind.as_str().into()));
            m.insert("id".into(), Value::String(c.id.clone()));
            m.insert("metadata".into(), Value::Object(c.metadata.clone()));
            m.insert("source".into(), to_lines(&c.source));
            if c.kind == CellKind::Code {
                m.insert("execution_count".into(), c.execution_count.map(Value::from).unwrap_or(Value::Null));
                m.insert("outputs".into(), Value::Array(c.outputs.iter().map(output_json).collect()));
            }
            Value::Object(m)
        })
        .collect();
    let mut root = Map::new();
    root.insert("cells".into(), Value::Array(cells));
    root.insert("metadata".into(), Value::Object(nb.metadata.clone()));
    root.insert("nbformat".into(), Value::from(nb.nbformat));
    root.insert("nbformat_minor".into(), Value::from(nb.nbformat_minor));
    Value::Object(root)
}

/// Pretty JSON with a one-space indent (what Jupyter writes) and a trailing newline.
pub fn to_string(nb: &Notebook) -> String {
    let v = to_value(nb);
    let mut buf = Vec::new();
    let fmt = serde_json::ser::PrettyFormatter::with_indent(b" ");
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, fmt);
    serde::Serialize::serialize(&v, &mut ser).expect("json");
    let mut s = String::from_utf8(buf).expect("utf8");
    s.push('\n');
    s
}

impl Notebook {
    pub fn parse(text: &str) -> Result<Self> {
        parse(text)
    }
    pub fn to_ipynb(&self) -> String {
        to_string(self)
    }
    /// Cheap identity for dirty tracking: hashes the serialized document.
    pub fn content_hash(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.to_ipynb().hash(&mut h);
        h.finish()
    }
    /// True when the text looks like an .ipynb document (cheap check before parsing).
    pub fn looks_like_ipynb(text: &str) -> bool {
        let t = text.trim_start_matches('\u{feff}').trim_start();
        t.starts_with('{') && t.contains("\"nbformat\"") && t.contains("\"cells\"")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADS: &str = r##"{
    "metadata": {
        "kernelspec": {"name": "SQL", "display_name": "SQL", "language": "sql"},
        "language_info": {"name": "sql", "version": ""}
    },
    "nbformat_minor": 2,
    "nbformat": 4,
    "cells": [
        {"cell_type": "markdown", "source": ["# Title\n", "text"], "metadata": {"azdata_cell_guid": "aaaa-1"}},
        {"cell_type": "code", "source": ["SELECT 1 AS one"], "metadata": {"azdata_cell_guid": "bbbb-2", "tags": []},
         "outputs": [
            {"output_type": "display_data", "data": {"text/html": ["<table>", "</table>"]}, "metadata": {}},
            {"output_type": "execute_result", "metadata": {}, "execution_count": 1,
             "data": {"application/vnd.dataresource+json": {"schema": {"fields": [{"name": "one"}]}, "data": [{"one": "1"}]}, "text/html": ["<table></table>"]}}
         ],
         "execution_count": 1}
    ]
}"##;

    #[test]
    fn reads_ads_notebook() {
        let nb = parse(ADS).unwrap();
        assert_eq!(nb.default_language(), CellLanguage::Sql);
        assert_eq!(nb.cells.len(), 2);
        assert_eq!(nb.cells[0].kind, CellKind::Markdown);
        assert_eq!(nb.cells[0].source, "# Title\ntext");
        assert_eq!(nb.cells[0].id, "aaaa-1");
        assert_eq!(nb.cells[1].execution_count, Some(1));
        assert_eq!(nb.cells[1].outputs.len(), 2);
        let data = nb.cells[1].outputs[1].data().unwrap();
        assert!(data.json(DATARESOURCE_MIME).is_some());
        assert_eq!(data.html(), Some("<table></table>"));
        assert_eq!(nb.title_hint().as_deref(), Some("Title"));
    }

    #[test]
    fn roundtrip_keeps_metadata_and_outputs() {
        let nb = parse(ADS).unwrap();
        let text = to_string(&nb);
        let again = parse(&text).unwrap();
        assert_eq!(nb, again);
        assert!(text.starts_with("{\n \"cells\""));
        assert!(text.ends_with("}\n"));
        // minor is bumped to 4.5 so cell ids are legal
        assert_eq!(again.nbformat_minor, 5);
    }

    #[test]
    fn fabric_metadata() {
        let text = r#"{"nbformat":4,"nbformat_minor":5,"metadata":{"kernel_info":{"name":"synapse_pyspark"},"language_info":{"name":"python"},"microsoft":{"language":"python","language_group":"synapse_pyspark"},"dependencies":{"lakehouse":{"default_lakehouse":"lh-id","default_lakehouse_name":"test","default_lakehouse_workspace_id":"ws-id","known_lakehouses":[{"id":"lh-id"}]}}},
          "cells":[{"cell_type":"code","id":"c1","metadata":{"microsoft":{"language":"sparksql","language_group":"synapse_pyspark"}},"source":"%%sql\nSELECT 1","outputs":[],"execution_count":null},
                   {"cell_type":"code","id":"c2","metadata":{"tags":["parameters"]},"source":"x = 1","outputs":[{"output_type":"stream","name":"stdout","text":["hi\n"]},{"output_type":"error","ename":"E","evalue":"v","traceback":["a","b"]}],"execution_count":2}]}"#;
        let nb = parse(text).unwrap();
        assert!(nb.is_fabric());
        assert_eq!(nb.default_language(), CellLanguage::Python);
        assert_eq!(nb.cell_language(&nb.cells[0]), CellLanguage::Sql);
        assert!(nb.cells[1].is_parameters());
        let lh = nb.default_lakehouse().unwrap();
        assert_eq!(lh.name.as_deref(), Some("test"));
        assert_eq!(lh.workspace_id.as_deref(), Some("ws-id"));
        assert!(matches!(&nb.cells[1].outputs[1], Output::Error { ename, .. } if ename == "E"));
        assert_eq!(parse(&to_string(&nb)).unwrap(), nb);
    }

    #[test]
    fn table_output_survives() {
        let mut nb = Notebook::new(CellLanguage::Sql);
        let mut c = Cell::code("SELECT 1");
        c.outputs.push(Output::table(Some(1), &[1, 2, 3, 255], "<table/>".into(), "|x|".into(), "x".into(), 1, false));
        nb.cells.push(c);
        let again = parse(&to_string(&nb)).unwrap();
        assert_eq!(again.cells[0].outputs[0].data().unwrap().arrow().unwrap(), vec![1, 2, 3, 255]);
        assert_eq!(again.cells[0].outputs[0].table_rows(), Some(1));
    }

    #[test]
    fn rejects_non_notebooks() {
        assert!(parse("SELECT 1").is_err());
        assert!(parse("{\"nbformat\": 3, \"cells\": []}").is_err());
        assert!(Notebook::looks_like_ipynb("{\"cells\": [], \"nbformat\": 4}"));
        assert!(!Notebook::looks_like_ipynb("SELECT 1"));
    }
}
