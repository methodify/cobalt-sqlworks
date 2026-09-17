//! Hand-written T-SQL tokenizer.
//!
//! The lexer is deliberately forgiving: it never fails and never panics. Unterminated
//! strings, bracketed identifiers and block comments simply extend to the end of the
//! input. Tokens carry byte offsets into the source so callers can slice the original
//! text (`&src[tok.start..tok.end]`).
//!
//! Two entry points exist:
//!
//! * [`tokenize`] scans a whole script and emits [`TokenKind::Newline`] tokens.
//! * [`tokenize_line`] scans a single line given the [`LineState`] left by the previous
//!   line, returning the state for the next one. Editors cache per-line token vectors
//!   and re-tokenize only from the first changed line until the carried state matches.

use once_cell::sync::Lazy;
use std::collections::HashSet;

/// Classification of a lexed token.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TokenKind {
    /// Reserved / common T-SQL keyword (`SELECT`, `NOLOCK`, …).
    Keyword,
    /// Built-in function name immediately followed by `(` (`COUNT(`, `GETDATE(`).
    Function,
    /// Built-in data type name (`int`, `nvarchar`).
    Type,
    /// Plain identifier (`dbo`, `Orders`, `$action`).
    Identifier,
    /// `[bracketed identifier]` (with `]]` escapes).
    BracketedIdentifier,
    /// `"quoted identifier"` (with `""` escapes).
    QuotedIdentifier,
    /// `'string'` or `N'string'` literal (with `''` escapes).
    String,
    /// Numeric literal: `1`, `1.5`, `1e5`, `0x1F`, `$1.00`.
    Number,
    /// `-- comment` up to (not including) the line ending.
    LineComment,
    /// `/* comment */`, nesting allowed.
    BlockComment,
    /// `@variable` or `@@system_function`; a bare `@` is also a Variable.
    Variable,
    /// `#temp` or `##globaltemp` table name.
    TempTable,
    /// Operator such as `+`, `<>`, `+=`, `::`.
    Operator,
    /// Punctuation: `(`, `)`, `,`, `;`, `.`.
    Punct,
    /// Spaces and tabs.
    Whitespace,
    /// `\n`, `\r\n` or `\r`.
    Newline,
    /// Anything the lexer does not recognise (a single character).
    Unknown,
}

impl TokenKind {
    /// True for whitespace, newlines and comments — tokens that carry no syntax.
    pub fn is_trivia(self) -> bool {
        matches!(
            self,
            TokenKind::Whitespace | TokenKind::Newline | TokenKind::LineComment | TokenKind::BlockComment
        )
    }
    /// True for the identifier-like kinds that can name an object or column.
    pub fn is_name(self) -> bool {
        matches!(
            self,
            TokenKind::Identifier | TokenKind::BracketedIdentifier | TokenKind::QuotedIdentifier
        )
    }
}

/// A token: a kind plus a half-open byte range into the source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Token {
    /// What the token is.
    pub kind: TokenKind,
    /// Byte offset of the first byte.
    pub start: usize,
    /// Byte offset one past the last byte.
    pub end: usize,
}

impl Token {
    /// The token's text within `src` (the string it was lexed from).
    pub fn text<'a>(&self, src: &'a str) -> &'a str {
        &src[self.start..self.end]
    }
    /// Byte length of the token.
    pub fn len(&self) -> usize {
        self.end - self.start
    }
    /// True if the token is empty (never produced by the lexer, but harmless).
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }
}

/// Lexer state carried from one line to the next for incremental highlighting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct LineState {
    /// True while inside a `/* … */` comment (at any nesting depth).
    pub in_block_comment: bool,
    /// Nesting depth of the block comment (0 when not in one). Kept in sync with
    /// `in_block_comment`; needed because T-SQL block comments nest.
    pub block_comment_depth: u32,
    /// The opening delimiter of an unterminated literal: `'` (string), `"` (quoted
    /// identifier) or `[` (bracketed identifier).
    pub in_string: Option<char>,
}

impl LineState {
    /// The state at the start of a script.
    pub const INITIAL: LineState = LineState { in_block_comment: false, block_comment_depth: 0, in_string: None };
}

/// Reserved and commonly used T-SQL keywords (uppercase).
pub static KEYWORDS: &[&str] = &[
    "ADD", "ALL", "ALTER", "AND", "ANY", "APPLY", "AS", "ASC", "AUTHORIZATION", "BACKUP", "BEGIN",
    "BETWEEN", "BREAK", "BROWSE", "BULK", "BY", "CASCADE", "CASE", "CATCH", "CHECK", "CHECKPOINT",
    "CLOSE", "CLUSTERED", "COLLATE", "COLUMN", "COLUMNSTORE", "COMMIT", "COMPUTE", "CONSTRAINT",
    "CONTAINS", "CONTAINSTABLE", "CONTINUE", "CROSS", "CTE", "CURRENT", "CURRENT_DATE",
    "CURRENT_TIME", "CURRENT_TIMESTAMP", "CURRENT_USER", "CURSOR", "DATABASE", "DBCC",
    "DEALLOCATE", "DECLARE", "DEFAULT", "DELETE", "DENY", "DESC", "DISK", "DISTINCT", "DISTRIBUTED",
    "DOUBLE", "DROP", "DUMP", "ELSE", "ENCRYPTION", "END", "ERRLVL", "ESCAPE", "EXCEPT", "EXEC",
    "EXECUTE", "EXISTS", "EXIT", "EXTERNAL", "FETCH", "FILE", "FILLFACTOR", "FILTER", "FIRST",
    "FOR", "FOREIGN", "FREETEXT", "FREETEXTTABLE", "FROM", "FULL", "FUNCTION", "GO", "GOTO",
    "GRANT", "GROUP", "HAVING", "HOLDLOCK", "IDENTITY", "IDENTITY_INSERT", "IDENTITYCOL", "IF",
    "IN", "INCLUDE", "INDEX", "INNER", "INSERT", "INTERSECT", "INTO", "IS", "JOIN", "KEY", "KILL",
    "LAST", "LEFT", "LIKE", "LINENO", "LOAD", "MATCHED", "MERGE", "NATIONAL", "NEXT", "NOCHECK",
    "NOCOUNT", "NOEXPAND", "NOLOCK", "NONCLUSTERED", "NOT", "NULL", "NULLS", "OF", "OFF", "OFFSETS",
    "OFFSET", "ON", "ONLY", "OPEN", "OPENDATASOURCE", "OPENQUERY", "OPENROWSET", "OPENXML",
    "OPTION", "OR", "ORDER", "OUTER", "OUTPUT", "OVER", "PARTITION", "PERCENT", "PERSISTED",
    "PIVOT", "PLAN", "PRECISION", "PRIMARY", "PRINT", "PRIOR", "PROC", "PROCEDURE", "PUBLIC",
    "RAISERROR", "READ", "READCOMMITTED", "READONLY", "READPAST", "READTEXT", "READUNCOMMITTED",
    "RECOMPILE", "RECONFIGURE", "RECURSIVE", "REFERENCES", "REPEATABLEREAD", "REPLICATION",
    "RESTORE", "RESTRICT", "RETURN", "RETURNS", "REVERT", "REVOKE", "RIGHT", "ROLLBACK",
    "ROWCOUNT", "ROWGUIDCOL", "ROWLOCK", "ROWS", "RULE", "SAVE", "SCHEMA", "SCHEMABINDING",
    "SECURITYAUDIT", "SELECT", "SEMANTICKEYPHRASETABLE", "SEMANTICSIMILARITYDETAILSTABLE",
    "SEMANTICSIMILARITYTABLE", "SEQUENCE", "SERIALIZABLE", "SESSION_USER", "SET", "SETUSER",
    "SHUTDOWN", "SNAPSHOT", "SOME", "SPARSE", "STATISTICS", "SYNONYM", "SYSTEM_USER", "TABLE",
    "TABLESAMPLE", "TABLOCK", "TABLOCKX", "TEXTSIZE", "THEN", "THROW", "TIES", "TO", "TOP",
    "TRAN", "TRANSACTION", "TRIGGER", "TRUNCATE", "TRY", "TSEQUAL", "TYPE", "UNION", "UNIQUE",
    "UNPIVOT", "UPDATE", "UPDATETEXT", "UPDLOCK", "USE", "USER", "USING", "VALUES", "VARYING",
    "VIEW", "WAITFOR", "WHEN", "WHERE", "WHILE", "WITH", "WITHIN", "WRITETEXT", "XACT_ABORT",
    "XLOCK", "XML", "ANSI_NULLS", "ANSI_PADDING", "ANSI_WARNINGS", "ARITHABORT",
    "CONCAT_NULL_YIELDS_NULL", "QUOTED_IDENTIFIER", "NUMERIC_ROUNDABORT", "DATEFORMAT",
    "DATEFIRST", "LANGUAGE", "LOCK_TIMEOUT", "ISOLATION", "LEVEL", "UNCOMMITTED", "COMMITTED",
    "REPEATABLE", "MAXDOP", "OPTIMIZE", "ROBUST", "KEEPFIXED", "EXPAND", "VIEWS",
    "PARAMETERIZATION", "FORCED", "STATISTICS_NORECOMPUTE", "ALLOW_ROW_LOCKS", "ALLOW_PAGE_LOCKS",
    "DATA_COMPRESSION", "COLUMNSTORE_ARCHIVE", "ONLINE", "SORT_IN_TEMPDB", "DROP_EXISTING",
    "IGNORE_DUP_KEY", "PAD_INDEX", "MAXRECURSION", "INSERTED", "DELETED", "SYSTEM_TIME",
    "SYSTEM_VERSIONING", "HISTORY_TABLE", "MEMORY_OPTIMIZED", "DURABILITY", "SCHEMA_ONLY",
    "SCHEMA_AND_DATA", "NATIVE_COMPILATION", "ATOMIC", "SCOPED", "WITHOUT", "INCLUDE_NULL_VALUES",
    "REBUILD", "REORGANIZE", "DISABLE", "ENABLE", "RESUMABLE", "MAX_DURATION", "ABORT_AFTER_WAIT",
    "INSTEAD", "AFTER", "UNBOUNDED", "PRECEDING", "FOLLOWING", "GROUPING", "SETS", "CUBE",
    "ROLLUP", "APPLICATION", "CERTIFICATE", "ASYMMETRIC", "SYMMETRIC", "PRIVILEGES", "CREATE",
    "MAXVALUE", "MINVALUE", "INCREMENT", "CYCLE", "RESTART", "ROWS",
];

/// Built-in T-SQL function names (uppercase). A word from this list lexes as
/// [`TokenKind::Function`] only when followed by `(`.
pub static FUNCTIONS: &[&str] = &[
    // aggregates
    "AVG", "COUNT", "COUNT_BIG", "SUM", "MIN", "MAX", "STDEV", "STDEVP", "VAR", "VARP",
    "GROUPING", "GROUPING_ID", "CHECKSUM_AGG", "STRING_AGG", "APPROX_COUNT_DISTINCT",
    "APPROX_PERCENTILE_CONT", "APPROX_PERCENTILE_DISC", "PERCENTILE_CONT", "PERCENTILE_DISC",
    "CUME_DIST", "PERCENT_RANK",
    // conversion / null handling
    "CAST", "CONVERT", "TRY_CAST", "TRY_CONVERT", "PARSE", "TRY_PARSE", "COALESCE", "ISNULL",
    "NULLIF", "IIF", "CHOOSE", "ISNUMERIC", "ISDATE", "ISJSON",
    // date / time
    "GETDATE", "GETUTCDATE", "SYSDATETIME", "SYSUTCDATETIME", "SYSDATETIMEOFFSET", "DATEADD",
    "DATEDIFF", "DATEDIFF_BIG", "DATEPART", "DATENAME", "DATEFROMPARTS", "DATETIMEFROMPARTS",
    "DATETIME2FROMPARTS", "DATETIMEOFFSETFROMPARTS", "SMALLDATETIMEFROMPARTS", "TIMEFROMPARTS",
    "EOMONTH", "DAY", "MONTH", "YEAR", "DATETRUNC", "DATE_BUCKET", "SWITCHOFFSET", "TODATETIMEOFFSET",
    // string
    "FORMAT", "LEN", "LEFT", "RIGHT", "SUBSTRING", "CHARINDEX", "PATINDEX", "REPLACE", "STUFF",
    "TRIM", "LTRIM", "RTRIM", "UPPER", "LOWER", "CONCAT", "CONCAT_WS", "STRING_SPLIT",
    "STRING_ESCAPE", "REVERSE", "REPLICATE", "SPACE", "STR", "CHAR", "NCHAR", "ASCII", "UNICODE",
    "QUOTENAME", "SOUNDEX", "DIFFERENCE", "TRANSLATE", "DATALENGTH", "COMPRESS", "DECOMPRESS",
    "PARSENAME", "FORMATMESSAGE", "TEXTPTR", "TEXTVALID",
    // window / ranking
    "ROW_NUMBER", "RANK", "DENSE_RANK", "NTILE", "LAG", "LEAD", "FIRST_VALUE", "LAST_VALUE",
    // math
    "ABS", "ROUND", "FLOOR", "CEILING", "POWER", "SQRT", "SQUARE", "EXP", "LOG", "LOG10", "PI",
    "RAND", "SIGN", "SIN", "COS", "TAN", "ASIN", "ACOS", "ATAN", "ATN2", "COT", "DEGREES",
    "RADIANS", "GREATEST", "LEAST",
    // system / metadata
    "NEWID", "NEWSEQUENTIALID", "OBJECT_ID", "OBJECT_NAME", "OBJECT_SCHEMA_NAME",
    "OBJECT_DEFINITION", "SCHEMA_NAME", "SCHEMA_ID", "DB_NAME", "DB_ID", "COL_NAME", "COL_LENGTH",
    "COLUMNPROPERTY", "OBJECTPROPERTY", "OBJECTPROPERTYEX", "INDEXPROPERTY", "INDEX_COL",
    "DATABASEPROPERTYEX", "SERVERPROPERTY", "SUSER_NAME", "SUSER_SNAME", "SUSER_ID", "SUSER_SID",
    "USER_NAME", "USER_ID", "ORIGINAL_LOGIN", "HOST_NAME", "HOST_ID", "APP_NAME", "CONNECTIONPROPERTY",
    "SESSION_CONTEXT", "CONTEXT_INFO", "CURRENT_TRANSACTION_ID", "XACT_STATE", "ERROR_NUMBER",
    "ERROR_MESSAGE", "ERROR_SEVERITY", "ERROR_STATE", "ERROR_LINE", "ERROR_PROCEDURE",
    "SCOPE_IDENTITY", "IDENT_CURRENT", "IDENT_INCR", "IDENT_SEED", "IDENTITY", "CHECKSUM",
    "BINARY_CHECKSUM", "HASHBYTES", "ROWCOUNT_BIG", "GETANSINULL", "STATS_DATE", "TYPE_ID",
    "TYPE_NAME", "TYPEPROPERTY", "FULLTEXTCATALOGPROPERTY", "FULLTEXTSERVICEPROPERTY",
    "FILE_NAME", "FILE_ID", "FILEPROPERTY", "FILEGROUP_NAME", "FILEGROUP_ID", "PERMISSIONS",
    "HAS_PERMS_BY_NAME", "IS_MEMBER", "IS_ROLEMEMBER", "IS_SRVROLEMEMBER", "SESSIONPROPERTY",
    "OPENJSON", "JSON_VALUE", "JSON_QUERY", "JSON_MODIFY", "JSON_PATH_EXISTS", "JSON_OBJECT",
    "JSON_ARRAY", "OPENXML", "OPENQUERY", "OPENROWSET", "OPENDATASOURCE", "STRING_SPLIT",
    "GENERATE_SERIES", "PREDICT", "VECTOR_DISTANCE", "VECTOR_NORM", "VECTOR_NORMALIZE",
    "VECTORPROPERTY", "CERTENCODED", "CERTPRIVATEKEY", "ENCRYPTBYKEY", "DECRYPTBYKEY",
    "ENCRYPTBYPASSPHRASE", "DECRYPTBYPASSPHRASE", "SIGNBYCERT", "VERIFYSIGNEDBYCERT",
    "CRYPT_GEN_RANDOM", "SQL_VARIANT_PROPERTY", "FN_HELPCOLLATIONS", "FN_LISTEXTENDEDPROPERTY",
    "$PARTITION", "CURSOR_STATUS",
    "@@ROWCOUNT", "@@ERROR", "@@IDENTITY", "@@TRANCOUNT", "@@SPID", "@@VERSION", "@@SERVERNAME",
    "@@SERVICENAME", "@@PROCID", "@@NESTLEVEL", "@@FETCH_STATUS", "@@DATEFIRST", "@@LANGUAGE",
    "@@LOCK_TIMEOUT", "@@MAX_CONNECTIONS", "@@OPTIONS", "@@TEXTSIZE", "@@DBTS", "@@CURSOR_ROWS",
];

/// Built-in data type names (lowercase).
pub static TYPES: &[&str] = &[
    "int", "bigint", "smallint", "tinyint", "bit", "decimal", "numeric", "money", "smallmoney",
    "float", "real", "date", "time", "datetime", "datetime2", "smalldatetime", "datetimeoffset",
    "char", "varchar", "nchar", "nvarchar", "text", "ntext", "binary", "varbinary", "image",
    "uniqueidentifier", "xml", "sql_variant", "geography", "geometry", "hierarchyid", "json",
    "vector", "timestamp", "rowversion", "cursor", "table", "sysname",
];

/// System functions/variables addressed with the `@@` prefix (uppercase, prefix included).
pub static SYSTEM_VARIABLES: &[&str] = &[
    "@@ROWCOUNT", "@@ERROR", "@@IDENTITY", "@@TRANCOUNT", "@@SPID", "@@VERSION", "@@SERVERNAME",
    "@@SERVICENAME", "@@PROCID", "@@NESTLEVEL", "@@FETCH_STATUS", "@@DATEFIRST", "@@LANGUAGE",
    "@@LOCK_TIMEOUT", "@@MAX_CONNECTIONS", "@@OPTIONS", "@@TEXTSIZE", "@@DBTS", "@@CURSOR_ROWS",
    "@@CONNECTIONS", "@@CPU_BUSY", "@@IDLE", "@@IO_BUSY", "@@PACKET_ERRORS", "@@PACK_RECEIVED",
    "@@PACK_SENT", "@@TIMETICKS", "@@TOTAL_ERRORS", "@@TOTAL_READ", "@@TOTAL_WRITE", "@@LANGID",
    "@@MAX_PRECISION", "@@MICROSOFTVERSION", "@@REMSERVER",
];

static KEYWORD_SET: Lazy<HashSet<&'static str>> = Lazy::new(|| KEYWORDS.iter().copied().collect());
static FUNCTION_SET: Lazy<HashSet<&'static str>> = Lazy::new(|| FUNCTIONS.iter().copied().collect());
static TYPE_SET: Lazy<HashSet<String>> = Lazy::new(|| TYPES.iter().map(|t| t.to_ascii_uppercase()).collect());

/// True if `word` is a T-SQL keyword (case-insensitive).
pub fn is_keyword(word: &str) -> bool {
    KEYWORD_SET.contains(word.to_ascii_uppercase().as_str())
}

/// True if `word` is a built-in function name (case-insensitive).
pub fn is_function(word: &str) -> bool {
    FUNCTION_SET.contains(word.to_ascii_uppercase().as_str())
}

/// True if `word` is a built-in data type name (case-insensitive).
pub fn is_type(word: &str) -> bool {
    TYPE_SET.contains(word.to_ascii_uppercase().as_str())
}

/// Tokenize a complete script. Never panics; the concatenation of all token ranges
/// covers the whole input exactly.
pub fn tokenize(src: &str) -> Vec<Token> {
    Lexer::new(src, LineState::INITIAL).run().0
}

/// Tokenize one line (which may or may not include its line terminator) starting from
/// `state`, returning the tokens (offsets relative to `line`) and the state to feed to
/// the next line.
pub fn tokenize_line(line: &str, state: LineState) -> (Vec<Token>, LineState) {
    Lexer::new(line, state).run()
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || !c.is_ascii()
}

fn is_ident_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$' || c == '#' || c == '@' || !c.is_ascii()
}

struct Lexer<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
    tokens: Vec<Token>,
    state: LineState,
}

impl<'a> Lexer<'a> {
    fn new(src: &'a str, state: LineState) -> Self {
        Lexer { src, bytes: src.as_bytes(), pos: 0, tokens: Vec::new(), state }
    }

    fn peek(&self, off: usize) -> Option<u8> {
        self.bytes.get(self.pos + off).copied()
    }

    fn push(&mut self, kind: TokenKind, start: usize) {
        if self.pos > start {
            self.tokens.push(Token { kind, start, end: self.pos });
        }
    }

    /// Advance past the char at `pos` (handles multi-byte UTF-8).
    fn bump_char(&mut self) {
        match self.src[self.pos..].chars().next() {
            Some(c) => self.pos += c.len_utf8(),
            None => self.pos = self.bytes.len(),
        }
    }

    fn cur_char(&self) -> Option<char> {
        self.src[self.pos..].chars().next()
    }

    fn run(mut self) -> (Vec<Token>, LineState) {
        // Resume an unterminated construct from the previous line.
        if self.state.in_block_comment {
            let start = self.pos;
            self.consume_block_comment(self.state.block_comment_depth);
            self.push(TokenKind::BlockComment, start);
        } else if let Some(q) = self.state.in_string {
            let start = self.pos;
            let kind = match q {
                '\'' => TokenKind::String,
                '"' => TokenKind::QuotedIdentifier,
                _ => TokenKind::BracketedIdentifier,
            };
            self.consume_delimited(q);
            self.push(kind, start);
        }

        while self.pos < self.bytes.len() {
            self.next_token();
        }
        (self.tokens, self.state)
    }

    fn next_token(&mut self) {
        let start = self.pos;
        let b = self.bytes[self.pos];
        match b {
            b'\n' => {
                self.pos += 1;
                self.push(TokenKind::Newline, start);
            }
            b'\r' => {
                self.pos += 1;
                if self.peek(0) == Some(b'\n') {
                    self.pos += 1;
                }
                self.push(TokenKind::Newline, start);
            }
            b' ' | b'\t' | 0x0b | 0x0c => {
                while matches!(self.peek(0), Some(b' ' | b'\t' | 0x0b | 0x0c)) {
                    self.pos += 1;
                }
                self.push(TokenKind::Whitespace, start);
            }
            b'-' if self.peek(1) == Some(b'-') => {
                while let Some(c) = self.peek(0) {
                    if c == b'\n' || c == b'\r' {
                        break;
                    }
                    self.pos += 1;
                }
                self.push(TokenKind::LineComment, start);
            }
            b'/' if self.peek(1) == Some(b'*') => {
                self.pos += 2;
                self.consume_block_comment(1);
                self.push(TokenKind::BlockComment, start);
            }
            b'\'' => {
                self.pos += 1;
                self.consume_delimited('\'');
                self.push(TokenKind::String, start);
            }
            b'N' | b'n' if self.peek(1) == Some(b'\'') => {
                self.pos += 2;
                self.consume_delimited('\'');
                self.push(TokenKind::String, start);
            }
            b'"' => {
                self.pos += 1;
                self.consume_delimited('"');
                self.push(TokenKind::QuotedIdentifier, start);
            }
            b'[' => {
                self.pos += 1;
                self.consume_delimited('[');
                self.push(TokenKind::BracketedIdentifier, start);
            }
            b'@' => {
                self.pos += 1;
                if self.peek(0) == Some(b'@') {
                    self.pos += 1;
                }
                self.consume_ident_chars();
                self.push(TokenKind::Variable, start);
            }
            b'#' => {
                self.pos += 1;
                if self.peek(0) == Some(b'#') {
                    self.pos += 1;
                }
                self.consume_ident_chars();
                self.push(TokenKind::TempTable, start);
            }
            b'$' => {
                let next = self.peek(1);
                if next.is_some_and(|c| c.is_ascii_digit())
                    || (next == Some(b'.') && self.peek(2).is_some_and(|c| c.is_ascii_digit()))
                {
                    self.pos += 1;
                    self.consume_number();
                    self.push(TokenKind::Number, start);
                } else if next.is_some_and(|c| c.is_ascii_alphabetic() || c == b'_') {
                    self.pos += 1;
                    self.consume_ident_chars();
                    self.push(TokenKind::Identifier, start);
                } else {
                    self.pos += 1;
                    self.push(TokenKind::Unknown, start);
                }
            }
            b'0'..=b'9' => {
                self.consume_number();
                self.push(TokenKind::Number, start);
            }
            b'.' if self.peek(1).is_some_and(|c| c.is_ascii_digit()) => {
                self.consume_number();
                self.push(TokenKind::Number, start);
            }
            b'.' if self.peek(1) == Some(b'.') => {
                self.pos += 2;
                self.push(TokenKind::Operator, start);
            }
            b'(' | b')' | b',' | b';' | b'.' => {
                self.pos += 1;
                self.push(TokenKind::Punct, start);
            }
            b'<' | b'>' | b'!' | b'=' | b'+' | b'-' | b'*' | b'/' | b'%' | b'&' | b'|' | b'^' | b'~' | b':' => {
                self.consume_operator();
                if self.pos == start {
                    self.pos += 1;
                    self.push(TokenKind::Unknown, start);
                } else {
                    self.push(TokenKind::Operator, start);
                }
            }
            _ => {
                let c = self.cur_char().unwrap_or('\0');
                if is_ident_start(c) {
                    self.consume_ident_chars();
                    let word = &self.src[start..self.pos];
                    let kind = self.classify_word(word);
                    self.push(kind, start);
                } else {
                    self.bump_char();
                    self.push(TokenKind::Unknown, start);
                }
            }
        }
    }

    fn classify_word(&self, word: &str) -> TokenKind {
        let upper = word.to_ascii_uppercase();
        if FUNCTION_SET.contains(upper.as_str()) && self.next_nonspace_is_paren() {
            return TokenKind::Function;
        }
        if KEYWORD_SET.contains(upper.as_str()) {
            return TokenKind::Keyword;
        }
        if TYPE_SET.contains(&upper) {
            return TokenKind::Type;
        }
        TokenKind::Identifier
    }

    fn next_nonspace_is_paren(&self) -> bool {
        let mut i = self.pos;
        while let Some(&c) = self.bytes.get(i) {
            if c == b' ' || c == b'\t' {
                i += 1;
            } else {
                return c == b'(';
            }
        }
        false
    }

    fn consume_ident_chars(&mut self) {
        while let Some(c) = self.cur_char() {
            if is_ident_continue(c) {
                self.pos += c.len_utf8();
            } else {
                break;
            }
        }
    }

    fn consume_number(&mut self) {
        if self.peek(0) == Some(b'0') && matches!(self.peek(1), Some(b'x' | b'X')) {
            self.pos += 2;
            while self.peek(0).is_some_and(|c| c.is_ascii_hexdigit()) {
                self.pos += 1;
            }
            return;
        }
        while self.peek(0).is_some_and(|c| c.is_ascii_digit()) {
            self.pos += 1;
        }
        if self.peek(0) == Some(b'.') && self.peek(1) != Some(b'.') {
            self.pos += 1;
            while self.peek(0).is_some_and(|c| c.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(0), Some(b'e' | b'E')) {
            let save = self.pos;
            self.pos += 1;
            if matches!(self.peek(0), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if self.peek(0).is_some_and(|c| c.is_ascii_digit()) {
                while self.peek(0).is_some_and(|c| c.is_ascii_digit()) {
                    self.pos += 1;
                }
            } else {
                self.pos = save;
            }
        }
    }

    fn consume_operator(&mut self) {
        const TWO: &[&[u8; 2]] = &[
            b"<>", b"!=", b"<=", b">=", b"+=", b"-=", b"*=", b"/=", b"%=", b"&=", b"|=", b"^=", b"::",
            b"!<", b"!>",
        ];
        if let (Some(a), Some(b)) = (self.peek(0), self.peek(1)) {
            if TWO.iter().any(|op| op[0] == a && op[1] == b) {
                self.pos += 2;
                return;
            }
        }
        if matches!(
            self.peek(0),
            Some(b'<' | b'>' | b'=' | b'+' | b'-' | b'*' | b'/' | b'%' | b'&' | b'|' | b'^' | b'~')
        ) {
            self.pos += 1;
        }
    }

    /// Consume the body of a delimited literal opened by `open` (`'`, `"` or `[`), the
    /// opening delimiter already consumed. Doubled closers escape; an unterminated
    /// literal extends to the end of the input and records itself in the line state.
    fn consume_delimited(&mut self, open: char) {
        let close = if open == '[' { b']' } else { open as u8 };
        loop {
            match self.peek(0) {
                None => {
                    self.state.in_string = Some(open);
                    return;
                }
                Some(c) if c == close => {
                    if self.peek(1) == Some(close) {
                        self.pos += 2;
                    } else {
                        self.pos += 1;
                        self.state.in_string = None;
                        return;
                    }
                }
                Some(_) => self.bump_char(),
            }
        }
    }

    /// Consume a (possibly nested) block comment body at the given depth; the opening
    /// `/*` for the outermost level is already consumed.
    fn consume_block_comment(&mut self, mut depth: u32) {
        while depth > 0 {
            match (self.peek(0), self.peek(1)) {
                (None, _) => {
                    self.state.in_block_comment = true;
                    self.state.block_comment_depth = depth;
                    return;
                }
                (Some(b'/'), Some(b'*')) => {
                    depth += 1;
                    self.pos += 2;
                }
                (Some(b'*'), Some(b'/')) => {
                    depth -= 1;
                    self.pos += 2;
                }
                _ => self.bump_char(),
            }
        }
        self.state.in_block_comment = false;
        self.state.block_comment_depth = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<(TokenKind, &str)> {
        tokenize(src)
            .into_iter()
            .filter(|t| !matches!(t.kind, TokenKind::Whitespace | TokenKind::Newline))
            .map(|t| (t.kind, t.text(src)))
            .collect()
    }

    #[test]
    fn covers_input_exactly() {
        let src = "SELECT [a b], 'x''y', N'z' -- c\n/* /* n */ */ @v, #t, 1.5e3, 0x1F, $1.00 FROM dbo.t;";
        let toks = tokenize(src);
        let mut pos = 0;
        for t in &toks {
            assert_eq!(t.start, pos, "gap before {:?}", t);
            pos = t.end;
        }
        assert_eq!(pos, src.len());
    }

    #[test]
    fn classifies_basic_tokens() {
        let k = kinds("SELECT COUNT(*) AS n, name FROM dbo.[Order Details] o WHERE o.id <> 1");
        assert_eq!(k[0], (TokenKind::Keyword, "SELECT"));
        assert_eq!(k[1], (TokenKind::Function, "COUNT"));
        assert_eq!(k[2], (TokenKind::Punct, "("));
        assert_eq!(k[3], (TokenKind::Operator, "*"));
        assert_eq!(k[6], (TokenKind::Identifier, "n"));
        assert!(k.iter().any(|t| *t == (TokenKind::BracketedIdentifier, "[Order Details]")));
        assert!(k.iter().any(|t| *t == (TokenKind::Operator, "<>")));
        assert!(k.iter().any(|t| *t == (TokenKind::Number, "1")));
    }

    #[test]
    fn strings_and_escapes() {
        let k = kinds("'it''s' N'x' \"quoted\"\"id\" [a]]b]");
        assert_eq!(k[0], (TokenKind::String, "'it''s'"));
        assert_eq!(k[1], (TokenKind::String, "N'x'"));
        assert_eq!(k[2], (TokenKind::QuotedIdentifier, "\"quoted\"\"id\""));
        assert_eq!(k[3], (TokenKind::BracketedIdentifier, "[a]]b]"));
    }

    #[test]
    fn numbers() {
        let k = kinds("1 1.5 1e5 2.5E-3 0x1F $1.00 .5 3.");
        let nums: Vec<&str> = k.iter().filter(|t| t.0 == TokenKind::Number).map(|t| t.1).collect();
        assert_eq!(nums, vec!["1", "1.5", "1e5", "2.5E-3", "0x1F", "$1.00", ".5", "3."]);
    }

    #[test]
    fn operators() {
        let k = kinds("a += 1 b -= 2 c <> d e != f g >= h i :: j k .. l ~m n %= o |= p ^= q &= r <= s");
        let ops: Vec<&str> = k.iter().filter(|t| t.0 == TokenKind::Operator).map(|t| t.1).collect();
        assert_eq!(ops, vec!["+=", "-=", "<>", "!=", ">=", "::", "..", "~", "%=", "|=", "^=", "&=", "<="]);
    }

    #[test]
    fn comments_nested_and_line() {
        let k = kinds("/* a /* b */ c */ SELECT -- trailing\n1");
        assert_eq!(k[0], (TokenKind::BlockComment, "/* a /* b */ c */"));
        assert_eq!(k[1], (TokenKind::Keyword, "SELECT"));
        assert_eq!(k[2], (TokenKind::LineComment, "-- trailing"));
        assert_eq!(k[3], (TokenKind::Number, "1"));
    }

    #[test]
    fn variables_and_temp_tables() {
        let k = kinds("@x @@ROWCOUNT #t ##g @");
        assert_eq!(k[0], (TokenKind::Variable, "@x"));
        assert_eq!(k[1], (TokenKind::Variable, "@@ROWCOUNT"));
        assert_eq!(k[2], (TokenKind::TempTable, "#t"));
        assert_eq!(k[3], (TokenKind::TempTable, "##g"));
        assert_eq!(k[4], (TokenKind::Variable, "@"));
    }

    #[test]
    fn types_keywords_functions_case_insensitive() {
        let k = kinds("declare @x NVarChar(50) = left('ab', 1) select * from t left join u on 1=1");
        assert_eq!(k[0], (TokenKind::Keyword, "declare"));
        assert_eq!(k[2], (TokenKind::Type, "NVarChar"));
        assert_eq!(k[7], (TokenKind::Function, "left"));
        assert!(k.iter().any(|t| *t == (TokenKind::Keyword, "left")));
        assert!(is_keyword("Select") && is_function("getdate") && is_type("INT"));
        assert!(!is_keyword("foo"));
    }

    #[test]
    fn function_name_without_paren_is_identifier() {
        let k = kinds("SELECT format FROM t");
        assert_eq!(k[1], (TokenKind::Identifier, "format"));
    }

    #[test]
    fn unterminated_constructs_do_not_panic() {
        for src in ["'abc", "[abc", "\"abc", "/* abc", "/* a /* b */", "N'", "0x", "$", "@", "#", "1e", "é'"] {
            let toks = tokenize(src);
            assert_eq!(toks.last().map(|t| t.end), Some(src.len()), "{src}");
        }
        assert!(tokenize("").is_empty());
    }

    #[test]
    fn unicode_identifiers() {
        let k = kinds("SELECT naïve, 日本 FROM t");
        assert_eq!(k[1], (TokenKind::Identifier, "naïve"));
        assert_eq!(k[3], (TokenKind::Identifier, "日本"));
    }

    #[test]
    fn line_state_carries_block_comment() {
        let (t1, s1) = tokenize_line("SELECT 1 /* start", LineState::INITIAL);
        assert!(s1.in_block_comment);
        assert_eq!(t1.last().unwrap().kind, TokenKind::BlockComment);
        let (t2, s2) = tokenize_line("/* nested", s1);
        assert_eq!(s2.block_comment_depth, 2);
        assert_eq!(t2.len(), 1);
        let (t3, s3) = tokenize_line("*/ still */ SELECT 2", s2);
        assert!(!s3.in_block_comment);
        assert_eq!(t3[0], Token { kind: TokenKind::BlockComment, start: 0, end: 11 });
        assert_eq!(t3[2].kind, TokenKind::Keyword);
    }

    #[test]
    fn line_state_carries_string() {
        let (_, s1) = tokenize_line("SELECT 'abc", LineState::INITIAL);
        assert_eq!(s1.in_string, Some('\''));
        let (t2, s2) = tokenize_line("def' AS x", s1);
        assert_eq!(s2, LineState::INITIAL);
        assert_eq!(t2[0], Token { kind: TokenKind::String, start: 0, end: 4 });
        assert_eq!(t2[2].kind, TokenKind::Keyword);
    }

    #[test]
    fn newlines_are_tokens() {
        let toks = tokenize("a\r\nb\nc\rd");
        let nl: Vec<_> = toks.iter().filter(|t| t.kind == TokenKind::Newline).collect();
        assert_eq!(nl.len(), 3);
        assert_eq!(nl[0].len(), 2);
    }
}
