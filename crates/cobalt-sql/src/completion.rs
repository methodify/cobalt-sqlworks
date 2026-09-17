//! Context-aware completion for the query editor.
//!
//! The engine looks at the tokens before the cursor to decide *what kind* of thing is
//! being typed (an object after `FROM`, a column after `alias.`, a type after
//! `DECLARE @x`, …), gathers candidates from the [`DatabaseCatalog`] and the text
//! itself (aliases, declared variables, temp tables), filters them by the word under
//! the cursor and ranks them.
//!
//! Alias resolution is a light token scan over the current statement: it understands
//! `FROM x AS a`, `FROM x a`, `JOIN y b`, comma-separated sources, bracketed and
//! qualified names, table-valued function calls, derived tables and CTE names. It never
//! fails on broken SQL — unknown things simply yield fewer candidates.

use crate::batches::split_batches;
use crate::lexer::{tokenize, Token, TokenKind, FUNCTIONS, KEYWORDS, SYSTEM_VARIABLES, TYPES};
use crate::snippets::SNIPPETS;
use crate::statements::statement_at;
use cobalt_core::{quote_ident, ColumnInfo, DatabaseCatalog, ObjectKind, ObjectRef};

/// What a completion item represents.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CompletionKind {
    /// T-SQL keyword.
    Keyword,
    /// Built-in function.
    Function,
    /// Data type.
    Type,
    /// Schema.
    Schema,
    /// Table (also synonyms and temp tables).
    Table,
    /// View.
    View,
    /// Stored procedure.
    Procedure,
    /// Table-valued function.
    TableFunction,
    /// Scalar or aggregate function defined in the database.
    ScalarFunction,
    /// Column of a table in the current statement.
    Column,
    /// Table alias in the current statement.
    Alias,
    /// Declared `@variable` or `@@system` value.
    Variable,
    /// Editor snippet; `insert` is the raw body with tab stops (see [`crate::snippets::expand`]).
    Snippet,
    /// Database name.
    Database,
}

impl CompletionKind {
    /// Human-readable label for the kind.
    pub fn label(&self) -> &'static str {
        match self {
            CompletionKind::Keyword => "Keyword",
            CompletionKind::Function => "Function",
            CompletionKind::Type => "Type",
            CompletionKind::Schema => "Schema",
            CompletionKind::Table => "Table",
            CompletionKind::View => "View",
            CompletionKind::Procedure => "Stored Procedure",
            CompletionKind::TableFunction => "Table-valued Function",
            CompletionKind::ScalarFunction => "Scalar-valued Function",
            CompletionKind::Column => "Column",
            CompletionKind::Alias => "Alias",
            CompletionKind::Variable => "Variable",
            CompletionKind::Snippet => "Snippet",
            CompletionKind::Database => "Database",
        }
    }

    fn base_score(&self) -> i32 {
        match self {
            CompletionKind::Column => 100,
            CompletionKind::Alias => 95,
            CompletionKind::Variable => 92,
            CompletionKind::Table | CompletionKind::View | CompletionKind::Procedure | CompletionKind::Database => 90,
            CompletionKind::TableFunction => 86,
            CompletionKind::ScalarFunction => 84,
            CompletionKind::Schema => 80,
            CompletionKind::Type => 70,
            CompletionKind::Function => 60,
            CompletionKind::Keyword => 50,
            CompletionKind::Snippet => 45,
        }
    }
}

/// One completion candidate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionItem {
    /// Text shown in the list (and matched against the typed prefix).
    pub label: String,
    /// Text to insert in place of the replace range (bracket-quoted when needed).
    pub insert: String,
    /// What the item is.
    pub kind: CompletionKind,
    /// Secondary text: a type label for columns, a kind label for objects, etc.
    pub detail: Option<String>,
    /// Ranking score; higher sorts first.
    pub score: i32,
}

/// Input to [`complete`].
#[derive(Clone, Copy, Debug)]
pub struct CompletionRequest<'a> {
    /// The full editor text.
    pub text: &'a str,
    /// Cursor position as a byte offset into `text`.
    pub cursor: usize,
    /// The connected database's catalog, if any.
    pub catalog: Option<&'a DatabaseCatalog>,
    /// Database names on the server (for `USE …`).
    pub databases: &'a [String],
    /// Maximum items to return (0 = unlimited).
    pub max_items: usize,
}

/// Result of [`complete`]: the items plus the byte range of `text` they replace.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Completions {
    /// Ranked items.
    pub items: Vec<CompletionItem>,
    /// Start of the word being completed.
    pub replace_start: usize,
    /// End of the word being completed (may extend past the cursor).
    pub replace_end: usize,
}

/// Compute completions for the cursor position in `req`.
pub fn complete(req: &CompletionRequest) -> Completions {
    let text = req.text;
    let mut cursor = req.cursor.min(text.len());
    while !text.is_char_boundary(cursor) {
        cursor -= 1;
    }

    // --- word under the cursor -------------------------------------------------
    let bytes = text.as_bytes();
    let mut start = cursor;
    while start > 0 && is_word_byte(bytes[start - 1]) {
        start -= 1;
    }
    while !text.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = cursor;
    while end < bytes.len() && is_word_byte(bytes[end]) {
        end += 1;
    }
    while !text.is_char_boundary(end) {
        end += 1;
    }
    let mut prefix = text[start..cursor].to_string();
    if start > 0 && bytes[start - 1] == b'[' {
        start -= 1;
        if end < bytes.len() && bytes[end] == b']' {
            end += 1;
        }
    }
    let prefix_lower = prefix.to_lowercase();
    prefix = prefix_lower;

    let ctx = Ctx::new(text, start, req);
    let mut items = ctx.gather(&prefix);

    // --- filter, score, rank ------------------------------------------------------
    items.retain(|it| matches_prefix(&it.label, &prefix));
    for it in &mut items {
        it.score += it.kind.base_score();
        if !prefix.is_empty() {
            if it.label.starts_with(text[start..cursor].trim_start_matches('[')) {
                it.score += 5;
            }
            if it.label.eq_ignore_ascii_case(&prefix) {
                it.score += 10;
            }
        }
    }
    items.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.label.to_lowercase().cmp(&b.label.to_lowercase()))
            .then_with(|| a.label.cmp(&b.label))
    });
    items.dedup_by(|a, b| a.label == b.label && a.kind == b.kind && a.detail == b.detail);
    if req.max_items > 0 && items.len() > req.max_items {
        items.truncate(req.max_items);
    }
    Completions { items, replace_start: start, replace_end: end }
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'@' || b == b'#' || b == b'$' || b >= 0x80
}

fn matches_prefix(label: &str, prefix_lower: &str) -> bool {
    prefix_lower.is_empty() || label.to_lowercase().starts_with(prefix_lower)
}

/// Strip `[...]` / `"..."` and un-double the escaped closer.
fn unquote(s: &str) -> String {
    if s.len() >= 2 && s.starts_with('[') && s.ends_with(']') {
        s[1..s.len() - 1].replace("]]", "]")
    } else if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        s[1..s.len() - 1].replace("\"\"", "\"")
    } else if let Some(rest) = s.strip_prefix('[') {
        rest.replace("]]", "]")
    } else {
        s.to_string()
    }
}

fn kind_for(obj: &ObjectRef) -> Option<CompletionKind> {
    Some(match obj.kind {
        ObjectKind::Table | ObjectKind::Synonym => CompletionKind::Table,
        ObjectKind::View => CompletionKind::View,
        ObjectKind::Procedure => CompletionKind::Procedure,
        ObjectKind::TableFunction => CompletionKind::TableFunction,
        ObjectKind::ScalarFunction | ObjectKind::AggregateFunction => CompletionKind::ScalarFunction,
        _ => return None,
    })
}

fn item(label: impl Into<String>, insert: impl Into<String>, kind: CompletionKind, detail: Option<String>) -> CompletionItem {
    CompletionItem { label: label.into(), insert: insert.into(), kind, detail, score: 0 }
}

/// Which object kinds an object-position context wants.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ObjectFilter {
    /// Tables, views, synonyms, table functions.
    TableSources,
    /// Tables and views only.
    TablesViews,
    /// Tables only.
    Tables,
    /// Views only.
    Views,
    /// Procedures.
    Procedures,
    /// Functions (scalar + table).
    Functions,
    /// Everything.
    Any,
}

impl ObjectFilter {
    fn accepts(self, kind: ObjectKind) -> bool {
        use ObjectKind::*;
        match self {
            ObjectFilter::TableSources => matches!(kind, Table | View | Synonym | TableFunction),
            ObjectFilter::TablesViews => matches!(kind, Table | View | Synonym),
            ObjectFilter::Tables => matches!(kind, Table | Synonym),
            ObjectFilter::Views => matches!(kind, View),
            ObjectFilter::Procedures => matches!(kind, Procedure),
            ObjectFilter::Functions => matches!(kind, ScalarFunction | TableFunction | AggregateFunction),
            ObjectFilter::Any => matches!(kind, Table | View | Synonym | TableFunction | ScalarFunction | AggregateFunction | Procedure),
        }
    }
}

/// A table source found in the current statement.
#[derive(Clone, Debug)]
struct TableRef {
    /// Name parts as written (unquoted); empty for derived tables.
    parts: Vec<String>,
    alias: Option<String>,
    /// Index into `catalog.objects` when resolved.
    object: Option<usize>,
    /// Column names known from the text (CTE / derived table select list).
    text_columns: Vec<String>,
}

impl TableRef {
    fn display(&self) -> String {
        if let Some(a) = &self.alias {
            a.clone()
        } else {
            self.parts.last().cloned().unwrap_or_else(|| "(derived)".into())
        }
    }
    fn matches_name(&self, q: &str) -> bool {
        self.alias.as_deref().is_some_and(|a| a.eq_ignore_ascii_case(q))
            || (self.alias.is_none() && self.parts.last().is_some_and(|n| n.eq_ignore_ascii_case(q)))
    }
}

struct Ctx<'a> {
    text: &'a str,
    tokens: Vec<Token>,
    /// Indices (into `tokens`) of significant tokens ending at or before the word.
    before: Vec<usize>,
    catalog: Option<&'a DatabaseCatalog>,
    databases: &'a [String],
    /// Absolute byte range of the current statement.
    stmt: (usize, usize),
    /// Absolute byte range of the current batch.
    batch: (usize, usize),
    cursor_paren_depth: usize,
}

impl<'a> Ctx<'a> {
    fn new(text: &'a str, word_start: usize, req: &CompletionRequest<'a>) -> Self {
        let tokens = tokenize(text);
        let before: Vec<usize> =
            (0..tokens.len()).filter(|&i| !tokens[i].kind.is_trivia() && tokens[i].end <= word_start).collect();

        let mut batch = (0, text.len());
        let mut stmt = (0, text.len());
        let batches = split_batches(text);
        if let Some(b) = batches.iter().rev().find(|b| b.start_offset <= word_start) {
            batch = (b.start_offset, b.start_offset + b.sql.len());
            let local = word_start.saturating_sub(b.start_offset).min(b.sql.len());
            match statement_at(&b.sql, local) {
                Some(s) => stmt = (b.start_offset + s.start, b.start_offset + s.end.max(local)),
                None => stmt = batch,
            }
        }
        // Cursor beyond the statement's last token still belongs to it.
        if stmt.1 < word_start {
            stmt.1 = word_start;
        }

        let mut depth = 0usize;
        for &i in &before {
            if tokens[i].start < stmt.0 {
                continue;
            }
            match tokens[i].text(text) {
                "(" => depth += 1,
                ")" => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
        Ctx { text, tokens, before, catalog: req.catalog, databases: req.databases, stmt, batch, cursor_paren_depth: depth }
    }

    fn tok(&self, i: usize) -> &Token {
        &self.tokens[i]
    }
    fn txt(&self, i: usize) -> &'a str {
        self.tokens[i].text(self.text)
    }
    fn up(&self, i: usize) -> String {
        self.txt(i).to_ascii_uppercase()
    }
    /// The n-th significant token before the word (0 = immediately before).
    fn prev(&self, n: usize) -> Option<usize> {
        self.before.len().checked_sub(n + 1).map(|k| self.before[k])
    }
    fn prev_is(&self, n: usize, word: &str) -> bool {
        self.prev(n).is_some_and(|i| self.up(i) == word)
    }
    fn prev_kind(&self, n: usize) -> Option<TokenKind> {
        self.prev(n).map(|i| self.tok(i).kind)
    }

    /// First keyword of the current statement, uppercased.
    fn stmt_kind(&self) -> String {
        self.tokens
            .iter()
            .find(|t| t.start >= self.stmt.0 && !t.kind.is_trivia())
            .map(|t| t.text(self.text).to_ascii_uppercase())
            .unwrap_or_default()
    }

    /// Second keyword of the current statement (e.g. `TABLE` in `CREATE TABLE`).
    fn stmt_second(&self) -> String {
        self.tokens
            .iter()
            .filter(|t| t.start >= self.stmt.0 && !t.kind.is_trivia())
            .nth(1)
            .map(|t| t.text(self.text).to_ascii_uppercase())
            .unwrap_or_default()
    }

    // ------------------------------------------------------------------ gather

    fn gather(&self, prefix: &str) -> Vec<CompletionItem> {
        if prefix.starts_with('@') {
            return self.variables();
        }
        if prefix.starts_with('#') {
            return self.temp_tables();
        }
        if self.prev_kind(0) == Some(TokenKind::Punct) && self.prev(0).is_some_and(|i| self.txt(i) == ".") {
            return self.qualified();
        }

        let p0 = self.prev(0);
        let Some(p0) = p0 else {
            return self.statement_start();
        };
        let t0 = self.up(p0);
        match self.tok(p0).kind {
            TokenKind::Keyword => match t0.as_str() {
                "FROM" | "JOIN" | "APPLY" | "USING" | "MERGE" => self.objects(ObjectFilter::TableSources),
                "INTO" | "UPDATE" | "DELETE" => self.objects(ObjectFilter::TablesViews),
                "TABLE" if self.prev_is(1, "TRUNCATE") || self.prev_is(1, "ALTER") || self.prev_is(1, "DROP") => {
                    self.objects(ObjectFilter::Tables)
                }
                "VIEW" if self.prev_is(1, "ALTER") || self.prev_is(1, "DROP") => self.objects(ObjectFilter::Views),
                "PROCEDURE" | "PROC" if self.prev_is(1, "ALTER") || self.prev_is(1, "DROP") => {
                    self.objects(ObjectFilter::Procedures)
                }
                "FUNCTION" if self.prev_is(1, "ALTER") || self.prev_is(1, "DROP") => self.objects(ObjectFilter::Functions),
                "EXEC" | "EXECUTE" => self.objects(ObjectFilter::Procedures),
                "USE" => self.databases(),
                "AS" if self.in_cast_paren() => self.types(),
                "AS" if self.prev_kind(1).is_some_and(|k| k.is_name() || k == TokenKind::Punct) => {
                    // alias position or `CREATE VIEW v AS` — keywords are all we can offer.
                    self.keywords(false)
                }
                "GO" => self.statement_start(),
                "SELECT" | "WHERE" | "ON" | "AND" | "OR" | "BY" | "HAVING" | "WHEN" | "THEN" | "ELSE" | "CASE"
                | "DISTINCT" | "TOP" | "NOT" | "IN" | "LIKE" | "BETWEEN" | "RETURN" | "PRINT" | "VALUES" | "IS"
                | "ALL" | "ANY" | "SOME" | "EXISTS" | "OVER" | "PARTITION" | "ORDER" | "GROUP" | "OUTPUT"
                | "MATCHED" | "COALESCE" | "IIF" => {
                    if matches!(t0.as_str(), "ORDER" | "GROUP" | "PARTITION" | "OUTPUT" | "MATCHED" | "EXISTS" | "IS" | "NOT" | "OVER") {
                        // Mostly keywords follow (`BY`, `NULL`, `(`), but columns are still plausible.
                        let mut v = self.keywords(false);
                        v.extend(self.columns());
                        v
                    } else {
                        self.column_context()
                    }
                }
                "SET" => {
                    if self.stmt_kind() == "UPDATE" || !self.table_refs().is_empty() {
                        self.column_context()
                    } else {
                        let mut v = self.keywords(false);
                        v.extend(self.variables());
                        v
                    }
                }
                "DECLARE" => self.variables(),
                _ => self.keywords(false),
            },
            TokenKind::Function => self.keywords(false),
            TokenKind::Punct => match self.txt(p0) {
                "(" => {
                    if self.prev(1).is_some_and(|i| {
                        self.tok(i).kind == TokenKind::Function && matches!(self.up(i).as_str(), "CONVERT" | "TRY_CONVERT")
                    }) {
                        self.types()
                    } else if self.stmt_kind() == "DECLARE" || (self.prev_kind(1) == Some(TokenKind::Type)) {
                        // `DECLARE @x varchar(` — nothing useful.
                        Vec::new()
                    } else {
                        self.column_context()
                    }
                }
                "," => {
                    if self.in_column_definition_list() {
                        // `CREATE TABLE t (a int, |` — next is a column name.
                        Vec::new()
                    } else if self.stmt_kind() == "DECLARE" {
                        self.variables()
                    } else {
                        self.column_context()
                    }
                }
                ";" => self.statement_start(),
                _ => self.keywords(false),
            },
            TokenKind::Operator => self.column_context(),
            TokenKind::Variable => {
                if self.prev_is(1, "DECLARE")
                    || (matches!(self.stmt_kind().as_str(), "DECLARE" | "CREATE" | "ALTER")
                        && self.prev(1).is_some_and(|i| matches!(self.txt(i), "," | "(") || self.up(i) == "DECLARE"))
                    || self.prev(1).is_none()
                {
                    self.types()
                } else {
                    // `WHERE x = @p |`, `OPEN @cur |` … — keywords follow.
                    self.keywords(false)
                }
            }
            TokenKind::Type => {
                // `DECLARE @x int |` / `CAST(x AS int |` → keywords such as `=`, `NULL`, `)`.
                self.keywords(false)
            }
            TokenKind::Identifier | TokenKind::BracketedIdentifier | TokenKind::QuotedIdentifier | TokenKind::TempTable => {
                if self.in_column_definition_list()
                    || self.prev_is(1, "ADD")
                    || self.prev_is(1, "COLUMN")
                    || (self.stmt_kind() == "CREATE" && self.stmt_second() == "TYPE")
                {
                    self.types()
                } else {
                    self.keywords(false)
                }
            }
            _ => self.keywords(false),
        }
    }

    /// True inside the parenthesised column list of `CREATE TABLE … (` / `DECLARE @t TABLE (`.
    fn in_column_definition_list(&self) -> bool {
        if self.cursor_paren_depth == 0 {
            return false;
        }
        let kind = self.stmt_kind();
        let second = self.stmt_second();
        (kind == "CREATE" && second == "TABLE")
            || (kind == "DECLARE" && self.stmt_tokens().any(|i| self.up(i) == "TABLE"))
            || (kind == "ALTER" && second == "TABLE" && self.stmt_tokens().any(|i| self.up(i) == "ADD"))
    }

    /// Significant token indices inside the current statement, before the word.
    fn stmt_tokens(&self) -> impl Iterator<Item = usize> + '_ {
        self.before.iter().copied().filter(move |&i| self.tok(i).start >= self.stmt.0)
    }

    /// `CAST(expr AS |` / `TRY_CAST(expr AS |`.
    fn in_cast_paren(&self) -> bool {
        let mut depth = 0usize;
        for &i in self.before.iter().rev() {
            match self.txt(i) {
                ")" => depth += 1,
                "(" => {
                    if depth == 0 {
                        // token before this paren
                        let pos = self.before.iter().position(|&k| k == i).unwrap_or(0);
                        return pos > 0 && {
                            let f = self.before[pos - 1];
                            self.tok(f).kind == TokenKind::Function && matches!(self.up(f).as_str(), "CAST" | "TRY_CAST")
                        };
                    }
                    depth -= 1;
                }
                _ => {}
            }
        }
        false
    }

    // ---------------------------------------------------------------- sources

    fn statement_start(&self) -> Vec<CompletionItem> {
        self.keywords(true)
    }

    fn keywords(&self, with_snippets: bool) -> Vec<CompletionItem> {
        let mut v: Vec<CompletionItem> = KEYWORDS.iter().map(|k| item(*k, *k, CompletionKind::Keyword, None)).collect();
        v.extend(self.functions());
        if with_snippets {
            v.extend(SNIPPETS.iter().map(|s| item(s.prefix, s.body, CompletionKind::Snippet, Some(s.label.to_string()))));
        }
        v
    }

    fn functions(&self) -> Vec<CompletionItem> {
        FUNCTIONS
            .iter()
            .filter(|f| !f.starts_with('@'))
            .map(|f| item(*f, *f, CompletionKind::Function, Some("Function".into())))
            .collect()
    }

    fn types(&self) -> Vec<CompletionItem> {
        let mut v: Vec<CompletionItem> = TYPES.iter().map(|t| item(*t, *t, CompletionKind::Type, Some("Type".into()))).collect();
        if let Some(cat) = self.catalog {
            for o in cat.objects.iter().filter(|o| o.kind == ObjectKind::TableType) {
                v.push(item(o.name.clone(), qualified_insert(o), CompletionKind::Type, Some(o.kind.label().to_string())));
            }
        }
        v
    }

    fn databases(&self) -> Vec<CompletionItem> {
        self.databases
            .iter()
            .map(|d| item(d.clone(), quote_ident(d), CompletionKind::Database, Some("Database".into())))
            .collect()
    }

    fn objects(&self, filter: ObjectFilter) -> Vec<CompletionItem> {
        let mut v = Vec::new();
        if let Some(cat) = self.catalog {
            for s in &cat.schemas {
                v.push(item(s.clone(), quote_ident(s), CompletionKind::Schema, Some("Schema".into())));
            }
            for o in cat.objects.iter().filter(|o| filter.accepts(o.kind)) {
                if let Some(kind) = kind_for(o) {
                    v.push(item(o.name.clone(), qualified_insert(o), kind, Some(object_detail(o))));
                }
            }
        }
        if matches!(filter, ObjectFilter::TableSources | ObjectFilter::TablesViews | ObjectFilter::Tables) {
            v.extend(self.temp_tables());
            // CTE names declared in this statement.
            for r in self.table_refs().iter().filter(|r| r.object.is_none() && r.parts.len() == 1 && !r.text_columns.is_empty()) {
                v.push(item(r.parts[0].clone(), quote_ident(&r.parts[0]), CompletionKind::Table, Some("Common Table Expression".into())));
            }
        }
        v
    }

    fn temp_tables(&self) -> Vec<CompletionItem> {
        let mut names: Vec<&str> = self
            .tokens
            .iter()
            .filter(|t| t.kind == TokenKind::TempTable && t.len() > 1)
            .map(|t| t.text(self.text))
            .collect();
        names.sort_unstable_by_key(|n| n.to_ascii_lowercase());
        names.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
        names.into_iter().map(|n| item(n, n, CompletionKind::Table, Some("Temporary table".into()))).collect()
    }

    /// Variables declared in the current batch (plus `@@` system values).
    fn variables(&self) -> Vec<CompletionItem> {
        let sig: Vec<usize> = (0..self.tokens.len())
            .filter(|&i| !self.tokens[i].kind.is_trivia() && self.tok(i).start >= self.batch.0 && self.tok(i).end <= self.batch.1)
            .collect();
        let mut v: Vec<CompletionItem> = Vec::new();
        for (k, &i) in sig.iter().enumerate() {
            let t = self.tok(i);
            if t.kind != TokenKind::Variable || t.len() < 2 || self.txt(i).starts_with("@@") {
                continue;
            }
            let next = sig.get(k + 1).copied();
            let prev = k.checked_sub(1).map(|p| sig[p]);
            let declared = next.is_some_and(|n| {
                self.tok(n).kind == TokenKind::Type || matches!(self.up(n).as_str(), "TABLE" | "CURSOR" | "AS")
            }) || prev.is_some_and(|p| self.up(p) == "DECLARE" || (self.txt(p) == "," && next.is_some_and(|n| self.tok(n).kind.is_name())));
            if !declared {
                continue;
            }
            let name = self.txt(i);
            if v.iter().any(|it| it.label.eq_ignore_ascii_case(name)) {
                continue;
            }
            let mut detail = next.map(|n| self.txt(n).to_string()).unwrap_or_default();
            if let Some(n) = next {
                if sig.get(k + 2).is_some_and(|&p| self.txt(p) == "(") && self.tok(n).kind == TokenKind::Type {
                    // include `(len)` / `(p, s)`
                    let close = sig[k + 2..].iter().position(|&q| self.txt(q) == ")").map(|off| sig[k + 2 + off]);
                    if let Some(c) = close {
                        detail = self.text[self.tok(n).start..self.tok(c).end].to_string();
                    }
                }
            }
            let mut it = item(name, name, CompletionKind::Variable, Some(detail));
            it.score = 5; // declared variables rank above `@@` system values
            v.push(it);
        }
        v.extend(SYSTEM_VARIABLES.iter().map(|s| item(*s, *s, CompletionKind::Variable, Some("System".into()))));
        v
    }

    fn column_context(&self) -> Vec<CompletionItem> {
        let mut v = self.columns();
        for r in self.table_refs() {
            if let Some(a) = &r.alias {
                v.push(item(a.clone(), quote_ident(a), CompletionKind::Alias, Some(format!("Alias for {}", r.parts.join(".")))));
            } else if let Some(n) = r.parts.last() {
                v.push(item(n.clone(), quote_ident(n), CompletionKind::Alias, Some("Table".into())));
            }
        }
        v.extend(self.variables().into_iter().filter(|it| !it.label.starts_with("@@")));
        v.extend(self.keywords(false));
        v
    }

    /// Columns of every table in the current statement, labelled with their source.
    fn columns(&self) -> Vec<CompletionItem> {
        let refs = self.table_refs();
        let multi = refs.len() > 1;
        let mut v = Vec::new();
        for r in &refs {
            let src = r.display();
            for (name, detail) in self.columns_of(r) {
                let d = if multi { format!("{detail} · {src}") } else { detail };
                v.push(item(name.clone(), quote_ident(&name), CompletionKind::Column, Some(d)));
            }
        }
        v
    }

    fn columns_of(&self, r: &TableRef) -> Vec<(String, String)> {
        if let (Some(cat), Some(idx)) = (self.catalog, r.object) {
            let obj = &cat.objects[idx];
            if let Some(cols) = obj.object_id.and_then(|id| cat.columns.get(&id)) {
                return cols.iter().map(|c: &ColumnInfo| (c.name.clone(), c.type_label())).collect();
            }
            return Vec::new();
        }
        r.text_columns.iter().map(|c| (c.clone(), "Column".to_string())).collect()
    }

    /// `something.` — columns of an alias/table, or objects of a schema.
    fn qualified(&self) -> Vec<CompletionItem> {
        // Collect the qualifier chain: name (. name)* ending with the `.` before the word.
        let mut parts: Vec<String> = Vec::new();
        let mut n = 1; // prev(0) is the '.'
        while let Some(i) = self.prev(n) {
            let k = self.tok(i).kind;
            if k.is_name() || k == TokenKind::TempTable || k == TokenKind::Variable || k == TokenKind::Keyword || k == TokenKind::Type {
                parts.push(unquote(self.txt(i)));
            } else {
                break;
            }
            if self.prev(n + 1).is_some_and(|d| self.txt(d) == ".") {
                n += 2;
            } else {
                break;
            }
        }
        parts.reverse();
        if parts.is_empty() {
            return Vec::new();
        }
        // The keyword that opened this object position (if any), to pick a filter.
        let keyword_before = self.prev(parts.len() * 2).map(|i| self.up(i)).unwrap_or_default();
        let filter = match keyword_before.as_str() {
            "FROM" | "JOIN" | "APPLY" | "USING" | "MERGE" => ObjectFilter::TableSources,
            "INTO" | "UPDATE" | "DELETE" | "TABLE" => ObjectFilter::TablesViews,
            "EXEC" | "EXECUTE" => ObjectFilter::Procedures,
            _ => ObjectFilter::Any,
        };
        let refs = self.table_refs();

        match parts.len() {
            1 => {
                let q = &parts[0];
                // An alias always wins; an unaliased table name only if we know its columns
                // (otherwise `sales.` in `FROM sales.` must still list the schema's objects).
                if let Some(r) = refs
                    .iter()
                    .find(|r| r.matches_name(q) && (r.alias.is_some() || r.object.is_some() || !r.text_columns.is_empty()))
                {
                    return self.columns_of_ref(r);
                }
                let mut v = Vec::new();
                if let Some(cat) = self.catalog {
                    if cat.schemas.iter().any(|s| s.eq_ignore_ascii_case(q)) {
                        v.extend(self.schema_objects(q, filter));
                    }
                    if v.is_empty() {
                        if let Some(idx) = cat.objects.iter().position(|o| o.name.eq_ignore_ascii_case(q)) {
                            let r = TableRef { parts: vec![q.clone()], alias: None, object: Some(idx), text_columns: vec![] };
                            v.extend(self.columns_of_ref(&r));
                        }
                    }
                    if v.is_empty() && (cat.database.eq_ignore_ascii_case(q) || self.databases.iter().any(|d| d.eq_ignore_ascii_case(q))) {
                        v.extend(cat.schemas.iter().map(|s| item(s.clone(), quote_ident(s), CompletionKind::Schema, Some("Schema".into()))));
                    }
                }
                v
            }
            2 => {
                let mut v = Vec::new();
                if let Some(cat) = self.catalog {
                    if let Some(idx) = cat.objects.iter().position(|o| o.schema.eq_ignore_ascii_case(&parts[0]) && o.name.eq_ignore_ascii_case(&parts[1])) {
                        let r = TableRef { parts: parts.clone(), alias: None, object: Some(idx), text_columns: vec![] };
                        v.extend(self.columns_of_ref(&r));
                    } else if cat.schemas.iter().any(|s| s.eq_ignore_ascii_case(&parts[1])) {
                        v.extend(self.schema_objects(&parts[1], filter));
                    }
                }
                v
            }
            _ => {
                let mut v = Vec::new();
                if let Some(cat) = self.catalog {
                    let (s, t) = (&parts[parts.len() - 2], &parts[parts.len() - 1]);
                    if let Some(idx) = cat.objects.iter().position(|o| o.schema.eq_ignore_ascii_case(s) && o.name.eq_ignore_ascii_case(t)) {
                        let r = TableRef { parts: parts.clone(), alias: None, object: Some(idx), text_columns: vec![] };
                        v.extend(self.columns_of_ref(&r));
                    }
                }
                v
            }
        }
    }

    fn columns_of_ref(&self, r: &TableRef) -> Vec<CompletionItem> {
        self.columns_of(r)
            .into_iter()
            .map(|(name, detail)| item(name.clone(), quote_ident(&name), CompletionKind::Column, Some(detail)))
            .collect()
    }

    fn schema_objects(&self, schema: &str, filter: ObjectFilter) -> Vec<CompletionItem> {
        let Some(cat) = self.catalog else { return Vec::new() };
        cat.objects
            .iter()
            .filter(|o| o.schema.eq_ignore_ascii_case(schema) && filter.accepts(o.kind))
            .filter_map(|o| kind_for(o).map(|k| item(o.name.clone(), quote_ident(&o.name), k, Some(object_detail(o)))))
            .collect()
    }

    // ------------------------------------------------------------ table refs

    /// Scan the current statement for table sources and CTEs.
    fn table_refs(&self) -> Vec<TableRef> {
        let sig: Vec<usize> = (0..self.tokens.len())
            .filter(|&i| {
                let t = self.tok(i);
                !t.kind.is_trivia() && t.start >= self.stmt.0 && t.end <= self.stmt.1
            })
            .collect();
        let mut refs: Vec<TableRef> = Vec::new();
        let mut k = 0;
        // CTE chain at the start of the statement.
        if sig.first().is_some_and(|&i| self.up(i) == "WITH") {
            k = 1;
            while let Some(&name_i) = sig.get(k) {
                if !(self.tok(name_i).kind.is_name() || self.tok(name_i).kind == TokenKind::Keyword) {
                    break;
                }
                let name = unquote(self.txt(name_i));
                k += 1;
                let mut cols: Vec<String> = Vec::new();
                if sig.get(k).is_some_and(|&i| self.txt(i) == "(") {
                    let close = self.matching_paren(&sig, k);
                    cols = sig[k + 1..close.min(sig.len())]
                        .iter()
                        .filter(|&&i| self.tok(i).kind.is_name())
                        .map(|&i| unquote(self.txt(i)))
                        .collect();
                    k = close + 1;
                }
                if sig.get(k).is_some_and(|&i| self.up(i) == "AS") {
                    k += 1;
                }
                if sig.get(k).is_some_and(|&i| self.txt(i) == "(") {
                    let close = self.matching_paren(&sig, k);
                    if cols.is_empty() {
                        cols = self.select_list_columns(&sig[k + 1..close.min(sig.len())]);
                    }
                    k = close + 1;
                }
                refs.push(TableRef { parts: vec![name], alias: None, object: None, text_columns: cols });
                if sig.get(k).is_some_and(|&i| self.txt(i) == ",") {
                    k += 1;
                } else {
                    break;
                }
            }
        }

        let ctes: Vec<TableRef> = refs.clone();
        while k < sig.len() {
            let i = sig[k];
            if self.tok(i).kind == TokenKind::Keyword && matches!(self.up(i).as_str(), "FROM" | "JOIN" | "INTO" | "UPDATE" | "APPLY" | "USING" | "MERGE") {
                let allow_comma = matches!(self.up(i).as_str(), "FROM" | "UPDATE");
                k += 1;
                while let Some(r) = self.parse_source(&sig, &mut k, &ctes) {
                    refs.push(r);
                    if allow_comma && sig.get(k).is_some_and(|&c| self.txt(c) == ",") {
                        k += 1;
                    } else {
                        break;
                    }
                }
            } else {
                k += 1;
            }
        }
        refs
    }

    /// Index (into `sig`) of the `)` matching the `(` at `sig[open]`; `sig.len()` if unbalanced.
    fn matching_paren(&self, sig: &[usize], open: usize) -> usize {
        let mut depth = 0usize;
        for (off, &i) in sig[open..].iter().enumerate() {
            match self.txt(i) {
                "(" => depth += 1,
                ")" => {
                    depth -= 1;
                    if depth == 0 {
                        return open + off;
                    }
                }
                _ => {}
            }
        }
        sig.len()
    }

    /// Parse one table source at `sig[*k]`; advances `*k`.
    fn parse_source(&self, sig: &[usize], k: &mut usize, ctes: &[TableRef]) -> Option<TableRef> {
        let &i = sig.get(*k)?;
        let mut parts: Vec<String> = Vec::new();
        let mut text_columns = Vec::new();
        if self.txt(i) == "(" {
            let close = self.matching_paren(sig, *k);
            text_columns = self.select_list_columns(&sig[*k + 1..close.min(sig.len())]);
            *k = close + 1;
        } else {
            // name (. name)*, allowing empty middle parts (`db..t`).
            while let Some(&j) = sig.get(*k) {
                let kind = self.tok(j).kind;
                if kind.is_name() || kind == TokenKind::TempTable || kind == TokenKind::Variable || kind == TokenKind::Type {
                    parts.push(unquote(self.txt(j)));
                    *k += 1;
                } else if self.txt(j) == "." && !parts.is_empty() {
                    // `a..b`
                    parts.push(String::new());
                } else {
                    break;
                }
                if sig.get(*k).is_some_and(|&d| self.txt(d) == ".") {
                    *k += 1;
                    continue;
                }
                break;
            }
            if parts.is_empty() {
                return None;
            }
            // table-valued function call
            if sig.get(*k).is_some_and(|&p| self.txt(p) == "(") {
                let close = self.matching_paren(sig, *k);
                *k = close + 1;
            }
        }
        // optional AS alias
        if sig.get(*k).is_some_and(|&a| self.up(a) == "AS") {
            *k += 1;
        }
        let mut alias = None;
        if let Some(&a) = sig.get(*k) {
            if self.tok(a).kind.is_name() {
                alias = Some(unquote(self.txt(a)));
                *k += 1;
            }
        }
        // table hints
        if sig.get(*k).is_some_and(|&w| self.up(w) == "WITH") && sig.get(*k + 1).is_some_and(|&p| self.txt(p) == "(") {
            let close = self.matching_paren(sig, *k + 1);
            *k = close + 1;
        }
        let object = self.resolve(&parts);
        if object.is_none() && parts.len() == 1 {
            if let Some(c) = ctes.iter().find(|c| c.parts[0].eq_ignore_ascii_case(&parts[0])) {
                text_columns = c.text_columns.clone();
            }
        }
        Some(TableRef { parts, alias, object, text_columns })
    }

    fn resolve(&self, parts: &[String]) -> Option<usize> {
        let cat = self.catalog?;
        let (schema, name) = match parts {
            [n] => (None, n.as_str()),
            [s, n] => (Some(s.as_str()).filter(|s| !s.is_empty()), n.as_str()),
            [.., s, n] => (Some(s.as_str()).filter(|s| !s.is_empty()), n.as_str()),
            [] => return None,
        };
        let found = cat.find(schema, name)?;
        cat.objects.iter().position(|o| std::ptr::eq(o, found))
    }

    /// Column names produced by a `SELECT` list (used for CTEs and derived tables).
    fn select_list_columns(&self, body: &[usize]) -> Vec<String> {
        let mut out = Vec::new();
        let Some(sel) = body.iter().position(|&i| self.up(i) == "SELECT") else { return out };
        let mut k = sel + 1;
        // skip DISTINCT / TOP (n) / TOP n
        while let Some(&i) = body.get(k) {
            match self.up(i).as_str() {
                "DISTINCT" | "ALL" => k += 1,
                "TOP" => {
                    k += 1;
                    if body.get(k).is_some_and(|&p| self.txt(p) == "(") {
                        k = self.matching_paren(body, k) + 1;
                    } else {
                        k += 1;
                    }
                }
                _ => break,
            }
        }
        let mut depth = 0usize;
        let mut item_tokens: Vec<usize> = Vec::new();
        let finish = |item_tokens: &mut Vec<usize>, out: &mut Vec<String>| {
            if let Some(&last) = item_tokens.last() {
                let t = self.tok(last);
                if t.kind.is_name() {
                    out.push(unquote(self.txt(last)));
                } else if self.txt(last) == "*" {
                    // `t.*` / `*` — unknown columns
                }
            }
            item_tokens.clear();
        };
        while let Some(&i) = body.get(k) {
            let text = self.txt(i);
            if depth == 0 && (text == "," ) {
                finish(&mut item_tokens, &mut out);
            } else if depth == 0 && matches!(self.up(i).as_str(), "FROM" | "INTO") {
                break;
            } else {
                match text {
                    "(" => depth += 1,
                    ")" => depth = depth.saturating_sub(1),
                    _ => {}
                }
                item_tokens.push(i);
            }
            k += 1;
        }
        finish(&mut item_tokens, &mut out);
        out
    }
}

fn object_detail(o: &ObjectRef) -> String {
    if o.schema.eq_ignore_ascii_case("dbo") {
        o.kind.label().to_string()
    } else {
        format!("{} · {}", o.kind.label(), o.schema)
    }
}

fn qualified_insert(o: &ObjectRef) -> String {
    if o.schema.eq_ignore_ascii_case("dbo") {
        quote_ident(&o.name)
    } else {
        format!("{}.{}", quote_ident(&o.schema), quote_ident(&o.name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cobalt_core::SqlType;

    fn catalog() -> DatabaseCatalog {
        let mut cat = DatabaseCatalog { database: "Shop".into(), schemas: vec!["dbo".into(), "sales".into()], ..Default::default() };
        let mut add = |schema: &str, name: &str, kind: ObjectKind, id: i32, cols: &[(&str, SqlType)]| {
            cat.objects.push(ObjectRef { database: "Shop".into(), schema: schema.into(), name: name.into(), kind, object_id: Some(id) });
            if !cols.is_empty() {
                cat.columns.insert(
                    id,
                    cols.iter().enumerate().map(|(i, (n, t))| ColumnInfo::new(*n, t.clone(), i > 0, i)).collect(),
                );
            }
        };
        add("dbo", "Customers", ObjectKind::Table, 1, &[("CustomerId", SqlType::Int), ("Name", SqlType::NVarChar { len: Some(100) }), ("Email", SqlType::NVarChar { len: None })]);
        add("sales", "Orders", ObjectKind::Table, 2, &[("OrderId", SqlType::Int), ("CustomerId", SqlType::Int), ("Order Date", SqlType::Date), ("Total", SqlType::Money)]);
        add("sales", "Order Lines", ObjectKind::Table, 3, &[("OrderId", SqlType::Int), ("LineNo", SqlType::SmallInt), ("Qty", SqlType::Int)]);
        add("dbo", "vActiveCustomers", ObjectKind::View, 4, &[("CustomerId", SqlType::Int), ("Name", SqlType::NVarChar { len: Some(100) })]);
        add("dbo", "usp_GetOrders", ObjectKind::Procedure, 5, &[]);
        add("sales", "fn_OrdersFor", ObjectKind::TableFunction, 6, &[("OrderId", SqlType::Int)]);
        add("dbo", "fn_Tax", ObjectKind::ScalarFunction, 7, &[]);
        add("dbo", "OrderLineType", ObjectKind::TableType, 8, &[]);
        cat
    }

    fn run(text: &str, cursor: usize) -> Completions {
        let cat = catalog();
        let dbs = vec!["master".to_string(), "Shop".to_string()];
        complete(&CompletionRequest { text, cursor, catalog: Some(&cat), databases: &dbs, max_items: 0 })
    }

    /// Complete at the `|` marker.
    fn at(text_with_marker: &str) -> Completions {
        let cursor = text_with_marker.find('|').expect("marker");
        let text = text_with_marker.replacen('|', "", 1);
        run(&text, cursor)
    }

    fn labels(c: &Completions) -> Vec<&str> {
        c.items.iter().map(|i| i.label.as_str()).collect()
    }
    fn of_kind(c: &Completions, k: CompletionKind) -> Vec<&str> {
        c.items.iter().filter(|i| i.kind == k).map(|i| i.label.as_str()).collect()
    }

    #[test]
    fn after_from_lists_schemas_tables_views_and_tvfs() {
        let c = at("SELECT * FROM |");
        assert_eq!(of_kind(&c, CompletionKind::Schema), vec!["dbo", "sales"]);
        assert!(of_kind(&c, CompletionKind::Table).contains(&"Customers"));
        assert!(of_kind(&c, CompletionKind::Table).contains(&"Orders"));
        assert!(of_kind(&c, CompletionKind::View).contains(&"vActiveCustomers"));
        assert!(of_kind(&c, CompletionKind::TableFunction).contains(&"fn_OrdersFor"));
        assert!(of_kind(&c, CompletionKind::Procedure).is_empty());
        assert!(of_kind(&c, CompletionKind::Keyword).is_empty());
        assert_eq!((c.replace_start, c.replace_end), (14, 14));
    }

    #[test]
    fn prefix_filter_and_replace_range() {
        let c = at("SELECT * FROM cus|");
        assert_eq!(labels(&c), vec!["Customers"]);
        assert_eq!((c.replace_start, c.replace_end), (14, 17));
        let c = at("SELECT * FROM Cus|tomers WHERE 1=1");
        assert_eq!((c.replace_start, c.replace_end), (14, 23));
    }

    #[test]
    fn non_dbo_objects_insert_qualified_and_quoted() {
        let c = at("SELECT * FROM order|");
        let ol = c.items.iter().find(|i| i.label == "Order Lines").unwrap();
        assert_eq!(ol.insert, "sales.[Order Lines]");
        assert_eq!(ol.detail.as_deref(), Some("Table · sales"));
        let cust = at("SELECT * FROM cust|").items.remove(0);
        assert_eq!(cust.insert, "Customers");
        assert_eq!(cust.detail.as_deref(), Some("Table"));
    }

    #[test]
    fn after_schema_dot_lists_schema_objects() {
        let c = at("SELECT * FROM sales.|");
        assert_eq!(labels(&c), vec!["Order Lines", "Orders", "fn_OrdersFor"]);
        let c = at("SELECT * FROM [sales].or|");
        assert_eq!(labels(&c), vec!["Order Lines", "Orders"]);
        assert_eq!(c.items[0].insert, "[Order Lines]");
    }

    #[test]
    fn exec_lists_procedures() {
        let c = at("EXEC |");
        assert_eq!(of_kind(&c, CompletionKind::Procedure), vec!["usp_GetOrders"]);
        assert!(of_kind(&c, CompletionKind::Table).is_empty());
        let c = at("EXECUTE dbo.|");
        assert_eq!(labels(&c), vec!["usp_GetOrders"]);
    }

    #[test]
    fn alias_dot_gives_columns_with_type_detail() {
        let c = at("SELECT o.| FROM sales.Orders AS o");
        assert_eq!(labels(&c), vec!["CustomerId", "Order Date", "OrderId", "Total"]);
        let od = c.items.iter().find(|i| i.label == "Order Date").unwrap();
        assert_eq!(od.insert, "[Order Date]");
        assert_eq!(od.detail.as_deref(), Some("date, null"));
        assert_eq!(c.items.iter().find(|i| i.label == "OrderId").unwrap().detail.as_deref(), Some("int, not null"));
    }

    #[test]
    fn alias_forms_without_as_and_with_join() {
        let c = at("SELECT c.| FROM dbo.Customers c INNER JOIN sales.Orders o ON o.CustomerId = c.CustomerId");
        assert_eq!(labels(&c), vec!["CustomerId", "Email", "Name"]);
        let c = at("SELECT * FROM dbo.Customers c INNER JOIN [sales].[Orders] o ON o.|");
        assert_eq!(labels(&c), vec!["CustomerId", "Order Date", "OrderId", "Total"]);
        let c = at("SELECT * FROM Customers WHERE Customers.|");
        assert_eq!(labels(&c), vec!["CustomerId", "Email", "Name"]);
    }

    #[test]
    fn schema_table_dot_gives_columns() {
        let c = at("SELECT sales.Orders.| FROM sales.Orders");
        assert_eq!(labels(&c), vec!["CustomerId", "Order Date", "OrderId", "Total"]);
        let c = at("SELECT Shop.sales.Orders.| FROM Shop.sales.Orders");
        assert_eq!(labels(&c), vec!["CustomerId", "Order Date", "OrderId", "Total"]);
    }

    #[test]
    fn after_select_lists_columns_of_all_tables_then_functions_then_keywords() {
        let c = at("SELECT | FROM dbo.Customers c JOIN sales.Orders o ON 1=1");
        let cols = of_kind(&c, CompletionKind::Column);
        assert_eq!(cols, vec!["CustomerId", "CustomerId", "Email", "Name", "Order Date", "OrderId", "Total"]);
        let first = &c.items[0];
        assert!(first.detail.as_deref().unwrap().ends_with("· c") || first.detail.as_deref().unwrap().ends_with("· o"));
        assert_eq!(of_kind(&c, CompletionKind::Alias), vec!["c", "o"]);
        // columns rank above functions which rank above keywords
        let pos = |k: CompletionKind| c.items.iter().position(|i| i.kind == k).unwrap();
        assert!(pos(CompletionKind::Column) < pos(CompletionKind::Function));
        assert!(pos(CompletionKind::Function) < pos(CompletionKind::Keyword));
    }

    #[test]
    fn where_and_on_and_comma_contexts() {
        for src in [
            "SELECT * FROM dbo.Customers WHERE |",
            "SELECT * FROM dbo.Customers WHERE 1=1 AND |",
            "SELECT * FROM dbo.Customers c JOIN sales.Orders o ON |",
            "SELECT Name, | FROM dbo.Customers",
            "SELECT * FROM dbo.Customers ORDER BY |",
            "SELECT COUNT(| FROM dbo.Customers",
            "SELECT * FROM dbo.Customers WHERE Name = |",
        ] {
            let c = at(src);
            assert!(of_kind(&c, CompletionKind::Column).contains(&"Name"), "{src}: {:?}", labels(&c));
        }
    }

    #[test]
    fn prefix_ranks_columns_above_keywords() {
        let c = at("SELECT na| FROM dbo.Customers");
        assert_eq!(c.items[0].label, "Name");
        assert_eq!(c.items[0].kind, CompletionKind::Column);
        assert!(c.items.iter().any(|i| i.kind == CompletionKind::Keyword && i.label == "NATIONAL"));
    }

    #[test]
    fn update_set_gives_target_columns() {
        let c = at("UPDATE sales.Orders SET |");
        assert_eq!(of_kind(&c, CompletionKind::Column), vec!["CustomerId", "Order Date", "OrderId", "Total"]);
        let c = at("UPDATE |");
        assert!(of_kind(&c, CompletionKind::Table).contains(&"Orders"));
    }

    #[test]
    fn use_lists_databases() {
        let c = at("USE |");
        assert_eq!(labels(&c), vec!["master", "Shop"]);
        assert_eq!(c.items[0].kind, CompletionKind::Database);
    }

    #[test]
    fn declare_and_cast_give_types() {
        let c = at("DECLARE @x |");
        assert!(of_kind(&c, CompletionKind::Type).contains(&"int"));
        assert!(of_kind(&c, CompletionKind::Type).contains(&"OrderLineType"));
        assert!(of_kind(&c, CompletionKind::Keyword).is_empty());
        let c = at("SELECT CAST(x AS nv|");
        assert_eq!(labels(&c), vec!["nvarchar"]);
        let c = at("SELECT TRY_CAST(x AS |) FROM t");
        assert!(of_kind(&c, CompletionKind::Type).contains(&"bigint"));
        let c = at("SELECT CONVERT(|");
        assert!(of_kind(&c, CompletionKind::Type).contains(&"date"));
        let c = at("CREATE TABLE t (id |");
        assert!(of_kind(&c, CompletionKind::Type).contains(&"int"));
    }

    #[test]
    fn at_prefix_lists_declared_variables() {
        let c = at("DECLARE @count int = 0, @name nvarchar(50);\nDECLARE @t TABLE (x int)\nSELECT @|");
        let vars = of_kind(&c, CompletionKind::Variable);
        assert!(vars.starts_with(&["@count", "@name", "@t"]), "{vars:?}");
        assert!(vars.contains(&"@@ROWCOUNT"));
        assert_eq!(c.items.iter().find(|i| i.label == "@name").unwrap().detail.as_deref(), Some("nvarchar(50)"));
        let c = at("DECLARE @count int\nSELECT @@ro|");
        assert_eq!(labels(&c), vec!["@@ROWCOUNT"]);
    }

    #[test]
    fn variables_are_scoped_to_batch() {
        let c = at("DECLARE @a int\nGO\nDECLARE @b int\nSELECT @|");
        let vars = of_kind(&c, CompletionKind::Variable);
        assert!(vars.contains(&"@b") && !vars.contains(&"@a"));
    }

    #[test]
    fn statement_start_gives_keywords_and_snippets() {
        let c = at("|");
        assert!(of_kind(&c, CompletionKind::Keyword).contains(&"SELECT"));
        assert!(of_kind(&c, CompletionKind::Snippet).contains(&"sel"));
        let c = at("sel|");
        assert!(labels(&c).contains(&"SELECT") && labels(&c).contains(&"sel") && labels(&c).contains(&"selw"));
        assert_eq!(c.items.iter().find(|i| i.label == "sel").unwrap().kind, CompletionKind::Snippet);
        let c = at("SELECT 1;\n|");
        assert!(of_kind(&c, CompletionKind::Snippet).contains(&"cte"));
    }

    #[test]
    fn temp_tables_and_cte_columns() {
        let c = at("CREATE TABLE #tmp (a int)\nSELECT * FROM #|");
        assert_eq!(labels(&c), vec!["#tmp"]);
        let c = at("WITH c AS (SELECT CustomerId, Name AS N, COUNT(*) AS Cnt FROM dbo.Customers)\nSELECT c.| FROM c");
        assert_eq!(labels(&c), vec!["Cnt", "CustomerId", "N"]);
        let c = at("SELECT d.| FROM (SELECT OrderId, Total FROM sales.Orders) AS d");
        assert_eq!(labels(&c), vec!["OrderId", "Total"]);
    }

    #[test]
    fn works_without_catalog_and_on_garbage() {
        let text = "SELECT o.| FROM sales.Orders o";
        let cursor = text.find('|').unwrap();
        let text = text.replacen('|', "", 1);
        let c = complete(&CompletionRequest { text: &text, cursor, catalog: None, databases: &[], max_items: 5 });
        assert!(c.items.is_empty());
        let c = complete(&CompletionRequest { text: "((( 'x ;; [[ /* @", cursor: 17, catalog: None, databases: &[], max_items: 5 });
        assert!(c.items.len() <= 5);
        let c = complete(&CompletionRequest { text: "sel", cursor: 99, catalog: None, databases: &[], max_items: 3 });
        assert_eq!(c.items.len(), 3);
        assert_eq!((c.replace_start, c.replace_end), (0, 3));
    }

    #[test]
    fn bracket_prefix_is_part_of_replace_range() {
        let c = at("SELECT * FROM sales.[Ord|");
        assert_eq!(labels(&c), vec!["Order Lines", "Orders"]);
        assert_eq!((c.replace_start, c.replace_end), (20, 24));
        let c = at("SELECT * FROM sales.[Ord|]");
        assert_eq!((c.replace_start, c.replace_end), (20, 25));
    }

    #[test]
    fn max_items_and_stable_order() {
        let a = at("SELECT | FROM dbo.Customers");
        let b = at("SELECT | FROM dbo.Customers");
        assert_eq!(a, b);
        let cat = catalog();
        let c = complete(&CompletionRequest { text: "SELECT ", cursor: 7, catalog: Some(&cat), databases: &[], max_items: 4 });
        assert_eq!(c.items.len(), 4);
    }

    #[test]
    fn current_statement_only() {
        let c = at("SELECT * FROM dbo.Customers c;\nSELECT o.| FROM sales.Orders o");
        assert_eq!(labels(&c), vec!["CustomerId", "Order Date", "OrderId", "Total"]);
        let c = at("SELECT * FROM dbo.Customers c;\nSELECT c.| FROM sales.Orders o");
        assert!(c.items.is_empty());
    }
}
