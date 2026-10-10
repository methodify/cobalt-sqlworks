# LakeSail as a second Spark engine — proposal (2026-10-09)

*Status: slice A built and verified live 2026-10-09 (install, Spark SQL tab, notebook cells,
engine switch both ways); research and live probes in §2. The founder asked for an
experimental second runtime kind — [Sail](https://github.com/lakehq/sail), LakeSail's Rust
"drop-in Spark replacement" with OneLake and Delta built in — that users can pick for notebooks
and Spark SQL tabs, and switch back from, without replacing the local-spark-mcp engine.*

## 1. The idea in one paragraph

Today every Spark feature in Cobalt (notebooks, `%%sql`, Spark SQL query tabs, the Lakehouse pane)
runs on one **engine**: a JVM Spark driven by the local-spark-mcp worker. This adds a second
engine, **Sail**, behind the same session: a Python worker of our own that embeds Sail's Spark
Connect server (`pysail`, one wheel, no JDK, 80 ms to start) and speaks local-spark-mcp's socket
protocol to Cobalt, so the kernel actor, contexts, Arrow results, streamed SQL, interrupt, Spark
tabs and notebooks work unchanged. The engine is a **session property** like the write mode: pick
it on the Spark menu, the kernel picker or the Spark runtime page; switching restarts the session.
Sail reads and writes OneLake directly with the signed-in user's token through the token endpoint
Cobalt already runs. What Sail cannot do (shallow-clone sandboxes, the JVM-only Delta features,
Fabric's `notebookutils` surface) is stated in the chip and the pane rather than papered over.

## 2. What Sail is, and what we verified

**Facts** (docs.lakesail.com, GitHub, PyPI; 2026-10-09): Sail 0.7.2 (2026-09-29), Apache-2.0,
3.4k stars, releases every 2–4 weeks, a seed-funded company. Rust + DataFusion + Arrow; a Spark
Connect gRPC server, so PySpark code runs unchanged against `sc://127.0.0.1:<port>`; supports
PySpark clients 3.5.9 / 4.0.4 / 4.1.3 / 4.2.0. `pip install pysail` gives abi3 wheels for
win_amd64, linux x86_64/aarch64 and macOS (66 MB; the server lives in `_native.pyd` and can be
started in-process: `pysail.spark.SparkConnectServer(ip, port).start(background=True)`). Also an
Arrow Flight SQL server. Delta: reader 1–3 / writer 1–7, deletion vectors, column mapping,
time travel, UPDATE / DELETE / MERGE, schema evolution; no VACUUM / OPTIMIZE / RESTORE / CDF /
`DeltaTable` API. Catalogs: memory (per session), Iceberg REST, Unity, Glue, HMS, and a
**OneLake catalog** over Fabric's table API. Storage via `object_store`: `abfss://`,
`https://onelake.dfs.fabric.microsoft.com/...`; credentials from env vars only, including
object_store's **Fabric token service** provider (a GET to a URL that returns a bearer token,
refreshed before expiry). Config is `SAIL_*` env vars. Python/pandas/Arrow UDFs run in-process.
`EXPLAIN` prints DataFusion plans under `== Physical Plan ==` headers. Errors come back as
`AnalysisException` / `IllegalArgumentException` with a message and no stack.

**Probed live on this machine** (pysail 0.7.2, Cobalt's uv-managed Python 3.11, workspace
"Fabric test", lakehouses `test` and `test_no_schema`; every probe table removed afterwards):

| Probe | Result |
|---|---|
| In-process server start / first session | 0.08 s / 2.6 s (PySpark import dominates); RSS 33 MB idle, 186 MB after collecting 69,557 rows |
| Local Delta: CREATE, INSERT, UPDATE, DELETE, MERGE, VERSION AS OF, temp views, UDF | all work; `DESCRIBE HISTORY`, `SET`, `USE x` (needs `USE DATABASE x`) do not |
| Path read `delta.` + abfss OneLake path with `AZURE_STORAGE_TOKEN` | count 69,557 in 1.5 s; workspace/lakehouse name and GUID forms both work; the `https://onelake.dfs...` form does not |
| Token service: `AZURE_FABRIC_TOKEN_SERVICE_URL` + `AZURE_FABRIC_SESSION_TOKEN` + `AZURE_ALLOW_HTTP=true` | Sail GETs `…/token?resource=https://storage.azure.com` with `x-ms-partner-token: <secret>`, expects the raw JWT, refreshes from `exp` — the shape of Cobalt's endpoint |
| OneLake catalog (`type="onelake"`, `url="Fabric test/test.Lakehouse"`, `api="delta"`) | SHOW DATABASES / TABLES correct for schema and plain lakehouses; `SELECT` works only for `publicholidays`: every table with an `integer`, `long` or `decimal(p,s)` column fails with "Failed to get table: unknown error: status code 200 OK" (Fabric's table API returns lowercase Spark type names that Sail's Unity client rejects); CREATE TABLE through the catalog is 405 (Fabric has no staging tables); DROP TABLE through the catalog **deletes the OneLake folder** |
| Two catalogs in `catalog.list` without `catalog.default_catalog` | the session dies silently ("session … is not running"); fine with a default set |
| External tables by path: `CREATE TABLE db.t USING delta LOCATION 'abfss://…'` | 0.3–1.2 s each (schema inference); then SELECT, INSERT, UPDATE, DELETE, MERGE, INSERT OVERWRITE, VERSION AS OF all work against OneLake, DML returns a `count` frame; DROP TABLE keeps the data |
| Session isolation (`builder.remote(url).create()` twice) | temp views, current database, conf and memory-catalog tables are per session |
| `interruptAll()` on a running query | the query fails within the second; the session survives |
| Client Arrow batches (`client._execute_and_fetch_as_iterator(req, {})`) | 9 batches / 69,557 rows in 0.8 s; `toLocalIterator` 100k rows in 0.26 s |
| PySpark 4.1.3 client (Fabric Runtime 2.0's line) | same results as 4.2.0 |

Three gaps go upstream (§7): the OneLake catalog type names, the silent multi-catalog death, and
`USE <db>` / `DESCRIBE HISTORY`. None blocks the build: Cobalt registers lakehouse tables by path
itself, which is what the JVM worker's `OneLakeCatalog` does lazily today.

## 3. Product shape

**Choosing the engine.** Settings › Notebooks & Spark › Spark runtime gets an **Engine** row:
*Local Spark (JVM) — Fabric Runtime 2.0 / 1.3* or *LakeSail (experimental) — Sail 0.7.2,
no Java*. Each engine has its own install state and buttons on that page (the Sail install is one
uv environment, ~250 MB, no JDK, no Ivy warm). The same choice appears where sessions are
managed: the **Spark menu › Engine** submenu, the notebook **kernel picker** (two entries:
"Local Spark (fabric-2.0)" and "LakeSail 0.7.2 · experimental") and the Spark tab's session chip.
Picking the other engine while a session runs asks: *Restart the session on LakeSail? Variables
and temp views are lost.* The choice is remembered (`spark.engine`), so the next session, the early
start and the smoke test use it.

**What the user sees.** Every Spark label names the engine: status bar "Sail 0.7.2 · up 4m",
chip "LakeSail (0.7.2) · ready", history source "LakeSail (0.7.2) · test", session log title. The
kernel picker's hover and the getting-started pane carry one line of truth: *LakeSail runs Spark
SQL and the PySpark DataFrame API without Java; no RDDs, no Scala/Java UDFs, no shadow clones.*

**Lakehouses.** Binding is unchanged: a notebook or tab binds a workspace / lakehouse; tables
resolve as `table`, `schema.table`, `lakehouse.schema.table` (`lakehouse.table` on plain
lakehouses) and `Files/` is not relative (use `abfss://` or the pane's "Insert read cell", which
writes the full path on Sail). The Lakehouse pane lists tables and Files as today (Cobalt-side
listings); the shadow actions (Clone now / Discard / Rewind) and the Shadows window are hidden
on Sail, with the line *LakeSail has no sandbox: this session is read-only / writes go to
OneLake*.

**Write modes on Sail.** `readonly` (default) and `writethrough`. There is no sandbox: Sail has
no shallow clone, and a Python-side clone (delta-rs writing a local log whose `add` paths point at
OneLake) is a later slice. In `readonly` the worker refuses statements that would write a
lakehouse table (INSERT / UPDATE / DELETE / MERGE / DROP / ALTER / CREATE … LOCATION on an
`abfss://` path) with a message naming the chip; Python writes through `df.write` are caught by
the storage layer in a later slice.

**Running.** Unchanged: F5 on a Spark tab streams Arrow batches per statement with the cap and
"Run again without the cap"; DML shows "(n rows affected)" from Sail's `count` frame; Est. plan
shows Sail's DataFusion plan under the same section headers; Parse analyzes without running;
Cancel interrupts. Notebooks: `%%sql`, `display(df)`, a bare trailing DataFrame, streamed
output, Stop — same code paths, the bootstrap runs in every context.

## 4. Architecture

**Engine kind.** `SparkSettings.engine: "pyspark" | "sail"` (default `pyspark`). The runtime
manifest gains a `sail` section (`pysail` version, `pyspark_client` version pinned to the
default profile's Spark line, the Python version). `RuntimeDirs::env_dir("sail")` is the
environment; no JDK, Ivy or jars. `RuntimeStatus` gains `engine` and inspects only the parts that
engine needs (`is_ready()` ignores the JDK on Sail). `Installed` gains `sail_version` and
`sail_pyspark`. `install::provision` takes the engine from the plan: Sail = uv, Python,
`uv pip install pysail==<v> pyspark-client==<v> "pandas<3" ipython httpx` plus the worker file,
then a smoke start instead of the Ivy warm. `healthcheck` runs the Sail worker's own
`--healthcheck` (imports, versions).

**The Sail worker** (`crates/cobalt-runtime/python/cobalt_sail_worker.py`, embedded with
`include_str!` and written into the environment at install; ~900 lines). Same framing as
local-spark-mcp (4-byte length + JSON; `binary: [n…]` + raw Arrow IPC streams; `event` frames
`stdout` / `stderr` / `batch`), same launch (`-m cobalt_sail_worker --port N --control-port M`),
same methods where they make sense:

| Method | On Sail |
|---|---|
| `init` | sets `SAIL_*` and `AZURE_*` env (token service URL + secret from Cobalt's endpoint, `AZURE_ALLOW_HTTP`, the catalog list), starts `SparkConnectServer` in-process on a random port, creates the root session, records lakehouses; result = `info()` with `engine: "sail"`, `engine_version`, `spark_version` (client), `features` |
| `create_context` / `drop_context` | one Spark Connect session per context (`builder.remote(url).create()`), one IPython namespace per context (same swap technique as the JVM worker), `USE CATALOG` / `USE DATABASE` for the default lakehouse |
| `register_lakehouse` | records name / id / workspace and restarts the embedded server with the lakehouse as one more Unity catalog (Cobalt's endpoint answers for it) |
| `run_code` | IPython `run_cell` with the `_Tee` streaming; `display(df)` and a captured bare DataFrame via `df.limit(n+1).toArrow()`; the namespace has `spark`, `F`, `T`, `Window`, `display` and a `notebookutils` stub that raises a clear error (slice B brings the shim) |
| `run_sql` | `spark.sql` (analysis forced so an unknown table fails with the statement); `stream: true` iterates the client's Arrow batches and emits `batch` events; DML returns `metrics {affected_rows, source: "result"}` from Sail's `count` frame |
| `list_tables` | from the DFS listing (cached per lakehouse, refreshed on demand) |
| `interrupt` (control) | `session.interruptAll()` for the context + `interrupt_main()` for Python |
| `status` (control) | `cell_running`, elapsed, context — no job list |
| `info`, `ping`, `shutdown` | as today; `shutdown` stops the server |
| `shadow_status`, `preload`, `mount_table`, `sync_files`, `mirror_status` … | not advertised (`features` says so); Cobalt hides the UI that needs them |

`features` = `arrow streaming interrupt capture_result job_description register_lakehouse
contexts sql_stream commit_metrics catalog_listing engine_sail`. Fatal detection: a dead
server (the in-process server stopping, `UNAVAILABLE` gRPC status) marks the reply `fatal`, and
Cobalt respawns the worker as it does for a dead JVM.

**The lakehouse catalog (built 2026-10-09, replacing the first slice's mounting).** The first
build registered tables lazily as external Delta tables (`CREATE TABLE … LOCATION`, ~0.75 s
each, serial, once per context), which did not survive a lakehouse with hundreds of tables.
Measured alternatives: parallel registration (5 tables 3.9 s → 1.5 s) still pays per table per
context; explicit column lists do not skip the Delta-log read and must match the metadata
exactly. What shipped instead: `sail_catalog.rs`, a loopback Unity-Catalog-compatible endpoint
in Cobalt (next to the token endpoint) backed by Fabric's OneLake table API
(`onelake.table.fabric.microsoft.com/delta/<ws>/<lh>/api/2.1/unity-catalog`, the storage
token). Sail gets one `unity` catalog per lakehouse pointing at it. GET /schemas and /tables
are one Fabric call each (cached), /tables/{name} one call per table on first touch; the
endpoint normalises Spark type names to Unity's spelling (the bug in Sail's own OneLake
catalog), rewrites storage locations to the `abfss://` form by ids, refuses DELETE (Fabric
deletes the folder) and serves Unity's staging flow for CREATE in write-through mode only.
Verified live: SHOW DATABASES / TABLES, SELECT on every fixture table, DESCRIBE, EXPLAIN,
INSERT / UPDATE / DELETE / MERGE / INSERT OVERWRITE and time travel through the catalog; a
whole session costs five upstream calls. Plain lakehouses are `<lakehouse>.dbo.<table>`
(Fabric's own mapping); legacy top-level tables of a schema-enabled lakehouse appear as schemas
in Fabric's listing and stay path-only.

**Nested types and the Fabric package roster (2026-10-09).** Fabric's table API reports a
struct / array / map column as just `struct` / `array` / `map`; Sail's Unity client needs the
full type as `type_json` ("Struct type missing 'fields' array" otherwise). The endpoint now
reads the table's Delta log (`_delta_log/0000…N.json` `metaData.schemaString`, the same
heuristic as the completion catalog) on first touch and rewrites every column's type from it,
so nested, decimal and timestamp types reach Sail exactly. On the Cobalt side, nested Arrow
columns are rendered to text with Arrow's own formatter instead of `cast` (which has no
struct → text), which fixed the same table on the JVM engine. The Sail environment can also
carry a **Fabric package roster** (the Python packages of a Fabric runtime at Fabric's versions
as listed in Microsoft's `synapse-spark-runtime` repository, minus Fabric-only and GPU/Linux-only
wheels; setting `spark.sail_profile`); the install tries the roster as one resolution and falls
back to one package at a time, naming what would not install. Cobalt first carried its own
34-package list for fabric-2.0 and sent local-spark-mcp an advisory
(`docs/requests/local-spark-mcp-fabric-packages.md`); local-spark-mcp 0.8.0 answered with a
curated roster per profile in its `profiles.json` (56 for fabric-2.0, 57 for fabric-1.3, with the
source file and commit and an exclusion list), so since then the rosters come from the copy of
that file embedded in `cobalt-runtime` (`Manifest::rosters()`, which also supplies the profile
pins that `manifest.json` used to mirror by hand) and the same list serves both engines: the JVM
profile's environment gets it through `spark.profile_packages`. local-spark-mcp 0.8.1 added
per-platform fallbacks as data (`python_packages_fallbacks`: scipy on Python < 3.12), which Cobalt
applies per the environment's Python and reports as "platform fallback". One lesson from the
first live run: a roster pin can silently move a package the engine itself depends on (a resolver
does not re-check what it was not asked about) — Fabric 2.0's `protobuf==5.29.6` under
pyspark-client 4.1.3, whose generated code needs protobuf 6.33, stopped the Connect client
loading. `sail.reserved` in `manifest.json` names such packages (left out of the roster on Sail,
`Roster::without`), `sail.packages` carries the floor (`protobuf>=6.33,<7`), and the engine's
pins are re-asserted after every roster install (`reassert_sail_pins`). The JVM engine takes the
roster whole: local-spark-mcp validated it against its own pins, and a smoke test here confirmed.

**Tokens.** `onelake_tokens::TokenServer` accepts the secret in `x-ms-partner-token` as well as
`X-Token-Secret` (object_store's Fabric provider sends the former) and ignores the
`resource=` query (it only ever asks for storage). Nothing else changes: the token is the
Fabric account's, refreshed by the resolver, served on loopback with a per-start secret.

**Cobalt side.** `kernel::start` branches on the engine: the Sail path checks the Sail
environment, needs no jar or JDK, passes `onelake {endpoint, secret}`, `lakehouses`,
`default_lakehouse`, `write_mode`, `default_sql_limit` and nothing JVM-specific; the worker
module name comes from `WorkerConfig.module`. `KernelUi.engine` carries the kind for the labels;
`KernelState::label()`, `sparkq::session_label`, `kernel_log_window`, the status bar and the
history source read it. `kernel_fabric` maps `sandbox` to `readonly` on Sail. The Lakehouse pane
and the Shadows window check the engine. The kernel picker and the Spark menu offer both
engines; the restart consumer carries the chosen engine. The agent's `kernel` JSON and
`runtime {engine}` expose it; `settings` patches `spark.engine`.

**Files / notebookutils (slice B, built 2026-10-10).** The pure-Python halves of local-spark-mcp
(`lazy_files`, `files`, `host_credential`, `notebookutils_shim`) have no JVM dependency; the Sail
environment installs the base `local-spark-mcp` package (no `fabric-*` extra, so no
pyspark/delta-spark; `Manifest::sail_requirements`) and the worker's `_FilesMixin` wires the
same objects over the Sail engine: a `FilesMirror` on the same `<state>/lakehouses` folder the
JVM engine uses, `LazyFilesHooks` (lazy by default, `files_mode` from the settings),
`link_default` for `/lakehouse/default`, `HostTokenCredential` against Cobalt's token endpoint,
the `NotebookUtils` shim in `sys.modules` and the namespace (`fs` over `/lakehouse/...` and
`abfss://`, `runtime.context`, `notebook.exit` → a clean cell end, `credentials`,
`variableLibrary`; `notebook.run` raises: Local Spark only), and `mirror_status` / `sync_files`
/ `clear_mirror` for the pane. The worker advertises `files` and `notebookutils` in `features`;
the pane enables its Files actions on Sail from that. The DataFrame API gets the same write rules
as SQL by patching the Connect client's writer classes once per process: read-only refuses
`save` to an `abfss://` OneLake path, `saveAsTable` / `insertInto` / `writeTo` on a lakehouse
table; write-through turns `saveAsTable` on a lakehouse table into a Delta write at the table's
path plus a catalog refresh (Sail's catalog-managed create needs a Unity table id), and refuses
`writeTo(...).create()` with that hint. Spark-side `Files/` stays absolute (`abfss://`).

## 5. Slices

- **Slice A — the engine (0.9.0).** Manifest `sail` pins; `spark.engine` setting; runtime page
  Engine row with Sail install / smoke / remove; the Sail worker (init, contexts, run_code,
  run_sql streaming + metrics, automount, register_lakehouse, list_tables, interrupt, status,
  info, healthcheck); token endpoint header; `kernel::start` branch; labels everywhere; kernel
  picker / Spark menu / chip engine choice with restart; read-only and writethrough modes;
  shadows UI hidden on Sail; agent surface; alpha notes; verified live: Spark tab (select, cap,
  Run to File, DML counts, errors, plan, Parse, completion), notebook (PySpark cell, `%%sql`,
  display, Stop), switching engines both ways, hot-exit restore.
- **Slice B — parity (0.9.5).** Built: `notebookutils` + lazy Files on Sail (local-spark-mcp's
  modules over the Sail engine), the DataFrame write guard in readonly and the `saveAsTable`
  path rewrite in write-through, the pane's Files actions on Sail, `notebook.exit`. Left as
  notes: DESCRIBE HISTORY / SET are Sail's own gaps (upstream list); the Sail version bump is
  manifest-only already.
- **Slice C — later.** Sandbox on Sail (delta-rs shallow clone); native OneLake catalog mode once
  the type-name bug is fixed; Flight SQL straight from Rust for Spark SQL tabs (no Python in the
  path); session presets that bundle engine + write mode + libraries.
- **Backlog (founder, 2026-10-09): the Fabric table API for the JVM engine too.** The
  catalog endpoint built for Sail reads Fabric's table API (one call per schema, one per table,
  with types). The JVM worker's `OneLakeCatalog` still discovers tables by crawling OneLake's
  DFS, and Cobalt's completion catalog reads each table's Delta log. A shared, cached
  Cobalt-side reader over the table API could feed all three: the JVM worker (an upstream ask
  to local-spark-mcp: accept a host-supplied listing instead of `listOneLakeTables`), the
  Lakehouse pane, and completion. Not worth it: pointing Spark at the Unity endpoint through the
  Unity connector, which would give up the sandbox clones that are the JVM engine's point; the
  per-table clone (1.5–2 s, already parallel on preload) is that engine's floor regardless of
  the catalog.

## 6. Decisions for the founder (recommendations in bold)

1. Engine scope: **a session property, one engine at a time** (switch = restart) vs. two live
   sessions side by side. The latter needs `sessions: Vec<Session>`, which the slate deferred.
2. Default write mode on Sail: **readonly** (no sandbox exists) vs. writethrough.
3. Where Sail's pins live: **the embedded manifest** (bump = Cobalt release) vs. a user-editable
   version field (experimental runtimes move fast; a "try 0.7.3" box is cheap to add later).
4. Client line: **pyspark-client 4.1.3** (Fabric Runtime 2.0's Spark) vs. 4.2.0 (Sail's own test
   pin).

## 7. Upstream asks (LakeSail; `docs/requests/lakesail-0.7.2.md`)

1. OneLake catalog, `api="delta"`: Fabric's table API returns `type_name` values such as
   `integer`, `long`, `decimal(18,2)`, `timestamp_ntz` (lowercase, Spark names, `type_text` and
   `type_json` null); Sail rejects every such table with "Failed to get table: unknown error:
   status code 200 OK". Payloads attached.
2. `catalog.list` with two or more entries and no `catalog.default_catalog`: sessions die at
   creation with no message; an error at config load would save an hour.
3. `USE <db>` without the `DATABASE` keyword and `DESCRIBE HISTORY` (Spark syntax users paste).
4. (nice to have) a documented per-session way to add a catalog, so a session can attach a
   lakehouse without restarting the server.

## 8. Risks

- Sail is pre-1.0 and renames config keys between releases; the manifest pin and the worker's
  healthcheck keep Cobalt's side stable, and the engine is labelled experimental.
- No stack traces from Sail; the worker surfaces the one-line message and the statement.
- Memory is unbounded by default; `runtime.memory_pool` could be set from driver memory later.
- Catalog registration by path depends on OneLake listings (one DFS call per lakehouse, one
  schema-inference per first touch of a table); large lakehouses are fine because mounting is lazy.
