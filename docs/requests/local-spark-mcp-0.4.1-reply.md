# Cobalt's reply to local-spark-mcp 0.4.1

*From Cobalt SQL Works, 2026-10-06. Follows `local-spark-mcp.md`, `local-spark-mcp-0.3.5-reply.md`
and `local-spark-mcp-0.4.0-reply.md`. Everything in 0.4.1 is adopted in Cobalt 0.7.3. One
correction to our 0.4.0 reply comes first, because it changes what you should spend time on.*

## Correction: the interrupt acknowledgement lag was ours

Finding 1 of our 0.4.0 reply said the `interrupt` reply arrived 10 s or more after the jobs were
cancelled. It did not. Cobalt binds its listeners non-blocking for the accept loop and, on
Windows, an accepted socket inherits that mode; our control-socket read therefore returned
`WouldBlock` at once and we logged it as a timeout. The data socket had the same inheritance and
was busy-polling. Both accepted sockets are now switched back to blocking, and measured on the
same box with 0.4.1:

```
cobalt: interrupt → interrupting after 0.0 s (spark jobs cancelled in 0.02s)
acknowledgement after 0.031 s: {"interrupted": true, "state": "interrupting",
  "detail": "spark jobs cancelled in 0.02s", "method": "run_code", "elapsed_s": 8.6}
```

So there is no Windows-side acknowledgement problem to chase. The second half of that finding
stood — the py4j `reentrant call` errors on the interrupted cell's stderr came from pyspark's
SIGINT handler, as you diagnosed — and 0.4.1's handler replacement fixed it: an interrupted
cell's output here is now just what it printed plus "Interrupted (Spark jobs cancelled)". Sorry
for the detour.

## Adopted

- **Pinned `v0.4.1`.** Protocol 2 as before; the runtime manager upgrades in place (26 s here).
- **Host tokens (item 5b).** Cobalt's loopback endpoint answers `GET /token?scope=<scope>` with
  the same `X-Token-Secret`: `https://storage.azure.com/.default` (and no scope) → the OneLake
  storage token, `https://api.fabric.microsoft.com/.default` → the Fabric REST token, both minted
  silently from the signed-in account's refresh token. `https://vault.azure.net/.default` gets a
  404 with a one-line explanation (Cobalt holds no Key Vault consent), which surfaces in the call
  that needed it, as you intended.
- **Preload is yours again (items 5a, 6).** `init` carries `preload: [<default lakehouse>]` (or
  `["all"]`); the app-side OneLake listing and chunked `mount_tables` from our 0.3.5 reply are
  deleted. The Shadows window polls `preload_status` on the control socket every 3 s while it is
  open, so the bar moves while a cell runs.
- **`capture_result` (finding 3).** `run_code` goes out with `capture_result: true`; each
  `displays` entry maps onto the blob that follows and becomes a grid; for a `source: "result"`
  entry Cobalt drops the trailing `Out[n]:` repr from the cell's text. Our bootstrap display
  hook, the IPC files under the runtime folder and the marker lines are gone; the only helper
  Cobalt still installs is the `%%sql` runner, which splits statements and hands the last frame
  to your `display(df, limit)`. `init`'s `default_sql_limit` is the user's "Rows a Spark
  DataFrame brings back" setting. The legacy hook remains only for an environment not yet updated
  past 0.4.0, and the session log says so.
- **`healthcheck.protocol_version`** shows on the Spark runtime page.
- **`status` (ask 6)** is not used by the UI yet; it will feed the Stop button's tooltip.

Verified on Windows (Python 3.11, fabric-2.0, Spark 4.1.1), plain session: a bare
`spark.range(7).selectExpr(...)` → grid of 7 rows, `display(df, limit=3)` → 3 rows with the text
after it intact, a bare pandas frame → grid, a `%%sql` cell with two statements → grid from the
second, a bare `42` → `Out[n]: 42` kept as text; streamed output still arrives per line; a
`spark.range(10**13)` aggregate interrupted cleanly with the session intact and the next cell
running normally.

## Live output from the schema-enabled test lakehouse

Workspace "Fabric test", lakehouse `test` (both layouts: `Tables/{sales_import, r2e_stream,
cobalt_nb_writethrough}` at the top level, `Tables/dbo/{cobalt_export_schema, cobalt_export_test,
publicholidays, r2e_dialog, r2e_stream2}`). Windows, Python 3.11, fabric-2.0, Spark 4.1.1,
`write_mode: sandbox`, `preload: ["test"]`, all tokens from Cobalt's endpoint (1 request served).

**What works.** Discovery and the catalog wiring: `info.lakehouse_schemas = {"test": ["dbo"]}`,
`spark.conf.get("spark.sql.catalog.test")` → `ch.fs.OneLakeSchemaCatalog`, `SHOW NAMESPACES IN
test` → `['dbo']`, `spark.catalog.listCatalogs()` → `spark_catalog, test`, `SHOW TABLES IN
test.dbo` runs (0 rows, see below). The preload listed all 8 tables over the host token in 5.6 s.

**What fails — every table, both layouts, the same error:**

```
SELECT COUNT(*) AS n FROM test.sales_import
spark.table("test.dbo.publicholidays")
SELECT ... FROM test.dbo.publicholidays
DESCRIBE EXTENDED test.dbo.publicholidays
spark.table("spark_catalog.test.sales_import")
→ AnalysisException: [UNSUPPORTED_DATASOURCE_FOR_DIRECT_QUERY]
  Unsupported data source type for direct query on files: delta SQLSTATE: 0A000; line 1 pos 0

preload_status.lakehouses.test.errors:
  cobalt_nb_writethrough / r2e_stream / sales_import →
      [UNSUPPORTED_DATASOURCE_FOR_DIRECT_QUERY] ... delta
  dbo/cobalt_export_schema, dbo/cobalt_export_test, dbo/publicholidays, ... →
      [TABLE_OR_VIEW_NOT_FOUND] The table or view `test__dbo`.`<t>` cannot be found
```

**Why (from a diagnostic cell in the same session):**

```
spark.sql.extensions            = io.delta.sql.DeltaSparkSessionExtension   (set, and live)
spark.sql.catalog.spark_catalog = ch.fs.OneLakeCatalog
spark.jars.packages             = io.delta:delta-spark_4.1_2.13:4.2.0,org.apache.hadoop:hadoop-azure:3.4.1
current catalog: test   current database: dbo      ← the V2 catalog is the session's current catalog

spark.read.format("delta").load("abfss://…/Tables/sales_import").count()  → 20000   (Delta is fine)
spark.sql("SELECT COUNT(*) FROM delta.`abfss://…/Tables/sales_import`")   → UNSUPPORTED_DATASOURCE_FOR_DIRECT_QUERY
spark.table("spark_catalog.test.sales_import")                             → UNSUPPORTED_DATASOURCE_FOR_DIRECT_QUERY
SHOW TABLES IN spark_catalog.test__dbo                                     → []
```

With the current catalog set to `test`, a two-part `delta.`<path>`` identifier is resolved as
`test.delta.<path>` — a table named `<path>` in namespace `delta` of the V2 catalog — so Delta's
analyzer rule for path tables never sees it and Spark's generic direct-query check rejects
`delta`. Every materialization path that goes through `delta.`…`` SQL (the shallow clone of a
first touch, the preload's mounts, and therefore the `test__dbo` session database the schema
catalog delegates to) fails for as long as the current catalog is not `spark_catalog`. That is
also why the 0.4.0 session, which never changed the current catalog, mounted these same tables.

Two ways out, either is fine for Cobalt: leave `spark_catalog` current at start (make `USE test`
the user's choice, and have `OneLakeSchemaCatalog` resolve its tables without depending on the
session's current catalog), or materialize through the DataFrame/Delta API
(`spark.read.format("delta").load(path)` / `DeltaTable.forPath`) and `spark_catalog.`-qualified
identifiers instead of `delta.`path`` SQL, so the current catalog cannot matter. A regression
test with a non-`spark_catalog` current catalog would have caught this.

**The plain-layout lakehouse works end to end.** `test_no_schema` (new, empty, no schemas):
`info.lakehouse_schemas` has no entry for it, no catalog is registered, databases are
`default, test, test__dbo, test_no_schema`; preload finished `done 0/0`; `SHOW TABLES IN
test_no_schema` → 0 rows; `spark.range(5)....saveAsTable("test_no_schema.cobalt_sandbox_t1")`
followed by a bare `spark.table(...)` → a 5-row grid; `SHOW TABLES` → 1 row; the Shadows window
lists `(test_no_schema, cobalt_sandbox_t1, written)`; OneLake untouched (sandbox).

## Asks

0. **The schema catalog and the current catalog** (above) — this one blocks every table of a
   schema-enabled lakehouse, so it is the one to take first. Cobalt ships 0.7.3 on 0.4.1 with a
   note that schema-enabled lakehouses need the next worker release.
1. **`status` for the Stop button.** Already shipped as asked; no change needed. If `cell` could
   also carry the active Spark job's description (`spark.jobGroup`/`callSite.short`), the
   tooltip could say *what* is running, not only for how long.
2. **Blob-free `displays` for small frames** is not needed; the blob path is fine. No ask.
3. **PyPI**: a far-future item on Cobalt's side; the git-tag pin stays. Nothing for you.
