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
    /// Inside an unterminated `$$ … $$` body (PostgreSQL, DuckDB).
    pub in_dollar: bool,
}

impl LineState {
    /// The state at the start of a script.
    pub const INITIAL: LineState = LineState { in_block_comment: false, block_comment_depth: 0, in_string: None, in_dollar: false };
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

/// Which SQL the lexer and the completer speak: T-SQL (the default) or Spark SQL, which
/// quotes identifiers with backticks, writes strings with `'` or `"`, and has its own keywords
/// and functions on top of the shared ones.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Dialect {
    /// SQL Server, Azure SQL, Fabric warehouse: `[name]`, `"name"`, T-SQL keywords.
    #[default]
    TSql,
    /// Spark SQL on the local session: `` `name` ``, `"text"` strings, Spark keywords and functions.
    Spark,
    /// The local engine (DataFusion): backticks and `"` quote names, Spark's keyword set, the
    /// engine's own function names (supplied by the host).
    Local,
    /// SQLite: `"name"`, `[name]` and `` `name` `` all quote, its own keywords and functions.
    Sqlite,
    /// DuckDB: `"name"` quotes, PostgreSQL-flavoured keywords, its own functions.
    DuckDb,
    /// PostgreSQL: `"name"` quotes, `$$ dollar-quoted $$` strings, its keywords and functions.
    Postgres,
    /// DAX against a semantic model: `'Table'` quoted table names ([`TokenKind::QuotedIdentifier`]),
    /// `[Column or Measure]` ([`TokenKind::BracketedIdentifier`]), `"text"` strings, `//` as well
    /// as `--` line comments, `INFO.VIEW.TABLES` dotted function names kept as one token, and any
    /// identifier followed by `(` lexed as a [`TokenKind::Function`].
    Dax,
}

impl Dialect {
    /// Spark SQL and the local engine share quoting and keywords.
    pub fn is_sparkish(self) -> bool {
        matches!(self, Dialect::Spark | Dialect::Local)
    }
}

/// DAX keywords (uppercase): the query statements and the operators spelled as words.
pub static DAX_KEYWORDS: &[&str] = &[
    "EVALUATE", "DEFINE", "MEASURE", "VAR", "RETURN", "ORDER", "BY", "START", "AT", "ASC", "DESC",
    "TABLE", "COLUMN", "FUNCTION", "TRUE", "FALSE", "BLANK", "NOT", "AND", "OR", "IN",
];

/// Spark SQL keywords that T-SQL does not have (uppercase).
pub static SPARK_KEYWORDS: &[&str] = &[
    "ANALYZE", "ANTI", "ARRAY", "BUCKET", "BUCKETS", "CACHE", "CATALOG", "CATALOGS", "CLONE", "CLUSTER",
    "CLUSTERED", "CODEGEN", "COLUMNS", "COMMENT", "COST", "DATABASES", "DEEP", "DELTA", "DESCRIBE",
    "DIRECTORY", "DISTRIBUTE", "DIV", "DRY", "EXTENDED", "FORMATTED", "FUNCTIONS", "GLOBAL", "HISTORY",
    "IF", "IGNORE", "ILIKE", "INTERVAL", "LATERAL", "LAZY", "LIMIT", "LOCATION", "MAP", "MATCHED",
    "MSCK", "NAMESPACE", "NAMESPACES", "NATURAL", "NULLS", "OPTIMIZE", "OPTIONS", "OVERWRITE",
    "PARTITION", "PARTITIONED", "PARTITIONS", "PURGE", "QUALIFY", "REFRESH", "REGEXP", "RENAME",
    "REPAIR", "REPLACE", "RESTORE", "RLIKE", "ROLLUP", "SCHEMAS", "SEMI", "SHALLOW", "SHOW", "SORT",
    "STATISTICS", "STRUCT", "TABLES", "TABLESAMPLE", "TBLPROPERTIES", "TEMPORARY", "TIMESTAMP",
    "TRUNCATE", "UNCACHE", "UNSET", "USING", "VACUUM", "VERSION", "VIEWS", "WINDOW", "ZORDER",
];

/// Spark SQL built-in functions that T-SQL does not have (uppercase); lexed as functions when
/// followed by `(`.
pub static SPARK_FUNCTIONS: &[&str] = &[
    "AGGREGATE", "APPROX_COUNT_DISTINCT", "APPROX_PERCENTILE", "ARRAY", "ARRAY_AGG", "ARRAY_CONTAINS",
    "ARRAY_DISTINCT", "ARRAY_EXCEPT", "ARRAY_INTERSECT", "ARRAY_JOIN", "ARRAY_MAX", "ARRAY_MIN",
    "ARRAY_POSITION", "ARRAY_REMOVE", "ARRAY_SORT", "ARRAY_UNION", "ARRAYS_ZIP", "BOOL_AND", "BOOL_OR",
    "COLLECT_LIST", "COLLECT_SET", "CONCAT_WS", "CURRENT_CATALOG", "CURRENT_DATABASE", "CURRENT_SCHEMA",
    "DATE_ADD", "DATE_FORMAT", "DATE_SUB", "DATE_TRUNC", "DATEDIFF", "DAYOFWEEK", "DAYOFYEAR", "DECODE",
    "ELEMENT_AT", "ENCODE", "EXPLODE", "EXPLODE_OUTER", "FILTER", "FIRST", "FIRST_VALUE", "FLATTEN",
    "FROM_JSON", "FROM_UNIXTIME", "FROM_UTC_TIMESTAMP", "GET_JSON_OBJECT", "GREATEST", "HASH", "IFNULL",
    "INITCAP", "INLINE", "INPUT_FILE_NAME", "INSTR", "JSON_TUPLE", "LAST", "LAST_DAY", "LAST_VALUE",
    "LEAST", "LENGTH", "LEVENSHTEIN", "LOCATE", "LPAD", "MAP", "MAP_FROM_ENTRIES", "MAP_KEYS",
    "MAP_VALUES", "MD5", "MOD", "MONTHS_BETWEEN", "NAMED_STRUCT", "NEXT_DAY", "NULLIF", "NVL", "NVL2",
    "PERCENTILE", "PERCENTILE_APPROX", "POSEXPLODE", "POSITION", "POW", "RAISE_ERROR", "RAND", "RANDN",
    "REGEXP_EXTRACT", "REGEXP_EXTRACT_ALL", "REGEXP_REPLACE", "REPEAT", "RPAD", "SEQUENCE", "SHA1",
    "SHA2", "SHIFTLEFT", "SIZE", "SLICE", "SORT_ARRAY", "SPLIT", "SPLIT_PART", "STACK", "STDDEV",
    "STDDEV_POP", "STDDEV_SAMP", "STRUCT", "SUBSTRING_INDEX", "TO_DATE", "TO_JSON", "TO_TIMESTAMP",
    "TO_UNIX_TIMESTAMP", "TO_UTC_TIMESTAMP", "TRANSFORM", "TRY_CAST", "TRY_DIVIDE", "TRY_TO_TIMESTAMP",
    "TYPEOF", "UNIX_TIMESTAMP", "UUID", "VARIANCE", "VAR_POP", "VAR_SAMP", "WEEKDAY", "WEEKOFYEAR",
    "XXHASH64",
];

static KEYWORD_SET: Lazy<HashSet<&'static str>> = Lazy::new(|| KEYWORDS.iter().copied().collect());
static DAX_KEYWORD_SET: Lazy<HashSet<&'static str>> = Lazy::new(|| DAX_KEYWORDS.iter().copied().collect());
static DAX_FUNCTION_SET: Lazy<HashSet<&'static str>> = Lazy::new(|| crate::dax_functions::DAX_FUNCTIONS.iter().copied().collect());
static SPARK_KEYWORD_SET: Lazy<HashSet<&'static str>> = Lazy::new(|| KEYWORDS.iter().chain(SPARK_KEYWORDS.iter()).copied().collect());
static SPARK_FUNCTION_SET: Lazy<HashSet<&'static str>> = Lazy::new(|| FUNCTIONS.iter().chain(SPARK_FUNCTIONS.iter()).copied().collect());
static SQLITE_KEYWORD_SET: Lazy<HashSet<&'static str>> = Lazy::new(|| KEYWORDS.iter().chain(SQLITE_KEYWORDS.iter()).copied().collect());
static SQLITE_FUNCTION_SET: Lazy<HashSet<&'static str>> = Lazy::new(|| SQLITE_FUNCTIONS.iter().copied().collect());
static DUCKDB_KEYWORD_SET: Lazy<HashSet<&'static str>> = Lazy::new(|| KEYWORDS.iter().chain(SQLITE_KEYWORDS.iter()).chain(DUCKDB_KEYWORDS.iter()).copied().collect());
static DUCKDB_FUNCTION_SET: Lazy<HashSet<&'static str>> = Lazy::new(|| DUCKDB_FUNCTIONS.iter().copied().collect());
static POSTGRES_KEYWORD_SET: Lazy<HashSet<&'static str>> = Lazy::new(|| KEYWORDS.iter().chain(POSTGRES_KEYWORDS.iter()).copied().collect());
static POSTGRES_FUNCTION_SET: Lazy<HashSet<&'static str>> = Lazy::new(|| POSTGRES_FUNCTIONS.iter().copied().collect());

/// PostgreSQL keywords on top of the shared ones.
pub static POSTGRES_KEYWORDS: &[&str] = &[
    "ILIKE", "SIMILAR", "LATERAL", "RETURNING", "USING", "DO", "NOTHING", "CONFLICT", "LIMIT", "OFFSET", "FETCH", "FIRST", "NEXT",
    "ONLY", "INTERVAL", "TIMESTAMPTZ", "SERIAL", "BIGSERIAL", "SMALLSERIAL", "BOOLEAN", "VACUUM", "ANALYZE", "ANALYSE", "EXPLAIN",
    "VERBOSE", "COPY", "STDIN", "STDOUT", "LISTEN", "NOTIFY", "UNLISTEN", "ROLE", "EXTENSION", "SEQUENCE", "TYPE", "DOMAIN", "ENUM",
    "LANGUAGE", "PLPGSQL", "RETURNS", "SETOF", "STABLE", "IMMUTABLE", "VOLATILE", "STRICT", "CALL", "PERFORM", "RAISE", "NOTICE",
    "EXCEPTION", "LOOP", "ELSIF", "ARRAY", "GENERATED", "IDENTITY", "ALWAYS", "MATERIALIZED", "CONCURRENTLY", "TABLESAMPLE",
    "OVERLAPS", "ISNULL", "NOTNULL", "RECURSIVE", "WINDOW", "FILTER", "PARTITION", "RANGE", "ROWS", "GROUPS", "PRECEDING",
    "FOLLOWING", "UNBOUNDED", "CURRENT", "EXCLUDE", "TIES", "GROUPING", "SETS", "CUBE", "ROLLUP", "TEMP", "TEMPORARY", "UNLOGGED",
    "INHERITS", "PARTITION", "COLLATE", "CAST", "IF", "EXISTS", "SAVEPOINT", "RELEASE", "ISOLATION", "LEVEL", "SERIALIZABLE",
    "REPEATABLE", "READ", "COMMITTED", "UNCOMMITTED", "DEFERRABLE", "INITIALLY", "DEFERRED", "IMMEDIATE", "GRANT", "REVOKE",
    "PRIVILEGES", "OWNER", "OWNED", "COMMENT", "CLUSTER", "REINDEX", "TRUNCATE", "CASCADE", "RESTRICT", "RENAME", "ATTACH", "SHOW",
    "RESET", "SESSION", "LOCAL", "TIME", "ZONE", "DISCARD", "PREPARE", "DEALLOCATE", "TRIGGER", "BEFORE", "AFTER", "INSTEAD",
    "EACH", "ROW", "STATEMENT", "BEGIN", "END", "DECLARE", "CURSOR", "MOVE", "LOCK", "SHARE", "NOWAIT", "SKIP", "LOCKED", "WITHIN",
    "ORDINALITY", "VARIADIC", "INOUT", "OUT", "OPERATOR", "AGGREGATE", "POLICY", "PUBLICATION", "SUBSCRIPTION", "TABLESPACE",
    "TEXT", "JSONB", "JSON", "UUID", "BYTEA", "INET", "CIDR", "MACADDR", "TSVECTOR", "TSQUERY", "NUMERIC", "REGCLASS", "OID",
];

/// PostgreSQL's commonly used functions (scalar, aggregate, window, set-returning).
pub static POSTGRES_FUNCTIONS: &[&str] = &[
    "ABS", "ACOS", "AGE", "ARRAY_AGG", "ARRAY_APPEND", "ARRAY_CAT", "ARRAY_LENGTH", "ARRAY_POSITION", "ARRAY_REMOVE",
    "ARRAY_TO_STRING", "ASCII", "ASIN", "ATAN", "ATAN2", "AVG", "BIT_AND", "BIT_LENGTH", "BIT_OR", "BOOL_AND", "BOOL_OR", "BTRIM",
    "CARDINALITY", "CBRT", "CEIL", "CEILING", "CHAR_LENGTH", "CHARACTER_LENGTH", "CHR", "CLOCK_TIMESTAMP", "COALESCE", "CONCAT",
    "CONCAT_WS", "CORR", "COS", "COUNT", "COVAR_POP", "COVAR_SAMP", "CUME_DIST", "CURRENT_DATABASE", "CURRENT_DATE",
    "CURRENT_SCHEMA", "CURRENT_SETTING", "CURRENT_TIME", "CURRENT_TIMESTAMP", "CURRENT_USER", "DATE_BIN", "DATE_PART",
    "DATE_TRUNC", "DECODE", "DEGREES", "DENSE_RANK", "DIV", "ENCODE", "EVERY", "EXP", "EXTRACT", "FIRST_VALUE", "FLOOR", "FORMAT",
    "GCD", "GEN_RANDOM_UUID", "GENERATE_SERIES", "GENERATE_SUBSCRIPTS", "GREATEST", "GROUPING", "INITCAP", "ISFINITE",
    "JSON_AGG", "JSON_ARRAY_ELEMENTS", "JSON_ARRAY_LENGTH", "JSON_BUILD_ARRAY", "JSON_BUILD_OBJECT", "JSON_EACH", "JSON_EXTRACT_PATH",
    "JSON_EXTRACT_PATH_TEXT", "JSON_OBJECT", "JSON_OBJECT_AGG", "JSON_TYPEOF", "JSONB_AGG", "JSONB_ARRAY_ELEMENTS", "JSONB_BUILD_OBJECT",
    "JSONB_EACH", "JSONB_EXTRACT_PATH", "JSONB_OBJECT_AGG", "JSONB_PATH_QUERY", "JSONB_PRETTY", "JSONB_SET", "JSONB_TYPEOF",
    "JUSTIFY_DAYS", "JUSTIFY_HOURS", "JUSTIFY_INTERVAL", "LAG", "LAST_VALUE", "LCM", "LEAD", "LEAST", "LEFT", "LENGTH", "LN",
    "LOCALTIME", "LOCALTIMESTAMP", "LOG", "LOG10", "LOWER", "LPAD", "LTRIM", "MAKE_DATE", "MAKE_INTERVAL", "MAKE_TIME",
    "MAKE_TIMESTAMP", "MAKE_TIMESTAMPTZ", "MAX", "MD5", "MIN", "MOD", "MODE", "NEXTVAL", "NOW", "NTH_VALUE", "NTILE", "NULLIF",
    "NUM_NONNULLS", "NUM_NULLS", "OCTET_LENGTH", "OVERLAY", "PERCENT_RANK", "PERCENTILE_CONT", "PERCENTILE_DISC", "PG_BACKEND_PID",
    "PG_CANCEL_BACKEND", "PG_DATABASE_SIZE", "PG_GET_CONSTRAINTDEF", "PG_GET_FUNCTIONDEF", "PG_GET_INDEXDEF", "PG_GET_VIEWDEF",
    "PG_RELATION_SIZE", "PG_SIZE_PRETTY", "PG_SLEEP", "PG_TABLE_SIZE", "PG_TOTAL_RELATION_SIZE", "PG_TYPEOF", "PI", "POSITION",
    "POWER", "QUOTE_IDENT", "QUOTE_LITERAL", "QUOTE_NULLABLE", "RADIANS", "RANDOM", "RANK", "REGEXP_COUNT", "REGEXP_MATCH",
    "REGEXP_MATCHES", "REGEXP_REPLACE", "REGEXP_SPLIT_TO_ARRAY", "REGEXP_SPLIT_TO_TABLE", "REGEXP_SUBSTR", "REGR_SLOPE", "REPEAT",
    "REPLACE", "REVERSE", "RIGHT", "ROUND", "ROW_NUMBER", "ROW_TO_JSON", "RPAD", "RTRIM", "SCALE", "SESSION_USER", "SETSEED",
    "SETVAL", "SHA256", "SIGN", "SIN", "SPLIT_PART", "SQRT", "STARTS_WITH", "STATEMENT_TIMESTAMP", "STDDEV", "STDDEV_POP",
    "STDDEV_SAMP", "STRING_AGG", "STRING_TO_ARRAY", "STRING_TO_TABLE", "STRPOS", "SUBSTR", "SUBSTRING", "SUM", "TAN", "TIMEOFDAY",
    "TO_ASCII", "TO_CHAR", "TO_DATE", "TO_HEX", "TO_JSON", "TO_JSONB", "TO_NUMBER", "TO_TIMESTAMP", "TRANSACTION_TIMESTAMP",
    "TRANSLATE", "TRIM", "TRIM_SCALE", "TRUNC", "UNNEST", "UPPER", "UUID_GENERATE_V4", "VAR_POP", "VAR_SAMP", "VARIANCE", "VERSION",
    "WIDTH_BUCKET", "XMLAGG", "XMLELEMENT",
];

/// DuckDB keywords on top of the shared and SQLite ones.
pub static DUCKDB_KEYWORDS: &[&str] = &[
    "ILIKE", "SIMILAR", "QUALIFY", "PIVOT", "UNPIVOT", "SAMPLE", "USING", "LATERAL", "POSITIONAL", "ASOF", "SEMI", "ANTI",
    "STRUCT", "MAP", "LIST", "ARRAY", "UNION", "ENUM", "MACRO", "SEQUENCE", "TYPE", "COPY", "INSTALL", "LOAD", "ATTACH",
    "DETACH", "CHECKPOINT", "FORCE", "DESCRIBE", "SUMMARIZE", "SHOW", "TABLES", "COMMENT", "RETURNING", "BY", "NAME",
    "EXCLUDE", "REPLACE", "COLUMNS", "GROUPING", "SETS", "CUBE", "ROLLUP", "INTERVAL", "TIMESTAMPTZ", "HUGEINT", "UUID",
    "VARCHAR", "BLOB", "DOUBLE", "BOOLEAN", "ANALYZE", "VACUUM", "PRAGMA", "SET", "RESET", "TRANSACTION", "ROLLBACK", "ABORT",
    "ISNULL", "NOTNULL", "OVERLAPS", "FETCH", "FIRST", "NEXT", "ONLY", "PERCENT", "TRY_CAST", "VARIABLE", "GLOB",
];

/// DuckDB's commonly used functions (scalar, aggregate, window, table functions).
pub static DUCKDB_FUNCTIONS: &[&str] = &[
    "ABS", "ACOS", "AGE", "ALIAS", "APPROX_COUNT_DISTINCT", "APPROX_QUANTILE", "ARG_MAX", "ARG_MIN", "ARRAY_AGG", "ARRAY_LENGTH",
    "ASIN", "ATAN", "ATAN2", "AVG", "BIT_AND", "BIT_OR", "BIT_XOR", "BOOL_AND", "BOOL_OR", "CBRT", "CEIL", "CEILING", "CENTURY",
    "COALESCE", "CONCAT", "CONCAT_WS", "CONTAINS", "CORR", "COS", "COUNT", "COUNT_IF", "COVAR_POP", "COVAR_SAMP", "CUME_DIST",
    "CURRENT_DATE", "CURRENT_TIME", "CURRENT_TIMESTAMP", "DATE_ADD", "DATE_DIFF", "DATE_PART", "DATE_SUB", "DATE_TRUNC", "DATEDIFF",
    "DATEPART", "DATETRUNC", "DAY", "DAYNAME", "DAYOFMONTH", "DAYOFWEEK", "DAYOFYEAR", "DECADE", "DEGREES", "DENSE_RANK", "ENDS_WITH",
    "ENTROPY", "EPOCH", "EPOCH_MS", "EXP", "FACTORIAL", "FAVG", "FIRST", "FIRST_VALUE", "FLOOR", "FORMAT", "FORMAT_BYTES", "FROM_JSON",
    "FSUM", "GAMMA", "GCD", "GENERATE_SERIES", "GEOMEAN", "GREATEST", "GROUP_CONCAT", "HASH", "HISTOGRAM", "HOUR", "IFNULL", "INSTR",
    "ISFINITE", "ISINF", "ISNAN", "JSON", "JSON_ARRAY", "JSON_EXTRACT", "JSON_EXTRACT_STRING", "JSON_GROUP_ARRAY", "JSON_GROUP_OBJECT",
    "JSON_KEYS", "JSON_OBJECT", "JSON_STRUCTURE", "JSON_TRANSFORM", "JSON_TYPE", "JSON_VALID", "KURTOSIS", "LAG", "LAST", "LAST_DAY",
    "LAST_VALUE", "LCASE", "LCM", "LEAD", "LEAST", "LEFT", "LEN", "LENGTH", "LGAMMA", "LIST", "LIST_AGGREGATE", "LIST_CONTAINS",
    "LIST_DISTINCT", "LIST_FILTER", "LIST_SORT", "LIST_TRANSFORM", "LIST_VALUE", "LN", "LOG", "LOG10", "LOG2", "LOWER", "LPAD", "LTRIM",
    "MAD", "MAKE_DATE", "MAKE_TIME", "MAKE_TIMESTAMP", "MAP", "MAX", "MAX_BY", "MD5", "MEDIAN", "MIN", "MIN_BY", "MINUTE", "MOD", "MODE",
    "MONTH", "MONTHNAME", "NANOSECOND", "NEXTVAL", "NOW", "NTH_VALUE", "NTILE", "NULLIF", "OCTET_LENGTH", "ORD", "PERCENT_RANK",
    "PERCENTILE_CONT", "PERCENTILE_DISC", "PI", "POSITION", "POW", "POWER", "PREFIX", "PRINTF", "PRODUCT", "QUANTILE", "QUANTILE_CONT",
    "QUANTILE_DISC", "QUARTER", "RADIANS", "RANDOM", "RANGE", "RANK", "READ_CSV", "READ_CSV_AUTO", "READ_JSON", "READ_JSON_AUTO",
    "READ_PARQUET", "REGEXP_EXTRACT", "REGEXP_EXTRACT_ALL", "REGEXP_FULL_MATCH", "REGEXP_MATCHES", "REGEXP_REPLACE", "REGEXP_SPLIT_TO_ARRAY",
    "REGR_SLOPE", "REPEAT", "REPLACE", "REVERSE", "RIGHT", "ROUND", "ROW", "ROW_NUMBER", "RPAD", "RTRIM", "SECOND", "SETSEED", "SHA256",
    "SIGN", "SIN", "SKEWNESS", "SPLIT", "SPLIT_PART", "SQRT", "STARTS_WITH", "STDDEV", "STDDEV_POP", "STDDEV_SAMP", "STRFTIME",
    "STRING_AGG", "STRING_SPLIT", "STRING_TO_ARRAY", "STRLEN", "STRPOS", "STRPTIME", "STRUCT_PACK", "SUBSTR", "SUBSTRING", "SUFFIX", "SUM",
    "TAN", "TIME_BUCKET", "TO_DAYS", "TO_HOURS", "TO_JSON", "TO_MINUTES", "TO_SECONDS", "TO_TIMESTAMP", "TODAY", "TRANSLATE", "TRIM",
    "TRUNC", "TRY_CAST", "TRY_STRPTIME", "TYPEOF", "UCASE", "UNICODE", "UNNEST", "UPPER", "UUID", "VAR_POP", "VAR_SAMP", "VARIANCE",
    "VERSION", "WEEK", "WEEKDAY", "WEEKOFYEAR", "YEAR", "YEARWEEK",
];

/// SQLite keywords on top of the shared ones.
pub static SQLITE_KEYWORDS: &[&str] = &[
    "PRAGMA", "ATTACH", "DETACH", "VACUUM", "REINDEX", "ANALYZE", "GLOB", "REGEXP", "MATCH", "ISNULL", "NOTNULL", "WITHOUT",
    "ROWID", "STRICT", "RETURNING", "CONFLICT", "ABORT", "FAIL", "IGNORE", "REPLACE", "INDEXED", "AUTOINCREMENT", "TEMP",
    "TEMPORARY", "VIRTUAL", "USING", "RECURSIVE", "WINDOW", "FILTER", "OVER", "PARTITION", "EXCLUDE", "GROUPS", "TIES", "RANGE",
    "ROWS", "PRECEDING", "FOLLOWING", "UNBOUNDED", "CURRENT", "GENERATED", "ALWAYS", "STORED", "DEFERRED", "IMMEDIATE",
    "EXCLUSIVE", "SAVEPOINT", "RELEASE", "COLLATE", "NOCASE", "RTRIM", "BINARY", "LIMIT", "OFFSET", "IF", "EXISTS", "CAST",
    "EXPLAIN", "QUERY", "PLAN", "RAISE", "INSTEAD", "EACH", "ROW", "BEFORE", "AFTER", "TRIGGER", "BEGIN", "END", "DO", "NOTHING",
];

/// SQLite's built-in functions (core, date and time, JSON, math, aggregates, window).
pub static SQLITE_FUNCTIONS: &[&str] = &[
    "ABS", "CHANGES", "CHAR", "COALESCE", "CONCAT", "CONCAT_WS", "FORMAT", "GLOB", "HEX", "IFNULL", "IIF", "INSTR", "LAST_INSERT_ROWID",
    "LENGTH", "LIKE", "LIKELIHOOD", "LIKELY", "LOWER", "LTRIM", "MAX", "MIN", "NULLIF", "OCTET_LENGTH", "PRINTF", "QUOTE", "RANDOM",
    "RANDOMBLOB", "REPLACE", "ROUND", "RTRIM", "SIGN", "SOUNDEX", "SQLITE_VERSION", "SQLITE_SOURCE_ID", "SUBSTR", "SUBSTRING",
    "TOTAL_CHANGES", "TRIM", "TYPEOF", "UNICODE", "UNHEX", "UNLIKELY", "UPPER", "ZEROBLOB",
    "DATE", "TIME", "DATETIME", "JULIANDAY", "UNIXEPOCH", "STRFTIME", "TIMEDIFF",
    "JSON", "JSONB", "JSON_ARRAY", "JSON_ARRAY_LENGTH", "JSON_EXTRACT", "JSON_INSERT", "JSON_OBJECT", "JSON_PATCH", "JSON_REMOVE",
    "JSON_REPLACE", "JSON_SET", "JSON_TYPE", "JSON_VALID", "JSON_QUOTE", "JSON_GROUP_ARRAY", "JSON_GROUP_OBJECT", "JSON_EACH", "JSON_TREE",
    "AVG", "COUNT", "GROUP_CONCAT", "STRING_AGG", "SUM", "TOTAL",
    "ROW_NUMBER", "RANK", "DENSE_RANK", "PERCENT_RANK", "CUME_DIST", "NTILE", "LAG", "LEAD", "FIRST_VALUE", "LAST_VALUE", "NTH_VALUE",
    "ACOS", "ACOSH", "ASIN", "ASINH", "ATAN", "ATAN2", "ATANH", "CEIL", "CEILING", "COS", "COSH", "DEGREES", "EXP", "FLOOR", "LN", "LOG",
    "LOG10", "LOG2", "MOD", "PI", "POW", "POWER", "RADIANS", "SIN", "SINH", "SQRT", "TAN", "TANH", "TRUNC",
    "PRAGMA_TABLE_INFO", "PRAGMA_TABLE_XINFO", "PRAGMA_INDEX_LIST", "PRAGMA_FOREIGN_KEY_LIST", "PRAGMA_DATABASE_LIST",
];
static FUNCTION_SET: Lazy<HashSet<&'static str>> = Lazy::new(|| FUNCTIONS.iter().copied().collect());
static TYPE_SET: Lazy<HashSet<String>> = Lazy::new(|| TYPES.iter().map(|t| t.to_ascii_uppercase()).collect());

/// True if `word` is a T-SQL keyword (case-insensitive).
pub fn is_keyword(word: &str) -> bool {
    KEYWORD_SET.contains(word.to_ascii_uppercase().as_str())
}

/// True if `word` is a keyword of `dialect` (case-insensitive).
pub fn is_keyword_in(word: &str, dialect: Dialect) -> bool {
    let set = match dialect {
        Dialect::TSql => &*KEYWORD_SET,
        Dialect::Sqlite => &*SQLITE_KEYWORD_SET,
        Dialect::DuckDb => &*DUCKDB_KEYWORD_SET,
        Dialect::Postgres => &*POSTGRES_KEYWORD_SET,
        Dialect::Spark | Dialect::Local => &*SPARK_KEYWORD_SET,
        Dialect::Dax => &*DAX_KEYWORD_SET,
    };
    set.contains(word.to_ascii_uppercase().as_str())
}

/// True if `word` is a built-in DAX function name (case-insensitive; dotted names such as
/// `INFO.VIEW.TABLES` included).
pub fn is_dax_function(word: &str) -> bool {
    DAX_FUNCTION_SET.contains(word.to_ascii_uppercase().as_str())
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

/// Tokenize a complete script in `dialect` (Spark: backtick identifiers, `"` strings, no
/// bracketed identifiers, Spark keywords and functions).
pub fn tokenize_dialect(src: &str, dialect: Dialect) -> Vec<Token> {
    let mut lx = Lexer::new(src, LineState::INITIAL);
    lx.dialect = dialect;
    lx.run().0
}

/// Tokenize one line (which may or may not include its line terminator) starting from
/// `state`, returning the tokens (offsets relative to `line`) and the state to feed to
/// the next line.
pub fn tokenize_line(line: &str, state: LineState) -> (Vec<Token>, LineState) {
    Lexer::new(line, state).run()
}

/// [`tokenize_line`] in `dialect`. With [`LineState::INITIAL`] and the whole text this is
/// [`tokenize_dialect`] plus the final state, which says whether the text ends inside an
/// unterminated string, quoted name or block comment.
pub fn tokenize_line_dialect(line: &str, state: LineState, dialect: Dialect) -> (Vec<Token>, LineState) {
    let mut lx = Lexer::new(line, state);
    lx.dialect = dialect;
    lx.run()
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
    dialect: Dialect,
}

impl<'a> Lexer<'a> {
    fn new(src: &'a str, state: LineState) -> Self {
        Lexer { src, bytes: src.as_bytes(), pos: 0, tokens: Vec::new(), state, dialect: Dialect::TSql }
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
        } else if self.state.in_dollar {
            let start = self.pos;
            self.consume_dollar(b"$$");
            self.push(TokenKind::String, start);
        } else if let Some(q) = self.state.in_string {
            let start = self.pos;
            let kind = match q {
                '\'' if self.dialect == Dialect::Dax => TokenKind::QuotedIdentifier,
                '\'' => TokenKind::String,
                // SQLite (like SQL Server with QUOTED_IDENTIFIER on) reads "double quotes" as an identifier
                '"' if !matches!(self.dialect, Dialect::TSql | Dialect::Sqlite | Dialect::DuckDb | Dialect::Postgres) => TokenKind::String,
                '"' | '`' => TokenKind::QuotedIdentifier,
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
            b'/' if self.dialect == Dialect::Dax && self.peek(1) == Some(b'/') => {
                while let Some(c) = self.peek(0) {
                    if c == b'\n' || c == b'\r' {
                        break;
                    }
                    self.pos += 1;
                }
                self.push(TokenKind::LineComment, start);
            }
            b'$' if matches!(self.dialect, Dialect::Postgres | Dialect::DuckDb) && self.dollar_tag_len() > 0 => {
                // `$$ … $$` or `$tag$ … $tag$`: one string token, semicolons and quotes inside and all
                let n = self.dollar_tag_len();
                let tag = self.bytes[self.pos..self.pos + n].to_vec();
                self.pos += n;
                self.consume_dollar(&tag);
                self.push(TokenKind::String, start);
            }
            b'\'' => {
                self.pos += 1;
                self.consume_delimited('\'');
                self.push(if self.dialect == Dialect::Dax { TokenKind::QuotedIdentifier } else { TokenKind::String }, start);
            }
            b'N' | b'n' if self.dialect != Dialect::Dax && self.peek(1) == Some(b'\'') => {
                self.pos += 2;
                self.consume_delimited('\'');
                self.push(TokenKind::String, start);
            }
            b'"' => {
                self.pos += 1;
                self.consume_delimited('"');
                self.push(if matches!(self.dialect, Dialect::TSql | Dialect::Sqlite | Dialect::DuckDb | Dialect::Postgres) { TokenKind::QuotedIdentifier } else { TokenKind::String }, start);
            }
            b'`' if self.dialect.is_sparkish() => {
                self.pos += 1;
                self.consume_delimited('`');
                self.push(TokenKind::QuotedIdentifier, start);
            }
            b'[' if self.dialect.is_sparkish() => {
                self.pos += 1;
                self.push(TokenKind::Punct, start);
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
            b'{' | b'}' | b':' if self.dialect == Dialect::Dax => {
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
                    if self.dialect == Dialect::Dax {
                        self.consume_dotted_segments();
                    }
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
        if self.dialect == Dialect::Dax {
            // DAX has no types and every `name(` is a call (built-in or a model's own function),
            // except the statement keywords: `EVALUATE (…)` and `RETURN (…)` stay keywords.
            let paren = self.next_nonspace_is_paren();
            if DAX_KEYWORD_SET.contains(upper.as_str())
                && !(paren && matches!(upper.as_str(), "TRUE" | "FALSE" | "BLANK" | "NOT" | "AND" | "OR"))
            {
                return TokenKind::Keyword;
            }
            if paren {
                return TokenKind::Function;
            }
            return TokenKind::Identifier;
        }
        let (functions, keywords) = match self.dialect {
            Dialect::TSql => (&*FUNCTION_SET, &*KEYWORD_SET),
            Dialect::Sqlite => (&*SQLITE_FUNCTION_SET, &*SQLITE_KEYWORD_SET),
            Dialect::DuckDb => (&*DUCKDB_FUNCTION_SET, &*DUCKDB_KEYWORD_SET),
            Dialect::Postgres => (&*POSTGRES_FUNCTION_SET, &*POSTGRES_KEYWORD_SET),
            Dialect::Spark | Dialect::Local => (&*SPARK_FUNCTION_SET, &*SPARK_KEYWORD_SET),
            Dialect::Dax => unreachable!(),
        };
        if functions.contains(upper.as_str()) && self.next_nonspace_is_paren() {
            return TokenKind::Function;
        }
        if keywords.contains(upper.as_str()) {
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

    /// DAX: `INFO.VIEW.TABLES`, `IF.EAGER`, `STDEV.P` are one name. Extend the identifier over
    /// every `.segment` that follows it.
    fn consume_dotted_segments(&mut self) {
        while self.peek(0) == Some(b'.') {
            let Some(c) = self.src[self.pos + 1..].chars().next() else { break };
            if !is_ident_start(c) {
                break;
            }
            self.pos += 1;
            self.consume_ident_chars();
        }
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
            if self.dialect == Dialect::Dax && matches!((a, b), (b'&', b'&') | (b'|', b'|') | (b'=', b'=')) {
                self.pos += 2;
                return;
            }
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
    /// At a `$`: the length of a dollar-quote opener (`$$`, `$tag$`) or 0 (a `$1` parameter,
    /// a lone `$`).
    fn dollar_tag_len(&self) -> usize {
        let mut i = self.pos + 1;
        if let Some(c) = self.bytes.get(i) {
            if c.is_ascii_digit() {
                return 0;
            }
        }
        while let Some(&c) = self.bytes.get(i) {
            if c == b'$' {
                return i + 1 - self.pos;
            }
            if !(c.is_ascii_alphanumeric() || c == b'_') {
                return 0;
            }
            i += 1;
        }
        0
    }

    /// Consume up to and including the closing `tag`; unterminated bodies run to the end of
    /// the text (and resume as `$$` on the next line).
    fn consume_dollar(&mut self, tag: &[u8]) {
        match self.bytes[self.pos..].windows(tag.len().max(1)).position(|w| w == tag) {
            Some(off) => {
                self.pos += off + tag.len();
                self.state.in_dollar = false;
            }
            None => {
                self.pos = self.bytes.len();
                self.state.in_dollar = true;
            }
        }
    }

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
        assert!(k.contains(&(TokenKind::BracketedIdentifier, "[Order Details]")));
        assert!(k.contains(&(TokenKind::Operator, "<>")));
        assert!(k.contains(&(TokenKind::Number, "1")));
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
        assert!(k.contains(&(TokenKind::Keyword, "left")));
        assert!(is_keyword("Select") && is_function("getdate") && is_type("INT"));
        assert!(!is_keyword("foo"));
        assert!(is_keyword_in("lateral", Dialect::Spark) && !is_keyword_in("lateral", Dialect::TSql));
    }

    #[test]
    fn spark_dialect_quotes() {
        let t = tokenize_dialect("SELECT `my col`, \"text\" FROM a[0] LATERAL VIEW explode(x)", Dialect::Spark);
        let kinds: Vec<(TokenKind, &str)> = t.iter().map(|k| (k.kind, &"SELECT `my col`, \"text\" FROM a[0] LATERAL VIEW explode(x)"[k.start..k.end])).collect();
        assert!(kinds.contains(&(TokenKind::QuotedIdentifier, "`my col`")));
        assert!(kinds.contains(&(TokenKind::String, "\"text\"")));
        assert!(kinds.contains(&(TokenKind::Punct, "[")));
        assert!(kinds.contains(&(TokenKind::Keyword, "LATERAL")));
        assert!(kinds.contains(&(TokenKind::Function, "explode")));
        // T-SQL keeps its rules
        let t = tokenize("SELECT \"col\" FROM [t]");
        assert!(t.iter().any(|k| k.kind == TokenKind::QuotedIdentifier) && t.iter().any(|k| k.kind == TokenKind::BracketedIdentifier));
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

#[cfg(test)]
mod postgres_tests {
    use super::*;

    fn kinds(src: &str, d: Dialect) -> Vec<(TokenKind, String)> {
        tokenize_dialect(src, d).into_iter().filter(|t| !matches!(t.kind, TokenKind::Whitespace | TokenKind::Newline)).map(|t| (t.kind, src[t.start..t.end].to_string())).collect()
    }

    #[test]
    fn dollar_quoted_bodies_are_one_string() {
        let src = "CREATE FUNCTION f() RETURNS int AS $$ SELECT 1; SELECT 'x'; $$ LANGUAGE sql;";
        let toks = kinds(src, Dialect::Postgres);
        let body = toks.iter().find(|(k, _)| *k == TokenKind::String).expect("a string token");
        assert_eq!(body.1, "$$ SELECT 1; SELECT 'x'; $$");
        // a tagged body, with a lone $$ inside it
        let src = "DO $fn$ BEGIN RAISE NOTICE '$$'; END $fn$;";
        let toks = kinds(src, Dialect::Postgres);
        assert_eq!(toks.iter().filter(|(k, _)| *k == TokenKind::String).count(), 1);
        assert!(toks.iter().any(|(k, t)| *k == TokenKind::String && t.starts_with("$fn$") && t.ends_with("$fn$")));
        // $1 is a parameter, not a quote; T-SQL does not know dollar quoting
        let toks = kinds("SELECT $1, $$x$$", Dialect::Postgres);
        assert!(toks.iter().any(|(k, t)| *k == TokenKind::String && t == "$$x$$"));
        assert!(!kinds("SELECT $$x$$", Dialect::TSql).iter().any(|(k, _)| *k == TokenKind::String));
    }

    #[test]
    fn unterminated_dollar_body_resumes_on_the_next_line() {
        let (_, st) = tokenize_line_dialect("DO $$ BEGIN", LineState::INITIAL, Dialect::Postgres);
        assert!(st.in_dollar);
        let (toks, st) = tokenize_line_dialect("END $$; SELECT 1", st, Dialect::Postgres);
        assert!(!st.in_dollar);
        assert_eq!(toks[0].kind, TokenKind::String);
        assert_eq!(&"END $$; SELECT 1"[toks[0].start..toks[0].end], "END $$");
    }

    #[test]
    fn postgres_dialect_words() {
        assert!(is_keyword_in("ILIKE", Dialect::Postgres) && is_keyword_in("RETURNING", Dialect::Postgres));
        let toks = kinds("SELECT jsonb_build_object('a', 1), x::text FROM t", Dialect::Postgres);
        assert!(toks.iter().any(|(k, t)| *k == TokenKind::Function && t == "jsonb_build_object"));
        assert!(toks.iter().any(|(k, t)| *k == TokenKind::QuotedIdentifier || *k != TokenKind::String || t != "\"x\""));
        let toks = kinds("SELECT \"Name\" FROM t", Dialect::Postgres);
        assert!(toks.iter().any(|(k, t)| *k == TokenKind::QuotedIdentifier && t == "\"Name\""));
    }
}
