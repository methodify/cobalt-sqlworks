//! The in-memory notebook: cells, outputs, and the metadata conventions Cobalt understands.

use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// Arrow IPC stream, base64 in the mime bundle: Cobalt's tabular output.
pub const ARROW_MIME: &str = "application/vnd.apache.arrow.stream";
/// Azure Data Studio's tabular output (`{schema: {fields: [{name}]}, data: [{col: val}]}`).
pub const DATARESOURCE_MIME: &str = "application/vnd.dataresource+json";
pub const HTML_MIME: &str = "text/html";
pub const PLAIN_MIME: &str = "text/plain";
pub const MARKDOWN_MIME: &str = "text/markdown";
pub const PNG_MIME: &str = "image/png";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CellKind {
    Markdown,
    Code,
    Raw,
}

impl CellKind {
    pub fn as_str(self) -> &'static str {
        match self {
            CellKind::Markdown => "markdown",
            CellKind::Code => "code",
            CellKind::Raw => "raw",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "markdown" => CellKind::Markdown,
            "raw" => CellKind::Raw,
            _ => CellKind::Code,
        }
    }
}

/// What a code cell is written in. Resolved per cell: the cell's own `microsoft.language`
/// metadata, else a `%%sql`-style magic on its first line, else the notebook's default.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum CellLanguage {
    Sql,
    Python,
    Scala,
    R,
    Other(String),
}

impl CellLanguage {
    pub fn label(&self) -> &str {
        match self {
            CellLanguage::Sql => "SQL",
            CellLanguage::Python => "PySpark",
            CellLanguage::Scala => "Scala",
            CellLanguage::R => "SparkR",
            CellLanguage::Other(s) => s.as_str(),
        }
    }

    /// Fabric's `metadata.microsoft.language` value.
    pub fn fabric_name(&self) -> &str {
        match self {
            CellLanguage::Sql => "sparksql",
            CellLanguage::Python => "python",
            CellLanguage::Scala => "scala",
            CellLanguage::R => "sparkr",
            CellLanguage::Other(s) => s.as_str(),
        }
    }

    pub fn from_name(s: &str) -> Self {
        let l = s.to_ascii_lowercase();
        if l.contains("sql") {
            CellLanguage::Sql
        } else if l.contains("python") || l == "pyspark" {
            CellLanguage::Python
        } else if l.contains("scala") {
            CellLanguage::Scala
        } else if l == "r" || l.contains("sparkr") {
            CellLanguage::R
        } else {
            CellLanguage::Other(s.to_string())
        }
    }
}

/// A `metadata.dependencies.lakehouse` entry (Fabric).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LakehouseRef {
    pub id: String,
    pub name: Option<String>,
    pub workspace_id: Option<String>,
}

/// One mime type → payload map. Text mimes hold a `Value::String` (lines joined), JSON mimes hold
/// their object, binary mimes hold base64 text.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct MimeBundle {
    pub entries: BTreeMap<String, Value>,
}

impl MimeBundle {
    pub fn text(&self, mime: &str) -> Option<&str> {
        self.entries.get(mime).and_then(Value::as_str)
    }
    pub fn plain(&self) -> Option<&str> {
        self.text(PLAIN_MIME)
    }
    pub fn html(&self) -> Option<&str> {
        self.text(HTML_MIME)
    }
    pub fn markdown(&self) -> Option<&str> {
        self.text(MARKDOWN_MIME)
    }
    pub fn json(&self, mime: &str) -> Option<&Value> {
        self.entries.get(mime).filter(|v| !v.is_string())
    }
    /// Decoded bytes of a base64 payload (whitespace tolerated).
    pub fn binary(&self, mime: &str) -> Option<Vec<u8>> {
        use base64::Engine;
        let s = self.text(mime)?;
        let compact: String = s.chars().filter(|c| !c.is_whitespace()).collect();
        base64::engine::general_purpose::STANDARD.decode(compact).ok()
    }
    pub fn arrow(&self) -> Option<Vec<u8>> {
        self.binary(ARROW_MIME)
    }
    pub fn set_text(&mut self, mime: &str, text: impl Into<String>) {
        self.entries.insert(mime.to_string(), Value::String(text.into()));
    }
    pub fn set_json(&mut self, mime: &str, value: Value) {
        self.entries.insert(mime.to_string(), value);
    }
    pub fn set_binary(&mut self, mime: &str, bytes: &[u8]) {
        use base64::Engine;
        self.entries.insert(mime.to_string(), Value::String(base64::engine::general_purpose::STANDARD.encode(bytes)));
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Output {
    Stream { name: String, text: String },
    ExecuteResult { execution_count: Option<u64>, data: MimeBundle, metadata: Map<String, Value> },
    DisplayData { data: MimeBundle, metadata: Map<String, Value> },
    Error { ename: String, evalue: String, traceback: Vec<String> },
}

impl Output {
    pub fn stdout(text: impl Into<String>) -> Self {
        Output::Stream { name: "stdout".into(), text: text.into() }
    }
    pub fn stderr(text: impl Into<String>) -> Self {
        Output::Stream { name: "stderr".into(), text: text.into() }
    }
    pub fn error(ename: impl Into<String>, evalue: impl Into<String>, traceback: Vec<String>) -> Self {
        Output::Error { ename: ename.into(), evalue: evalue.into(), traceback }
    }

    /// Cobalt's tabular output: the Arrow IPC stream plus previews for tools without Arrow.
    /// `metadata.cobalt` records the row count and whether the payload was truncated.
    pub fn table(execution_count: Option<u64>, arrow_ipc: &[u8], html: String, markdown: String, plain: String, rows: u64, truncated: bool) -> Self {
        let mut data = MimeBundle::default();
        data.set_binary(ARROW_MIME, arrow_ipc);
        data.set_text(HTML_MIME, html);
        data.set_text(MARKDOWN_MIME, markdown);
        data.set_text(PLAIN_MIME, plain);
        let mut metadata = Map::new();
        let mut cobalt = Map::new();
        cobalt.insert("rows".into(), Value::from(rows));
        cobalt.insert("truncated".into(), Value::Bool(truncated));
        metadata.insert("cobalt".into(), Value::Object(cobalt));
        Output::ExecuteResult { execution_count, data, metadata }
    }

    pub fn data(&self) -> Option<&MimeBundle> {
        match self {
            Output::ExecuteResult { data, .. } | Output::DisplayData { data, .. } => Some(data),
            _ => None,
        }
    }

    pub fn is_error(&self) -> bool {
        matches!(self, Output::Error { .. }) || matches!(self, Output::Stream { name, .. } if name == "stderr")
    }

    /// Row count recorded with a tabular output.
    pub fn table_rows(&self) -> Option<u64> {
        match self {
            Output::ExecuteResult { metadata, .. } | Output::DisplayData { metadata, .. } => metadata.get("cobalt").and_then(|c| c.get("rows")).and_then(Value::as_u64),
            _ => None,
        }
    }

    /// Best text rendering: Markdown, then plain text, then the first line of HTML stripped of tags.
    pub fn preview_text(&self) -> String {
        match self {
            Output::Stream { text, .. } => text.clone(),
            Output::Error { ename, evalue, traceback } => {
                if traceback.is_empty() {
                    format!("{ename}: {evalue}")
                } else {
                    strip_ansi(&traceback.join("\n"))
                }
            }
            Output::ExecuteResult { data, .. } | Output::DisplayData { data, .. } => {
                if let Some(t) = data.plain() {
                    t.to_string()
                } else if let Some(m) = data.markdown() {
                    m.to_string()
                } else if let Some(h) = data.html() {
                    strip_tags(h)
                } else if data.entries.contains_key(PNG_MIME) {
                    "[image]".to_string()
                } else {
                    data.entries.keys().map(|k| format!("[{k}]")).collect::<Vec<_>>().join(" ")
                }
            }
        }
    }
}

/// Drop ANSI colour escapes (IPython tracebacks carry them).
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for d in chars.by_ref() {
                    if d.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

fn strip_tags(html: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cell {
    /// nbformat 4.5 cell id; stable across edits and moves.
    pub id: String,
    pub kind: CellKind,
    pub source: String,
    pub metadata: Map<String, Value>,
    pub outputs: Vec<Output>,
    pub execution_count: Option<u64>,
}

pub fn new_cell_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..8].to_string()
}

impl Cell {
    pub fn code(source: impl Into<String>) -> Self {
        Self { id: new_cell_id(), kind: CellKind::Code, source: source.into(), metadata: Map::new(), outputs: Vec::new(), execution_count: None }
    }
    pub fn markdown(source: impl Into<String>) -> Self {
        Self { id: new_cell_id(), kind: CellKind::Markdown, source: source.into(), metadata: Map::new(), outputs: Vec::new(), execution_count: None }
    }

    pub fn tags(&self) -> Vec<String> {
        self.metadata.get("tags").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default()
    }
    pub fn is_parameters(&self) -> bool {
        self.tags().iter().any(|t| t == "parameters")
    }
    pub fn set_parameters(&mut self, on: bool) {
        let mut tags = self.tags();
        tags.retain(|t| t != "parameters");
        if on {
            tags.push("parameters".into());
        }
        if tags.is_empty() {
            self.metadata.remove("tags");
        } else {
            self.metadata.insert("tags".into(), Value::Array(tags.into_iter().map(Value::String).collect()));
        }
    }

    /// The `name = value` assignments of a parameters cell, in order: one per line, `value`
    /// as written (an inline `# comment` dropped). Fabric's "Run with parameters" and
    /// `notebookutils.notebook.run(path, args)` override exactly these.
    pub fn assignments(&self) -> Vec<(String, String)> {
        parse_assignments(&self.source)
    }

    /// The cell's own language override (`metadata.microsoft.language`), if any.
    pub fn language_override(&self) -> Option<CellLanguage> {
        self.metadata.get("microsoft").and_then(|m| m.get("language")).and_then(Value::as_str).map(CellLanguage::from_name)
    }
    pub fn set_language_override(&mut self, lang: Option<&CellLanguage>, group: Option<&str>) {
        match lang {
            Some(l) => {
                let ms = self.metadata.entry("microsoft").or_insert_with(|| Value::Object(Map::new()));
                if let Value::Object(m) = ms {
                    m.insert("language".into(), Value::String(l.fabric_name().to_string()));
                    if let Some(g) = group {
                        m.insert("language_group".into(), Value::String(g.to_string()));
                    }
                }
            }
            None => {
                if let Some(Value::Object(m)) = self.metadata.get_mut("microsoft") {
                    m.remove("language");
                    m.remove("language_group");
                    if m.is_empty() {
                        self.metadata.remove("microsoft");
                    }
                }
            }
        }
    }

    /// A `%%magic` on the first non-blank line: `("sql", "SELECT 1")` for `%%sql\nSELECT 1`.
    pub fn split_magic(&self) -> (Option<String>, &str) {
        let trimmed_start = self.source.trim_start_matches(['\n', '\r', ' ', '\t']);
        if let Some(rest) = trimmed_start.strip_prefix("%%") {
            let name: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
            if !name.is_empty() {
                let after = &rest[name.len()..];
                let body = match after.find('\n') {
                    Some(i) => &after[i + 1..],
                    None => "",
                };
                return (Some(name.to_ascii_lowercase()), body);
            }
        }
        (None, self.source.as_str())
    }

    /// Markdown heading or first line, for cell summaries.
    pub fn first_line(&self) -> &str {
        self.source.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Notebook {
    pub metadata: Map<String, Value>,
    pub cells: Vec<Cell>,
    pub nbformat: u64,
    pub nbformat_minor: u64,
}

impl Default for Notebook {
    fn default() -> Self {
        Notebook::new(CellLanguage::Sql)
    }
}

impl Notebook {
    /// An empty notebook with the metadata the language's native tool expects: SQL notebooks
    /// use Azure Data Studio's kernelspec, PySpark notebooks Fabric's.
    pub fn new(language: CellLanguage) -> Self {
        let mut metadata = Map::new();
        match language {
            CellLanguage::Sql => {
                metadata.insert("kernelspec".into(), serde_json::json!({"name": "SQL", "display_name": "SQL", "language": "sql"}));
                metadata.insert("language_info".into(), serde_json::json!({"name": "sql", "version": ""}));
            }
            _ => {
                metadata.insert("kernel_info".into(), serde_json::json!({"name": "synapse_pyspark"}));
                metadata.insert("kernelspec".into(), serde_json::json!({"name": "synapse_pyspark", "display_name": "Synapse PySpark", "language": "Python"}));
                metadata.insert("language_info".into(), serde_json::json!({"name": "python"}));
                metadata.insert("microsoft".into(), serde_json::json!({"language": "python", "language_group": "synapse_pyspark"}));
            }
        }
        Self { metadata, cells: Vec::new(), nbformat: 4, nbformat_minor: 5 }
    }

    /// The language code cells have unless they say otherwise.
    pub fn default_language(&self) -> CellLanguage {
        let from = |v: Option<&Value>| v.and_then(Value::as_str).map(CellLanguage::from_name);
        from(self.metadata.get("microsoft").and_then(|m| m.get("language")))
            .or_else(|| from(self.metadata.get("language_info").and_then(|m| m.get("name"))))
            .or_else(|| from(self.metadata.get("kernelspec").and_then(|m| m.get("language"))))
            .or_else(|| from(self.metadata.get("kernelspec").and_then(|m| m.get("name"))))
            .unwrap_or(CellLanguage::Sql)
    }

    /// `metadata.microsoft.language_group` (Fabric), used when tagging cells.
    pub fn language_group(&self) -> Option<String> {
        self.metadata.get("microsoft").and_then(|m| m.get("language_group")).and_then(Value::as_str).map(str::to_string)
    }

    /// A cell's language: a `%%sql` / `%%pyspark` / … first line decides (as on Fabric, where
    /// the magic switches the language whatever the cell's dropdown says), then the cell's own
    /// language metadata, then the notebook's default.
    pub fn cell_language(&self, cell: &Cell) -> CellLanguage {
        if let (Some(magic), _) = cell.split_magic() {
            return match magic.as_str() {
                "sql" => CellLanguage::Sql,
                "pyspark" | "python" => CellLanguage::Python,
                "spark" | "scala" => CellLanguage::Scala,
                "sparkr" | "r" => CellLanguage::R,
                other => CellLanguage::Other(other.to_string()),
            };
        }
        if let Some(l) = cell.language_override() {
            return l;
        }
        self.default_language()
    }

    pub fn default_lakehouse(&self) -> Option<LakehouseRef> {
        let lh = self.metadata.get("dependencies")?.get("lakehouse")?;
        let id = lh.get("default_lakehouse")?.as_str()?.to_string();
        Some(LakehouseRef { id, name: lh.get("default_lakehouse_name").and_then(Value::as_str).map(str::to_string), workspace_id: lh.get("default_lakehouse_workspace_id").and_then(Value::as_str).map(str::to_string) })
    }

    pub fn is_fabric(&self) -> bool {
        self.metadata.contains_key("kernel_info") && self.metadata.get("kernel_info").and_then(|k| k.get("name")).and_then(Value::as_str).map(|n| n.starts_with("synapse")).unwrap_or(false) || self.metadata.contains_key("dependencies")
    }

    pub fn title_hint(&self) -> Option<String> {
        self.cells.iter().find(|c| c.kind == CellKind::Markdown).and_then(|c| c.source.lines().find_map(|l| l.trim().strip_prefix('#').map(|t| t.trim_start_matches('#').trim().to_string()))).filter(|t| !t.is_empty())
    }

    /// Drop outputs and execution counts from every cell.
    pub fn clear_outputs(&mut self) {
        for c in &mut self.cells {
            c.outputs.clear();
            c.execution_count = None;
        }
    }

    pub fn find_cell(&self, id: &str) -> Option<usize> {
        self.cells.iter().position(|c| c.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magic_split() {
        let c = Cell::code("%%sql\nSELECT 1\n");
        assert_eq!(c.split_magic(), (Some("sql".into()), "SELECT 1\n"));
        let c = Cell::code("SELECT 1");
        assert_eq!(c.split_magic(), (None, "SELECT 1"));
        let c = Cell::code("  \n%%configure -f\n{}");
        assert_eq!(c.split_magic().0.as_deref(), Some("configure"));
    }

    #[test]
    fn languages() {
        let nb = Notebook::new(CellLanguage::Python);
        assert_eq!(nb.default_language(), CellLanguage::Python);
        assert_eq!(nb.cell_language(&Cell::code("%%sql\nSELECT 1")), CellLanguage::Sql);
        let mut c = Cell::code("x");
        c.set_language_override(Some(&CellLanguage::Sql), Some("synapse_pyspark"));
        assert_eq!(nb.cell_language(&c), CellLanguage::Sql);
        assert_eq!(c.metadata["microsoft"]["language"], "sparksql");
        // the magic wins over the dropdown: a "PySpark" cell that starts with %%sql is SQL
        let mut c = Cell::code("%%sql\nSELECT 1");
        c.set_language_override(Some(&CellLanguage::Python), Some("synapse_pyspark"));
        assert_eq!(nb.cell_language(&c), CellLanguage::Sql);
        let mut c = Cell::code("%%pyspark\nprint(1)");
        c.set_language_override(Some(&CellLanguage::Sql), Some("synapse_pyspark"));
        assert_eq!(nb.cell_language(&c), CellLanguage::Python);
        c.set_language_override(None, None);
        assert!(!c.metadata.contains_key("microsoft"));
        let sql = Notebook::new(CellLanguage::Sql);
        assert_eq!(sql.default_language(), CellLanguage::Sql);
    }

    #[test]
    fn parameters_tag() {
        let mut c = Cell::code("a = 1");
        assert!(!c.is_parameters());
        c.set_parameters(true);
        assert!(c.is_parameters());
        c.set_parameters(false);
        assert!(!c.metadata.contains_key("tags"));
    }

    #[test]
    fn table_output_roundtrips_bytes() {
        let o = Output::table(Some(3), b"ARROW", "<table/>".into(), "|a|".into(), "a".into(), 10, false);
        assert_eq!(o.data().unwrap().arrow().unwrap(), b"ARROW");
        assert_eq!(o.table_rows(), Some(10));
        assert_eq!(o.preview_text(), "a");
    }

    #[test]
    fn ansi_stripped() {
        assert_eq!(strip_ansi("\u{1b}[0;31mNameError\u{1b}[0m: x"), "NameError: x");
    }
}

impl Notebook {
    /// The cell tagged `parameters` (the first one when several carry the tag).
    pub fn parameters_cell(&self) -> Option<usize> {
        self.cells.iter().position(|c| c.kind == CellKind::Code && c.is_parameters())
    }
}

/// `name = value` lines of Python source (a parameters cell). Lines that are not a simple
/// assignment (calls, blocks, comments, `a, b = …`, `x += 1`) are left out.
pub fn parse_assignments(source: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in source.lines() {
        let l = line.trim();
        if l.is_empty() || l.starts_with('#') || line.starts_with(' ') || line.starts_with('\t') {
            continue;
        }
        let Some((name, value)) = l.split_once('=') else { continue };
        let name = name.trim();
        if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') || name.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(true) {
            continue;
        }
        if value.starts_with('=') {
            continue; // `==`
        }
        let value = strip_inline_comment(value).trim().to_string();
        if value.is_empty() {
            continue;
        }
        if let Some(e) = out.iter_mut().find(|(n, _)| n == name) {
            e.1 = value;
        } else {
            out.push((name.to_string(), value));
        }
    }
    out
}

fn strip_inline_comment(v: &str) -> &str {
    let mut quote: Option<char> = None;
    let mut prev = ' ';
    for (i, c) in v.char_indices() {
        match quote {
            Some(q) if c == q && prev != '\\' => quote = None,
            Some(_) => {}
            None if c == '\'' || c == '"' => quote = Some(c),
            None if c == '#' => return &v[..i],
            None => {}
        }
        prev = c;
    }
    v
}

/// Split a cell's source into the code to run and the `%pip install` / `!pip install` /
/// `%conda install` lines it carried (the package specs of those lines). Fabric runs them in
/// the session; locally the packages belong to the runtime environment, so the host reports them.
pub fn strip_pip_lines(source: &str) -> (String, Vec<String>) {
    let mut code = String::new();
    let mut packages: Vec<String> = Vec::new();
    for line in source.lines() {
        let l = line.trim();
        let rest = l.strip_prefix("%pip").or_else(|| l.strip_prefix("!pip")).or_else(|| l.strip_prefix("%conda")).or_else(|| l.strip_prefix("!conda")).or_else(|| l.strip_prefix("!python -m pip")).or_else(|| l.strip_prefix("!uv pip"));
        match rest.map(str::trim_start) {
            Some(r) if r.starts_with("install") => {
                let args = r["install".len()..].trim();
                for tok in args.split_whitespace() {
                    if tok.starts_with('-') || tok == "install" {
                        continue;
                    }
                    let spec = tok.trim_matches(|c| c == '"' || c == '\'').to_string();
                    if !spec.is_empty() && !packages.contains(&spec) {
                        packages.push(spec);
                    }
                }
            }
            _ => {
                code.push_str(line);
                code.push('\n');
            }
        }
    }
    if packages.is_empty() {
        return (source.to_string(), packages);
    }
    (code.trim_end().to_string(), packages)
}

#[cfg(test)]
mod parameters_tests {
    use super::*;

    #[test]
    fn assignments_of_a_parameters_cell() {
        let src = "# params\nstart_date = '2026-01-01'  # inclusive\nlimit = 100\nname = \"a # not a comment\"\nx == 3\nif True:\n    y = 2\nfoo(z=1)\nlimit = 200\n";
        let a = parse_assignments(src);
        assert_eq!(a, vec![("start_date".to_string(), "'2026-01-01'".to_string()), ("limit".to_string(), "200".to_string()), ("name".to_string(), "\"a # not a comment\"".to_string())]);
        let mut c = Cell::code(src);
        assert!(c.assignments().len() == 3);
        c.set_parameters(true);
        let nb = Notebook { cells: vec![Cell::markdown("x"), c], ..Default::default() };
        assert_eq!(nb.parameters_cell(), Some(1));
    }

    #[test]
    fn pip_lines_come_out() {
        let (code, pk) = strip_pip_lines("%pip install polars==1.9 \"dwlib>=0.3\" -q\nimport polars\n!pip install --upgrade pandas\nprint(1)");
        assert_eq!(pk, vec!["polars==1.9", "dwlib>=0.3", "pandas"]);
        assert_eq!(code, "import polars\nprint(1)");
        let (code, pk) = strip_pip_lines("print(1)");
        assert!(pk.is_empty() && code == "print(1)");
        let (code, pk) = strip_pip_lines("%pip install x");
        assert_eq!(pk, vec!["x"]);
        assert!(code.is_empty());
    }
}
