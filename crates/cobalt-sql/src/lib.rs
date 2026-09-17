//! T-SQL language services for Cobalt SQL Works: lexing, batch and statement
//! splitting, completion, formatting, snippets and keyword casing.
//!
//! Everything here is pure, synchronous Rust with no I/O and no `unsafe`. The modules
//! are independent building blocks that the editor composes:
//!
//! | Module | Purpose |
//! |---|---|
//! | [`lexer`] | Hand-written tokenizer with a per-line incremental API for highlighting. |
//! | [`batches`] | Split a script on `GO` separator lines. |
//! | [`statements`] | Split a batch into statements; find the statement at the cursor. |
//! | [`completion`] | Context-aware completion from tokens + a [`cobalt_core::DatabaseCatalog`]. |
//! | [`format`] | Pretty-print SQL via `sqlformat` (SQL Server dialect). |
//! | [`snippets`] | Built-in snippets and tab-stop expansion. |
//! | [`casing`] | Upper/lower-case keywords without touching identifiers or strings. |
//!
//! All public items are re-exported at the crate root.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod batches;
pub mod casing;
pub mod completion;
pub mod format;
pub mod lexer;
pub mod snippets;
pub mod statements;

pub use batches::*;
pub use casing::*;
pub use completion::*;
pub use format::*;
pub use lexer::*;
pub use snippets::*;
pub use statements::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn end_to_end_script_pipeline() {
        let script = "SELECT 1;\nSELECT 2\nGO\nDECLARE @x int = 3\nSELECT @x";
        let batches = split_batches(script);
        assert_eq!(batches.len(), 2);
        let s = statements(&batches[0].sql);
        assert_eq!(s.len(), 2);
        let s2 = statements(&batches[1].sql);
        assert_eq!(s2.len(), 2);
        assert_eq!(s2[1].line, 2);
        assert_eq!(s2[1].line + batches[1].start_line - 1, 5);
        let formatted = format(&batches[0].sql, &FormatOptions::default());
        assert!(formatted.starts_with("SELECT"));
        assert_eq!(uppercase_keywords("select @x"), "SELECT @x");
    }
}
