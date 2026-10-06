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

## 8. Small things

- `info` could include `java_home`, `python`, `hadoop_home`, `ivy_dir` as resolved, so the
  host can show them without re-deriving.
- A `healthcheck` method that does not need a SparkSession (import check + versions +
  Java resolution verdict), for the host's runtime status page.
