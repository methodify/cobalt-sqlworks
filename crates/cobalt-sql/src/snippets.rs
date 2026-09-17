//! Editor snippets with VS Code-style tab stops.
//!
//! A snippet body may contain `${n:placeholder}`, `${n}` or `$n` tab stops. `$0` marks
//! the final cursor position. `\$` inserts a literal dollar sign.

/// A snippet: a trigger prefix, a human label and the body with tab stops.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Snippet {
    /// The word the user types to trigger the snippet (`sel`, `cte`, …).
    pub prefix: &'static str,
    /// Short description shown in the completion list.
    pub label: &'static str,
    /// The body with tab stops.
    pub body: &'static str,
}

/// The built-in snippets.
pub static SNIPPETS: &[Snippet] = &[
    Snippet { prefix: "sel", label: "SELECT TOP (100) * FROM …", body: "SELECT TOP (100) *\nFROM ${1:table}$0" },
    Snippet {
        prefix: "selw",
        label: "SELECT … WHERE …",
        body: "SELECT TOP (100) *\nFROM ${1:table}\nWHERE ${2:condition}$0",
    },
    Snippet {
        prefix: "cte",
        label: "WITH cte AS (…) SELECT",
        body: "WITH ${1:cte} AS (\n    SELECT ${2:*}\n    FROM ${3:table}\n)\nSELECT *\nFROM ${1:cte}$0",
    },
    Snippet {
        prefix: "ins",
        label: "INSERT INTO … VALUES",
        body: "INSERT INTO ${1:table} (${2:columns})\nVALUES (${3:values});$0",
    },
    Snippet {
        prefix: "upd",
        label: "UPDATE … SET … WHERE",
        body: "UPDATE ${1:table}\nSET ${2:column} = ${3:value}\nWHERE ${4:condition};$0",
    },
    Snippet { prefix: "del", label: "DELETE FROM … WHERE", body: "DELETE FROM ${1:table}\nWHERE ${2:condition};$0" },
    Snippet {
        prefix: "proc",
        label: "CREATE OR ALTER PROCEDURE",
        body: "CREATE OR ALTER PROCEDURE ${1:dbo}.${2:ProcedureName}\n    ${3:@param int}\nAS\nBEGIN\n    SET NOCOUNT ON;\n\n    $0\nEND",
    },
    Snippet {
        prefix: "fn",
        label: "CREATE OR ALTER FUNCTION (scalar)",
        body: "CREATE OR ALTER FUNCTION ${1:dbo}.${2:FunctionName}\n(\n    ${3:@param int}\n)\nRETURNS ${4:int}\nAS\nBEGIN\n    RETURN $0\nEND",
    },
    Snippet {
        prefix: "tbl",
        label: "CREATE TABLE with identity key",
        body: "CREATE TABLE ${1:dbo}.${2:TableName}\n(\n    ${3:Id} int IDENTITY(1,1) NOT NULL PRIMARY KEY,\n    $0\n);",
    },
    Snippet {
        prefix: "idx",
        label: "CREATE NONCLUSTERED INDEX",
        body: "CREATE NONCLUSTERED INDEX ${1:IX_Table_Column}\nON ${2:dbo.Table} (${3:column})$0;",
    },
    Snippet {
        prefix: "trycatch",
        label: "BEGIN TRY … END TRY BEGIN CATCH … END CATCH",
        body: "BEGIN TRY\n    $0\nEND TRY\nBEGIN CATCH\n    THROW;\nEND CATCH",
    },
    Snippet {
        prefix: "tran",
        label: "BEGIN TRAN … COMMIT",
        body: "BEGIN TRAN;\n\n$0\n\nCOMMIT;",
    },
    Snippet {
        prefix: "case",
        label: "CASE WHEN … THEN … ELSE … END",
        body: "CASE WHEN ${1:condition} THEN ${2:result} ELSE ${3:other} END$0",
    },
    Snippet {
        prefix: "while",
        label: "WHILE … BEGIN … END",
        body: "WHILE ${1:condition}\nBEGIN\n    $0\nEND",
    },
    Snippet {
        prefix: "ifexists",
        label: "IF OBJECT_ID(…) IS NOT NULL DROP …",
        body: "IF OBJECT_ID(N'${1:dbo.TableName}', N'${2:U}') IS NOT NULL\n    DROP ${3:TABLE} ${1:dbo.TableName};$0",
    },
];

/// Find a built-in snippet by its prefix (case-insensitive).
pub fn find(prefix: &str) -> Option<&'static Snippet> {
    SNIPPETS.iter().find(|s| s.prefix.eq_ignore_ascii_case(prefix))
}

/// Expand a snippet body: replace tab stops with their placeholder text and return the
/// expanded text plus the byte ranges of each tab stop in tab order (`$1`, `$2`, …,
/// with `$0` last). Repeated stops (e.g. two `${1:cte}`) each produce a range.
pub fn expand(body: &str) -> (String, Vec<(usize, usize)>) {
    let mut out = String::with_capacity(body.len());
    let mut stops: Vec<(u32, usize, usize)> = Vec::new();
    let mut chars = body.char_indices().peekable();
    while let Some((_, c)) = chars.next() {
        match c {
            '\\' => match chars.peek() {
                Some((_, '$')) | Some((_, '\\')) | Some((_, '}')) => {
                    let (_, e) = chars.next().unwrap();
                    out.push(e);
                }
                _ => out.push('\\'),
            },
            '$' => match chars.peek().copied() {
                Some((_, '{')) => {
                    chars.next();
                    let mut num = String::new();
                    while let Some((_, d)) = chars.peek().copied() {
                        if d.is_ascii_digit() {
                            num.push(d);
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    let index: u32 = num.parse().unwrap_or(0);
                    let start = out.len();
                    match chars.peek().copied() {
                        Some((_, ':')) => {
                            chars.next();
                            let mut depth = 0usize;
                            for (_, p) in chars.by_ref() {
                                if p == '{' {
                                    depth += 1;
                                } else if p == '}' {
                                    if depth == 0 {
                                        break;
                                    }
                                    depth -= 1;
                                }
                                out.push(p);
                            }
                        }
                        Some((_, '}')) => {
                            chars.next();
                        }
                        _ => {
                            // Malformed: treat as literal.
                            out.push_str("${");
                            out.push_str(&num);
                            continue;
                        }
                    }
                    stops.push((index, start, out.len()));
                }
                Some((_, d)) if d.is_ascii_digit() => {
                    let mut num = String::new();
                    while let Some((_, d)) = chars.peek().copied() {
                        if d.is_ascii_digit() {
                            num.push(d);
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    let index: u32 = num.parse().unwrap_or(0);
                    stops.push((index, out.len(), out.len()));
                }
                _ => out.push('$'),
            },
            _ => out.push(c),
        }
    }
    stops.sort_by_key(|&(idx, start, _)| (if idx == 0 { u32::MAX } else { idx }, start));
    (out, stops.into_iter().map(|(_, s, e)| (s, e)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_placeholders_in_order() {
        let (text, stops) = expand("SELECT TOP (100) *\nFROM ${1:table}$0");
        assert_eq!(text, "SELECT TOP (100) *\nFROM table");
        assert_eq!(stops, vec![(24, 29), (29, 29)]);
        assert_eq!(&text[stops[0].0..stops[0].1], "table");
    }

    #[test]
    fn zero_stop_is_last_and_repeats_are_kept() {
        let (text, stops) = expand("$0 ${2:b} ${1:a} ${1:a}");
        assert_eq!(text, " b a a");
        assert_eq!(stops, vec![(3, 4), (5, 6), (1, 2), (0, 0)]);
    }

    #[test]
    fn escapes_and_bare_dollars() {
        let (text, stops) = expand("\\$1 ${1} $x $5.00");
        assert_eq!(text, "$1  $x .00");
        assert_eq!(stops, vec![(3, 3), (7, 7)]);
    }

    #[test]
    fn all_builtins_expand_and_have_unique_prefixes() {
        let mut seen = std::collections::HashSet::new();
        for s in SNIPPETS {
            assert!(seen.insert(s.prefix), "duplicate prefix {}", s.prefix);
            let (text, stops) = expand(s.body);
            assert!(!text.contains("${"), "{}: {text}", s.prefix);
            for (a, b) in stops {
                assert!(a <= b && b <= text.len());
                assert!(text.is_char_boundary(a) && text.is_char_boundary(b));
            }
        }
        assert!(find("SEL").is_some());
        assert!(find("nope").is_none());
    }
}
