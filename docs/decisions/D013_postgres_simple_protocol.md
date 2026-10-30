# D013 — PostgreSQL: the simple query protocol, statement by statement, each described first

**Status:** decided by Claude while building the PostgreSQL half of slice E, 2026-10-30 ·
**Revisit when:** the grid gets a decimal column type built from text, or tokio-postgres can
stream a multi-statement script over the extended protocol

## Context

tokio-postgres offers two ways to run SQL. The extended protocol (`query`, `query_raw`) binds
parameters, returns typed binary rows and streams them, but takes exactly one statement per
call and cannot run `DO` blocks, `COPY` or `BEGIN … COMMIT` scripts as one unit. The simple
protocol (`simple_query_raw`) takes a whole script, lets the server split it (dollar-quoted
bodies and all), streams every result set — and returns every value as text with column names
but no types.

Cobalt's grid wants types: integers that sort as numbers, dates that filter as dates, booleans
that are booleans. It also wants scripts: the editor runs what the user selected, which is
rarely one statement.

## Decision

The PostgreSQL driver splits the script with Cobalt's own splitter (`Dialect::Postgres`, which
lexes `$$ … $$` and `$tag$ … $tag$` bodies as one string token) and runs the statements one
after another over the simple protocol. A statement that returns rows (`SELECT`, `WITH`,
`VALUES`, `TABLE`, `SHOW`, `CALL`, `FETCH`, anything with `RETURNING`) is first *described* —
`prepare` without execution — so its column types come from the server's own parse, taken just
before it runs so DDL earlier in the script is seen. The text rows are then converted: `bool`,
`int2/4/8`, `float4/8`, `date`, `time`, `timestamp`, `timestamptz` (normalised to UTC),
`bytea` (`\x` hex) and `uuid` into their Arrow types; a value that does not parse becomes
NULL and is counted in a message. Everything else — `numeric`, arrays, `json`/`jsonb`,
`interval`, ranges, user types — stays text, labelled with the PostgreSQL type name
(`SqlType::Other`), which shows every digit the server sent rather than a rounded float.

Errors stop the run at the failing statement (SQLSTATE and the line in the script, from the
error position plus the statement's start line); after an error the driver sends `ROLLBACK`
so a transaction the script opened does not leave the session aborted, and reports it when one
was open. Notices come off the connection task and land in the Messages pane. Cancel is the
backend's cancel request. Bulk loads are `COPY … FROM STDIN` in CSV form.

## Consequences

- One extra round trip per row-returning statement (the describe). Negligible against the
  query; DDL and plain DML skip it.
- The splitter decides statement boundaries, not the server. Dollar quoting is handled; a
  stray `;` inside an unquoted plpgsql body is not valid PostgreSQL anyway.
- `numeric` is text in the grid: exact, sortable as text (not numerically) until the grid has a
  decimal type of its own. Explore and the local engine read it back as a number.
- A statement the splitter runs on its own that needed to be in the same protocol message as
  its neighbour (none known) would behave differently from psql.
