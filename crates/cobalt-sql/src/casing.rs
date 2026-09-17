//! Keyword casing transforms.

use crate::lexer::{tokenize, TokenKind};

/// Upper-case every keyword, built-in function and type name in `sql`, leaving
/// identifiers, strings, comments and everything else untouched.
pub fn uppercase_keywords(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    for t in tokenize(sql) {
        let text = t.text(sql);
        match t.kind {
            TokenKind::Keyword | TokenKind::Function | TokenKind::Type => out.push_str(&text.to_ascii_uppercase()),
            _ => out.push_str(text),
        }
    }
    out
}

/// Lower-case every keyword, built-in function and type name in `sql`.
pub fn lowercase_keywords(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    for t in tokenize(sql) {
        let text = t.text(sql);
        match t.kind {
            TokenKind::Keyword | TokenKind::Function | TokenKind::Type => out.push_str(&text.to_ascii_lowercase()),
            _ => out.push_str(text),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uppercases_only_syntax_words() {
        let sql = "select count(*) as select_count, 'from' from [from] f where f.x = @x -- select\n/* from */ and cast(1 as int) > 0";
        assert_eq!(
            uppercase_keywords(sql),
            "SELECT COUNT(*) AS select_count, 'from' FROM [from] f WHERE f.x = @x -- select\n/* from */ AND CAST(1 AS INT) > 0"
        );
    }

    #[test]
    fn lowercase_roundtrip() {
        assert_eq!(lowercase_keywords("SELECT Name FROM T"), "select Name from T");
        assert_eq!(uppercase_keywords(""), "");
    }
}
