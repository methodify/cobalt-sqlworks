//! Files dragged onto the window. Every path is classified first (`classify`), by extension
//! and, when the extension is ambiguous, by sniffing the content; the hover overlay shows
//! what the drop will do with each file, and the drop does it through the paths the File
//! menu already uses (open, import, the ADS / connections importers, the Files sidebar).
//!
//! What lands where:
//! - `.sql` (and `.txt` that reads like a query): a query tab of the dialect the text looks
//!   like — T-SQL, Spark SQL, DAX / MDX — or the kind "A click on + opens" when the text does
//!   not say. `.dax` / `.msdax` / `.mdx` / `.hql` / `.sparksql` name their dialect outright.
//! - `.sqlplan` (or an XML file that is a showplan): the plan viewer.
//! - `.ipynb`, a Fabric `notebook-content.py`: a notebook; any other `.py`: a PySpark notebook
//!   with the file as its first cell.
//! - `.cobalt-dash.json`, `.cobalt-explore.json`: a dashboard, an Explore.
//! - `.csv .tsv .txt .parquet .arrow .feather .ipc .xlsx .jsonl` and JSON arrays: the Import
//!   dialog on the active tab's connection.
//! - A Cobalt connections export (`cobalt-connections.json`): Import connections; an Azure
//!   Data Studio `settings.json`: the ADS importer.
//! - A folder with a `_delta_log`: a Spark SQL tab reading the Delta table; any other folder:
//!   the Files sidebar opens on it.

use crate::ops::Ctx;
use crate::state::{AppState, ToastKind};
use std::path::{Path, PathBuf};

/// The query dialect a text looks like.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueryKind {
    TSql,
    Spark,
    Dax,
    /// The local engine (only ever chosen as the "+ opens" default).
    Local,
    /// Nothing in the text decides it.
    Unknown,
}

impl QueryKind {
    pub fn label(self) -> &'static str {
        match self {
            QueryKind::TSql => "SQL query",
            QueryKind::Spark => "Spark SQL query",
            QueryKind::Dax => "DAX query",
            QueryKind::Local => "local query",
            QueryKind::Unknown => "query",
        }
    }
}

/// What a dropped path will become.
#[derive(Clone, Debug, PartialEq)]
pub enum DropAction {
    Query { kind: QueryKind, sniffed: bool },
    Plan,
    Notebook,
    /// A Python file that is not a notebook: a PySpark notebook with the file as one cell.
    PythonNotebook,
    Dashboard,
    Explore,
    Import,
    /// A data file the local engine reads: a local table with a Local tab over it.
    LocalTable,
    /// A SQLite database file (by its header): a file connection with a query tab.
    SqliteDb,
    /// A DuckDB database file (by header): a connection.
    DuckDb,
    Connections,
    AdsSettings,
    DeltaFolder,
    Folder,
    /// Nothing Cobalt opens.
    Unsupported,
}

impl DropAction {
    /// The verb line of the hover overlay.
    pub fn describe(&self, default_kind: &str) -> String {
        match self {
            DropAction::Query { kind: QueryKind::Unknown, .. } => format!("Open as a {} (the + default; the text does not say which)", kind_label(default_kind)),
            DropAction::Query { kind, sniffed } => format!("Open as a {}{}", kind.label(), if *sniffed { " (from the text)" } else { "" }),
            DropAction::Plan => "Open the execution plan".into(),
            DropAction::Notebook => "Open the notebook".into(),
            DropAction::PythonNotebook => "Open as a PySpark notebook (one cell)".into(),
            DropAction::Dashboard => "Open the dashboard".into(),
            DropAction::Explore => "Open in Explore".into(),
            DropAction::Import => "Import the data into the active connection".into(),
            DropAction::LocalTable => "Open as a local table (query it here; Import into a connection from the tab)".into(),
            DropAction::SqliteDb => "Open as a SQLite database (a connection in the Servers tree)".into(),
            DropAction::DuckDb => "Open as a DuckDB database (a connection in the Servers tree)".into(),
            DropAction::Connections => "Import the connections".into(),
            DropAction::AdsSettings => "Import connections from Azure Data Studio settings".into(),
            DropAction::DeltaFolder => "Open the Delta table as a local table".into(),
            DropAction::Folder => "Open the folder in the Files sidebar".into(),
            DropAction::Unsupported => "Not a file Cobalt opens".into(),
        }
    }
}

fn kind_label(default_kind: &str) -> &'static str {
    match default_kind {
        "spark" => "Spark SQL query",
        "dax" => "DAX query",
        "local" => "local query",
        _ => "SQL query",
    }
}

const DATA_EXTS: [&str; 12] = ["csv", "tsv", "tab", "psv", "parquet", "pq", "arrow", "feather", "ipc", "arrows", "jsonl", "ndjson"];
const BOOK_EXTS: [&str; 3] = ["xlsx", "xlsm", "xls"];

/// Classify a path: the extension first, the content when the extension leaves it open.
pub fn classify(path: &Path) -> DropAction {
    if path.is_dir() {
        return if path.join("_delta_log").is_dir() { DropAction::DeltaFolder } else { DropAction::Folder };
    }
    let name = path.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    if cobalt_core::DashboardFile::is_dashboard_path(path) {
        return DropAction::Dashboard;
    }
    if cobalt_core::explore::ExploreSpec::is_explore_path(path) {
        return DropAction::Explore;
    }
    if cobalt_driver::duckdb::is_duckdb_file(path) {
        return DropAction::DuckDb;
    }
    if cobalt_driver::sqlite::is_sqlite_file(path) {
        return DropAction::SqliteDb;
    }
    let ext = path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "sqlplan" => return DropAction::Plan,
        "ipynb" => return DropAction::Notebook,
        "dax" | "msdax" | "mdx" => return DropAction::Query { kind: QueryKind::Dax, sniffed: false },
        "hql" | "sparksql" => return DropAction::Query { kind: QueryKind::Spark, sniffed: false },
        "py" => return if name == "notebook-content.py" || head(path, 4096).contains("# Fabric notebook source") { DropAction::Notebook } else { DropAction::PythonNotebook },
        e if DATA_EXTS.contains(&e) => return DropAction::LocalTable,
        e if BOOK_EXTS.contains(&e) => return DropAction::Import,
        _ => {}
    }
    let text = head(path, 64 * 1024);
    match ext.as_str() {
        "sql" => {
            let kind = sniff_query(&text);
            DropAction::Query { kind, sniffed: kind != QueryKind::Unknown }
        }
        "txt" => {
            // a query saved as text, else a delimited data file
            let kind = sniff_query(&text);
            if kind != QueryKind::Unknown { DropAction::Query { kind, sniffed: true } } else { DropAction::LocalTable }
        }
        "xml" => {
            if text.contains("<ShowPlanXML") { DropAction::Plan } else { DropAction::Unsupported }
        }
        "json" => classify_json(&text),
        "" => {
            let kind = sniff_query(&text);
            if kind != QueryKind::Unknown { DropAction::Query { kind, sniffed: true } } else { DropAction::Unsupported }
        }
        _ => DropAction::Unsupported,
    }
}

/// A JSON file: a Cobalt connections export, an Azure Data Studio settings file, a data
/// array (import), or nothing we open.
fn classify_json(text: &str) -> DropAction {
    let t = text.trim_start_matches('\u{feff}').trim_start();
    if t.contains("\"datasource.connections\"") || t.contains("\"datasource.connectionGroups\"") {
        return DropAction::AdsSettings;
    }
    if t.starts_with('{') && t.contains("\"profiles\"") && t.contains("\"version\"") {
        return DropAction::Connections;
    }
    if t.starts_with('[') {
        return DropAction::Import;
    }
    DropAction::Unsupported
}

/// The first `max` bytes of a file as text (lossy), empty when unreadable.
fn head(path: &Path, max: usize) -> String {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(path) else { return String::new() };
    let mut buf = vec![0u8; max];
    let mut n = 0;
    while n < max {
        match f.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(_) => break,
        }
    }
    buf.truncate(n);
    String::from_utf8_lossy(&buf).into_owned()
}

/// Which dialect a query text reads as. Scores T-SQL and Spark SQL tells against each other;
/// DAX and MDX are unmistakable (EVALUATE / DEFINE, `ON COLUMNS`, `$SYSTEM`).
pub fn sniff_query(text: &str) -> QueryKind {
    let body: String = text.lines().filter(|l| !l.trim_start().starts_with("--")).collect::<Vec<_>>().join("\n");
    let upper = body.to_ascii_uppercase();
    let first = upper.split_whitespace().next().unwrap_or("");
    if first == "EVALUATE" || first == "DEFINE" || upper.contains("\nEVALUATE") || upper.contains("$SYSTEM.") {
        return QueryKind::Dax;
    }
    if (upper.contains(" ON COLUMNS") || upper.contains(" ON ROWS") || upper.contains(" ON AXIS(")) && (upper.contains("SELECT") && upper.contains("FROM [")) {
        return QueryKind::Dax; // MDX runs on the same tab
    }
    let tsql = [
        ("\nGO\n", 4), ("\nGO\r\n", 4), ("[DBO].", 3), ("DBO.", 2), ("DECLARE @", 4), ("SET NOCOUNT", 4), ("NOLOCK", 4), ("ISNULL(", 2), ("GETDATE()", 3), ("CONVERT(", 2), ("NVARCHAR", 3), ("IDENTITY(", 3), ("BEGIN TRAN", 3), ("RAISERROR", 4), ("PRINT '", 2), ("OBJECT_ID(", 3), ("CROSS APPLY", 3), ("OUTER APPLY", 3), ("DATEADD(", 2), ("IIF(", 2), ("OPENROWSET", 3), ("SYS.", 2), ("INFORMATION_SCHEMA", 1), ("SP_", 2), ("@@", 3), ("#TEMP", 2), ("TOP (", 2), ("TOP 1", 1), ("WITH (", 1), ("MERGE ", 1), ("GO\n", 1), ("STRING_SPLIT", 2), ("SELECT TOP", 2), ("UNIQUEIDENTIFIER", 3), ("DATETIME2", 2), ("VARCHAR(MAX)", 3), ("EXEC ", 2), ("TRY_CAST(", 1), ("FOR JSON", 3), ("FOR XML", 3),
    ];
    let spark = [
        ("USING DELTA", 4), ("USING PARQUET", 4), ("LATERAL VIEW", 4), ("EXPLODE(", 3), ("%%SQL", 4), ("%SQL", 2), ("OPTIMIZE ", 2), ("VACUUM ", 3), ("ZORDER", 4), ("MSCK REPAIR", 4), ("SHOW TABLES", 2), ("DESCRIBE EXTENDED", 3), ("DESCRIBE HISTORY", 4), ("TEMP VIEW", 3), ("TEMPORARY VIEW", 3), ("INSERT OVERWRITE", 4), ("PARTITIONED BY", 3), ("TBLPROPERTIES", 4), ("\nLIMIT ", 3), (" LIMIT ", 2), ("COLLECT_LIST(", 3), ("COLLECT_SET(", 3), ("ARRAY_CONTAINS(", 3), ("DATE_FORMAT(", 2), ("AS STRING", 3), ("STRUCT<", 4), ("MAP<", 3), ("ARRAY<", 3), ("CACHE TABLE", 4), ("REFRESH TABLE", 3), ("SPARK_CATALOG", 4), ("DELTA.`", 4), ("ABFSS://", 4), ("FILES/", 2), ("`", 2), ("CREATE OR REPLACE TABLE", 2), ("REGEXP_EXTRACT(", 2), ("NAMED_STRUCT(", 3), ("UNIX_TIMESTAMP(", 2), ("FROM_UNIXTIME(", 2), ("SIZE(", 1), ("DISTRIBUTE BY", 3), ("CLUSTER BY", 3), ("SORT BY", 1), ("IF NOT EXISTS", 1), ("COMMENT '", 1), ("SET SPARK.", 4), ("DOUBLE", 1), ("BIGINT", 1),
    ];
    let score = |tells: &[(&str, i32)]| -> i32 { tells.iter().filter(|(t, _)| upper.contains(t)).map(|(_, w)| *w).sum() };
    let (t, s) = (score(&tsql), score(&spark));
    // brackets around identifiers are a strong T-SQL tell, backticks a strong Spark one
    let brackets = body.matches("].").count() + body.matches(" [").count();
    let t = t + (brackets.min(5) as i32) * 2;
    if t == 0 && s == 0 {
        return QueryKind::Unknown;
    }
    if t >= s + 2 {
        QueryKind::TSql
    } else if s >= t + 2 {
        QueryKind::Spark
    } else {
        QueryKind::Unknown
    }
}

/// A short name for the overlay.
fn short(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.display().to_string())
}

/// The overlay lines while files hover over the window: one per file, the action each gets.
pub fn hover_lines(paths: &[PathBuf], default_kind: &str) -> Vec<(String, String, bool)> {
    paths
        .iter()
        .take(12)
        .map(|p| {
            let a = classify(p);
            let ok = a != DropAction::Unsupported;
            (short(p), a.describe(default_kind), ok)
        })
        .collect()
}

/// Open every dropped path the way `classify` says. Returns the number opened.
pub fn drop_files(state: &mut AppState, cx: &Ctx, paths: &[PathBuf]) -> usize {
    let mut opened = 0;
    let default_kind = cx.settings.appearance.new_tab_kind.clone();
    for path in paths {
        let action = classify(path);
        tracing::info!(path = %path.display(), ?action, "file dropped");
        let ok = match action {
            DropAction::Query { kind, .. } => open_query_file(state, cx, path, kind, &default_kind),
            DropAction::Plan => {
                crate::ops::open_plan_file(state, cx, path.clone());
                true
            }
            DropAction::Notebook => crate::notebook::open_path(state, cx, path.clone()),
            DropAction::PythonNotebook => match std::fs::read_to_string(path) {
                Ok(src) => {
                    crate::notebook::new_tab_from_source(state, cx, cobalt_notebook::CellLanguage::Python, &src, &short(path));
                    true
                }
                Err(e) => {
                    cx.toast(ToastKind::Error, format!("Could not read {}: {e}", short(path)));
                    false
                }
            },
            DropAction::Dashboard => crate::dashboard::open_path(state, cx, path.clone()),
            DropAction::Explore => crate::exploreq::open_path(state, cx, path).is_some(),
            DropAction::SqliteDb => crate::ops::open_sqlite_database(state, cx, path, false).is_some(),
            DropAction::DuckDb => crate::ops::open_file_database(state, cx, path, false, Some(cobalt_core::EngineFamily::DuckDb)).is_some(),
            DropAction::LocalTable => match crate::localq::open_file_table(state, cx, path) {
                Ok(_) => true,
                Err(e) => {
                    cx.toast(ToastKind::Warning, format!("{}: {e}", short(path)));
                    false
                }
            },
            DropAction::Import => {
                // the import lands in the active tab's connection; a tab is opened when there is none
                let idx = match state.active_tab.filter(|&i| state.tabs.get(i).map(|t| !t.is_notebook() && !t.is_dashboard() && !t.is_explore() && t.spark.is_none() && t.tabular.is_none()).unwrap_or(false)) {
                    Some(i) => i,
                    None => {
                        let (profile, db) = state.active().map(|t| (t.profile.as_ref().map(|p| p.id), t.conn.database().map(str::to_string))).unwrap_or((None, None));
                        crate::ops::new_query_tab(state, cx, profile, db, None, false)
                    }
                };
                crate::ops::open_import_dialog(state, cx, idx, Some(path.clone()));
                true
            }
            DropAction::Connections => {
                crate::ops::import_connections(state, cx, path);
                true
            }
            DropAction::AdsSettings => {
                state.dialog = crate::state::Dialog::AdsImport { path: path.to_string_lossy().to_string(), summary: None, error: None };
                true
            }
            DropAction::DeltaFolder => match crate::localq::open_file_table(state, cx, path) {
                Ok(_) => true,
                Err(e) => {
                    // the local reader could not: the Spark session can
                    cx.toast(ToastKind::Info, format!("{}: {e}; opening it in Spark SQL instead", short(path)));
                    let (binding, names) = crate::sparkq::inherited_binding(state);
                    let text = format!("SELECT * FROM delta.`{}` LIMIT 100", path.display().to_string().replace('\\', "/"));
                    let idx = crate::sparkq::new_tab(state, cx, binding, names, Some(text));
                    let t = &mut state.tabs[idx];
                    t.title = format!("{} (Delta)", short(path));
                    t.custom_title = true;
                    true
                }
            },
            DropAction::Folder => {
                state.files.root = Some(path.clone());
                state.files.cache.clear();
                state.files.expanded.clear();
                state.sidebar_visible = true;
                state.sidebar_view = crate::state::SidebarView::Files;
                true
            }
            DropAction::Unsupported => {
                cx.toast(ToastKind::Info, format!("{}: not a file Cobalt opens (SQL, DAX, notebooks, plans, dashboards, data files, connection exports).", short(path)));
                false
            }
        };
        if ok {
            opened += 1;
        }
    }
    opened
}

/// A query file into a tab of its dialect. T-SQL goes the File › Open way (a tab on the
/// active connection, or the file's existing tab); Spark SQL inherits the active lakehouse;
/// DAX inherits the active model or asks for one.
fn open_query_file(state: &mut AppState, cx: &Ctx, path: &Path, kind: QueryKind, default_kind: &str) -> bool {
    let kind = match kind {
        QueryKind::Unknown => match default_kind {
            "spark" => QueryKind::Spark,
            "dax" => QueryKind::Dax,
            "local" => QueryKind::Local,
            _ => QueryKind::TSql,
        },
        k => k,
    };
    if let Some(i) = state.tabs.iter().position(|t| t.file_path.as_deref() == Some(path)) {
        state.active_tab = Some(i);
        return true;
    }
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t.strip_prefix('\u{feff}').map(str::to_string).unwrap_or(t),
        Err(e) => {
            cx.toast(ToastKind::Error, format!("Could not open {}: {e}", short(path)));
            return false;
        }
    };
    let title = short(path);
    match kind {
        QueryKind::TSql => {
            // a pristine query tab takes the file; else a new tab on the active connection
            let pristine = state.active_tab.filter(|&i| state.tabs.get(i).map(|t| t.text.is_empty() && t.file_path.is_none() && t.run.is_none() && t.notebook.is_none() && t.dashboard.is_none() && t.explore.is_none() && t.spark.is_none() && t.tabular.is_none()).unwrap_or(false));
            let idx = match pristine {
                Some(i) => i,
                None => {
                    let (profile, db) = state.active().map(|t| (t.profile.as_ref().map(|p| p.id), t.conn.database().map(str::to_string))).unwrap_or((None, None));
                    crate::ops::new_query_tab(state, cx, profile, db, None, false)
                }
            };
            let t = &mut state.tabs[idx];
            t.text = text;
            t.file_path = Some(path.to_path_buf());
            t.title = title;
            t.custom_title = true;
            t.mark_saved();
            t.editor.request_focus = true;
            state.active_tab = Some(idx);
        }
        QueryKind::Spark => {
            let (binding, names) = crate::sparkq::inherited_binding(state);
            let idx = crate::sparkq::new_tab(state, cx, binding, names, Some(text));
            let t = &mut state.tabs[idx];
            t.file_path = Some(path.to_path_buf());
            t.title = title;
            t.custom_title = true;
            t.mark_saved();
        }
        QueryKind::Local => {
            let idx = crate::localq::new_tab(state, cx, Some(text));
            let t = &mut state.tabs[idx];
            t.file_path = Some(path.to_path_buf());
            t.title = title;
            t.custom_title = true;
            t.mark_saved();
        }
        QueryKind::Dax => {
            let inherited = state.active().and_then(|t| t.tabular.as_ref()).map(|tt| (tt.target.clone(), tt.profile));
            match inherited {
                Some((target, profile)) => {
                    let idx = crate::tabularq::choose(state, cx, None, false, target, profile);
                    let t = &mut state.tabs[idx];
                    t.text = text;
                    t.file_path = Some(path.to_path_buf());
                    t.title = title;
                    t.custom_title = true;
                    t.mark_saved();
                }
                None => {
                    // the chooser opens the tab once a model is picked, then loads the file
                    crate::tabularq::open_chooser(state, cx, None, false);
                    if let crate::state::Dialog::ChooseModel { file, .. } = &mut state.dialog {
                        *file = Some(path.to_path_buf());
                    }
                }
            }
        }
        QueryKind::Unknown => unreachable!(),
    }
    true
}

/// Load a query file into a freshly made DAX tab (the chooser's pick for a dropped file).
pub fn load_into_tab(state: &mut AppState, cx: &Ctx, idx: usize, path: &Path) {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let t = &mut state.tabs[idx];
            t.text = text.strip_prefix('\u{feff}').map(str::to_string).unwrap_or(text);
            t.file_path = Some(path.to_path_buf());
            t.title = short(path);
            t.custom_title = true;
            t.mark_saved();
        }
        Err(e) => cx.toast(ToastKind::Error, format!("Could not open {}: {e}", short(path))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_tsql() {
        assert_eq!(sniff_query("SET NOCOUNT ON;\nSELECT TOP (10) * FROM dbo.Orders WITH (NOLOCK)\nGO\n"), QueryKind::TSql);
        assert_eq!(sniff_query("DECLARE @d datetime2 = GETDATE();\nSELECT [Name] FROM [dbo].[Customers] WHERE Id = @@IDENTITY"), QueryKind::TSql);
    }

    #[test]
    fn sniffs_spark() {
        assert_eq!(sniff_query("CREATE TABLE sales USING DELTA PARTITIONED BY (year) AS SELECT * FROM src LIMIT 10"), QueryKind::Spark);
        assert_eq!(sniff_query("SELECT `order id`, collect_list(item) FROM delta.`Files/x` LATERAL VIEW explode(items) t AS item"), QueryKind::Spark);
        assert_eq!(sniff_query("%%sql\nSELECT cast(a as string) FROM t LIMIT 5"), QueryKind::Spark);
    }

    #[test]
    fn sniffs_dax_and_mdx() {
        assert_eq!(sniff_query("-- top products\nEVALUATE TOPN(10, 'Product')"), QueryKind::Dax);
        assert_eq!(sniff_query("DEFINE MEASURE Sales[x] = 1\nEVALUATE ROW(\"x\", [x])"), QueryKind::Dax);
        assert_eq!(sniff_query("SELECT {[Measures].[Sales]} ON COLUMNS, [Date].[Year].MEMBERS ON ROWS FROM [Model]"), QueryKind::Dax);
        assert_eq!(sniff_query("SELECT * FROM $SYSTEM.TMSCHEMA_MEASURES"), QueryKind::Dax);
    }

    #[test]
    fn plain_sql_is_unknown() {
        assert_eq!(sniff_query("SELECT id, name FROM customers WHERE id = 1"), QueryKind::Unknown);
        assert_eq!(sniff_query(""), QueryKind::Unknown);
    }

    #[test]
    fn json_kinds() {
        assert_eq!(classify_json("{\"datasource.connections\": []}"), DropAction::AdsSettings);
        assert_eq!(classify_json("{\"version\": 1, \"profiles\": []}"), DropAction::Connections);
        assert_eq!(classify_json("[{\"a\": 1}]"), DropAction::Import);
        assert_eq!(classify_json("{\"a\": 1}"), DropAction::Unsupported);
    }

    #[test]
    fn classifies_by_extension_and_content() {
        let dir = std::env::temp_dir().join(format!("cobalt-drop-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let w = |name: &str, body: &str| {
            let p = dir.join(name);
            std::fs::write(&p, body).unwrap();
            p
        };
        assert_eq!(classify(&w("a.sql", "SELECT 1")), DropAction::Query { kind: QueryKind::Unknown, sniffed: false });
        assert_eq!(classify(&w("b.sql", "SELECT * FROM t USING DELTA LIMIT 1 -- `x`")), DropAction::Query { kind: QueryKind::Spark, sniffed: true });
        assert_eq!(classify(&w("c.dax", "x")), DropAction::Query { kind: QueryKind::Dax, sniffed: false });
        assert_eq!(classify(&w("d.txt", "a,b\n1,2")), DropAction::LocalTable);
        assert_eq!(classify(&w("e.txt", "EVALUATE 'T'")), DropAction::Query { kind: QueryKind::Dax, sniffed: true });
        assert_eq!(classify(&w("f.xml", "<ShowPlanXML xmlns=\"x\"/>")), DropAction::Plan);
        assert_eq!(classify(&w("g.xml", "<other/>")), DropAction::Unsupported);
        assert_eq!(classify(&w("h.csv", "a,b")), DropAction::LocalTable);
        assert_eq!(classify(&w("i.parquet", "")), DropAction::LocalTable);
        assert_eq!(classify(&w("j.xlsx", "")), DropAction::Import);
        assert_eq!(classify(&w("notebook-content.py", "# Fabric notebook source\n")), DropAction::Notebook);
        assert_eq!(classify(&w("script.py", "print(1)")), DropAction::PythonNotebook);
        assert_eq!(classify(&w("p.cobalt-dash.json", "{}")), DropAction::Dashboard);
        assert_eq!(classify(&w("q.cobalt-explore.json", "{}")), DropAction::Explore);
        assert_eq!(classify(&w("r.ipynb", "{}")), DropAction::Notebook);
        assert_eq!(classify(&w("s.sqlplan", "")), DropAction::Plan);
        assert_eq!(classify(&w("t.exe", "")), DropAction::Unsupported);
        std::fs::create_dir_all(dir.join("tbl/_delta_log")).unwrap();
        assert_eq!(classify(&dir.join("tbl")), DropAction::DeltaFolder);
        assert_eq!(classify(&dir), DropAction::Folder);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
