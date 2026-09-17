//! Split a script into `GO`-separated batches.
//!
//! `GO` is not T-SQL: it is a client-side separator recognised by SSMS, `sqlcmd` and
//! ADS. A separator line is `GO`, optionally followed by a repeat count (`GO 3`), an
//! optional `;`, and an optional trailing comment. `GO` inside a block comment or a
//! string spanning lines is not a separator.

use crate::lexer::{tokenize_line, LineState, TokenKind};

/// One batch of a script.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Batch {
    /// The batch text (trailing line terminator removed, otherwise verbatim).
    pub sql: String,
    /// 1-based line in the original script where the batch text begins.
    pub start_line: u32,
    /// Byte offset in the original script where the batch text begins.
    pub start_offset: usize,
}

/// Split `script` on `GO` separator lines. A `GO n` count repeats the preceding batch
/// `n` times (as `n` consecutive entries sharing the same position). Batches that are
/// entirely whitespace are dropped.
pub fn split_batches(script: &str) -> Vec<Batch> {
    let mut out = Vec::new();
    let mut state = LineState::INITIAL;
    let mut batch_start: Option<(usize, u32)> = None; // (offset, line)
    let mut batch_end = 0usize;
    let mut line_no: u32 = 0;
    let mut offset = 0usize;

    let flush = |start: Option<(usize, u32)>, end: usize, times: usize, out: &mut Vec<Batch>| {
        if let Some((so, sl)) = start {
            let text = script[so..end].trim_end_matches(['\r', '\n']);
            if !text.trim().is_empty() {
                for _ in 0..times.max(1) {
                    out.push(Batch { sql: text.to_string(), start_line: sl, start_offset: so });
                }
            }
        }
    };

    for line in split_lines_inclusive(script) {
        line_no += 1;
        let line_start = offset;
        offset += line.len();
        let (tokens, next_state) = tokenize_line(line, state);
        let sep = if state.in_block_comment || state.in_string.is_some() {
            None
        } else {
            go_count(line, &tokens)
        };
        state = next_state;
        match sep {
            Some(count) => {
                flush(batch_start, batch_end, count, &mut out);
                batch_start = None;
            }
            None => {
                if batch_start.is_none() {
                    if line.trim().is_empty() {
                        continue; // leading blank lines are not part of the batch
                    }
                    batch_start = Some((line_start, line_no));
                }
                batch_end = offset;
            }
        }
    }
    flush(batch_start, batch_end, 1, &mut out);
    out
}

/// Iterate lines including their terminators so offsets stay exact.
fn split_lines_inclusive(s: &str) -> impl Iterator<Item = &str> {
    let mut rest = s;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let bytes = rest.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'\n' => {
                    i += 1;
                    break;
                }
                b'\r' => {
                    i += 1;
                    if bytes.get(i) == Some(&b'\n') {
                        i += 1;
                    }
                    break;
                }
                _ => i += 1,
            }
        }
        let (line, tail) = rest.split_at(i);
        rest = tail;
        Some(line)
    })
}

/// If `line` (already tokenized) is a `GO` separator, return the repeat count.
fn go_count(line: &str, tokens: &[crate::lexer::Token]) -> Option<usize> {
    let mut sig = tokens.iter().filter(|t| !t.kind.is_trivia());
    let first = sig.next()?;
    if first.kind != TokenKind::Keyword || !first.text(line).eq_ignore_ascii_case("GO") {
        return None;
    }
    let mut count = 1usize;
    let mut saw_count = false;
    let mut saw_semi = false;
    for t in sig {
        match t.kind {
            TokenKind::Number if !saw_count && !saw_semi => {
                count = t.text(line).parse().ok()?;
                saw_count = true;
            }
            TokenKind::Punct if t.text(line) == ";" && !saw_semi => saw_semi = true,
            _ => return None,
        }
    }
    Some(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_on_go_lines() {
        let b = split_batches("select 1\nGO\nselect 2\ngo\r\nselect 3");
        assert_eq!(b.len(), 3);
        assert_eq!(b[0], Batch { sql: "select 1".into(), start_line: 1, start_offset: 0 });
        assert_eq!(b[1], Batch { sql: "select 2".into(), start_line: 3, start_offset: 12 });
        assert_eq!(b[2].sql, "select 3");
        assert_eq!(b[2].start_line, 5);
    }

    #[test]
    fn go_with_count_and_semicolon() {
        let b = split_batches("insert t default values\nGO 3\nselect 1\nGO;\n  GO 2 ; -- twice\nselect 2");
        let sqls: Vec<&str> = b.iter().map(|b| b.sql.as_str()).collect();
        assert_eq!(sqls, vec!["insert t default values"; 3].into_iter().chain(["select 1", "select 2"]).collect::<Vec<_>>());
    }

    #[test]
    fn go_inside_comment_or_string_is_not_a_separator() {
        let b = split_batches("select '\nGO\n' as s\n/*\nGO\n*/\nselect 2\nGO\nselect 3");
        assert_eq!(b.len(), 2);
        assert!(b[0].sql.contains("GO\n' as s"));
        assert_eq!(b[1].sql, "select 3");
    }

    #[test]
    fn go_as_identifier_in_middle_of_line_is_not_a_separator() {
        let b = split_batches("select 1 go\nGO extra\nselect 2");
        assert_eq!(b.len(), 1);
    }

    #[test]
    fn empty_batches_dropped() {
        let b = split_batches("GO\n\nGO\n  \nselect 1\nGO\nGO\n");
        assert_eq!(b.len(), 1);
        assert_eq!(b[0], Batch { sql: "select 1".into(), start_line: 5, start_offset: 12 });
        assert!(split_batches("").is_empty());
        assert!(split_batches("GO").is_empty());
    }

    #[test]
    fn multi_line_batch_keeps_text() {
        let script = "-- header\nSELECT 1,\n  2\nGO\n";
        let b = split_batches(script);
        assert_eq!(b[0].sql, "-- header\nSELECT 1,\n  2");
        assert_eq!(b[0].start_line, 1);
    }
}
