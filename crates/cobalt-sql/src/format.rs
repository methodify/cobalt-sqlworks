//! SQL formatting via [`sqlformat`] with Cobalt defaults.

/// Formatting options.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormatOptions {
    /// Convert keywords to upper case.
    pub uppercase_keywords: bool,
    /// Indent width in spaces.
    pub indent: usize,
    /// Blank lines to leave between top-level statements (0 = none).
    pub lines_between_queries: usize,
}

impl Default for FormatOptions {
    fn default() -> Self {
        FormatOptions { uppercase_keywords: true, indent: 4, lines_between_queries: 1 }
    }
}

/// Reformat `sql`. Bracketed identifiers, `@variables` and `N'…'` literals survive the
/// round trip; the formatter never fails, so odd input comes back re-spaced rather than
/// rejected.
pub fn format(sql: &str, opts: &FormatOptions) -> String {
    let options = sqlformat::FormatOptions {
        indent: sqlformat::Indent::Spaces(opts.indent.clamp(0, u8::MAX as usize) as u8),
        // `Some(false)` would force lower case; `None` leaves the author's casing alone.
        uppercase: if opts.uppercase_keywords { Some(true) } else { None },
        // sqlformat counts line breaks, not blank lines.
        lines_between_queries: (opts.lines_between_queries + 1).clamp(1, u8::MAX as usize) as u8,
        dialect: sqlformat::Dialect::SQLServer,
        ..Default::default()
    };
    sqlformat::format(sql, &sqlformat::QueryParams::None, &options)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_and_uppercases() {
        let out = format("select a, b from t where a = 1", &FormatOptions::default());
        assert_eq!(out, "SELECT\n    a,\n    b\nFROM\n    t\nWHERE\n    a = 1");
    }

    #[test]
    fn brackets_variables_and_unicode_strings_survive() {
        let src = "select [Order Details].[Product ID], @p, N'it''s' from [Order Details] where [a]]b] = 1";
        let out = format(src, &FormatOptions::default());
        assert!(out.contains("[Order Details].[Product ID]"), "{out}");
        assert!(out.contains("[a]]b]"), "{out}");
        assert!(out.contains("@p"), "{out}");
        assert!(out.contains("N'it''s'"), "{out}");
    }

    #[test]
    fn respects_indent_and_case_options() {
        let opts = FormatOptions { uppercase_keywords: false, indent: 2, lines_between_queries: 2 };
        let out = format("SELECT a FROM t; select b from u", &opts);
        assert!(out.starts_with("SELECT\n  a"), "{out}");
        assert!(out.contains("\n\n\nselect\n  b"), "{out}");
    }

    #[test]
    fn empty_input() {
        assert_eq!(format("", &FormatOptions::default()), "");
    }
}
