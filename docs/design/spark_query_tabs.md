# Spark SQL query tabs — proposal (2026-10-08)

*Status: proposed; founder's call pending (see §6). Written after the founder asked, playing
with 0.8.1: "what if the query view could connect to or spawn a Spark session and run Spark SQL
against the workspace / lakehouse of my choice?"*

## 1. The idea in one paragraph

A query tab (editor, Run, grid, Messages, Run to File, history) whose *connection* is the local
Spark session rather than a TDS endpoint. You pick a lakehouse the way you pick a server, type
Spark SQL, press F5, and the rows land in the ordinary grid. Notebooks stay the place for
Python and narrative; the query tab is the place for "just run this query against the lakehouse"
without a notebook, a cell, or `%%sql`. It reuses everything the notebook kernel already does
(session, contexts, lakehouse binding, Arrow results, interrupt) and everything the query tab
already does (grid, exports, history, formatting).

## 2. What exists that this builds on

- The kernel actor (`kernel.rs`): one account-scoped Spark session, per-notebook **contexts**
  (own namespace and SparkSession in the shared JVM, created with a default lakehouse), Arrow
  replies, interrupt on the control socket, session lifecycle and early start (slate 4).
- `__cobalt_sql(text, limit)`: splits statements, runs them, shows the last frame (fixed on
  2026-10-08 to exist in every context).
- The Lakehouse pane: tables and Files of the active notebook's lakehouse, Cobalt-side
  OneLake listings, "insert a read cell".
- Query tabs: `EditorTab` with `ConnState`, `RunView` (result sets as `Arc<ResultSet>`, messages,
  elapsed, history id), Run to File, exports from the grid, the Messages pane, `ExecOptions`.
  A notebook cell's `RunView` is the same struct, so a Spark reply already becomes a `RunView`
  (`notebook.rs` pump: `result_set_from_ipc` → `ResultSetView`).

## 3. Product shape

**Connecting.** The Servers sidebar gets a root node **Local Spark** under the server groups.
Its children are lakehouses: the ones bound to open notebooks, pinned Fabric lakehouses, and
recent ones (store), plus "Choose a lakehouse…" (the Fabric picker already used by
`set_lakehouse`). Double-click a lakehouse → a new tab `SparkSQL_1 · test`, connected to the
session (starting it if needed, with the same chip states as a notebook: *starting*, *ready*,
*busy*). The tab toolbar's connection button reads `Local Spark · test (Fabric test)`; the
database dropdown lists the lakehouse's schemas for a schema-enabled lakehouse (`dbo`, …) and
is the lakehouse name otherwise. "Disconnect" drops the tab's context; the session itself
follows the lifecycle setting (idle stop at 60 min by default), exactly like notebooks.

**Running.** F5 / Run (all, selection, current statement) sends the text to the tab's own
context `qt-<tab>` through the kernel actor with a new helper `__cobalt_sql_all(text, limit)`:
every statement runs in order and **every** result-producing statement comes back as its own
result set (T-SQL tab semantics, not the notebook's last-frame rule). Statements that return
nothing (`CREATE`, `INSERT`, `MERGE`, `USE`) produce a message line ("Statement 2 completed in
1.4 s"; Spark gives no affected-row count — a later upstream ask). Errors show the analysis
message as one error line (the `SparkSqlError` compaction from the `%%sql` fix) and the run
ends as *failed*, the remaining statements are skipped. Cancel = interrupt on the control
socket. Elapsed and the status bar work as today. History records the text with source
`Local Spark (fabric-2.0) · test`.

**Results.** The grid, filters, sort, viewer, copy, exports and "Save as table" are the normal
ones. The row cap is Settings → Notebooks → "Rows a Spark DataFrame brings back"; the pane
header says "first 10,000 rows" when truncated. There is no fetch-more: Spark delivers a
collected frame, so "more" means re-running with a higher cap (one-click "Run again without
the cap" in the banner). **Run to File** runs the statement with no cap and hands the Arrow
frame to the existing export writers (CSV/Parquet/Delta/OneLake); streaming row batches is a
later upstream ask (`run_sql` with batches), not needed to ship.

**Plans.** "Est. plan" runs `EXPLAIN EXTENDED` and shows the text plan in a Plan tab (text,
monospace, collapsible sections: Parsed / Analyzed / Optimized / Physical); "Actual plan" is
hidden for Spark tabs; Parse runs `EXPLAIN` (analysis only, no job). The showplan viewer is
SQL Server only.

**Editing.** Spark SQL syntax: the T-SQL lexer covers keywords and strings well enough; add
backtick identifiers and Spark functions to the highlighter and completion. Completion offers
the lakehouse's tables and columns from the Lakehouse pane's listing (schema-aware), which the
pane already fetches. The pane follows the active Spark tab like it follows a notebook; its
"Insert read cell" becomes "Insert SELECT" on a query tab.

**Format** reuses the T-SQL formatter (keywords, line breaks); backticks pass through.

## 4. Architecture

- `ConnState` gains a variant `Spark { lakehouse: Option<LakehouseRef>, context: String }` or,
  cleaner, `EditorTab.engine: Engine::{Tds, Spark}` with the Spark binding stored like a
  notebook's (`NotebookFabric` → reuse as `SparkBinding`). `ops::run` branches on it: TDS tabs
  go to the session actor as today, Spark tabs build a `RunReq` (code = helper call, context,
  context lakehouse, job description = first line) and push it to the kernel; the reply is
  matched by `(tab, run)` in a `pump_spark_tabs` beside the notebook pump.
- One helper per statement result: `__cobalt_sql_all` displays each frame (`display(df,
  limit)`) and prints a marker per non-result statement, which the outcome parser turns into
  messages. Nothing new is needed from the worker for slice A.
- The Servers tree node is UI over the store (`spark_recent_lakehouses`) plus Fabric state.
- History source, tab snapshot (hot exit restores the binding, not the session) and the agent
  verbs (`connect {spark: {workspace, lakehouse}}`, `state` carries `engine`).

## 5. Slices

- **A — core (first release).** Local Spark root with lakehouses; Spark tab with
  connect / run / cancel / results / messages / errors / history / exports / Run to File
  (collected); status bar and tab title; hot-exit restore. ≈ 3–4 days.
- **B — editing comfort.** Completion from the pane, Spark keywords/functions, backticks,
  text EXPLAIN plan tab, "Insert SELECT" from the pane, "Open in a Spark tab" from a
  notebook SQL cell. ≈ 2 days.
- **C — upstream asks (later).** `run_sql` streaming batches (true Run to File and fetch
  more), affected-row counts for DML, two-part names `schema.table` against the default
  schema-enabled lakehouse (also hits `%%sql` today — see `docs/requests/local-spark-mcp-0.6.5-request.md`).

## 6. Decisions needed from the founder

1. **Entry point.** A *Local Spark* root in the Servers tree listing lakehouses
   (recommended: people think "connect to X"), or a "Spark" switch in the tab's connection
   picker. Both can exist; the tree is the one to build first.
2. **Result semantics.** Every statement's rows as its own result set (recommended, matches
   the T-SQL tab) vs the notebook's last-frame rule.
3. **Scope of the first slice**: A alone, or A + the text EXPLAIN plan from B.
4. Go / no-go, and whether it lands before or after the remaining notebook items
   (parameters cell, `%pip` reporting, charts).
