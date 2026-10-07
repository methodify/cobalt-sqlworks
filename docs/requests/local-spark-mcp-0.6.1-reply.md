# Cobalt's reply to local-spark-mcp 0.5.1, 0.6.0 and 0.6.1

*From Cobalt SQL Works, 2026-10-07. Follows `local-spark-mcp-0.5.0-reply.md`. All three adopted in
Cobalt 0.8 (pin `v0.6.1`, `files_mode: "lazy"` by default with a Settings switch back to
`mirror`), verified live on the `test` and `test_no_schema` lakehouses.*

## Adopted

- **0.5.1.** `create_context(..., name=<notebook title>)`; `drop_context(force=true)` on the
  control socket when the closing notebook's cell is running (the data-socket drop otherwise);
  `info.contexts[].idle_s` noted for a per-notebook idle indicator later.
- **0.6.0 / 0.6.1.** `files_mode: "lazy"` in `init` whenever the installed package is 0.6.0+.
  Settings → Notebooks & Spark → Lakehouse Files offers `lazy` / `mirror`; the Spark runtime page
  shows the mirror's size (the state folder's `lakehouses/`) with a Clear button. The two Spark
  facts (absolute path for `delta.`…``, `file:/` locations on shadows) are in Cobalt's alpha notes.
- Agent: `kernel {action: call, method, params}` runs any worker method, which is how the live
  checks below were driven (`info`, `mirror_status`, `clear_mirror`).

## Verified live (Windows, Python 3.11, fabric-2.0, Spark 4.1.1, sandbox)

Notebook F1, default lakehouse `test` (which has `Files/{Readme.txt, cobalt_export_test.parquet,
images, sample_datasets}` on OneLake, nothing synced locally):

```
os.listdir("/lakehouse/default/Files")                      → ['Readme.txt', 'cobalt_export_test.parquet', 'images', 'sample_datasets']
spark.read.format("binaryFile").load("Files/").count()      → 2
open("/lakehouse/default/Files/cobalt_lazy_test.txt","w")   → exists True, size 19, read back "hello from cobalt",
                                                              listing now merges it: True
spark.range(3).write.csv("Files/cobalt_sandbox_csv")        → refused (sandbox), before anything reached OneLake
open("/lakehouse/default/Files/does_not_exist.bin")         → FileNotFoundError: [Errno 2] No such file or directory in OneLake
                                                              (lakehouse 'test', Files/does_not_exist.bin). If a native reader needs it
                                                              on disk, pull it first: sync_files(...)
info                                                        → files_mode lazy, files_hooks true,
                                                              contexts[nb-…].name = "Notebook_2",
                                                              files_fs = lakehouse://<ws>@<lh>.onelake.dfs.fabric.microsoft.com
mirror_status                                               → test: fetched 0 files, local_files 1 (19 bytes); test_no_schema: empty
```

Notebook F2, default lakehouse `test_no_schema`, same session: `/lakehouse/default/Files/
cobalt_lazy_test.txt` exists → **False** (its own lakehouse). Closing F2 while a `time.sleep(40)`
cell ran: `drop_context(force)` on the control socket → `{"dropped": false, "scheduled": true}`;
the context went when the sleep returned (the Windows pure-Python limit, as documented).
`clear_mirror` → both lakehouses' mirror dirs removed; F1's `os.path.exists(...)` → False.

One observation, not a bug: `spark._jsc.hadoopConfiguration().get("fs.defaultFS")` still says
`file:///` because that is the SparkContext's configuration; the per-session value that Spark
actually uses is the one `info.contexts[].files_fs` reports. Worth a line in your docs so nobody
checks the wrong knob the way I did.

## Asks

None blocking. Two small ones for whenever:

1. `mirror_status` could carry a `total_bytes` so a host does not sum per-lakehouse numbers;
   Cobalt sizes the directory itself meanwhile.
2. When `drop_context(force)` is scheduled, an event or a `status.contexts` flag
   (`dropping: true`) would let the host show "closing…" instead of a stale count until the
   cell ends.

That closes the sessions slate's upstream list: contexts, attach after init, job descriptions,
persisted-clone listing, idle signal, lazy Files. Thank you.
