# Local query: SQL over results, files and the lakehouse, in-process — design (2026-10-21)

*Status: approved by the founder 2026-10-21 on every decision in §6 (release held until all slices are in). Slices A and B built 2026-10-21 (`cobalt-local`, `localq.rs`, the Local tab, Local root, Query this result, T-SQL sugar; verified live against the Docker SQL Server; streaming-grid performance measured unchanged). Originally: design for the founder's alignment. Pulls forward the V2 bet from
`product_design.md` §5.1 ("the result engine is the product": local re-query, result diff,
pivot and totals, streaming everything), §5.5 (chained notebook cells) and D006 (DataFusion as
the planned local engine), and takes stock of what the app has become since those were written.*

## 1. The idea in one paragraph

Every result Cobalt shows is already a typed Arrow dataset with disk spill (D006). This slate
adds a SQL engine over those datasets, in-process, with nothing to install: **DataFusion**, the
Rust query engine Sail is built on, embedded in the app. A result set becomes a table the moment
it lands; so does a Parquet, CSV, JSON-lines or Arrow file dropped on the window, a Delta folder,
and (later) a lakehouse table read straight from OneLake with the signed-in token. A **Local
query** tab runs SQL over any of them with the same grid, exports, charts, profiler and
dashboard pins as every other tab, and because two result tabs are two tables, it joins a
production result to a development one, diffs two runs of the same query, or compares Fabric to
on-prem, with zero server round trips. On top of the engine sit three things people reach for
without wanting to write SQL: **Diff** (two results, a key, added / removed / changed rows),
**Explore over local tables** (the pivot builder from semantic models, now with a SQL emitter),
and **chained notebook cells** (a cell's result is a table the next cell queries). Memory stays
inside the existing budget: the engine reads result chunks through the same spill-aware store,
and DataFusion's own operators spill when a sort or join outgrows the pool.

## 2. What exists that this builds on (taking stock, 0.10.4)

| Piece | Where | What it gives this slate |
|---|---|---|
| Arrow result store with spill, views, display cache | `cobalt-results` (`ResultSet`, `raw_batches`, `view_batches`, `from_batches`, `append`, `MemoryBudget`) | The table provider's source, and the sink for local results: a local query's output is just another `ResultSet` fed batch by batch, so the grid, filters, sort, find, totals, charts, profiler and Run to File work unchanged |
| Every exporter reads `Source::Set` / `Source::Stream` | `cobalt-export`, `ops::run_to_export` | Run to File and Export from a local tab for free |
| Save as table into a connection | `ops::open_results_to_table` | "Push this local result to a server" exists already |
| Column profiler | `ui/results/profile.rs` | Works on local results; also the natural "click a bar to filter" entry into a local query |
| Pivot (local matrix) and the Explore builder | `cobalt-results::pivot`, `cobalt_core::explore::ExploreSpec` (engine-agnostic by design, one `ExploreSource` so far), `cobalt-tabular::explore::to_dax` | A second emitter, SQL, makes Explore work on any local table |
| Drop files on the window | `drop.rs` (`DropAction::Import`, `DeltaFolder`) | Data files and Delta folders get a better landing: a local table, no connection needed |
| Delta log reader | `delta_schema.rs` (`fields_from_commit`, checkpoint parsing, parquet part listing) | Reused for the Delta table provider fallback (and delta-rs's provider does the full job) |
| delta-rs pinned to Arrow 58 | `cobalt-export-delta` (deltalake 0.32.4) | delta-rs 0.32 pins **DataFusion 53.1 on Arrow 58**: the engine version is decided for us and its `datafusion` feature gives a Delta `TableProvider` with deletion vectors, column mapping and time travel |
| Local Spark (Sail) | `cobalt-runtime`, `sparkq.rs` | The same engine family one process away; a later "send to the Spark session" action, not the primary path (needs the runtime installed, a Python worker and a socket hop) |
| Spark SQL lexer, completion by dialect | `cobalt-sql` (`Dialect::{TSql, Spark, Dax}`) | A `Dialect::Local` (DataFusion's PostgreSQL-flavoured SQL) reuses the Spark lexer's backtick and `"` quoting; function names come from the engine at runtime |
| Tab kinds, the + menu, the model chooser pattern | `shell.rs`, `tabularq.rs` | A `local` tab kind with its own toolbar, welcome pane and status bar branch, following the Spark / DAX tab pattern |
| Notebooks with SQL cells and cached results | `cobalt-notebook`, `notebook.rs` | Cell outputs are `ResultSet`s: chaining is registering them |
| Dashboards: tile sources | `TileSource::{Sql, SemanticModel}` | A `Local` source over files (stable) rather than over result tabs (ephemeral) |
| Agent | `agent.rs` | `local_query`, `local_tables`, `diff` verbs on the same pattern as `dax_query` / `explore` |

What is new, in one line each: the engine crate, the result-set table provider, the Local query
tab, the Local tables panel, file and Delta tables, Diff, the SQL emitter for Explore, local
notebook cells, and the OneLake reader.

## 3. Product shape

### 3.1 Local tables: what can be queried

A **local table** is anything the engine can scan. The app keeps a registry (the **Local tables**
panel, a root in the Servers tree named *Local*, and the catalog of a Local query tab):

- **Results.** Every result set of every open tab (SQL, Spark SQL, DAX, Explore, notebook cells)
  is registered as it arrives, with a name derived from the tab title and the set number:
  `sqlquery_1`, `sqlquery_1_2`, `dax_3`, `explore_7`. Names are stable for the tab's life, unique,
  lower-case, and renamable (right-click › Rename, or `-- @name prod_orders` as the first line of
  the query that produced it). Re-running a tab replaces the table's data, so a local query
  re-run sees the new rows. Closing the tab unregisters it (the local tab that referenced it
  says so on its next run). The most recent result is also `_` (underscore), the way shells do.
- **Files.** Parquet, CSV / TSV (header and delimiter sniffed as Import does today), JSON lines,
  Arrow IPC, and Delta folders (through delta-rs's provider, so deletion vectors and column
  mapping work; time travel by `VERSION AS OF` later). Opened by drop, by File › Open, by the
  Files sidebar (right-click › Query), or by `CREATE EXTERNAL TABLE` in a local tab. A file table
  is named after the file (`sales`, `orders_2026`) and reads from disk on every scan, so a 10 GB
  Parquet file costs nothing until queried and streams through the grid like a server result.
- **Scratch tables** (slice D). `CREATE TABLE scratch.x AS SELECT …` or *Save as local table*
  persists a result as Parquet under the app data folder, and it is there next session: the
  place to keep "prod as of Tuesday" for a diff on Friday.
- **Lakehouse tables** (slice D). A Delta table under a Fabric lakehouse, read from OneLake with
  the signed-in user's token through `object_store`, no SQL endpoint, no Spark: right-click a
  lakehouse table in the Fabric panel › *Query locally*. Reads only; large tables are a
  conscious act (the row estimate from the Delta log is shown first).

### 3.2 The Local query tab

A query tab of kind `local` (the + menu, File › New local query, "A click on + opens", and the
`Ctrl+N` family): the same editor with `Dialect::Local` highlighting and completion (table
names, columns, DataFusion's functions straight from the engine's registry, with signatures),
F5 / Ctrl+Enter, Cancel (the engine's task is aborted; partial rows stay), Est. plan (DataFusion's
logical and physical plan as text under the plan tab, as Spark tabs show Sail's), Parse, Format,
Run to File, Export, history. Results land in the normal results area, batch by batch, under the
row cap with "fetch more" (the engine stream is paused, not dropped). The toolbar's connection
chip reads *Local* with the tables in scope and the memory in use; the status bar says `Local ·
DataFusion 53`. The getting-started pane lists the local tables with a *Query* link on each,
starting points (`SELECT * FROM _ LIMIT 100`, a join template, a diff template), and the
dialect notes people trip on (no `TOP`, `LIMIT`; `"quoted"` or `` `quoted` `` names, not
`[brackets]`; `||` concatenates; `ILIKE`; `date_trunc`; `arrow_typeof`).

**T-SQL sugar.** Most people arriving at this tab think in T-SQL. A thin rewrite before the
engine handles the obvious: `[name]` → `"name"`, `SELECT TOP (n)` → `… LIMIT n`, `ISNULL(a, b)` →
`COALESCE(a, b)`, `GETDATE()` → `now()`, `LEN(` → `length(`, `+` between two string literals →
`||`. The rewrite is shown in the Messages pane ("rewrote TOP 10 as LIMIT 10") so nobody is
surprised, and it is a setting (on by default). Anything beyond that is the engine's job.

**Where results go.** A local query's result is itself a local table (`local_2`), so queries
chain: filter in one tab, aggregate in the next.

### 3.3 "Query this result" and the join / diff shortcuts

On every result set (toolbar and context menu): **Query this result** opens a Local query tab
with `SELECT * FROM sqlquery_1 LIMIT 100` and the table's columns in the completion. **Join
with…** picks another local table and prefills `SELECT … FROM a JOIN b ON a.<key> = b.<key>`
with a key guessed from matching column names. **Diff with…** opens the Diff view (§3.4).
**Save as local table** persists it (slice D). The Local tables panel has the same actions per
row, plus *Reveal* (activates the producing tab) and *Rename*.

### 3.4 Diff

Two local tables, one or more key columns (guessed from shared names; `id`-like columns first),
and optionally the columns to compare. The engine runs a `FULL OUTER JOIN` on the keys and
classifies each row as **added**, **removed**, **changed** or **same** (`IS DISTINCT FROM`
across the compared columns, nulls equal), and the result lands in a **Diff view**: the grid with
rows tinted (green / red / amber), changed cells shown as `old → new` with the cell colored,
counts in the header (`12 added · 3 removed · 118 changed · 40,211 same`), a filter chip row
(Added / Removed / Changed / Same), and the usual export. The generated SQL is one click away
("Open as SQL"), so a diff becomes a saved query. Diff has its own entry points: the result set
menu, the Local tables panel, History (*Run again and diff*: re-run a past query against its
connection and diff the new result with the old one, the "did it change since Tuesday" question),
and the agent.

### 3.5 Explore over local tables

The Explore builder (fields on the left, Rows / Columns / Values / Filters wells, drag and drop,
Top N, subtotals, matrix or flat, filters with distinct-value pickers, save as
`.cobalt-explore.json`, pin to a dashboard) gets a second source, `ExploreSource::Local { table }`,
and a SQL emitter (`GROUP BY` with the matrix built by `cobalt-results::pivot` as today;
subtotals through `GROUPING SETS`; Top N through `ORDER BY … LIMIT`; filters as `WHERE`; the
distinct-value picker through `SELECT DISTINCT`). *Explore this result* on any result set, and
*Explore* on any file or lakehouse table. Measures are the aggregations (Sum, Count, Count
distinct, Average, Min, Max) as today for columns; a local table has no model measures.

### 3.6 Files land as tables

A data file dropped on the window (today: the Import dialog, which needs a connection) opens
as a **local table tab**: a Local query tab bound to the file with `SELECT * FROM "sales" LIMIT
1000` already run, the file's schema in the catalog, and a toolbar with *Import into a
connection…* (the existing Import dialog, now fed from a local table), *Explore*, *Profile* and
*Save as Parquet / Delta* (export). A Delta folder dropped on the window opens the same way
(today it opens a Spark SQL tab, which needs the runtime); the Spark route stays as *Open in
Spark* for what the local reader cannot do. The Files sidebar gets *Query* on data files and
Delta folders.

### 3.7 Chained notebook cells

A notebook gains a **Local SQL** cell language (`cobalt-notebook` already carries the language
per cell; the engine runs it the way the kernel runs Spark cells). Every cell's result set is
registered as `cell_<n>` and `_` for the next cell, whatever engine produced it: a SQL cell on
Fabric, a Spark cell, a DAX cell, a local cell. A runbook can pull from two servers in two cells
and reconcile them in a third, entirely local, with the evidence cached in the document. (D009's
Python cells keep running on the Spark runtime; a Python cell can still read a local table through
the Arrow Flight bridge later.)

### 3.8 Dashboards

A tile source `Local { sql }` for queries over **files and scratch tables** only: those are
stable across sessions, result tabs are not. A tile over a lakehouse table (slice D) is the
no-endpoint lakehouse dashboard §5.3 of the product design wanted.

### 3.9 Memory, size, cancel

- **Memory.** The engine's memory pool is the existing budget (`Settings › Advanced › memory
  budget`); DataFusion's sort, aggregate and join operators spill to the spill directory when the
  pool runs dry, and result chunks are read through the store's LRU, so a join of two spilled
  results works and stays within budget. The status bar shows the engine's pool use during a run.
- **Streaming.** The engine produces a `RecordBatch` stream; the tab appends batches into a
  `ResultSet` under the row cap exactly as the TDS driver does, so the grid shows rows while the
  query runs and a 50-million-row file scan never materialises.
- **Cancel.** The engine's task is a tokio task on the session runtime; Cancel aborts it and the
  partial result stays with the "cancelled" state.
- **Binary.** DataFusion adds roughly 15–20 MB to the binary and two to three minutes to a cold
  build; it lives in its own crate (`cobalt-local`) so incremental builds of the app do not pay
  for it, and `deltalake`'s `datafusion` feature is turned on in `cobalt-export-delta` (same
  DataFusion version, so one copy).

### 3.10 Agent

`local_query {text, run?, name?}` (a Local query tab, returning the result like `dax_query`),
`local_tables {action: list | rename | register_file | unregister, …}`, `diff {a, b, keys, columns?}`
(returns the counts and the first rows of each class), `explore {action: new, table}` on a local
source. Labels: `tab Local_1`, `local table <name>`, `diff filter added|removed|changed|same`.

## 4. Architecture

```
crates/cobalt-local        the engine: DataFusion 53 SessionContext per app, the memory pool and
                           disk manager wired to MemoryBudget / spill dir, the registry of tables
                           (results, files, scratch, lakehouse), ResultSetProvider (TableProvider
                           over cobalt-results::ResultSet: schema from ColumnInfo, scan = the
                           chunks in view order through the spill LRU, projection pushdown),
                           file tables via ListingTable (parquet/csv/json/arrow), Delta via
                           deltalake's provider, the T-SQL sugar rewriter, EXPLAIN text, the
                           function catalog for completion, Diff (SQL generation + row classes),
                           the SQL emitter for ExploreSpec
crates/cobalt-sql          Dialect::Local (lexer: backticks and double quotes, DataFusion keywords),
                           completion from a LocalCatalog
crates/cobalt-core         ExploreSource::Local, TileSource::Local, settings.local
crates/cobalt-app          localq.rs (tab kind: new_tab / run / cancel / explain, registering
                           results as they arrive in apply_event, the panel state), ui/local.rs
                           (Local tables panel, the Local root in Servers, diff view, welcome
                           pane, toolbar chip), drop.rs (data files → local table tab), notebook
                           Local SQL cells, agent verbs
```

**Registering results.** `AppState::apply_event` already receives every `ResultSet` the session
creates; it hands the `Arc<ResultSet>` to the registry with the derived name. The provider holds
the `Arc`, so a closed tab's table lives until the last local query over it finishes, and the
registry drops it after. A result set still streaming is queryable as far as it has arrived
(the provider snapshots the chunk count at scan start), and the tab says "ran over 40,000 rows
while 120,000 were still arriving".

**Running.** `localq::run` builds the SQL (sugar applied), asks the engine for a stream on the
session runtime, and feeds batches to a fresh `ResultSet` through the same `Event::Batch` path
the drivers use, so cancel, row cap and the grid's streaming updates are the existing code. A
local run has no connection, so the run view's messages carry the rewrite notes, the plan on
request, and the elapsed time.

**Diff.** A `diff.rs` in `cobalt-local`: given two tables, keys and compare columns, it emits the
SQL and reads the result into a `ResultSet` with a leading `__change` column (`added` /
`removed` / `changed` / `same`) and, for changed rows, both values per compared column (`col`,
`col__was`); the Diff view is the grid with a row painter keyed on `__change` and a cell painter
for `__was` pairs. Counts come from a second aggregate query over the same join.

**Explore SQL.** `cobalt-local::explore::to_sql(spec, schema) -> SqlQuery` mirrors
`cobalt-tabular::explore::to_dax`: group columns, aggregated values with `COUNT(DISTINCT …)` and
friends, `GROUPING SETS` for subtotals (the existing matrix builder reads the subtotal columns),
`WHERE` from the filter specs (`IN`, `BETWEEN`, `ILIKE`, relative dates through `now() -
INTERVAL`), `ORDER BY … LIMIT` for Top N.

**Lakehouse reads (slice D).** delta-rs's `DeltaTableBuilder` with `object_store`'s Azure
backend and a bearer token from the Fabric sign-in (the OneLake export path already does this
for writes), registered as a `TableProvider`; the token refresh is the resolver's job as today.

## 5. Slices

- **A — the engine and the tab** (≈ the size of semantic models slice A): `cobalt-local` with
  the result-set provider, memory pool and spill, the Local query tab with completion / plan /
  cancel / history, result registration with names and `_`, *Query this result*, the Local
  tables panel and the Servers root, the T-SQL sugar, the agent verbs, the welcome pane. The
  demo: run a query on prod and one on dev, join them locally.
- **B — files and Delta**: file tables (Parquet, CSV, JSON lines, Arrow), Delta folders through
  delta-rs's provider, the local table tab as the landing for dropped data files and Delta
  folders (Import into a connection from it), *Query* in the Files sidebar, `CREATE EXTERNAL
  TABLE`. The demo: drop a 2 GB Parquet, query it, profile it, push a filtered slice to Fabric.
- **C — Diff and Explore**: the Diff view with its entry points (result sets, the panel, History's
  *Run again and diff*), the SQL emitter and `ExploreSource::Local` so Explore works on any
  local table, *Explore this result*. The demo: diff two runs of a query; pivot a CSV without
  writing SQL.
- **D — persistence and the lakehouse**: scratch tables, chained notebook cells with Local SQL
  cells, lakehouse tables read from OneLake, `TileSource::Local`, *Send to the Spark session*
  (register a local table as a temp view in the Sail / Spark session through Arrow IPC).

Each slice ships on its own; A alone is the feature people have asked for since the product
design.

## 6. Decisions for the founder (recommendations in bold)

1. **Engine: in-process DataFusion 53** (pinned by delta-rs 0.32 on our Arrow 58; nothing to
   install, instant, the same engine family as Sail) rather than routing results through the Sail
   session. Sail stays the Spark engine; a bridge between the two comes in D. The cost is binary
   size and build time (§3.9), isolated in one crate.
2. **Table names: derived from the tab title plus set number, renamable, plus `_` for the latest
   result.** The alternative, `r1 … rN`, is shorter to type but means nothing a day later.
3. **Dropped data files open as a local table tab** (query, profile, explore immediately; *Import
   into a connection* is a button there) instead of the Import dialog. The Import dialog stays
   reachable from File and from the local tab.
4. **T-SQL sugar on by default, visible in Messages**, limited to the six rewrites listed; not a
   T-SQL emulation.
5. **Diff is its own view** (tinted rows, old → new cells, class filters, counts) rather than a
   plain result with a status column; the SQL behind it is one click away.
6. **Explore gets a SQL emitter** so the same builder serves semantic models and local tables;
   no second pivot UI.
7. **Lakehouse reads from OneLake in-process are slice D**, behind an explicit action with a row
   estimate; sandboxed Spark remains the way to write.
8. **Dialect: DataFusion's as is** (PostgreSQL-flavoured) with the sugar; no attempt to run
   T-SQL locally.

## 7. Risks

- **Arrow version drift.** DataFusion, delta-rs and our Arrow must agree; today they do (58).
  The quarterly Arrow bump (D006) becomes a three-crate bump; the lockfile pin ritual covers it.
- **Type edges.** TDS types we map to Arrow decimals, `datetimeoffset` (timestamp with zone),
  `uniqueidentifier` (fixed binary 16, shown as text) and `sql_variant` (text) need engine-side
  casts the dialect notes explain; the result provider exposes exactly the Arrow types the grid
  already has, so what the grid shows is what SQL sees.
- **Names collide** when two tabs share a title (`SQLQuery_1` twice after a restore); the
  registry suffixes `_b` and the panel shows which is which.
- **Memory under a bad join.** A cross join of two large results spills to disk rather than
  taking the app down, but can fill the temp volume; the budget setting and a "stop at N rows"
  guard (the row cap) bound the damage, and the status bar shows pool use.
- **Build time** for contributors and CI: two to three minutes more per cold build; the Linux
  Docker build is already at ten.

## 8. Before execution

Nothing from the founder's side this time: no registrations, no endpoints. Live verification
needs only the Docker SQL Server and the test files; slice D's lakehouse read uses the "Fabric
test" workspace under the standing rules.
