# Cobalt's reply to local-spark-mcp 0.4.2

*From Cobalt SQL Works, 2026-10-06. Follows `local-spark-mcp-0.4.1-reply.md`. Short one: the fix
holds on the real schema-enabled lakehouse, and Cobalt 0.7.3 ships pinned to 0.4.2.*

## Confirmed on the schema-enabled lakehouse (`test`, workspace "Fabric test")

Windows, Python 3.11, fabric-2.0, Spark 4.1.1, `write_mode: sandbox`, `preload: ["test"]`, all
tokens from Cobalt's endpoint. Same cells as the 0.4.1 report, now:

```
spark.conf.get("spark.sql.catalog.test")      → ch.fs.OneLakeSchemaCatalog
SHOW NAMESPACES IN test                       → ['dbo']
preload_status                                → done 8/8, failed 0, 24.5 s
  {"test": {"done": 8, "errors": {}, "failed": 0, "seconds": 24.5, "state": "done", "total": 8}}
SHOW TABLES IN test.dbo                       → 5 rows
  notice: preloaded 8 tables across 1 lakehouse(s) in 25 s (32 workers)
spark.table("test.dbo.publicholidays").count()→ 69557, and .limit(3) → a 3-row grid
SELECT COUNT(*) AS n FROM test.sales_import   → 1 row (top-level table, unqualified lakehouse form)
SELECT countryOrRegion, COUNT(*) … FROM test.dbo.publicholidays GROUP BY … LIMIT 5 → 5 rows
DESCRIBE EXTENDED test.dbo.publicholidays     → Location = file:/C:/…/spark-runtime/state/… (the sandbox clone)
shadow_status                                 → 8 clones, all `read`:
  test.{cobalt_nb_writethrough, r2e_stream, sales_import}
  test__dbo.{cobalt_export_schema, cobalt_export_test, publicholidays, r2e_dialog, r2e_stream2}
```

Plain-layout lakehouse `test_no_schema` (empty) unchanged from the 0.4.1 report: no catalog
registered, preload `done 0/0`, sandbox `saveAsTable` + bare `spark.table` → 5-row grid,
`SHOW TABLES` → 1 row, shadow `written`. Both sessions also show
`info.lakehouse_schemas = {"test": ["dbo"]}`.

Plain session on 0.4.2: native displays as before; interrupt of a `spark.range(10**13)` aggregate
acknowledged in 0.1 s (`spark jobs cancelled in 0.04s`), cell cancelled, session intact, next cell
ran. The upgrade from 0.4.1 took 33 s through the runtime manager.

## Adopted / noted

- Pin `v0.4.2`. No protocol change; nothing else to adapt on our side.
- `status.cell.jobs` and `info.current_catalog`: noted, not surfaced in the UI yet. When Cobalt
  wires the Stop tooltip it will set `spark.sparkContext.setJobDescription(<cell's first line>)`
  as you suggest — through the worker rather than by prepending to the user's code, so tracebacks
  keep their line numbers; a `run_code` parameter `job_description` would be the cleanest way to
  do that if you are open to it.
- The interrupt-then-respawn fix: Cobalt had not seen it (our interrupt tests were few), good to
  have it closed.
- The `spark_catalog.delta.`path`` note is in Cobalt's alpha notes for users who write path
  queries with a V2 catalog current.
- PyPI remains a far-future item on Cobalt's side; the git-tag pin stays.

## Open

Nothing blocking. Thanks for the fast turnaround across 0.3.5 → 0.4.2 in one day.
