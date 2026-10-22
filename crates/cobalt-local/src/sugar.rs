//! T-SQL sugar for the local engine: the handful of habits a SQL Server person brings to a
//! PostgreSQL-flavoured dialect, rewritten before the engine sees the text and reported back
//! so nothing is silent. Deliberately small: `[name]` → `"name"`, `SELECT TOP (n)` → `LIMIT n`,
//! `ISNULL(` → `COALESCE(`, `GETDATE()` / `SYSDATETIME()` → `now()`, `LEN(` → `length(`.
//! Everything inside string literals and comments is left alone.

/// One statement rewritten, with a note per rewrite applied.
pub fn rewrite(statement: &str) -> (String, Vec<String>) {
    rewrite_with(statement, &[])
}

/// `rewrite` plus case-insensitive names: a bare word that matches one of `known` (table and
/// column names with upper-case letters, which the engine would otherwise fold to lower case)
/// becomes that name, quoted.
pub fn rewrite_with(statement: &str, known: &[String]) -> (String, Vec<String>) {
    let mut notes = Vec::new();
    let text = brackets(statement, &mut notes);
    let text = functions(&text, &mut notes);
    let text = top(&text, &mut notes);
    let text = names(&text, known, &mut notes);
    (text, notes)
}

/// Words the name pass leaves alone even when a column is called the same.
const RESERVED: &[&str] = &[
    "select", "from", "where", "group", "by", "order", "having", "limit", "offset", "as", "and", "or", "not", "in", "is", "null", "on", "join", "left", "right", "inner", "outer", "full", "cross", "natural", "using", "distinct", "case", "when", "then", "else", "end", "cast", "try_cast", "over", "partition", "rows", "range", "between", "like", "ilike", "similar", "union", "intersect", "except", "all", "any", "some", "exists", "true", "false", "asc", "desc", "nulls", "first", "last", "with", "recursive", "values", "insert", "into", "create", "table", "external", "drop", "if", "replace", "boolean", "int", "integer", "bigint", "smallint", "tinyint", "float", "double", "real", "decimal", "numeric", "varchar", "char", "text", "string", "date", "time", "timestamp", "interval", "year", "month", "day", "hour", "minute", "second", "week", "quarter", "unsigned", "to", "extract", "filter", "within", "escape", "top", "explain", "analyze", "verbose", "show", "describe", "set",
];

fn names(text: &str, known: &[String], notes: &mut Vec<String>) -> String {
    let known: Vec<&String> = known.iter().filter(|n| n.chars().any(|c| c.is_ascii_uppercase())).collect();
    if known.is_empty() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    for (is_code, seg) in segments(text) {
        if !is_code {
            out.push_str(&seg);
            continue;
        }
        let b = seg.as_bytes();
        let mut i = 0;
        while i < b.len() {
            let c = b[i];
            if c.is_ascii_alphabetic() || c == b'_' {
                let start = i;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                    i += 1;
                }
                let word = &seg[start..i];
                let next = seg[i..].trim_start().chars().next();
                let lower = word.to_ascii_lowercase();
                let hit = if next == Some('(') || RESERVED.contains(&lower.as_str()) || known.iter().any(|k| *k == word) {
                    None
                } else {
                    let mut m = known.iter().filter(|k| k.eq_ignore_ascii_case(word));
                    match (m.next(), m.next()) {
                        (Some(k), None) => Some(k),
                        _ => None,
                    }
                };
                match hit {
                    Some(k) => {
                        out.push('"');
                        out.push_str(&k.replace('"', "\"\""));
                        out.push('"');
                        notes.push(format!("{word} → \"{k}\" (names are case-insensitive)"));
                    }
                    None => out.push_str(word),
                }
            } else if c.is_ascii_digit() {
                // a number (digits, dots, exponents): copied through, never a name
                let start = i;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'.') {
                    i += 1;
                }
                out.push_str(&seg[start..i]);
            } else {
                out.push(c as char);
                i += 1;
            }
        }
    }
    out
}

/// Split into segments: code vs literal/comment, so rewrites touch only code.
fn segments(text: &str) -> Vec<(bool, String)> {
    let mut out: Vec<(bool, String)> = Vec::new();
    let mut code = String::new();
    let b = text.as_bytes();
    let mut i = 0;
    let flush = |code: &mut String, out: &mut Vec<(bool, String)>| {
        if !code.is_empty() {
            out.push((true, std::mem::take(code)));
        }
    };
    while i < b.len() {
        let c = b[i];
        if c == b'\'' || c == b'"' || c == b'`' {
            // a literal or quoted identifier (doubled quote escapes)
            let q = c;
            let start = i;
            i += 1;
            while i < b.len() {
                if b[i] == q {
                    if i + 1 < b.len() && b[i + 1] == q {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            flush(&mut code, &mut out);
            out.push((false, text[start..i.min(text.len())].to_string()));
        } else if c == b'-' && i + 1 < b.len() && b[i + 1] == b'-' {
            let start = i;
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            flush(&mut code, &mut out);
            out.push((false, text[start..i].to_string()));
        } else if c == b'/' && i + 1 < b.len() && b[i + 1] == b'*' {
            let start = i;
            i += 2;
            while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(b.len());
            flush(&mut code, &mut out);
            out.push((false, text[start..i].to_string()));
        } else {
            // advance by one char (UTF-8 aware)
            let ch = text[i..].chars().next().unwrap();
            code.push(ch);
            i += ch.len_utf8();
        }
    }
    flush(&mut code, &mut out);
    out
}

fn rebuild(segs: Vec<(bool, String)>) -> String {
    segs.into_iter().map(|(_, s)| s).collect()
}

fn brackets(text: &str, notes: &mut Vec<String>) -> String {
    let mut n = 0;
    let segs = segments(text)
        .into_iter()
        .map(|(code, s)| {
            if !code {
                return (code, s);
            }
            let mut out = String::with_capacity(s.len());
            let mut rest = s.as_str();
            while let Some(open) = rest.find('[') {
                let after = &rest[open + 1..];
                match after.find(']') {
                    Some(close) if !after[..close].contains('\n') && !after[..close].is_empty() => {
                        out.push_str(&rest[..open]);
                        out.push('"');
                        out.push_str(&after[..close].replace('"', "\"\""));
                        out.push('"');
                        n += 1;
                        rest = &after[close + 1..];
                    }
                    _ => {
                        out.push_str(&rest[..open + 1]);
                        rest = after;
                    }
                }
            }
            out.push_str(rest);
            (code, out)
        })
        .collect();
    if n > 0 {
        notes.push(format!("[bracketed] names rewritten as \"quoted\" ({n})"));
    }
    rebuild(segs)
}

fn functions(text: &str, notes: &mut Vec<String>) -> String {
    let rules: [(&str, &str, &str); 4] = [("ISNULL(", "COALESCE(", "ISNULL → COALESCE"), ("GETDATE()", "now()", "GETDATE() → now()"), ("SYSDATETIME()", "now()", "SYSDATETIME() → now()"), ("LEN(", "length(", "LEN → length")];
    let segs = segments(text)
        .into_iter()
        .map(|(code, s)| {
            if !code {
                return (code, s);
            }
            let mut out = s;
            for (from, to, note) in rules {
                let (next, count) = replace_word_ci(&out, from, to);
                if count > 0 {
                    notes.push(format!("{note} ({count})"));
                }
                out = next;
            }
            (code, out)
        })
        .collect();
    rebuild(segs)
}

/// Replace `from` (case-insensitive) where it starts at a word boundary.
fn replace_word_ci(s: &str, from: &str, to: &str) -> (String, usize) {
    let lower = s.to_ascii_lowercase();
    let needle = from.to_ascii_lowercase();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    let mut count = 0;
    while let Some(pos) = lower[i..].find(&needle) {
        let at = i + pos;
        let boundary = at == 0 || !(lower.as_bytes()[at - 1].is_ascii_alphanumeric() || lower.as_bytes()[at - 1] == b'_');
        out.push_str(&s[i..at]);
        if boundary {
            out.push_str(to);
            count += 1;
        } else {
            out.push_str(&s[at..at + needle.len()]);
        }
        i = at + needle.len();
    }
    out.push_str(&s[i..]);
    (out, count)
}

/// `SELECT [DISTINCT] TOP (n)` / `TOP n` → the TOP removed and ` LIMIT n` appended to the
/// statement (unless it has a LIMIT already). Only the first SELECT of the statement.
fn top(text: &str, notes: &mut Vec<String>) -> String {
    let segs = segments(text);
    // find TOP in the first code segment that has SELECT
    let mut found: Option<(usize, usize, usize, String)> = None; // (segment, start, end, n)
    for (si, (code, s)) in segs.iter().enumerate() {
        if !code {
            continue;
        }
        let lower = s.to_ascii_lowercase();
        let Some(sel) = find_word(&lower, "select") else { continue };
        let mut after = sel + 6;
        // skip whitespace and DISTINCT / ALL
        loop {
            let trimmed = lower[after..].trim_start();
            let skipped = lower[after..].len() - trimmed.len();
            after += skipped;
            if trimmed.starts_with("distinct") && !trimmed[8..].starts_with(|c: char| c.is_alphanumeric() || c == '_') {
                after += 8;
            } else if trimmed.starts_with("all") && !trimmed[3..].starts_with(|c: char| c.is_alphanumeric() || c == '_') {
                after += 3;
            } else {
                break;
            }
        }
        if !lower[after..].starts_with("top") || lower[after + 3..].starts_with(|c: char| c.is_alphanumeric() || c == '_') {
            break;
        }
        let mut j = after + 3;
        let rest = &lower[j..];
        let t = rest.trim_start();
        j += rest.len() - t.len();
        let (n, end) = if let Some(inner) = t.strip_prefix('(') {
            let Some(close) = inner.find(')') else { break };
            (inner[..close].trim().to_string(), j + 1 + close + 1)
        } else {
            let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
            if digits.is_empty() {
                break;
            }
            (digits.clone(), j + digits.len())
        };
        if n.parse::<u64>().is_err() {
            break;
        }
        found = Some((si, after, end, n));
        break;
    }
    let Some((si, start, end, n)) = found else { return rebuild(segs) };
    let has_limit = segs.iter().any(|(code, s)| *code && find_word(&s.to_ascii_lowercase(), "limit").is_some());
    let mut segs = segs;
    let s = &mut segs[si].1;
    let mut replaced = String::new();
    replaced.push_str(&s[..start]);
    replaced.push_str(s[end..].trim_start());
    *s = replaced;
    let mut out = rebuild(segs);
    if !has_limit {
        let trimmed = out.trim_end();
        let semi = trimmed.ends_with(';');
        let body = if semi { trimmed[..trimmed.len() - 1].trim_end() } else { trimmed };
        out = format!("{body} LIMIT {n}{}", if semi { ";" } else { "" });
        notes.push(format!("TOP {n} → LIMIT {n}"));
    } else {
        notes.push(format!("TOP {n} removed (the statement has a LIMIT)"));
    }
    out
}

fn find_word(lower: &str, word: &str) -> Option<usize> {
    let mut i = 0;
    while let Some(pos) = lower[i..].find(word) {
        let at = i + pos;
        let before = at == 0 || !(lower.as_bytes()[at - 1].is_ascii_alphanumeric() || lower.as_bytes()[at - 1] == b'_');
        let after_i = at + word.len();
        let after = after_i >= lower.len() || !(lower.as_bytes()[after_i].is_ascii_alphanumeric() || lower.as_bytes()[after_i] == b'_');
        if before && after {
            return Some(at);
        }
        i = after_i;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_case_insensitive() {
        let known = vec!["countryOrRegion".to_string(), "holidayName".to_string(), "lower".to_string()];
        let (t, n) = rewrite_with("SELECT countryorregion, COUNT(*) FROM publicholidays WHERE HolidayName = 'x' AND date > '2020-01-01' GROUP BY CountryOrRegion, countryOrRegion", &known);
        assert_eq!(t, "SELECT \"countryOrRegion\", COUNT(*) FROM publicholidays WHERE \"holidayName\" = 'x' AND date > '2020-01-01' GROUP BY \"countryOrRegion\", countryOrRegion");
        assert_eq!(n.len(), 3);
        // a function call, a literal and a comment stay
        let (t, _) = rewrite_with("SELECT holidayname('a') -- countryorregion
, 'countryorregion', 12.5e3", &known);
        assert_eq!(t, "SELECT holidayname('a') -- countryorregion
, 'countryorregion', 12.5e3");
    }

    #[test]
    fn brackets_become_quotes() {
        let (s, n) = rewrite("SELECT [Order Id], [name] FROM [dbo].[Orders] WHERE note = '[not this]'");
        assert_eq!(s, "SELECT \"Order Id\", \"name\" FROM \"dbo\".\"Orders\" WHERE note = '[not this]'");
        assert_eq!(n.len(), 1);
    }

    #[test]
    fn top_becomes_limit() {
        let (s, n) = rewrite("SELECT TOP (10) a, b FROM t ORDER BY a;");
        assert_eq!(s, "SELECT a, b FROM t ORDER BY a LIMIT 10;");
        assert_eq!(n, vec!["TOP 10 → LIMIT 10"]);
        let (s, _) = rewrite("select distinct top 5 x from t");
        assert_eq!(s, "select distinct x from t LIMIT 5");
        let (s, n) = rewrite("SELECT TOP 3 x FROM t LIMIT 2");
        assert_eq!(s, "SELECT x FROM t LIMIT 2");
        assert_eq!(n, vec!["TOP 3 removed (the statement has a LIMIT)"]);
    }

    #[test]
    fn functions_rename() {
        let (s, n) = rewrite("SELECT ISNULL(a, 0), isnull(b, ''), LEN(c), GETDATE(), sysdatetime(), mylen(x) FROM t -- LEN(") ;
        assert_eq!(s, "SELECT COALESCE(a, 0), COALESCE(b, ''), length(c), now(), now(), mylen(x) FROM t -- LEN(");
        assert_eq!(n.len(), 4);
    }

    #[test]
    fn untouched_when_nothing_applies() {
        let (s, n) = rewrite("SELECT a, \"b c\" FROM t WHERE x = 'TOP 5' /* [x] */");
        assert_eq!(s, "SELECT a, \"b c\" FROM t WHERE x = 'TOP 5' /* [x] */");
        assert!(n.is_empty());
    }
}
