# File databases: SQLite as a first-class connection, the local engine as the bridge, and what follows

*Status: approved and built. Slices A–D shipped in 0.13.0 (2026-10-28) with the ten §6 decisions
taken as recommended; slice E's DuckDB half shipped in 0.14.0 (2026-10-30) behind the same
file-connection shape, with the engine loaded at run time rather than linked in (D011, revising
§3.11's build-cost premise); slice E's PostgreSQL half shipped in 0.15.0 (2026-10-30) as a
network engine behind the same engine picker and driver shape (D013: the simple query protocol,
statement by statement, each described first). The slate is complete.
Written for the founder's alignment on 2026-10-28. Picks up §5.9 of the product design
("after SQL Server: DuckDB and SQLite, then PostgreSQL") and D002's V2 engines list, and takes
stock of what the app has become since: the local engine, file tables, scratch tables, Explore
over any table, editor groups. Decisions for the founder are in §6; slices in §5.*

## 1. The idea in one paragraph

Cobalt already lets you drop a Parquet, CSV or Delta folder on the window and query it in place.
A SQLite file looks like the same kind of thing, but it is not: it has tables with declared
types, indexes, views, triggers, foreign keys, transactions, and it is written to as often as
it is read. Treating it as a read-only file table would waste most of what people use SQLite
for. The proposal is to make a SQLite database a **connection**: it appears in the Servers
tree, its objects are browsable, its SQL runs through the same session machinery as SQL Server
(results grid, history, snapshots, dashboards, notebooks, Explore), with its own dialect in the
editor and `EXPLAIN QUERY PLAN` where the plan viewer would be. The drop, the file picker and a
"New database" all end there. Then the parts that make it more than "another engine": any
SQLite table as a local table, so it joins results, Parquet and scratch tables in the local
engine; results, imports and exports going *into* SQLite, including a built-in **scratch
database** that gives every user a writable place to keep things without setting anything up;
and **Edit Data**, the staged-edits grid the product design promised, built first where it is
safest and most natural. DuckDB and PostgreSQL follow the same path afterwards; the decision on
when is in §6.

## 2. What exists that this builds on (taking stock, 0.12.1)

- **The driver trait is engine-agnostic.** `cobalt_driver::Driver::connect` yields a
  `Box<dyn Connection>`: `execute` streams `StreamItem`s (result set start with `ColumnInfo`s,
  Arrow `RecordBatch`es, rows affected, messages, done), plus the catalog surface
  (`list_databases / schemas / objects / columns / parameters / indexes / keys`, `load_catalog`,
  `script`, `table_stats`, `distinct_values`) and `cancel_handle`. One implementation exists
  (`mssql` over tiberius-ng). `EngineKind` + `Capabilities` (plans, procedures, transactions,
  read-only, multiple databases, …) already gate UI per engine (Fabric endpoints are read-only,
  Synapse has no `SET STATISTICS`).
- **Everything above the driver is engine-blind**: the session actor, tabs, the results grid,
  history, snapshots, Import (file → table through a connection), Save results as table,
  Explore over SQL tables (T-SQL emitter), dashboards, notebooks' SQL cells.
- **Profiles assume a server**: `ConnectionProfile { server, port, database, auth, … }` with
  `AuthMethod::{SqlLogin, EntraInteractive, …}`; the connection dialog is SQL Server shaped.
- **Dialects**: `Dialect::{TSql, Spark, Dax, Local}` in cobalt-sql (lexer, keywords, functions,
  quoting, completion, formatter); the Explore emitter has `SqlDialect::{DataFusion, TSql}`.
- **The local engine** (`cobalt-local`, DataFusion): results, files, Delta, OneLake and
  Parquet scratch tables as tables; Diff; Explore; `CREATE TABLE … AS`.
- **Drops**: `drop::classify` knows query files, notebooks, plans, data files and Delta folders.
- **rusqlite 0.40 (bundled, chrono, serde_json) is already a workspace dependency**: the
  store (profiles, history, snapshots) is SQLite. No new native build for the engine itself.
- **Exports**: CSV, TSV, JSON, JSON Lines, XML, Markdown, Excel, Parquet, Arrow IPC, Delta.

## 3. Product shape

### 3.1 A SQLite database is a connection

Open one three ways: drop a `.sqlite`, `.sqlite3`, `.db`, `.db3` (or any file whose first 16
bytes are SQLite's header) on the window; *Open SQLite database…* on the Servers tree's `+`
menu and the File menu; *New SQLite database…* there too (creates an empty file). All three
make a **file connection**: a profile whose engine is SQLite and whose "server" is the path. It
appears in the Servers tree with a file icon and the file's name, under an automatic **Files**
group, and opens a query tab connected to it. A file connection is remembered like any profile
(recent connections on the welcome page, the library) and can be renamed, coloured, grouped,
marked read-only guard, or removed. The connection dialog grows an **engine** choice at the
top: SQL Server (today's form) or SQLite (a path, a *Read-only* switch, and a *Create if
missing* switch). A file that is not writable opens read-only and says so in the toolbar chip.

### 3.2 The editor speaks SQLite

A new `Dialect::Sqlite`: SQLite's keywords and its functions (core, date and time, `json_*`,
window functions, math), `PRAGMA` names with their arguments, `"name"` quoting (SQLite accepts
`[name]` and `` `name` `` too, and the lexer tolerates both), statements split on `;` (no
`GO`), the formatter's SQLite rules (no `TOP`, `LIMIT` placement, `||` concatenation). The
status bar's language chip says *SQLite*. Completion comes from the catalog (§3.3). The T-SQL
sugar is not applied here: the dialect is SQLite's own.

**Plans.** *Est. plan* runs `EXPLAIN QUERY PLAN` and shows its tree (SCAN / SEARCH USING
INDEX / USE TEMP B-TREE…) in the plan pane as text with indentation, the way Local tabs show
the engine's plan. *Actual plan* is off (SQLite has no equivalent). *Parse* uses
`sqlite3_prepare` without stepping.

### 3.3 The object explorer, from `sqlite_schema`

Under the connection: **Tables**, **Views**, **Indexes**, **Triggers** (from `sqlite_schema`),
and **Attached** for `ATTACH DATABASE` members (`PRAGMA database_list`), each with its own
objects. A table expands to columns (`PRAGMA table_xinfo`: declared type, `NOT NULL`, default,
primary key, generated), keys (`PRAGMA foreign_key_list`) and indexes (`PRAGMA index_list` /
`index_info`); hover shows the row count (a lazy `SELECT count(*)`, cached per run) and the
size estimate from `page_count × page_size` for the file. *Select Top 1000 Rows* writes
`LIMIT 1000`; *Script as* CREATE returns the stored `sql` verbatim (SQLite keeps it), DROP and
SELECT are generated; the completion catalog is the same walk. Explore (§3.6) and Query locally
(§3.5) are on every table's menu.

### 3.4 Results: dynamic types, honestly

SQLite types values, not columns. A column of declared affinity INTEGER may hold text. The
grid's Arrow columns need one type, so: the driver types each result column by the **declared
affinity** of the column when the result is a plain column reference (INTEGER → Int64, REAL →
Float64, TEXT → Utf8, BLOB → Binary, NUMERIC and untyped → Utf8), and by the **first batch's
storage classes** for expressions. A value whose storage class does not fit is coerced when it
parses (`'12'` into an Int64 column) and otherwise becomes NULL, and the Messages pane says how
many values in which column were set aside and why, so nothing is silently wrong. Dates stay
whatever SQLite stored (text, Julian real or epoch integer); the column header's hover names
the declared type. BLOBs show as hex with a length, and the cell viewer shows them whole.
Everything else in the grid is unchanged: sort, filter, totals, chart, copy, export, Save as
table, Query locally, Explore, Diff.

### 3.5 The bridge: SQLite tables in the local engine

*Query locally* on a SQLite table registers it as a local table (`<file>_<table>`, renamable)
read on every scan through rusqlite with projection pushed down, so it joins results, Parquet,
Delta, OneLake and scratch tables in a Local tab, Diffs against a result (yesterday's export
against today's table), and Explores through the local emitter. The Local sidebar lists it
under a new **Databases** group with the file path. This is the "file-table" experience the
founder first reached for, available on demand without giving up the connection.

### 3.6 Writing into SQLite

- **Save results as table** targets any connected profile; SQLite is one now: `CREATE TABLE`
  from the Arrow schema (affinities chosen from the Arrow types) and batched `INSERT` in one
  transaction, with the existing dialog's new / existing / replace choices.
- **Import** (file → table) to a SQLite connection the same way; a CSV becomes a real table
  with declared types, ready to index and edit.
- **Export to SQLite** (`Format::Sqlite`): a result set, or every result set of a run, written
  as tables into a new or existing `.sqlite` file. With Import's source = a server table, this
  is "take this table with me": a SQL Server or Fabric table snapshotted into a local file.
- **The scratch database.** Cobalt creates `scratch.sqlite` in the app data folder the first
  time it is needed and lists it in the Servers tree as **Scratch** (a built-in file connection
  that cannot be removed, only emptied). Save results as table defaults to it when the tab has
  no writable connection; Import offers it; Local tabs can `Query locally` its tables. The
  Parquet scratch tables of the local engine stay as they are for the one-click "keep this
  result" case; the scratch database is the place for tables you will write to, index, and
  come back to.

### 3.7 Explore over SQLite

The emitter gets `SqlDialect::Sqlite`: `LIMIT` for Top N, `LIKE` (SQLite's is
case-insensitive for ASCII; `ESCAPE '\'`), `datetime('now', '-30 days')` for relative dates,
and subtotals as a `UNION ALL` of the grouped query and its rollup levels with explicit
`IsSubtotal` columns, since SQLite has no `GROUP BY ROLLUP`. The distinct-value picker and
everything else is the same code path as SQL tables.

### 3.8 Edit Data, staged

The product design's §5.7, built first on SQLite where every table has a rowid or a declared
key and the whole database is yours. On a table opened with *Edit Data* (tree menu) or on a
result grid that is a single-table `SELECT` with the key columns present, cells become
editable: double-click or F2 to edit, Tab to move, a row inserts at the bottom, Delete marks a
row. Nothing is written as you type: a **Pending changes** strip shows the count and opens the
list of statements Cobalt will run (`UPDATE … WHERE rowid = …`, `INSERT`, `DELETE`), **Save**
runs them in one transaction and re-runs the query, **Discard** drops them, and the read-only
guard asks first. The same model, with keys from the catalog, is what SQL Server gets next;
this slate ships the grid, the staging, and the SQLite writer.

### 3.9 Drops, files and discovery

The drop overlay says *Open as a SQLite database* (by header, not extension, so a `.db` that
is something else is left alone) and offers *Query a table locally* on hover with modifiers
off; the Files sidebar shows SQLite files with a database icon and *Open database* / *Query
locally*; the welcome page's recent connections include file connections. A file that is
already open as a connection is activated, not opened twice; a file that moved shows the usual
"connection failed" with the path and a *Locate…* button.

### 3.10 Agent

`add_profile {engine: "sqlite", path, read_only?, create?}`, `open_database {path}`,
`new_database {path}`; `state` tabs and library entries carry `engine`; `export {format:
"sqlite", path, table?}`; `edit {action: set|insert|delete|save|discard|pending, row, col,
value}` for Edit Data; everything else (tree, run, results, explore, local tables) is unchanged
because the engine is behind the same trait.

### 3.11 What comes after, and why not now

- **DuckDB** is the natural sibling: a file database with an analytics engine and a friendly
  dialect, and `.duckdb` files people already have. The `duckdb` crate bundles libduckdb: a
  long C++ build on every cold compile and tens of megabytes of binary, for an engine whose
  file-analytics role the local engine already fills (Parquet, CSV, Delta, OneLake, joins, in
  process). It fits behind the same `EngineKind::DuckDb` and the same file-connection UI with a
  DuckDB dialect; the right time is after SQLite proves the path and the build cost is weighed
  on its own (§6.6).
- **PostgreSQL** is a network engine with its own wire protocol (`tokio-postgres`), auth
  stories and dialect; D002 puts it after DuckDB. Nothing in this slate is specific to files
  except the profile's path field, so it inherits the engine picker and the driver shape.
  *Done in 0.15.0:* `EngineFamily::Postgres` on profiles (server, port, database, SQL login),
  the driver in `cobalt-driver/src/postgres` (D013), `Dialect::Postgres` with dollar quoting,
  the tree by schema, writes through `COPY`, Explore in the local engine's dialect, Edit Data
  with the primary key from the catalog snapshot.

## 4. Architecture

**Driver.** `cobalt-driver` gains `sqlite/` behind `Driver`. rusqlite is synchronous, so each
connection owns a dedicated thread: the async `Connection` methods send a command over a
channel and await the reply; `execute` streams rows from `Statement::query` in chunks of a few
thousand into Arrow builders typed per §3.4, each chunk a `StreamItem::Rows`; `cancel_handle`
wraps `rusqlite::InterruptHandle` (`sqlite3_interrupt`). `PRAGMA busy_timeout` is set so a
writer in another process does not fail reads instantly; `journal_mode` is left as the file has
it. `EngineKind::Sqlite` with capabilities: no procedures, functions or sequences; triggers and
transactions yes; multiple databases = attached databases; no `SET` statements; estimated plans
yes (text), actual plans no.

**Profiles.** `ConnectionProfile.engine: EngineFamily` (`Tds` by serde default, so older
libraries load unchanged) and `AuthMethod::None` for file connections; `server` holds the path
for a file connection and the host for the rest. The library's tree groups file connections
under **Files** automatically unless the user groups them. The session actor picks the driver
by `engine`.

**Dialect.** `Dialect::Sqlite` in cobalt-sql: keyword and function tables, the PRAGMA list, the
statement splitter already handling `;`, quoting rules in `quote_for`, formatter cases for
`LIMIT` and `||`. The catalog for completion comes from the driver's `load_catalog`.

**Explore.** `SqlDialect::Sqlite` in `cobalt-local::explore`: LIMIT, LIKE ESCAPE, `datetime`
arithmetic, subtotals by UNION ALL; the T-SQL and DataFusion paths untouched.

**Bridge.** `cobalt-local` gets `SqliteTableProvider` (rusqlite, projection pushdown, the
same chunking as the driver) registered by `register_sqlite_table(path, table)`;
`TableSource::Sqlite { path, table }` in the registry; the Local sidebar's Databases group.

**Writes.** `cobalt-export::Format::Sqlite` writes tables from Arrow batches (affinity from
Arrow type, batched inserts in a transaction). Save results as table and Import reach SQLite
through the connection path with a per-engine DDL generator (`CREATE TABLE` affinities). The
scratch database is a profile the app creates at startup when missing, flagged built-in.

**Edit Data.** A `GridEdits` layer on the result view: a map of (row, column) → new value,
inserted rows, deleted rows; statements generated against the table's key (rowid, declared
PK, or the result's key columns); the Pending changes strip; Save runs the statements as one
batch through the connection (`BEGIN … COMMIT`, rolled back on the first error with the error
shown) and re-runs the query; the grid draws edited cells and deleted rows with tints the way
Diff does. Engine-specific parts are the key discovery and the SQL text, both behind a small
trait that the T-SQL implementation fills in later.

**Alternatives considered.** *SQLite as a DataFusion table only*: no DDL, DML, indexes,
triggers, transactions, Edit Data; rejected as the primary path, kept as the bridge. *sqlx*:
async, but a second SQLite build and a different API from the store's; rusqlite is already
here. *DuckDB now*: §3.11.

## 5. Slices

- **A. The connection.** Driver, engine family on profiles, the connection dialog's engine
  choice, Open / New database, drops by header, the Servers tree with Files group and the
  object explorer, `Dialect::Sqlite` with completion and formatting, `EXPLAIN QUERY PLAN`,
  history and snapshots, the agent verbs. Verified against a generated sample database.
- **B. Writing.** Save results as table and Import into SQLite, Export to SQLite, the scratch
  database, "take this table with me".
- **C. The bridge.** SQLite tables as local tables, the Local sidebar's Databases group,
  Explore over SQLite (the emitter dialect), Diff between a SQLite table and anything.
- **D. Edit Data.** The staged-edits grid on SQLite tables and single-table results, Pending
  changes, Save and Discard, the agent's `edit` verb; the key-and-SQL trait left ready for
  T-SQL.
- **E (the founder's call, §6.6). DuckDB** behind the same file-connection shape (0.14.0),
  then **PostgreSQL** behind the same engine picker (0.15.0).

## 6. Decisions for the founder (recommendations in bold)

1. **A SQLite database is a connection, with "Query locally" as the file-table view on
   demand**, rather than a file table that happens to come from SQLite.
2. **rusqlite on a thread per connection**, not sqlx: it is already in the workspace, and the
   store and the engine share one SQLite build.
3. **Result columns typed by declared affinity, misfits coerced or set to NULL with a count in
   Messages**, rather than everything as text. Honest and fast; the message keeps it honest.
4. **A built-in scratch database** in the app data folder, listed in the Servers tree, the
   default target for Save results as table when the tab's connection cannot take it.
5. **Edit Data ships in this slate, on SQLite**, with the staged-edits model that SQL Server
   reuses next; not a SQLite-only grid.
6. **DuckDB waits for its own decision after slice A ships.** The build and binary cost is
   real and the file-analytics role is already covered in process; `.duckdb` files people hand
   around are the argument for it, and that argument is better made once the file-connection
   path exists.
7. **PostgreSQL stays after DuckDB**, as D002 ordered.
8. **Files are recognised by header, not extension**; `.db` is ambiguous.
9. **Attached databases appear as databases** (the toolbar's database dropdown, the tree).
10. **Explore subtotals on SQLite by UNION ALL**, not hidden.

## 7. Risks

- **Dynamic typing** (§3.4) will surprise someone with a text value in an INTEGER column; the
  Messages note and the header hover are the mitigation, and the all-text fallback is one
  setting away if needed.
- **Locked files**: another process writing the database; `busy_timeout` and a clear message.
  Encrypted databases (SQLCipher) are out of scope and say so.
- **Big tables and `count(*)`**: lazy and cached; the tree never counts without a hover.
- **Edit Data correctness**: `WITHOUT ROWID` tables need a declared key; views and joins are
  not editable; every statement names the key columns; Save is one transaction.
- **Profile model change**: `engine` and `AuthMethod::None` are additive with serde defaults;
  older builds reading a newer library ignore file connections' engine and would try to connect
  to the path as a host and fail politely.
- **Drop ambiguity**: header detection, never extension alone.

## 8. Before execution

Nothing from the founder's side. Verification needs a generated sample database (a script
makes one with typed and mistyped values, indexes, a view, a trigger, foreign keys), the Docker
SQL Server for the "take this table with me" path, and the agent scripts.
