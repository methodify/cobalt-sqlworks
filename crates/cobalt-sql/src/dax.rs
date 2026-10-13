//! DAX language services for the semantic-model query tab: completion over a
//! [`ModelCatalog`], the DAX snippets, and the top-level statement splitter.
//!
//! Highlighting needs nothing from here: [`crate::lexer::tokenize_dialect`] with
//! [`Dialect::Dax`] yields the ordinary [`crate::lexer::Token`] stream (`'Table'` is a
//! [`TokenKind::QuotedIdentifier`], `[Column]` a [`TokenKind::BracketedIdentifier`], `"text"` a
//! [`TokenKind::String`], `//` `--` `/* */` comments, `INFO.VIEW.TABLES` one
//! [`TokenKind::Function`]).
//!
//! The completer is byte-level and context-driven. It looks at the lexed token under the
//! cursor and answers one of five situations:
//!
//! | Cursor is …                              | Offers                                                        |
//! |------------------------------------------|---------------------------------------------------------------|
//! | inside a `"string"` or a comment         | nothing                                                       |
//! | inside `'…` (closed or not)              | tables, inserted as `'Table'` (the closing quote is replaced) |
//! | inside `'Table'[…` or `Table[…`          | that table's columns, inserted as `[Column]`                  |
//! | inside a bare `[…`                       | measures as `[Measure]`, then every column as `'Table'[Column]`|
//! | anywhere else (a word or nothing)        | keywords, functions, tables, measures, snippets               |
//!
//! Every item in a result shares one replace range ([`Completions::replace_start`] ..
//! [`Completions::replace_end`]); in the bracket cases it starts at the `[` so both `[Measure]`
//! and `'Table'[Column]` inserts are correct. A named table that the model does not have gives no
//! columns (the popup closes) rather than a wrong guess.

use std::ops::Range;

use cobalt_core::tabular::{dax_measure_ref, dax_table_ref, ModelCatalog};

use crate::completion::{CompletionItem, CompletionKind, Completions};
use crate::dax_functions::{dax_function_category, DAX_FUNCTIONS};
use crate::lexer::{tokenize_dialect, tokenize_line_dialect, Dialect, LineState, TokenKind};
use crate::snippets::Snippet;

/// A DAX function as the model reports it (`INFO.FUNCTIONS()`), or as the app reads it from
/// any other source. Overrides the static list's entry of the same name in completions.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DaxFunctionDoc {
    /// The function name as typed (`SUMX`, `INFO.VIEW.TABLES`).
    pub name: String,
    /// One-line description; shown as the detail when there is no parameter text.
    pub description: Option<String>,
    /// The parameter list as text (`(Table, Expression)`); shown as the detail when present.
    pub parameters: Option<String>,
}

/// Input to [`complete_dax`].
#[derive(Clone, Copy, Debug)]
pub struct DaxCompletionRequest<'a> {
    /// The full editor text.
    pub text: &'a str,
    /// Cursor position as a byte offset into `text` (clamped to a char boundary).
    pub cursor: usize,
    /// The model's catalog, when loaded. Without it only keywords, functions and snippets come back.
    pub model: Option<&'a ModelCatalog>,
    /// The model's own function list; merged over the static [`DAX_FUNCTIONS`].
    pub functions: &'a [DaxFunctionDoc],
    /// Maximum items to return (0 = unlimited).
    pub max_items: usize,
}

/// The DAX snippets, offered in the identifier context by their prefix (same tab-stop syntax
/// as the T-SQL snippets; expand with [`crate::snippets::expand`]).
pub static DAX_SNIPPETS: &[Snippet] = &[
    Snippet { prefix: "evaluate", label: "EVALUATE …", body: "EVALUATE\n    ${1:'Table'}$0" },
    Snippet { prefix: "topn", label: "EVALUATE TOPN(100, …)", body: "EVALUATE TOPN(100, ${1:'Table'})$0" },
    Snippet {
        prefix: "summarize",
        label: "EVALUATE SUMMARIZECOLUMNS(…)",
        body: "EVALUATE\nSUMMARIZECOLUMNS(\n    ${1:'Table'[Column]},\n    \"${2:Value}\", ${3:[Measure]}\n)$0",
    },
    Snippet {
        prefix: "define",
        label: "DEFINE MEASURE … EVALUATE ROW(…)",
        body: "DEFINE\n    MEASURE ${1:'Table'}[${2:Name}] = ${3:expression}\nEVALUATE\n    ROW(\"Value\", [${2:Name}])$0",
    },
    Snippet { prefix: "info", label: "EVALUATE INFO.VIEW.TABLES()", body: "EVALUATE INFO.VIEW.TABLES()$0" },
];

/// Keywords offered in the identifier context (the two-word forms are inserted whole).
const KEYWORD_ITEMS: &[&str] = &[
    "EVALUATE", "DEFINE", "MEASURE", "VAR", "RETURN", "ORDER BY", "START AT", "ASC", "DESC", "TABLE", "COLUMN", "TRUE",
    "FALSE", "NOT", "AND", "OR", "IN",
];

/// What the cursor sits in.
#[derive(Debug, PartialEq, Eq)]
enum Context {
    /// A string or a comment: nothing to offer.
    Nothing,
    /// Inside `'…`: `prefix` is what follows the quote (unescaped).
    Table { prefix: String, start: usize, end: usize },
    /// Inside `[…`: `table` is the name written before the bracket, `None` for a bare bracket.
    Bracket { table: Option<String>, prefix: String, start: usize, end: usize },
    /// A plain word (possibly empty).
    Ident { prefix: String, start: usize, end: usize },
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b >= 0x80
}

/// The replace end for a token the cursor is inside: the token's end when it is closed, the
/// cursor when it is not, and never past the end of the cursor's line (an unclosed `'` or `[`
/// swallows the following lines in the lexer).
fn clamp_end(text: &str, cursor: usize, token_end: usize, unterminated: bool) -> usize {
    let end = if unterminated { cursor } else { token_end };
    match text[cursor..end].find(['\n', '\r']) {
        Some(nl) => cursor + nl,
        None => end,
    }
}

fn context(text: &str, cursor: usize) -> Context {
    let (tokens, state) = tokenize_line_dialect(text, LineState::INITIAL, Dialect::Dax);
    let last = tokens.len().checked_sub(1);
    for (i, t) in tokens.iter().enumerate() {
        if t.start >= cursor {
            break;
        }
        if cursor > t.end {
            continue;
        }
        // t.start < cursor <= t.end
        let unterminated = Some(i) == last && (state.in_string.is_some() || state.in_block_comment);
        let inside = cursor < t.end || unterminated;
        match t.kind {
            TokenKind::LineComment => return Context::Nothing,
            TokenKind::BlockComment | TokenKind::String if inside => return Context::Nothing,
            TokenKind::QuotedIdentifier if inside => {
                let prefix = text[t.start + 1..cursor].replace("''", "'");
                return Context::Table { prefix, start: t.start, end: clamp_end(text, cursor, t.end, unterminated) };
            }
            TokenKind::BracketedIdentifier if inside => {
                let prefix = text[t.start + 1..cursor].replace("]]", "]");
                let table = tokens[..i].iter().rev().find(|p| !p.kind.is_trivia()).and_then(|p| match p.kind {
                    TokenKind::QuotedIdentifier => {
                        let s = p.text(text);
                        let inner = s.strip_prefix('\'').unwrap_or(s);
                        Some(inner.strip_suffix('\'').unwrap_or(inner).replace("''", "'"))
                    }
                    TokenKind::Identifier => Some(p.text(text).to_string()),
                    _ => None,
                });
                return Context::Bracket { table, prefix, start: t.start, end: clamp_end(text, cursor, t.end, unterminated) };
            }
            _ => {}
        }
    }
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
    Context::Ident { prefix: text[start..cursor].to_string(), start, end }
}

/// A candidate before filtering: the item plus the text the prefix is matched against.
struct Candidate {
    item: CompletionItem,
    key: String,
}

fn cand(label: impl Into<String>, insert: impl Into<String>, kind: CompletionKind, detail: Option<String>, score: i32) -> Candidate {
    let label = label.into();
    Candidate { key: label.clone(), item: CompletionItem { label, insert: insert.into(), kind, detail, score } }
}

/// `Some(bonus)` when `key` matches `prefix_lower`: a large bonus for a prefix-start match, none
/// for a contains match; `None` when it does not match.
fn match_bonus(key: &str, prefix_lower: &str) -> Option<i32> {
    if prefix_lower.is_empty() {
        return Some(0);
    }
    let lower = key.to_lowercase();
    if lower.starts_with(prefix_lower) {
        Some(if lower == prefix_lower { 1010 } else { 1000 })
    } else if lower.contains(prefix_lower) {
        Some(0)
    } else {
        None
    }
}

fn finish(mut cands: Vec<Candidate>, prefix: &str, start: usize, end: usize, max_items: usize) -> Completions {
    let prefix_lower = prefix.to_lowercase();
    cands.retain_mut(|c| match match_bonus(&c.key, &prefix_lower) {
        Some(b) => {
            c.item.score += b;
            true
        }
        None => false,
    });
    let mut items: Vec<CompletionItem> = cands.into_iter().map(|c| c.item).collect();
    items.sort_by(|a, b| {
        b.score.cmp(&a.score).then_with(|| a.label.to_lowercase().cmp(&b.label.to_lowercase())).then_with(|| a.label.cmp(&b.label))
    });
    if max_items > 0 && items.len() > max_items {
        items.truncate(max_items);
    }
    Completions { items, replace_start: start, replace_end: end }
}

fn table_detail(t: &cobalt_core::tabular::Table) -> String {
    let mode = t.storage_mode.as_deref().filter(|m| !m.is_empty()).unwrap_or("table");
    if t.hidden {
        format!("{mode} · hidden")
    } else {
        mode.to_string()
    }
}

fn measure_detail(table: &cobalt_core::tabular::Table, m: &cobalt_core::tabular::Measure) -> String {
    match m.format_string.as_deref().filter(|f| !f.is_empty()) {
        Some(f) => format!("{f} · {}", table.name),
        None => format!("measure · {}", table.name),
    }
}

fn column_detail(c: &cobalt_core::tabular::Column) -> String {
    if c.hidden {
        format!("{} · hidden", c.data_type.label())
    } else {
        c.data_type.label().to_string()
    }
}

/// Compute DAX completions at the cursor. Never panics: the cursor is clamped to the text and
/// to a char boundary, and broken DAX simply yields fewer items.
pub fn complete_dax(req: &DaxCompletionRequest) -> Completions {
    let text = req.text;
    let mut cursor = req.cursor.min(text.len());
    while !text.is_char_boundary(cursor) {
        cursor -= 1;
    }
    match context(text, cursor) {
        Context::Nothing => Completions { items: Vec::new(), replace_start: cursor, replace_end: cursor },
        Context::Table { prefix, start, end } => {
            let mut cands = Vec::new();
            if let Some(model) = req.model {
                for t in &model.tables {
                    let score = if t.hidden { 85 } else { 90 };
                    cands.push(cand(&t.name, dax_table_ref(&t.name), CompletionKind::Table, Some(table_detail(t)), score));
                }
            }
            finish(cands, &prefix, start, end, req.max_items)
        }
        Context::Bracket { table: Some(name), prefix, start, end } => {
            let mut cands = Vec::new();
            if let Some(t) = req.model.and_then(|m| m.table(&name)) {
                for c in &t.columns {
                    let score = if c.hidden { 95 } else { 100 };
                    cands.push(cand(&c.name, dax_measure_ref(&c.name), CompletionKind::Column, Some(column_detail(c)), score));
                }
            }
            finish(cands, &prefix, start, end, req.max_items)
        }
        Context::Bracket { table: None, prefix, start, end } => {
            let mut cands = Vec::new();
            if let Some(model) = req.model {
                for (t, m) in model.all_measures() {
                    let score = if m.hidden { 98 } else { 100 };
                    cands.push(cand(&m.name, dax_measure_ref(&m.name), CompletionKind::Measure, Some(measure_detail(t, m)), score));
                }
                for t in &model.tables {
                    for c in &t.columns {
                        let score = if c.hidden || t.hidden { 85 } else { 90 };
                        let mut k = cand(
                            format!("{}[{}]", t.name, c.name),
                            format!("{}{}", dax_table_ref(&t.name), dax_measure_ref(&c.name)),
                            CompletionKind::Column,
                            Some(column_detail(c)),
                            score,
                        );
                        k.key = c.name.clone();
                        cands.push(k);
                    }
                }
            }
            finish(cands, &prefix, start, end, req.max_items)
        }
        Context::Ident { prefix, start, end } => {
            let mut cands = Vec::new();
            for k in KEYWORD_ITEMS {
                cands.push(cand(*k, *k, CompletionKind::Keyword, None, 65));
            }
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            for f in req.functions {
                if f.name.is_empty() || !seen.insert(f.name.to_ascii_uppercase()) {
                    continue;
                }
                let detail = f
                    .parameters
                    .as_deref()
                    .filter(|p| !p.is_empty())
                    .map(str::to_string)
                    .or_else(|| f.description.as_deref().and_then(|d| d.lines().next()).filter(|d| !d.is_empty()).map(str::to_string))
                    .or_else(|| dax_function_category(&f.name).map(str::to_string));
                cands.push(cand(&f.name, format!("{}(", f.name), CompletionKind::Function, detail, 60));
            }
            for name in DAX_FUNCTIONS.iter() {
                if seen.insert((*name).to_string()) {
                    let detail = dax_function_category(name).map(str::to_string);
                    cands.push(cand(*name, format!("{name}("), CompletionKind::Function, detail, 60));
                }
            }
            if let Some(model) = req.model {
                for t in &model.tables {
                    let score = if t.hidden { 85 } else { 90 };
                    cands.push(cand(&t.name, dax_table_ref(&t.name), CompletionKind::Table, Some(table_detail(t)), score));
                }
                for (t, m) in model.all_measures() {
                    let score = if m.hidden { 86 } else { 88 };
                    cands.push(cand(&m.name, dax_measure_ref(&m.name), CompletionKind::Measure, Some(measure_detail(t, m)), score));
                }
            }
            for s in DAX_SNIPPETS {
                cands.push(cand(s.prefix, s.body, CompletionKind::Snippet, Some(s.label.to_string()), 45));
            }
            finish(cands, &prefix, start, end, req.max_items)
        }
    }
}

/// Split a DAX query into its top-level parts: the `DEFINE` block (if any) and each
/// `EVALUATE … [ORDER BY …] [START AT …]`. Lexical: a part starts at a `DEFINE` or `EVALUATE`
/// keyword outside strings, quoted names, brackets, parentheses and braces, and ends at the
/// last non-trivia token before the next one. Text with neither keyword is one part (trimmed of
/// whitespace and comments); blank text gives none.
pub fn dax_statements(text: &str) -> Vec<Range<usize>> {
    let tokens = tokenize_dialect(text, Dialect::Dax);
    let mut depth = 0usize;
    let mut starts = Vec::new();
    for (i, t) in tokens.iter().enumerate() {
        match t.kind {
            TokenKind::Punct => match t.text(text) {
                "(" | "{" => depth += 1,
                ")" | "}" => depth = depth.saturating_sub(1),
                _ => {}
            },
            TokenKind::Keyword if depth == 0 && (t.text(text).eq_ignore_ascii_case("DEFINE") || t.text(text).eq_ignore_ascii_case("EVALUATE")) => {
                starts.push(i);
            }
            _ => {}
        }
    }
    if starts.is_empty() {
        let first = tokens.iter().find(|t| !t.kind.is_trivia());
        let last = tokens.iter().rev().find(|t| !t.kind.is_trivia());
        return match (first, last) {
            (Some(f), Some(l)) => std::iter::once(f.start..l.end).collect(),
            _ => Vec::new(),
        };
    }
    let mut out = Vec::with_capacity(starts.len());
    for (k, &si) in starts.iter().enumerate() {
        let limit = starts.get(k + 1).copied().unwrap_or(tokens.len());
        let end = tokens[si..limit].iter().rev().find(|t| !t.kind.is_trivia()).map(|t| t.end).unwrap_or(tokens[si].end);
        out.push(tokens[si].start..end);
    }
    out
}

/// The parts to run for "Run current statement": the `DEFINE` block when there is one, then the
/// `EVALUATE` under the cursor (on the gap between two, the previous one; before the first or
/// inside `DEFINE`, the first). Join the slices with a newline to get the query text. Empty when
/// the text has no parts.
pub fn dax_statement_at(text: &str, cursor: usize) -> Vec<Range<usize>> {
    let parts = dax_statements(text);
    let is_define = |r: &Range<usize>| text[r.clone()].get(..6).is_some_and(|w| w.eq_ignore_ascii_case("DEFINE"));
    let define = parts.iter().find(|r| is_define(r)).cloned();
    let evals: Vec<&Range<usize>> = parts.iter().filter(|r| !is_define(r)).collect();
    let chosen = evals
        .iter()
        .find(|r| r.start <= cursor && cursor <= r.end)
        .or_else(|| evals.iter().rev().find(|r| r.end <= cursor))
        .or(evals.first())
        .map(|r| (*r).clone());
    define.into_iter().chain(chosen).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Token;
    use cobalt_core::tabular::{Column, Measure, Table, TabularDataType};

    fn kinds(src: &str) -> Vec<(TokenKind, &str)> {
        tokenize_dialect(src, Dialect::Dax)
            .into_iter()
            .filter(|t| !matches!(t.kind, TokenKind::Whitespace | TokenKind::Newline))
            .map(|t| (t.kind, t.text(src)))
            .collect()
    }

    const QUERY: &str = "// line comment\n-- sql style\n/* block\n   comment */\nDEFINE\n    MEASURE 'Sales'[Total] = SUMX('Sales', 'Sales'[Qty] * 1.5e2 + .5)\nEVALUATE\n    SUMMARIZECOLUMNS('O''Brien Co'[Name]]x], \"Label \"\"q\"\"\", [Total])\nEVALUATE INFO.VIEW.TABLES()\nORDER BY [Total] DESC";

    #[test]
    fn lexes_a_representative_query() {
        let toks = tokenize_dialect(QUERY, Dialect::Dax);
        let mut pos = 0;
        for t in &toks {
            assert_eq!(t.start, pos, "gap before {t:?}");
            pos = t.end;
        }
        assert_eq!(pos, QUERY.len());
        let k = kinds(QUERY);
        assert_eq!(k[0], (TokenKind::LineComment, "// line comment"));
        assert_eq!(k[1], (TokenKind::LineComment, "-- sql style"));
        assert_eq!(k[2], (TokenKind::BlockComment, "/* block\n   comment */"));
        assert_eq!(k[3], (TokenKind::Keyword, "DEFINE"));
        assert_eq!(k[4], (TokenKind::Keyword, "MEASURE"));
        assert_eq!(k[5], (TokenKind::QuotedIdentifier, "'Sales'"));
        assert_eq!(k[6], (TokenKind::BracketedIdentifier, "[Total]"));
        assert_eq!(k[7], (TokenKind::Operator, "="));
        assert_eq!(k[8], (TokenKind::Function, "SUMX"));
        assert!(k.contains(&(TokenKind::Number, "1.5e2")));
        assert!(k.contains(&(TokenKind::Number, ".5")));
        assert!(k.contains(&(TokenKind::Operator, "*")));
        assert!(k.contains(&(TokenKind::Keyword, "EVALUATE")));
        assert!(k.contains(&(TokenKind::Function, "SUMMARIZECOLUMNS")));
        assert!(k.contains(&(TokenKind::QuotedIdentifier, "'O''Brien Co'")));
        assert!(k.contains(&(TokenKind::BracketedIdentifier, "[Name]]x]")));
        assert!(k.contains(&(TokenKind::String, "\"Label \"\"q\"\"\"")));
        assert!(k.contains(&(TokenKind::Function, "INFO.VIEW.TABLES")));
        assert!(k.contains(&(TokenKind::Keyword, "ORDER")));
        assert!(k.contains(&(TokenKind::Keyword, "BY")));
        assert!(k.contains(&(TokenKind::Keyword, "DESC")));
        assert!(!k.iter().any(|t| t.0 == TokenKind::Type || t.0 == TokenKind::Unknown), "{k:?}");
    }

    #[test]
    fn dax_operators_braces_and_word_functions() {
        let k = kinds("x IN {1, 2} && NOT(a) || b == c <> d ^ e & \"s\" : f -- end");
        let ops: Vec<&str> = k.iter().filter(|t| t.0 == TokenKind::Operator).map(|t| t.1).collect();
        assert_eq!(ops, vec!["&&", "||", "==", "<>", "^", "&"]);
        assert!(k.contains(&(TokenKind::Keyword, "IN")));
        assert!(k.contains(&(TokenKind::Punct, "{")));
        assert!(k.contains(&(TokenKind::Punct, "}")));
        assert!(k.contains(&(TokenKind::Punct, ":")));
        assert!(k.contains(&(TokenKind::Function, "NOT")));
        assert_eq!(k.last().copied(), Some((TokenKind::LineComment, "-- end")));
        // EVALUATE ( … ) stays a keyword; an unknown name( is a call; a bare function name is not.
        let k = kinds("EVALUATE (MyFunc(Sales)) RETURN (1)");
        assert_eq!(k[0], (TokenKind::Keyword, "EVALUATE"));
        assert_eq!(k[2], (TokenKind::Function, "MyFunc"));
        assert_eq!(k[4], (TokenKind::Identifier, "Sales"));
        assert!(k.contains(&(TokenKind::Keyword, "RETURN")));
        // N'x' is not a string prefix in DAX; `"` is a string, `'` a name.
        let k = kinds("N'T'[c] \"q\"");
        assert_eq!(k[0], (TokenKind::Identifier, "N"));
        assert_eq!(k[1], (TokenKind::QuotedIdentifier, "'T'"));
        assert_eq!(k[3], (TokenKind::String, "\"q\""));
        assert!(crate::lexer::is_dax_function("info.view.columns") && !crate::lexer::is_dax_function("select"));
        assert!(crate::lexer::is_keyword_in("evaluate", Dialect::Dax) && !crate::lexer::is_keyword_in("evaluate", Dialect::TSql));
    }

    #[test]
    fn unterminated_dax_constructs_cover_input() {
        for src in ["'abc", "[abc", "\"abc", "/* abc", "// abc", "INFO.", "INFO.VIEW.", "x.", "1e", "é'", "'Vente€'[Mont"] {
            let toks = tokenize_dialect(src, Dialect::Dax);
            assert_eq!(toks.last().map(|t| t.end), Some(src.len()), "{src}");
        }
        let (t, s) = tokenize_line_dialect("EVALUATE 'Sa", LineState::INITIAL, Dialect::Dax);
        assert_eq!(s.in_string, Some('\''));
        assert_eq!(t.last().unwrap().kind, TokenKind::QuotedIdentifier);
        let (t2, s2) = tokenize_line_dialect("les'[x]", s, Dialect::Dax);
        assert_eq!(s2, LineState::INITIAL);
        assert_eq!(t2[0], Token { kind: TokenKind::QuotedIdentifier, start: 0, end: 4 });
    }

    // ------------------------------------------------------------------ completion

    fn col(name: &str, ty: TabularDataType, hidden: bool) -> Column {
        Column { name: name.into(), data_type: ty, hidden, ..Default::default() }
    }

    fn model() -> ModelCatalog {
        ModelCatalog {
            name: "Sales model".into(),
            tables: vec![
                Table {
                    name: "Sales".into(),
                    storage_mode: Some("Import".into()),
                    columns: vec![
                        col("Amount", TabularDataType::Double, false),
                        col("Qty", TabularDataType::Int64, false),
                        col("SalesKey", TabularDataType::Int64, true),
                    ],
                    measures: vec![
                        Measure { name: "Total Sales".into(), format_string: Some("#,0".into()), ..Default::default() },
                        Measure { name: "Order Count".into(), ..Default::default() },
                    ],
                    ..Default::default()
                },
                Table {
                    name: "Date".into(),
                    storage_mode: Some("DirectLake".into()),
                    columns: vec![col("Year", TabularDataType::Int64, false), col("Month Name", TabularDataType::String, false)],
                    ..Default::default()
                },
            ],
            ..Default::default()
        }
    }

    /// `|` marks the cursor.
    fn at(marked: &str) -> Completions {
        at_with(marked, &[])
    }

    fn at_with(marked: &str, functions: &[DaxFunctionDoc]) -> Completions {
        let cursor = marked.find('|').expect("cursor marker");
        let text = marked.replacen('|', "", 1);
        let m = model();
        complete_dax(&DaxCompletionRequest { text: &text, cursor, model: Some(&m), functions, max_items: 0 })
    }

    fn labels(c: &Completions) -> Vec<&str> {
        c.items.iter().map(|i| i.label.as_str()).collect()
    }

    fn of_kind(c: &Completions, k: CompletionKind) -> Vec<&str> {
        c.items.iter().filter(|i| i.kind == k).map(|i| i.label.as_str()).collect()
    }

    #[test]
    fn nothing_inside_strings_and_comments() {
        assert!(at("EVALUATE ROW(\"ab|c\", 1)").items.is_empty());
        assert!(at("EVALUATE ROW(\"ab|").items.is_empty());
        assert!(at("// hel|lo\nEVALUATE 'Sales'").items.is_empty());
        assert!(at("-- hello|\nEVALUATE 'Sales'").items.is_empty());
        assert!(at("/* x| */ EVALUATE 'Sales'").items.is_empty());
        assert!(at("/* x|").items.is_empty());
        // just after a closed string or comment is the identifier context again
        assert!(!at("/* x */|").items.is_empty());
    }

    #[test]
    fn unclosed_quote_offers_tables() {
        let c = at("EVALUATE 'Sa|");
        assert_eq!(labels(&c), vec!["Sales"]);
        assert_eq!(c.items[0].insert, "'Sales'");
        assert_eq!(c.items[0].kind, CompletionKind::Table);
        assert_eq!(c.items[0].detail.as_deref(), Some("Import"));
        assert_eq!((c.replace_start, c.replace_end), (9, 12));
        // an already-closed quote is replaced along with the name, so it is not doubled
        let c = at("EVALUATE 'Sa|'");
        assert_eq!((c.replace_start, c.replace_end), (9, 13));
        assert_eq!(c.items[0].insert, "'Sales'");
        // empty prefix: every table, visible first, DirectLake detail
        let c = at("EVALUATE '|");
        assert_eq!(labels(&c), vec!["Date", "Sales"]);
        assert_eq!(c.items[0].detail.as_deref(), Some("DirectLake"));
        // an unclosed quote never swallows the next line into the replace range
        let c = at("EVALUATE 'Sa|\nORDER BY [x]");
        assert_eq!((c.replace_start, c.replace_end), (9, 12));
    }

    #[test]
    fn table_bracket_offers_that_tables_columns() {
        let c = at("EVALUATE FILTER('Sales', 'Sales'[|)");
        assert_eq!(labels(&c), vec!["Amount", "Qty", "SalesKey"]);
        assert!(c.items.iter().all(|i| i.kind == CompletionKind::Column));
        assert_eq!(c.items[0].insert, "[Amount]");
        assert_eq!(c.items[0].detail.as_deref(), Some("decimal number"));
        assert_eq!(c.items[2].detail.as_deref(), Some("whole number · hidden"));
        assert!(c.items[2].score < c.items[1].score);
        assert_eq!((c.replace_start, c.replace_end), (32, 33));
        // bare table name, prefix, closed bracket included in the range
        let c = at("EVALUATE SUMX(Sales, Sales[Q|ty])");
        assert_eq!(labels(&c), vec!["Qty"]);
        assert_eq!((c.replace_start, c.replace_end), (26, 31));
        // the table in a DEFINE MEASURE
        let c = at("DEFINE MEASURE 'Date'[M|");
        assert_eq!(labels(&c), vec!["Month Name"]);
        assert_eq!(c.items[0].insert, "[Month Name]");
        // a name the model does not have gives nothing
        assert!(at("EVALUATE 'Nope'[|").items.is_empty());
    }

    #[test]
    fn bare_bracket_offers_measures_then_qualified_columns() {
        let c = at("EVALUATE ROW(\"v\", [|)");
        assert_eq!(of_kind(&c, CompletionKind::Measure), vec!["Order Count", "Total Sales"]);
        assert_eq!(c.items[0].insert, "[Order Count]");
        assert_eq!(c.items[0].detail.as_deref(), Some("measure · Sales"));
        assert_eq!(c.items[1].detail.as_deref(), Some("#,0 · Sales"));
        let cols = of_kind(&c, CompletionKind::Column);
        assert_eq!(cols, vec!["Date[Month Name]", "Date[Year]", "Sales[Amount]", "Sales[Qty]", "Sales[SalesKey]"]);
        let qty = c.items.iter().find(|i| i.label == "Sales[Qty]").unwrap();
        assert_eq!(qty.insert, "'Sales'[Qty]");
        assert_eq!(c.replace_start, 18);
        assert_eq!(c.replace_end, 19);
        // prefix matches the column name, not the table
        let c = at("VAR x = [Qt|");
        assert_eq!(labels(&c), vec!["Sales[Qty]"]);
        let c = at("VAR x = [tot|");
        assert_eq!(labels(&c), vec!["Total Sales"]);
        assert_eq!(c.replace_start, 8);
    }

    #[test]
    fn identifier_context_offers_keywords_functions_tables_measures_snippets() {
        let c = at("EVAL|");
        assert_eq!(c.items[0].label, "EVALUATE");
        assert_eq!(c.items[0].kind, CompletionKind::Keyword);
        assert!(labels(&c).contains(&"evaluate"));
        assert_eq!(c.items.iter().find(|i| i.label == "evaluate").unwrap().kind, CompletionKind::Snippet);
        assert_eq!((c.replace_start, c.replace_end), (0, 4));

        let c = at("EVALUATE SUMM|");
        assert_eq!(labels(&c), vec!["SUMMARIZE", "SUMMARIZECOLUMNS", "summarize"]);
        assert_eq!(c.items[0].insert, "SUMMARIZE(");
        assert_eq!(c.items[0].detail.as_deref(), Some("Table manipulation"));

        let c = at("EVALUATE info.view.t|");
        assert_eq!(labels(&c), vec!["INFO.VIEW.TABLES"]);
        assert_eq!(c.items[0].insert, "INFO.VIEW.TABLES(");
        assert_eq!(c.replace_start, 9);

        let c = at("EVALUATE Sal|");
        assert_eq!(c.items[0].label, "Sales");
        assert_eq!(c.items[0].insert, "'Sales'");
        assert_eq!(c.items[0].kind, CompletionKind::Table);

        let c = at("EVALUATE ROW(\"v\", Total|");
        assert_eq!(c.items[0].label, "Total Sales");
        assert_eq!(c.items[0].insert, "[Total Sales]");
        assert_eq!(c.items[0].kind, CompletionKind::Measure);
        assert!(labels(&c).contains(&"TOTALYTD"));

        // model objects rank above keywords; both are there
        let c = at("ORD|");
        assert_eq!(labels(&c)[..2], ["Order Count", "ORDER BY"]);
        let c = at("STA|");
        assert!(labels(&c).contains(&"START AT"));
    }

    #[test]
    fn prefix_start_ranks_above_contains_and_doc_detail_wins() {
        let c = at("EVALUATE date|");
        let l = labels(&c);
        let starts_end = l.iter().position(|x| !x.to_lowercase().starts_with("date")).unwrap();
        assert!(l[..starts_end].contains(&"DATEADD"));
        assert!(l[starts_end..].contains(&"EDATE"));
        assert!(l[starts_end..].contains(&"FIRSTDATE"));
        assert!(l[..starts_end].iter().all(|x| x.to_lowercase().starts_with("date")));
        assert!(l[starts_end..].iter().all(|x| x.to_lowercase().contains("date") && !x.to_lowercase().starts_with("date")));
        // exact match first
        let c = at("EVALUATE sum|");
        assert_eq!(c.items[0].label, "SUM");

        let docs = vec![
            DaxFunctionDoc { name: "SUMX".into(), description: Some("Sum of an expression".into()), parameters: Some("(Table, Expression)".into()) },
            DaxFunctionDoc { name: "MYUDF".into(), description: Some("Model function\nmore".into()), parameters: None },
        ];
        let c = at_with("EVALUATE sumx|", &docs);
        assert_eq!(labels(&c), vec!["SUMX"]);
        assert_eq!(c.items[0].detail.as_deref(), Some("(Table, Expression)"));
        let c = at_with("EVALUATE myu|", &docs);
        assert_eq!(c.items[0].detail.as_deref(), Some("Model function"));
    }

    #[test]
    fn max_items_stable_order_and_no_model() {
        let c = at("EVALUATE |");
        assert!(c.items.len() > 400);
        let m = model();
        let text = "EVALUATE ";
        let r = DaxCompletionRequest { text, cursor: text.len(), model: Some(&m), functions: &[], max_items: 7 };
        let a = complete_dax(&r);
        let b = complete_dax(&r);
        assert_eq!(a.items.len(), 7);
        assert_eq!(a, b);
        assert_eq!(a.items[0].label, "Date");
        assert_eq!(a.items[1].label, "Sales");
        assert_eq!(a.items[2].label, "Order Count");
        let r = DaxCompletionRequest { text: "EVALUATE 'S", cursor: 11, model: None, functions: &[], max_items: 0 };
        assert!(complete_dax(&r).items.is_empty());
        let r = DaxCompletionRequest { text: "EVALUATE SUM", cursor: 12, model: None, functions: &[], max_items: 0 };
        assert_eq!(complete_dax(&r).items[0].label, "SUM");
    }

    #[test]
    fn never_panics_at_any_offset_including_multibyte() {
        let m = model();
        for text in ["", "EVALUATE 'Vente€'[Montant]", "'Vente€", "[€", "€", "EVALUATE ROW(\"€\", [x]) // €\n/* € */ 'Dé'[Ñ]]", QUERY] {
            for cursor in 0..=text.len() + 2 {
                let _ = complete_dax(&DaxCompletionRequest { text, cursor, model: Some(&m), functions: &[], max_items: 10 });
                let _ = complete_dax(&DaxCompletionRequest { text, cursor, model: None, functions: &[], max_items: 0 });
                let _ = dax_statement_at(text, cursor);
            }
        }
    }

    #[test]
    fn snippets_expand() {
        let mut seen = std::collections::HashSet::new();
        for s in DAX_SNIPPETS {
            assert!(seen.insert(s.prefix));
            let (text, stops) = crate::snippets::expand(s.body);
            assert!(!text.contains("${"), "{text}");
            assert!(!stops.is_empty());
        }
        let (text, _) = crate::snippets::expand(DAX_SNIPPETS.iter().find(|s| s.prefix == "define").unwrap().body);
        assert_eq!(text, "DEFINE\n    MEASURE 'Table'[Name] = expression\nEVALUATE\n    ROW(\"Value\", [Name])");
    }

    // ------------------------------------------------------------------ statements

    #[test]
    fn splits_define_and_evaluates() {
        let parts = dax_statements(QUERY);
        let texts: Vec<&str> = parts.iter().map(|r| &QUERY[r.clone()]).collect();
        assert_eq!(texts.len(), 3);
        assert!(texts[0].starts_with("DEFINE") && texts[0].ends_with("+ .5)"), "{}", texts[0]);
        assert!(texts[1].starts_with("EVALUATE\n    SUMMARIZECOLUMNS") && texts[1].ends_with("[Total])"));
        assert_eq!(texts[2], "EVALUATE INFO.VIEW.TABLES()\nORDER BY [Total] DESC");

        // keywords inside strings, names, parentheses and braces do not split
        let t = "EVALUATE ROW(\"EVALUATE\", 1) // EVALUATE\nEVALUATE FILTER('Evaluate'[DEFINE], 'T'[x] IN {1, 2}) START AT 1";
        let p = dax_statements(t);
        assert_eq!(p.len(), 2);
        assert_eq!(&t[p[1].clone()], "EVALUATE FILTER('Evaluate'[DEFINE], 'T'[x] IN {1, 2}) START AT 1");
        assert_eq!(&t[p[0].clone()], "EVALUATE ROW(\"EVALUATE\", 1)");

        // no keyword at all: one trimmed part; blank: none
        assert_eq!(dax_statements("  -- c\n  SUM(1) \n"), vec![9..15]);
        assert!(dax_statements("  /* only */ ").is_empty());
        assert!(dax_statements("").is_empty());
    }

    #[test]
    fn statement_at_includes_define_and_picks_the_evaluate_under_the_cursor() {
        let parts = dax_statements(QUERY);
        let join = |rs: Vec<Range<usize>>| rs.iter().map(|r| &QUERY[r.clone()]).collect::<Vec<_>>().join("\n");
        let second = parts[2].start + 3;
        let s = join(dax_statement_at(QUERY, second));
        assert!(s.starts_with("DEFINE") && s.ends_with("ORDER BY [Total] DESC") && !s.contains("SUMMARIZECOLUMNS"));
        let s = join(dax_statement_at(QUERY, parts[1].start));
        assert!(s.contains("SUMMARIZECOLUMNS") && !s.contains("INFO.VIEW"));
        // inside DEFINE, or before everything: the first EVALUATE
        let s = join(dax_statement_at(QUERY, parts[0].start + 2));
        assert!(s.contains("SUMMARIZECOLUMNS"));
        let s = join(dax_statement_at(QUERY, 0));
        assert!(s.starts_with("DEFINE") && s.contains("SUMMARIZECOLUMNS"));
        // in the gap after the last one: the last one
        let s = join(dax_statement_at(QUERY, QUERY.len()));
        assert!(s.contains("INFO.VIEW"));
        // without DEFINE, just the EVALUATE
        let t = "EVALUATE 'A'\n\nEVALUATE 'B'";
        assert_eq!(dax_statement_at(t, 14), vec![14..26]);
        assert_eq!(dax_statement_at(t, 13), vec![0..12]);
        assert!(dax_statement_at("", 0).is_empty());
    }
}
