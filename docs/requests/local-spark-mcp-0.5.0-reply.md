# Cobalt's reply to local-spark-mcp 0.4.3 and 0.5.0

*From Cobalt SQL Works, 2026-10-07. Follows `local-spark-mcp-sessions.md`. Both releases are
adopted in Cobalt 0.8 (pin `v0.5.0`), verified live on the two test lakehouses.*

## Adopted

- **Contexts (0.5.0).** One context per notebook tab: `create_context(nb-<id>, default_lakehouse)`
  on the notebook's first run, `run_code(context=…)` for every cell, `drop_context` when the tab
  closes; the session keeps running. The default lakehouse is therefore the notebook's own, and
  two notebooks with different defaults share one session with no restart. Cobalt gates on
  `features` (`contexts`, `register_lakehouse`, `job_description`), not on versions — thank you
  for that field.
- **`register_lakehouse` (0.4.3).** A notebook from a workspace the session did not start with
  has its lakehouses registered before its first cell; the kernel chip shows the extra workspace
  ("+1"). A name clash with an already attached lakehouse is reported, not replaced. Only a
  different write mode, or a lakehouse notebook on a session started plain, still restarts.
- **`job_description` (0.4.3).** The cell's first non-magic line, 80 chars.
- `shadow_status.cloned_at / registered` and `status.idle_s`: noted for the lifecycle policy
  and "preload the tables I used last time" (slate 4), not surfaced yet.

## Verified live (Windows, Python 3.11, fabric-2.0, Spark 4.1.1)

Plain session, two notebooks:

```
A: import math; x = 42; CREATE TEMP VIEW v_a; spark.range(3).saveAsTable("shared_t")
   → context nb-06b031b7 created (database default)
B: x → NameError (good); math → NameError (good); spark.table("v_a") → AnalysisException (good)
   spark.table("shared_t").count() → 3 (shared, good)
   → context nb-28936a5c created (database default); kernel.contexts = 2
A again: "x still 42 and v_a 1"
close B → context nb-28936a5c dropped; kernel.contexts = 1; session still ready
```

Fabric-bound session (workspace "Fabric test"), two notebooks, different default lakehouses:

```
F1 (default lakehouse test, schema-enabled):
   → context created (database test__dbo)
   spark.catalog.currentDatabase() = test__dbo, currentCatalog = spark_catalog
   spark.table("publicholidays").count() = 69557   (notice: mounted test__dbo.publicholidays in 10.8 s)
F2 (default lakehouse test_no_schema):
   → context created (database test_no_schema)
   currentDatabase() = test_no_schema; listTables() = []
   spark.table("test.sales_import").count() = 20000 (notice: mounted test.sales_import in 4.3 s)
```

No restart between any of these; `info.features` =
`arrow, capture_result, contexts, interrupt, job_description, register_lakehouse, streaming`.
The environment upgrade from 0.4.2 took 41 s through the runtime manager.

Not exercised live: `register_lakehouse` across workspaces — the test tenant has one workspace
with lakehouses. The code path is in; the first notebook from a second workspace will tell.

## On the 0.6.0 proposal (lazy Files)

Go ahead as proposed: Spark's `Files/` → `abfss://` of the default lakehouse first (the low-risk
half, and the one that makes "remote by default" true), then the Python hooks. Two requests for
the hooks' design: scope them to the `/lakehouse/` prefixes only (so a library that opens its own
files never pays), and make the native-reader error name the exact `sync_files` call that would
fix it. With contexts in place, a context's `Files/` following *its* default lakehouse is the
behaviour Cobalt wants; if that has to wait for 0.6.0 that is fine, the session default is a
reasonable interim.

## Small follow-ups (not urgent)

1. `create_context` could accept `job_group` or a display name so `status.cell.context` and the
   Spark UI show the notebook's title rather than Cobalt's `nb-<hex>` id. Cosmetic.
2. `drop_context` on a context whose cell is running currently requires an interrupt first; a
   `force: true` that interrupts and drops in one call would simplify "close a running notebook".
3. `info.contexts[].cells` is useful; `last_activity` per context would feed a per-notebook idle
   indicator.

Thanks — this was the keystone, and it landed exactly as proposed.
