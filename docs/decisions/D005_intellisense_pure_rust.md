# D005 — IntelliSense: pure Rust, no SqlToolsService

**Decided:** 2026-09-16 · **By:** founder ("I agree 100%. Pure Rust. Good-not-great is fine.")

## Decision
Editor language features are built in Rust: own T-SQL lexer for highlighting, a per-connection
catalog cache (schemas, tables, views, columns, procs, functions) for completion, `sqlparser`
(`MsSqlDialect`) for statement splitting and alias resolution, `sqlformat` for formatting. An LSP
client stays possible behind a feature flag but SqlToolsService (.NET, ~100 MB) is not bundled.

## Why
Pure Rust is the identity of the project and keeps the binary small and startup instant.
The founder accepts incremental quality.

## Consequences
- V1 completion: keywords, objects in the current DB, columns of tables referenced in the
  current statement, alias-aware where `sqlparser` succeeds. Server-side `SET PARSEONLY` for
  error checking.
- Formatting options are limited to what `sqlformat` exposes until we write our own.
