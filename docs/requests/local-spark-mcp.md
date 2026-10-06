# Requests for local-spark-mcp (from Cobalt SQL Works)

Cobalt SQL Works (CSW) embeds local-spark-mcp as its PySpark engine: CSW spawns
`python -m local_spark_mcp.worker --port N` from a CSW-managed environment and speaks
the length-prefixed JSON socket protocol directly (the MCP/stdio layer is unused). The
items below would let CSW drop the workarounds it carries today. Pinned today:
`local-spark-mcp @ git+https://github.com/methodify/local-spark-mcp@v0.3.4`.

## 1. Machine-readable runtime manifest (needed first)

`src/local_spark_mcp/profiles.py` is the only source of truth for the version sets, and
it is Python. CSW currently mirrors it by hand in `crates/cobalt-runtime/src/manifest.rs`
(profiles `fabric-1.3` and `fabric-2.0`: pyspark, delta-spark, Python, Java majors and
preference order, Scala line, hadoop-azure, the Windows Python 3.11 rule from SPARK-53759).
Every change to `profiles.py` has to be copied over.

Ask: publish the same data as JSON, in two places:

- a static file in the repo at a stable path, e.g. `profiles.json` at the repo root
  (CSW fetches it at a tag: `https://raw.githubusercontent.com/methodify/local-spark-mcp/<tag>/profiles.json`);
- a CLI: `python -m local_spark_mcp.profiles --json` printing the same document from the
  installed package, so an installed environment can describe itself.

Suggested shape (one object per profile; keep the key names of `Profile`):

```json
{
  "schema": 1,
  "package": "local-spark-mcp",
  "version": "0.3.4",
  "default_profile": "fabric-1.3",
  "profiles": {
    "fabric-2.0": {
      "fabric_runtime": "2.0",
      "extra": "fabric-2.0",
      "pyspark": "4.1.1",
      "delta": "4.2.0",
      "python": "3.13",
      "python_windows": "3.11",
      "java_majors": [17, 21],
      "java_preferred": [21, 17],
      "scala": "2.13",
      "hadoop_azure": "3.4.1",
      "spark_major": 4
    }
  }
}
```

`python_windows` encodes the SPARK-53759 rule explicitly instead of leaving it to the
caller. If the rule is ever lifted (pyspark 4.1.2+ allowed), the manifest changes and CSW
follows without a release.

## 2. Arrow results

`run_sql` returns JSON rows (`SqlResult{columns, rows, ...}`), which loses types and is
slow past a few thousand rows. CSW shows DataFrames in an Arrow-native grid.

Ask: `run_sql_arrow` (and a `display(df)` hook in `run_code`) returning Arrow IPC stream
bytes, base64 in the JSON frame or as a second length-prefixed binary frame following the
JSON frame (preferred: `{"id", "ok", "result": {"arrow_bytes": N, ...}}` then N raw
bytes). `df.toArrow()` exists on Spark 4; on Spark 3.5 `df._collect_as_arrow()` works.

## 3. Interrupt

There is no way to cancel a running cell. Ask: a side channel (second socket, or a
`SIGINT`-style control message handled on a separate thread) that runs
`spark.sparkContext.cancelAllJobs()` and raises `KeyboardInterrupt` in the shell thread
(`PyThreadState_SetAsyncExc` / `_thread.interrupt_main()`), then reports the cell as
interrupted in `ExecResult`.

## 4. Publish to PyPI

`uv pip install "local-spark-mcp[fabric-2.0]==0.3.4"` would let CSW pin a version
without a git clone (which needs `git` on the user's machine; CSW cannot assume it).

## 5. Pre-warm command

The first `SparkSession` pulls Delta and hadoop-azure through Ivy. Ask: a
`python -m local_spark_mcp.warm` (or `--warm`) entry point that resolves the profile's
jars into a given `spark.jars.ivy` directory and exits, so an installer can do it with a
progress bar and the first notebook does not wait on Maven.

## 6. Streaming stdout

`run_code` returns stdout/stderr only when the cell finishes. For long cells CSW would
like interim frames (`{"id", "event": "stdout", "text": ...}`) before the final response.
Optional; the final-only form is acceptable for a first release.

## 7. Discard one shadow

`discard_shadow(only)` filters by state (`read` / `written`), not by name. A per-table variant
(`discard_shadow(table="lakehouse.table")`) would let a UI offer "Discard" next to one clone
without rewinding it first.

## 8. Extra Spark packages and jars

`build_spark` owns `spark.jars` (the catalog jar) and `spark.jars.packages` (through
`configure_spark_with_delta_pip`), so a host cannot add its own jars or Maven packages through
`extra_configs` without clobbering them. Ask: `init` parameters `extra_jars: list[str]` (merged
into `spark.jars`) and `extra_packages: list[str]` (appended to the delta helper's package list,
with Ivy resolving transitive dependencies). Cobalt currently puts user jars on
`spark.driver.extraClassPath` / `spark.executor.extraClassPath` and resolves Maven coordinates to
a single artifact itself.

## 9. Small things

- `info` could include `java_home`, `python`, `hadoop_home`, `ivy_dir` as resolved, so the
  host can show them without re-deriving.
- A `healthcheck` method that does not need a SparkSession (import check + versions +
  Java resolution verdict), for the host's runtime status page.


---

# Cobalt's reply to the 0.3.5 response (2026-10-06)

Thank you — 0.3.5 covers more than we asked. What Cobalt does with it:

- **Pinned `v0.3.5`.** `init` now passes `profile`; `fatal: true` already respawns the worker.
- **Item 8 replaces our workaround.** User jars go in `extra_jars` and Maven coordinates in
  `extra_packages`; Cobalt no longer puts jars on `extraClassPath` or downloads single artifacts
  from Maven Central. Transitive resolution through Ivy is exactly what we wanted.
- **Item 7 (per-table discard)** is wired to the Discard button in the Shadows window.
- **Item 9 (`healthcheck`)** runs on the Settings → Spark runtime page (`python -m
  local_spark_mcp.healthcheck --json`) and its problems/warnings show there; `info`'s new paths
  will go on the session log header.
- **Preload** is a per-notebook option ("Preload the default lakehouse's tables at session
  start") that passes `preload: [<default lakehouse>]`; the Shadows window polls `preload_status`
  and shows a progress bar. The MCP tool/config side we do not use.
- **Item 1 (`profiles.json`)**: Cobalt keeps an embedded copy for offline installs and will
  compare it against the tag's file in CI. `python_windows` for both profiles understood.
- **Item 5 (`warm`)**: our smoke test already starts a session through the protocol; we may switch
  the install step to `warm --ivy <dir> --java-home <jdk>` since it reports Ivy progress.

Answers to the open questions for 0.4.0:

1. **Interrupt before Arrow.** Cobalt already has Arrow results through a display hook that writes
   IPC files, so the visible gap today is Stop killing the session. Interrupt first, please.
2. **Second TCP socket** for the control channel, port advertised in the `init` reply. Our worker
   client is a simple request loop on one stream; a second socket keeps it untouched and lets the
   interrupt go out while a reply is pending. Suggested frame on that socket: `{"id", "method":
   "interrupt"}` → `{"id", "ok", "result": {"state": "interrupting"|"idle"}}`, and the pending
   `run_code` reply then carries `"interrupted": true` with whatever stdout was produced.
3. For the Arrow frame (protocol v2), a frame-type byte before the length is fine; please keep
   `protocol_version` in `init` so Cobalt can speak v1 to an older environment during the switch.
4. Streaming stdout events: the same socket as the reply is fine for us; we read frames until the
   one whose `id` matches the request and treat `event` frames as progress.

Found while adopting 0.3.5:

5. **Preload needs a credential the host does not give the worker.** The preload thread lists
   tables through `FabricAPIClient(credential=self._cred)` → `DefaultAzureCredential`, which fails
   in Cobalt's process model (tokens only reach the JVM through the HTTP token endpoint), so
   `preload: [...]` reported "preloaded 0 tables" and the credential chain's error text landed in
   the first user cell's output. Cobalt now drives the preload itself: it lists `Tables/` on
   OneLake with its own storage token and sends `mount_tables` in chunks of 8 so cells interleave.
   Two asks, either would let us hand this back to the worker: (a) `preload` accepting explicit
   table lists (`{"lakehouse": ["t1", "t2"]}`) so no REST call is needed in the worker, or (b) a
   token endpoint parameter for the Fabric API scope as well (the JVM already has one for
   storage), so `FabricAPIClient` can run on host-supplied tokens — that would also unblock
   `sync_files` and notebook discovery under a host. Also: a credential error inside the preload
   thread should not surface as a notice on an unrelated user cell.

6. **Schema-enabled lakehouses.** `FabricAPIClient.list_tables` uses
   `GET /workspaces/{ws}/lakehouses/{id}/tables`, which Fabric refuses for schema-enabled
   lakehouses (`400 UnsupportedOperationForSchemasEnabledLakehouse`) — so the worker's own
   preload fails there too (our test lakehouse is one). On storage such a lakehouse has
   `Tables/<schema>/<table>/_delta_log` (and may still have `Tables/<table>` for tables written
   by other tools; both coexist in ours). `OneLakeCatalog` only resolves `Tables/<table>`:
   `SHOW TABLES IN test` is empty, `test.dbo.publicholidays` fails with
   `REQUIRES_SINGLE_PART_NAMESPACE`, and no quoting reaches `dbo/publicholidays`. Ask: let the
   catalog list OneLake (`Tables/`, one level down for schema folders) instead of the REST
   endpoint, and expose schema tables — either as multi-part namespaces (`test.dbo.publicholidays`,
   the Fabric notebook spelling) or at least as `test.dbo_publicholidays`. Cobalt's preload
   mounts the top-level tables and reports the schema-folder ones as unreachable for now.
