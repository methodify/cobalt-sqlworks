//! Edit Data: staged edits on a result grid that is one SQLite table. Nothing is written as
//! you type; the grid keeps a `GridEdits` layer (changed cells, deleted rows, inserted rows),
//! the Pending changes strip shows the count and the statements, **Save** runs them as one
//! `BEGIN … COMMIT` script through the tab's connection (the SQLite driver rolls back on the
//! first error) and then runs the query again, **Discard** drops them.
//!
//! A grid is editable when its run was a single-table `SELECT` on a file connection and the
//! result carries the table's key: a `rowid` column, or every primary-key column. The tree's
//! *Edit Data* opens exactly such a query. The key discovery and the SQL text are the only
//! engine-specific parts; a T-SQL implementation slots in behind the same functions later.

use crate::ops::{new_query_tab, run_sql, Ctx};
use crate::state::{AppState, EditResume, GridEdits, MessageLine, ResultSetView, RunMode, ToastKind};
use cobalt_core::{ObjectRef, ProfileId, SqlType, TabId};
use cobalt_results::CellFormatter;
use cobalt_sql::lexer::{tokenize_dialect, Dialect, TokenKind};
use std::collections::BTreeMap;

/// The one table a `SELECT` reads, when the script is exactly that: one statement, no joins,
/// no DISTINCT, GROUP BY, UNION or subquery in FROM. The name comes back unquoted.
#[cfg(test)]
pub fn single_table(script: &str) -> Option<String> {
    match single_table_parts(script)? {
        (None, name) => Some(name),
        (Some(_), _) => None,
    }
}

/// [`single_table`] with the schema when the SELECT names one (`schema.table`).
pub fn single_table_parts(script: &str) -> Option<(Option<String>, String)> {
    let toks: Vec<_> = tokenize_dialect(script, Dialect::Sqlite).into_iter().filter(|t| !matches!(t.kind, TokenKind::LineComment | TokenKind::BlockComment | TokenKind::Whitespace | TokenKind::Newline)).collect();
    let text = |i: usize| &script[toks[i].start..toks[i].end];
    let upper = |i: usize| text(i).to_ascii_uppercase();
    if toks.is_empty() || upper(0) != "SELECT" {
        return None;
    }
    let mut from = None;
    let mut depth = 0i32;
    for i in 1..toks.len() {
        match text(i) {
            "(" => depth += 1,
            ")" => depth -= 1,
            _ => {}
        }
        if depth == 0 {
            let u = upper(i);
            if u == "FROM" {
                from = Some(i);
                break;
            }
            if u == "DISTINCT" {
                return None;
            }
        }
    }
    let from = from?;
    let name_at = |i: usize| -> Option<String> {
        let t = toks.get(i)?;
        Some(match t.kind {
            TokenKind::Identifier => text(i).to_string(),
            // the SQL Server lexer reads "double quotes" as a string; SQLite means an identifier
            TokenKind::QuotedIdentifier | TokenKind::String if text(i).starts_with('"') => text(i).trim_matches('"').replace("\"\"", "\""),
            TokenKind::QuotedIdentifier if text(i).starts_with('`') => text(i).trim_matches('`').to_string(),
            TokenKind::BracketedIdentifier => text(i).trim_start_matches('[').trim_end_matches(']').replace("]]", "]"),
            _ => return None,
        })
    };
    let mut name = name_at(from + 1)?;
    let mut schema = None;
    // after the table: nothing, or WHERE / ORDER BY / LIMIT, and no second statement
    let mut i = from + 2;
    if toks.get(i).map(|_| text(i) == ".").unwrap_or(false) {
        schema = Some(name);
        name = name_at(from + 3)?;
        i = from + 4;
    }
    if let Some(t) = toks.get(i) {
        let u = upper(i);
        if !matches!(u.as_str(), "WHERE" | "ORDER" | "LIMIT" | ";") {
            let _ = t;
            return None;
        }
    }
    depth = 0;
    while i < toks.len() {
        match text(i) {
            "(" => depth += 1,
            ")" => depth -= 1,
            _ => {}
        }
        if depth == 0 {
            let u = upper(i);
            if matches!(u.as_str(), "JOIN" | "GROUP" | "UNION" | "HAVING" | "INTERSECT" | "EXCEPT" | ",") && !after_order_by(&toks, script, i) {
                return None;
            }
            if u == ";" && i + 1 < toks.len() {
                return None;
            }
        }
        i += 1;
    }
    Some((schema, name))
}

/// A comma after ORDER BY lists sort columns, which is fine.
fn after_order_by(toks: &[cobalt_sql::lexer::Token], script: &str, i: usize) -> bool {
    if &script[toks[i].start..toks[i].end] != "," {
        return false;
    }
    toks[..i].iter().any(|t| script[t.start..t.end].eq_ignore_ascii_case("ORDER"))
}

fn tab_index(state: &AppState, tab: TabId) -> Option<usize> {
    state.tabs.iter().position(|t| t.id == tab)
}

/// Turn Edit Data on for a result set: find the table and its key; on failure a toast says why.
pub fn begin(state: &mut AppState, cx: &Ctx, idx: usize, set: usize) -> Result<(), String> {
    let Some(t) = state.tabs.get(idx) else { return Err("no tab".into()) };
    let pg = t.profile.as_ref().map(|p| p.engine == cobalt_core::EngineFamily::Postgres).unwrap_or(false);
    let Some(p) = t.profile.as_ref().filter(|p| p.is_file() || pg) else { return Err("Edit Data works on SQLite, DuckDB and PostgreSQL connections; SQL Server comes later.".into()) };
    let path = std::path::PathBuf::from(&p.server);
    let Some(run) = t.run.as_ref() else { return Err("Run a SELECT over one table first.".into()) };
    if run.is_live() {
        return Err("Wait for the query to finish.".into());
    }
    let Some(view) = run.result_sets.get(set) else { return Err("No result set to edit.".into()) };
    let Some((schema, table)) = single_table_parts(&run.script) else { return Err("Edit Data needs a query over one table: SELECT … FROM table [WHERE …] [ORDER BY …] [LIMIT …].".into()) };
    if schema.is_some() && !pg {
        return Err("Edit Data needs the table by its bare name on a file database.".into());
    }
    let table_cols = if pg {
        // the table's columns and key from the tab's catalog snapshot
        let schema_name = schema.clone().unwrap_or_else(|| "public".to_string());
        let Some(cat) = t.catalog.as_deref() else { return Err("The catalog is still loading; try again in a moment.".into()) };
        let Some(obj) = cat.objects.iter().find(|o| o.schema.eq_ignore_ascii_case(&schema_name) && o.name.eq_ignore_ascii_case(&table) && matches!(o.kind, cobalt_core::ObjectKind::Table | cobalt_core::ObjectKind::View)) else {
            return Err(format!("{schema_name}.{table} is not a table of this database."));
        };
        if obj.kind == cobalt_core::ObjectKind::View {
            return Err(format!("{table} is a view; only tables can be edited."));
        }
        obj.object_id.and_then(|id| cat.columns.get(&id)).cloned().unwrap_or_default()
    } else if p.engine == cobalt_core::EngineFamily::DuckDb {
        let db = cobalt_duckdb::open(&crate::ops::duckdb_root(cx), &path, true)?;
        let conn = cobalt_duckdb::Connection::new(&db)?;
        if !cobalt_duckdb::is_table(&conn, &table)? {
            return Err(format!("{table} is a view; only tables can be edited."));
        }
        cobalt_duckdb::table_columns(&conn, &table)?
    } else {
        let conn = cobalt_sqlite::open_read_only(&path)?;
        if !cobalt_sqlite::is_table(&conn, &table).map_err(|e| e.to_string())? {
            return Err(format!("{table} is a view; only tables can be edited."));
        }
        cobalt_sqlite::table_columns(&conn, &table).map_err(|e| e.to_string())?
    };
    let cols = &view.rs.columns;
    let find = |name: &str| cols.iter().position(|c| c.name.eq_ignore_ascii_case(name));
    let mut key: Vec<(String, usize)> = Vec::new();
    if let Some(i) = find("rowid").filter(|_| !pg) {
        key.push(("rowid".into(), i));
    } else {
        let pk: Vec<&str> = table_cols.iter().filter(|c| c.in_primary_key).map(|c| c.name.as_str()).collect();
        if pk.is_empty() {
            return Err(if pg { format!("{table} has no primary key, so its rows cannot be addressed for editing.") } else { format!("{table} has no primary key: select rowid AS \"rowid\" with the columns to edit it.") });
        }
        for k in pk {
            match find(k) {
                Some(i) => key.push((k.to_string(), i)),
                None => return Err(format!("Edit Data needs the key column {k} in the SELECT (or rowid AS \"rowid\").")),
            }
        }
    }
    let editable: Vec<usize> = cols.iter().enumerate().filter(|(_, c)| !c.name.eq_ignore_ascii_case("rowid") && table_cols.iter().any(|tc| tc.name.eq_ignore_ascii_case(&c.name))).map(|(i, _)| i).collect();
    if editable.is_empty() {
        return Err("None of the result's columns is a column of the table.".into());
    }
    let edits = GridEdits { table, schema, pg, key, editable, ..Default::default() };
    if let Some(v) = state.tabs[idx].run.as_mut().and_then(|r| r.result_sets.get_mut(set)) {
        v.edits = Some(edits);
        v.grid.focused = true;
    }
    Ok(())
}

/// The tree's *Edit Data*: a tab selecting the table with its key (the primary key columns,
/// or `rowid AS "rowid"`), run, then Edit Data turned on.
pub fn open_table(state: &mut AppState, cx: &Ctx, profile: ProfileId, obj: &ObjectRef) {
    let Some(p) = state.library.profile(profile).cloned() else { return };
    let path = std::path::PathBuf::from(&p.server);
    let has_pk = if p.engine == cobalt_core::EngineFamily::Postgres {
        // the tree's cached columns when it has them; begin() reports a missing key otherwise
        state.library.servers.get(&profile).and_then(|s| s.db_nodes.get(&obj.database)).and_then(|d| d.columns.get(&obj.object_id.unwrap_or(0))).and_then(|l| l.get()).map(|cols| cols.iter().any(|c| c.in_primary_key)).unwrap_or(true)
    } else if p.engine == cobalt_core::EngineFamily::DuckDb {
        cobalt_duckdb::open(&crate::ops::duckdb_root(cx), &path, true).and_then(|db| cobalt_duckdb::Connection::new(&db).and_then(|c| cobalt_duckdb::table_columns(&c, &obj.name))).map(|cols| cols.iter().any(|c| c.in_primary_key)).unwrap_or(false)
    } else {
        cobalt_sqlite::open_read_only(&path).and_then(|c| cobalt_sqlite::table_columns(&c, &obj.name).map_err(|e| e.to_string())).map(|cols| cols.iter().any(|c| c.in_primary_key)).unwrap_or(false)
    };
    let q = crate::ops::quoted_object(Some(&p), obj);
    let sql = if has_pk { format!("SELECT *\nFROM {q}\nLIMIT 1000") } else { format!("SELECT rowid AS \"rowid\", *\nFROM {q}\nLIMIT 1000") };
    let idx = new_query_tab(state, cx, Some(profile), None, Some((format!("Edit {}", obj.name), sql)), true);
    state.tabs[idx].edit_reopen = Some(0);
}

/// A SQL literal for a cell's text, typed by the column: numbers raw when they parse, bits as
/// 1/0, everything else quoted; `None` is NULL.
pub fn literal_in(t: &SqlType, v: Option<&str>, pg: bool) -> String {
    let Some(s) = v else { return "NULL".into() };
    let s = s.trim();
    let numeric = matches!(t, SqlType::TinyInt | SqlType::SmallInt | SqlType::Int | SqlType::BigInt | SqlType::Float | SqlType::Real | SqlType::Decimal { .. } | SqlType::Numeric { .. } | SqlType::Money | SqlType::SmallMoney);
    if matches!(t, SqlType::Bit) {
        return match s.to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "t" => if pg { "TRUE".into() } else { "1".into() },
            "false" | "0" | "no" | "f" => if pg { "FALSE".into() } else { "0".into() },
            _ => format!("'{}'", s.replace('\'', "''")),
        };
    }
    if numeric && s.parse::<f64>().is_ok() && !s.contains(char::is_whitespace) {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "''"))
}

/// The statements Save will run, in order: updates, deletes, inserts.
pub fn statements(view: &ResultSetView, fmt: &CellFormatter) -> Vec<String> {
    let Some(e) = view.edits.as_ref() else { return Vec::new() };
    let rs = &view.rs;
    let plain = cobalt_export::export_formatter(fmt);
    let q = cobalt_sqlite::quote;
    let pg = e.pg;
    let literal = |t: &SqlType, v: Option<&str>| literal_in(t, v, pg);
    let table = match &e.schema {
        Some(s) => format!("{}.{}", q(s), q(&e.table)),
        None => q(&e.table),
    };
    let key_where = |row: usize| -> String {
        e.key
            .iter()
            .map(|(name, col)| if rs.is_null(row, *col) { format!("{} IS NULL", q(name)) } else { format!("{} = {}", q(name), literal(&rs.columns[*col].sql_type, Some(&rs.cell_text(row, *col, &plain)))) })
            .collect::<Vec<_>>()
            .join(" AND ")
    };
    let data_rows = rs.visible_count();
    let mut out = Vec::new();
    let mut by_row: BTreeMap<usize, Vec<(usize, Option<String>)>> = BTreeMap::new();
    for ((row, col), v) in &e.cells {
        if *row < data_rows && !e.deleted.contains(row) && e.editable.contains(col) {
            by_row.entry(*row).or_default().push((*col, v.clone()));
        }
    }
    for (row, cells) in by_row {
        let sets: Vec<String> = cells.iter().map(|(col, v)| format!("{} = {}", q(&rs.columns[*col].name), literal(&rs.columns[*col].sql_type, v.as_deref()))).collect();
        out.push(format!("UPDATE {table} SET {} WHERE {};", sets.join(", "), key_where(row)));
    }
    for row in e.deleted.iter().filter(|r| **r < data_rows) {
        out.push(format!("DELETE FROM {table} WHERE {};", key_where(*row)));
    }
    for ins in &e.inserted {
        let filled: Vec<(usize, &String)> = e.editable.iter().filter_map(|&c| ins.get(c).and_then(|v| v.as_ref()).map(|v| (c, v))).collect();
        if filled.is_empty() {
            out.push(format!("INSERT INTO {table} DEFAULT VALUES;"));
        } else {
            let names: Vec<String> = filled.iter().map(|(c, _)| q(&rs.columns[*c].name)).collect();
            let values: Vec<String> = filled.iter().map(|(c, v)| literal(&rs.columns[*c].sql_type, Some(v))).collect();
            out.push(format!("INSERT INTO {table} ({}) VALUES ({});", names.join(", "), values.join(", ")));
        }
    }
    out
}

/// Save: the statements as one transaction through the tab's connection; the previous results
/// are kept aside so a failure brings them (and the staged edits) back.
pub fn save(state: &mut AppState, cx: &Ctx, idx: usize, set: usize) {
    let fmt = state.formatter.clone();
    let Some(t) = state.tabs.get_mut(idx) else { return };
    if t.is_running() {
        cx.toast(ToastKind::Warning, "A query is running in this tab.");
        return;
    }
    let Some(run) = t.run.as_ref() else { return };
    let Some(view) = run.result_sets.get(set) else { return };
    let stmts = statements(view, &fmt);
    if stmts.is_empty() {
        cx.toast(ToastKind::Info, "No pending changes.");
        return;
    }
    let original = run.script.clone();
    let count = stmts.len();
    let script = format!("BEGIN;\n{}\nCOMMIT;", stmts.join("\n"));
    let mut old = t.run.take().expect("run");
    if let Some(e) = old.result_sets.get_mut(set).and_then(|v| v.edits.as_mut()) {
        e.saving = true;
        e.error = None;
        e.editing = None;
    }
    t.edit_resume = Some(EditResume { set, script: original, run: Box::new(old), count });
    t.script_override = Some(script);
    run_sql(state, cx, idx, RunMode::All);
}

/// The save script finished: re-run the query (and reopen Edit Data on it), or bring the
/// previous results back with the error.
pub fn after_save(state: &mut AppState, cx: &Ctx, tab: TabId, ok: bool) {
    let Some(idx) = tab_index(state, tab) else { return };
    let Some(resume) = state.tabs[idx].edit_resume.take() else { return };
    if ok {
        cx.toast(ToastKind::Success, format!("Saved {} change{} to {}.", resume.count, if resume.count == 1 { "" } else { "s" }, resume.run.result_sets.get(resume.set).and_then(|v| v.edits.as_ref()).map(|e| e.table.clone()).unwrap_or_default()));
        let t = &mut state.tabs[idx];
        t.script_override = Some(resume.script);
        t.edit_reopen = Some(resume.set);
        run_sql(state, cx, idx, RunMode::All);
    } else {
        let t = &mut state.tabs[idx];
        let err = t.run.as_ref().and_then(|r| r.messages.iter().find(|m| m.is_error).map(|m| m.text.clone())).unwrap_or_else(|| "the save did not run".into());
        let mut old = resume.run;
        if let Some(e) = old.result_sets.get_mut(resume.set).and_then(|v| v.edits.as_mut()) {
            e.saving = false;
            e.error = Some(err.clone());
        }
        old.messages.push(MessageLine { text: format!("Save failed; nothing was written: {err}"), is_error: true, is_batch_header: false, line: None, path: None });
        t.run = Some(*old);
        cx.toast(ToastKind::Error, format!("Save failed: {err}"));
    }
}

/// The query ran again after a save (or the tree's Edit Data): Edit Data back on.
pub fn reopen(state: &mut AppState, cx: &Ctx, tab: TabId, set: usize) {
    let Some(idx) = tab_index(state, tab) else { return };
    if let Err(e) = begin(state, cx, idx, set) {
        tracing::info!(error = %e, "Edit Data did not reopen");
    }
}

/// Discard the staged changes; `off` also leaves Edit Data.
pub fn discard(state: &mut AppState, idx: usize, set: usize, off: bool) {
    if let Some(v) = state.tabs.get_mut(idx).and_then(|t| t.run.as_mut()).and_then(|r| r.result_sets.get_mut(set)) {
        if off {
            v.edits = None;
        } else if let Some(e) = v.edits.as_mut() {
            e.cells.clear();
            e.deleted.clear();
            e.inserted.clear();
            e.editing = None;
            e.error = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::single_table;

    #[test]
    fn single_table_detection() {
        let toks: Vec<(String, String)> = cobalt_sql::lexer::tokenize_dialect("SELECT *\nFROM \"customers\"\nLIMIT 1000", cobalt_sql::lexer::Dialect::Sqlite).into_iter().map(|t| (format!("{:?}", t.kind), "SELECT *\nFROM \"customers\"\nLIMIT 1000"[t.start..t.end].to_string())).collect();
        assert_eq!(single_table("SELECT *\nFROM \"customers\"\nLIMIT 1000").as_deref(), Some("customers"), "{toks:?}");
        assert_eq!(single_table("select id, name from customers where id > 2 order by name, id limit 10").as_deref(), Some("customers"));
        assert_eq!(single_table("SELECT rowid AS \"rowid\", * FROM orders").as_deref(), Some("orders"));
        assert_eq!(single_table("SELECT c.name, count(*) FROM orders o JOIN customers c ON c.id = o.customer_id GROUP BY c.name"), None);
        assert_eq!(single_table("SELECT DISTINCT city FROM customers"), None);
        assert_eq!(single_table("SELECT * FROM customers; SELECT 1"), None);
        assert_eq!(single_table("SELECT * FROM (SELECT * FROM customers)"), None);
        assert_eq!(single_table("SELECT a, b FROM t1, t2"), None);
    }
}
