//! Split a batch into statements.
//!
//! The scanner is token-based (see [`crate::lexer`]) and therefore immune to `;` and
//! keywords inside strings and comments. It splits on:
//!
//! * `;` at paren depth 0, and
//! * a statement-start keyword (`SELECT`, `INSERT`, `DECLARE`, …) that appears at the
//!   start of a line or right after `;`,
//!
//! while never splitting inside `( … )`, `BEGIN … END`, `BEGIN TRY … END TRY`,
//! `CASE … END`, a CTE chain (`WITH a AS (…), b AS (…) SELECT …`), the body of an
//! `IF`/`ELSE`/`WHILE`, an `INSERT … SELECT`, a `MERGE` (which must end in `;`), or the
//! body of a `CREATE PROCEDURE/FUNCTION/TRIGGER`. `SET` inside an `UPDATE` or `MERGE` is
//! not a statement start.

use crate::lexer::{tokenize, Token, TokenKind};

/// The location of one statement inside a batch. `start..end` is a half-open byte range
/// that excludes leading and trailing trivia (whitespace, comments) but includes a
/// terminating `;`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatementSpan {
    /// Byte offset of the first token.
    pub start: usize,
    /// Byte offset one past the last token (the `;` if present).
    pub end: usize,
    /// 1-based line on which the statement starts.
    pub line: u32,
}

impl StatementSpan {
    /// The statement text within `sql`.
    pub fn text<'a>(&self, sql: &'a str) -> &'a str {
        &sql[self.start..self.end]
    }
}

/// Keywords that begin a statement when found at the start of a line or after `;`.
pub static STATEMENT_START_KEYWORDS: &[&str] = &[
    "SELECT", "INSERT", "UPDATE", "DELETE", "MERGE", "WITH", "DECLARE", "SET", "EXEC", "EXECUTE",
    "CREATE", "ALTER", "DROP", "TRUNCATE", "IF", "WHILE", "BEGIN", "PRINT", "RAISERROR", "THROW",
    "USE", "GRANT", "REVOKE", "DENY", "BULK", "WAITFOR", "RETURN", "BACKUP", "RESTORE", "OPEN",
    "CLOSE", "DEALLOCATE", "FETCH", "COMMIT", "ROLLBACK", "SAVE", "GOTO", "BREAK", "CONTINUE",
    "DBCC", "KILL", "CHECKPOINT", "RECONFIGURE", "SHUTDOWN", "ENABLE", "DISABLE", "REVERT",
    "SETUSER", "READTEXT", "WRITETEXT", "UPDATETEXT",
];

fn is_start_keyword(word: &str) -> bool {
    let u = word.to_ascii_uppercase();
    STATEMENT_START_KEYWORDS.contains(&u.as_str())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Block {
    Begin,
    Case,
}

/// Compute the statement spans within a single batch.
pub fn statements(sql: &str) -> Vec<StatementSpan> {
    let tokens = tokenize(sql);
    Splitter::new(sql, &tokens).run()
}

/// 1-based line number of byte `offset` in `text` (`\n`, `\r\n` and lone `\r` all
/// count as line breaks). Offsets past the end are clamped.
pub fn line_of(text: &str, offset: usize) -> u32 {
    let bytes = text.as_bytes();
    let end = offset.min(bytes.len());
    let mut line = 1;
    let mut i = 0;
    while i < end {
        match bytes[i] {
            b'\n' => line += 1,
            b'\r' => {
                line += 1;
                if bytes.get(i + 1) == Some(&b'\n') {
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    line
}

/// The statement containing `cursor` (a byte offset). If the cursor sits on whitespace
/// between statements the previous statement is returned; before the first statement
/// the first one is returned. `None` only when the batch has no statements.
pub fn statement_at(sql: &str, cursor: usize) -> Option<StatementSpan> {
    let spans = statements(sql);
    if let Some(s) = spans.iter().find(|s| s.start <= cursor && cursor <= s.end) {
        return Some(*s);
    }
    spans.iter().rev().find(|s| s.end <= cursor).or(spans.first()).copied()
}

struct Splitter<'a> {
    sql: &'a str,
    tokens: &'a [Token],
    spans: Vec<StatementSpan>,
    /// Index of the first significant token of the current statement.
    cur_start: Option<usize>,
    /// Index of the last significant token seen.
    last_sig: Option<usize>,
    paren_depth: usize,
    blocks: Vec<Block>,
    /// Uppercased first keyword of the current statement.
    stmt_kind: String,
    /// Inside a `WITH … AS (…)` chain waiting for its DML verb.
    in_cte: bool,
    /// The next statement-start keyword is the body of an IF/ELSE/WHILE.
    expect_body: bool,
    /// `INSERT` waiting for its `SELECT`/`VALUES`/`EXEC`.
    insert_pending: bool,
    /// `MERGE` — no keyword splits until `;`.
    in_merge: bool,
    /// Inside a `CREATE PROCEDURE/FUNCTION/TRIGGER` — nothing splits until the end.
    module_body: bool,
}

impl<'a> Splitter<'a> {
    fn new(sql: &'a str, tokens: &'a [Token]) -> Self {
        Splitter {
            sql,
            tokens,
            spans: Vec::new(),
            cur_start: None,
            last_sig: None,
            paren_depth: 0,
            blocks: Vec::new(),
            stmt_kind: String::new(),
            in_cte: false,
            expect_body: false,
            insert_pending: false,
            in_merge: false,
            module_body: false,
        }
    }

    fn upper(&self, i: usize) -> String {
        self.tokens[i].text(self.sql).to_ascii_uppercase()
    }

    /// Next significant token after index `i`.
    fn next_sig(&self, i: usize) -> Option<usize> {
        (i + 1..self.tokens.len()).find(|&j| !self.tokens[j].kind.is_trivia())
    }

    /// True if only trivia separates token `i` from the previous line start or from a `;`.
    fn at_line_start(&self, i: usize) -> bool {
        let mut j = i;
        while j > 0 {
            j -= 1;
            let t = &self.tokens[j];
            match t.kind {
                TokenKind::Newline => return true,
                TokenKind::Whitespace | TokenKind::LineComment => {}
                TokenKind::BlockComment => {
                    if t.text(self.sql).contains(['\n', '\r']) {
                        return true;
                    }
                }
                TokenKind::Punct if t.text(self.sql) == ";" => return true,
                _ => return false,
            }
        }
        true
    }

    fn line_of(&self, offset: usize) -> u32 {
        line_of(self.sql, offset)
    }

    fn close(&mut self) {
        if let (Some(s), Some(e)) = (self.cur_start, self.last_sig) {
            let start = self.tokens[s].start;
            let end = self.tokens[e].end;
            if end > start {
                self.spans.push(StatementSpan { start, end, line: self.line_of(start) });
            }
        }
        self.cur_start = None;
        self.stmt_kind.clear();
        self.in_cte = false;
        self.expect_body = false;
        self.insert_pending = false;
        self.in_merge = false;
    }

    fn begin_statement(&mut self, i: usize, kind: &str) {
        self.cur_start = Some(i);
        self.stmt_kind = kind.to_string();
        self.in_cte = false;
        self.insert_pending = false;
        self.in_merge = false;
        match kind {
            "WITH" => self.in_cte = true,
            "INSERT" => self.insert_pending = true,
            "MERGE" => self.in_merge = true,
            _ => {}
        }
    }

    fn run(mut self) -> Vec<StatementSpan> {
        let n = self.tokens.len();
        let mut i = 0;
        while i < n {
            let tok = self.tokens[i];
            if tok.kind.is_trivia() {
                i += 1;
                continue;
            }
            let text = tok.text(self.sql);
            let upper = text.to_ascii_uppercase();
            let top = self.paren_depth == 0 && self.blocks.is_empty() && !self.module_body;

            match tok.kind {
                TokenKind::Punct if text == "(" => self.paren_depth += 1,
                TokenKind::Punct if text == ")" => self.paren_depth = self.paren_depth.saturating_sub(1),
                TokenKind::Punct if text == ";" && self.paren_depth == 0 => {
                    // `;` inside BEGIN…END ends the inner statement but not the outer.
                    if self.blocks.is_empty() && !self.module_body {
                        if self.cur_start.is_none() {
                            self.cur_start = Some(i);
                        }
                        self.last_sig = Some(i);
                        self.close();
                        i += 1;
                        continue;
                    } else {
                        self.in_merge = false;
                        self.insert_pending = false;
                        self.in_cte = false;
                    }
                }
                TokenKind::Keyword => {
                    let starts = is_start_keyword(&upper);
                    if starts && top && self.cur_start.is_some() && self.at_line_start(i) {
                        let suppressed = self.in_cte
                            || self.in_merge
                            || self.expect_body
                            || (upper == "SET" && matches!(self.stmt_kind.as_str(), "UPDATE" | "MERGE"))
                            || (self.insert_pending && matches!(upper.as_str(), "SELECT" | "EXEC" | "EXECUTE" | "WITH"))
                            || self.prev_is_as(i)
                            || (upper == "WITH" && !self.with_starts_cte(i));
                        if !suppressed {
                            self.close();
                        }
                    }
                    if self.cur_start.is_none() {
                        self.begin_statement(i, &upper);
                    } else if self.in_cte
                        && self.paren_depth == 0
                        && matches!(upper.as_str(), "SELECT" | "INSERT" | "UPDATE" | "DELETE" | "MERGE")
                    {
                        self.in_cte = false;
                        self.stmt_kind = upper.clone();
                        if upper == "MERGE" {
                            self.in_merge = true;
                        }
                    } else if self.insert_pending
                        && self.paren_depth == 0
                        && matches!(upper.as_str(), "SELECT" | "VALUES" | "EXEC" | "EXECUTE" | "DEFAULT" | "WITH")
                    {
                        if upper == "WITH" {
                            if self.with_starts_cte(i) {
                                self.insert_pending = false;
                                self.in_cte = true;
                            }
                        } else {
                            self.insert_pending = false;
                        }
                    }

                    // Block / flow tracking.
                    match upper.as_str() {
                        "BEGIN" => {
                            let next = self.next_sig(i).map(|j| self.upper(j)).unwrap_or_default();
                            if !matches!(next.as_str(), "TRAN" | "TRANSACTION" | "DISTRIBUTED" | "DIALOG" | "CONVERSATION") {
                                self.blocks.push(Block::Begin);
                            }
                            self.expect_body = false;
                        }
                        "CASE" => self.blocks.push(Block::Case),
                        "END" => {
                            let next = self.next_sig(i).map(|j| self.upper(j)).unwrap_or_default();
                            if next != "CONVERSATION" {
                                self.blocks.pop();
                            }
                        }
                        "IF" | "WHILE" => {
                            if self.paren_depth == 0 {
                                self.expect_body = true;
                            }
                        }
                        "ELSE" => {
                            // `CASE … ELSE …` is an expression, not flow control.
                            if self.paren_depth == 0 && self.blocks.last() != Some(&Block::Case) {
                                self.expect_body = true;
                            }
                        }
                        "SET" => {
                            if self.paren_depth == 0 {
                                self.set_seen = true;
                            }
                        }
                        "CREATE" | "ALTER" if self.cur_start == Some(i) => {
                            if self.creates_module(i) {
                                self.module_body = true;
                            }
                        }
                        _ => {
                            if self.expect_body && starts && self.paren_depth == 0 {
                                self.expect_body = false;
                            }
                        }
                    }
                }
                _ => {
                    if self.cur_start.is_none() {
                        self.begin_statement(i, &upper);
                    }
                }
            }
            self.last_sig = Some(i);
            i += 1;
        }
        self.close();
        self.spans
    }

    /// The previous significant token is `AS` (e.g. `CREATE VIEW v AS SELECT`).
    fn prev_is_as(&self, i: usize) -> bool {
        self.last_sig.filter(|&j| j < i).is_some_and(|j| self.upper(j) == "AS")
    }

    /// `WITH` begins a CTE when followed by a name (not `(` as in `WITH (NOLOCK)`).
    fn with_starts_cte(&self, i: usize) -> bool {
        self.next_sig(i).is_some_and(|j| self.tokens[j].kind.is_name() || self.tokens[j].kind == TokenKind::Keyword)
    }

    /// `CREATE [OR ALTER] PROC|PROCEDURE|FUNCTION|TRIGGER`.
    fn creates_module(&self, i: usize) -> bool {
        let mut j = i;
        for _ in 0..3 {
            match self.next_sig(j) {
                Some(k) => {
                    let u = self.upper(k);
                    if matches!(u.as_str(), "PROC" | "PROCEDURE" | "FUNCTION" | "TRIGGER") {
                        return true;
                    }
                    if !matches!(u.as_str(), "OR" | "ALTER") {
                        return false;
                    }
                    j = k;
                }
                None => return false,
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(sql: &str) -> Vec<&str> {
        statements(sql).iter().map(|s| s.text(sql)).collect()
    }

    #[test]
    fn semicolon_split() {
        assert_eq!(texts("select 1; select 2"), vec!["select 1;", "select 2"]);
    }

    #[test]
    fn line_start_keyword_split() {
        assert_eq!(texts("select 1\nselect 2\n  SELECT 3"), vec!["select 1", "select 2", "SELECT 3"]);
    }

    #[test]
    fn multi_line_select_with_cte() {
        let sql = "WITH a AS (\n  SELECT 1 AS x\n), b AS (\n  SELECT 2 AS y\n)\nSELECT *\nFROM a\nCROSS JOIN b\nSELECT 'next'";
        let t = texts(sql);
        assert_eq!(t.len(), 2);
        assert!(t[0].starts_with("WITH a") && t[0].ends_with("CROSS JOIN b"));
        assert_eq!(t[1], "SELECT 'next'");
    }

    #[test]
    fn if_begin_end_else_begin_end() {
        let sql = "IF @x = 1\nBEGIN\n    SELECT 1;\n    SELECT 2\nEND\nELSE\nBEGIN\n    SELECT 3\nEND\nSELECT 4";
        let t = texts(sql);
        assert_eq!(t.len(), 2);
        assert!(t[0].starts_with("IF @x") && t[0].ends_with("SELECT 3\nEND"));
        assert_eq!(t[1], "SELECT 4");
    }

    #[test]
    fn if_without_begin_takes_next_statement_as_body() {
        let sql = "IF EXISTS (SELECT 1 FROM t)\n    SELECT 'yes'\nELSE\n    SELECT 'no'\nSELECT 'after'";
        let t = texts(sql);
        assert_eq!(t.len(), 2);
        assert_eq!(t[1], "SELECT 'after'");
    }

    #[test]
    fn declare_then_select_same_line_is_one_statement() {
        assert_eq!(texts("DECLARE @x int = 1 SELECT @x"), vec!["DECLARE @x int = 1 SELECT @x"]);
        assert_eq!(texts("DECLARE @x int = 1\nSELECT @x"), vec!["DECLARE @x int = 1", "SELECT @x"]);
    }

    #[test]
    fn update_set_is_one_statement() {
        assert_eq!(texts("UPDATE t SET a = 1 WHERE b = 2"), vec!["UPDATE t SET a = 1 WHERE b = 2"]);
        assert_eq!(texts("UPDATE t\nSET a = 1\nWHERE b = 2\nSET NOCOUNT ON"), vec!["UPDATE t\nSET a = 1\nWHERE b = 2", "SET NOCOUNT ON"]);
    }

    #[test]
    fn comments_between_statements_are_excluded() {
        let sql = "-- first\nselect 1\n/* between */\n-- also\nselect 2 -- trailing";
        let s = statements(sql);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].text(sql), "select 1");
        assert_eq!(s[0].line, 2);
        assert_eq!(s[1].text(sql), "select 2");
        assert_eq!(s[1].line, 5);
    }

    #[test]
    fn strings_with_semicolons_and_keywords() {
        assert_eq!(texts("select 'a;b'; select ';\nselect'"), vec!["select 'a;b';", "select ';\nselect'"]);
    }

    #[test]
    fn parens_and_case_do_not_split() {
        let sql = "SELECT CASE WHEN 1=1 THEN 'a'\nELSE 'b'\nEND, (SELECT 1;\nSELECT 2) AS z\nSELECT 9";
        let t = texts(sql);
        assert_eq!(t.len(), 2);
        assert_eq!(t[1], "SELECT 9");
    }

    #[test]
    fn insert_select_and_merge() {
        let sql = "INSERT INTO t (a)\nSELECT a FROM s\nMERGE t AS tgt USING s ON 1=1\nWHEN MATCHED THEN\nUPDATE SET a = 1\nWHEN NOT MATCHED THEN\nINSERT (a)\nVALUES (1);\nSELECT 1";
        let t = texts(sql);
        assert_eq!(t.len(), 3);
        assert!(t[0].starts_with("INSERT INTO"));
        assert!(t[1].starts_with("MERGE") && t[1].ends_with("VALUES (1);"));
        assert_eq!(t[2], "SELECT 1");
    }

    #[test]
    fn create_proc_body_is_one_statement() {
        let sql = "CREATE OR ALTER PROCEDURE dbo.p AS\nSET NOCOUNT ON\nSELECT 1\nSELECT 2\nRETURN";
        assert_eq!(texts(sql).len(), 1);
        let view = "CREATE VIEW v AS\nSELECT 1 AS x\nSELECT 2";
        assert_eq!(texts(view).len(), 2);
    }

    #[test]
    fn begin_tran_and_try_catch() {
        let sql = "BEGIN TRAN\nSELECT 1\nCOMMIT\nBEGIN TRY\n  SELECT 2\nEND TRY\nBEGIN CATCH\n  THROW\nEND CATCH\nSELECT 3";
        let t = texts(sql);
        assert_eq!(t, vec!["BEGIN TRAN", "SELECT 1", "COMMIT", "BEGIN TRY\n  SELECT 2\nEND TRY", "BEGIN CATCH\n  THROW\nEND CATCH", "SELECT 3"]);
    }

    #[test]
    fn with_nolock_is_not_a_cte() {
        let sql = "SELECT * FROM t\nWITH (NOLOCK)\nSELECT 2";
        assert_eq!(texts(sql).len(), 2);
    }

    #[test]
    fn statement_at_positions() {
        let sql = "select 1;\n\nselect 2";
        assert_eq!(statement_at(sql, 3).unwrap().text(sql), "select 1;");
        assert_eq!(statement_at(sql, 10).unwrap().text(sql), "select 1;");
        assert_eq!(statement_at(sql, 12).unwrap().text(sql), "select 2");
        assert_eq!(statement_at(sql, sql.len()).unwrap().text(sql), "select 2");
        assert_eq!(statement_at("  select 1", 0).unwrap().text("  select 1"), "select 1");
        assert!(statement_at("  -- nothing", 3).is_none());
    }

    #[test]
    fn empty_and_trivia_only() {
        assert!(statements("").is_empty());
        assert!(statements("  \n/* c */\n").is_empty());
        assert_eq!(texts(";;"), vec![";", ";"]);
    }
}
