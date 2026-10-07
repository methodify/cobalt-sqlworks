# Requests for local-spark-mcp: sessions, contexts, Files (from Cobalt SQL Works)

*2026-10-06. Background: `docs/design/spark_sessions_slate.md` in the Cobalt repo. Cobalt is
moving from "a notebook starts and owns the worker" to "one account-scoped session that
notebooks attach to", the way Fabric's high-concurrency mode works. Everything below is
additive to protocol 2; the numbering continues the earlier request files.*

## 0. Contexts — one JVM, one isolated REPL per notebook (the keystone)

Sharing one namespace between notebooks is crosstalk: `df`, imports, `spark.conf.set`, `USE`,
temp views, UDFs and cached frames leak between them. Fabric's high-concurrency sessions
isolate each notebook in its own REPL context inside one Spark application; we would like the
worker to do the same.

- `create_context(id, default_lakehouse?, default_schema?)` → `{id}`. A context is its own
  Python namespace (a separate IPython shell, or an `exec` globals dict with IPython used only
  for formatting and tracebacks) plus its own `spark.newSession()`: isolated temp views, SQL
  conf, current database and UDF registry; shared `SparkContext`, catalog, clones, cached data,
  Ivy jars. `display`, `notebookutils`, `__cobalt_*` helpers are bound per context.
- `run_code` / `run_sql` take `context` (default: a `"default"` context created at `init`, so
  version-2 clients keep working). `interrupt` and `status` take an optional `context`;
  `status.cell` reports which context is running.
- `drop_context(id)` releases the namespace and the SparkSession; `info.contexts` lists them.
- Execution across contexts may stay sequential in a first version (the single request loop as
  today). Concurrent contexts — a thread per context, FAIR scheduler, one `run_code` in flight
  per context — can be a later step; the API should not preclude it.
- The default lakehouse of a context sets its current database (`spark_catalog.<lh>` or
  `<lh>__<default_schema>`) and what `Files/` means for it (see 3).

## 1. Attach a lakehouse or workspace after `init`

`register_lakehouse({name, id, workspace_id, schemas?, default_schema?, detect_schemas?})` and
`unregister_lakehouse(name)`, so a notebook from a workspace the session does not know can
attach without a restart. Same semantics as the `lakehouses` entries of `init` (session
database, schema catalog if schema-enabled, shadow dir). `info.lakehouses` reflects it.

## 2. `job_description` on `run_code` / `run_sql`

The cell's first line, set through `setJobDescription` around the call, so `status.cell.jobs`
and the Spark UI name the cell. Cobalt would rather not prepend code to the user's cell
(tracebacks keep their line numbers).

## 3. `files_mode: "lazy" | "mirror"` — Files without syncing the tree

Lakehouse `Files/` can be hundreds of GB; a mirror is never Cobalt's default. Proposed, with
`"lazy"` the default when a host endpoint is configured:

- Spark's relative `Files/…` resolves to the default lakehouse's `abfss://…/Files/…` directly
  (hadoop-azure, host token, streaming); nothing is mirrored by a Spark read or write.
- The Fabric path `/lakehouse/default/Files/…` (and `/lakehouse/<name>/Files/…`) is served by
  Python-level hooks, the way `notebookutils` is shimmed today: `builtins.open`,
  `os.listdir/scandir/stat`, `os.path.exists/isfile/isdir`, `pathlib.Path` methods. A single
  file is fetched into the mirror on first open; a directory listing comes from OneLake; writes
  land in the mirror and push only in writethrough. This covers pandas, csv, json, zip, PIL and
  most Python IO without pulling anything else.
- Native readers that bypass Python's `open` (DuckDB, Arrow `OSFile`, some ML loaders) keep
  needing a real local file: `sync_files(paths=[subtree])` stays the explicit pull, and the
  error a native reader gets for an unfetched path should say "pull the folder first".
- `mirror_status()` → per lakehouse: pulled subtrees, lazily fetched files, bytes;
  `clear_mirror(lakehouse?, paths?)`.
- `"mirror"` keeps today's behaviour for MCP users who want a full local copy.

## 4. Persisted clones, listed before use

With `persist_shadow: true`, a `shadow_status` (or `info.shadows`) that lists the persisted
clones and their first-touch versions *before* any table is touched, so a host can offer
"preload the tables I used last time" and show "cloned at <time>" next to each.

## 5. Idle signal

`status` already reports a running cell; an `idle_s` (seconds since the last cell or preload
finished) would let hosts implement an idle timeout without tracking it themselves. Minor.

---

Priorities from Cobalt's side: 0 first (it decides the shape of everything else), then 3, then
1; 2, 4 and 5 are small. Happy to test each against the `test` and `test_no_schema` lakehouses
as they land.
