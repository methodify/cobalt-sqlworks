# Cobalt's reply to local-spark-mcp 0.6.2

*From Cobalt SQL Works, 2026-10-07. Short: 0.6.2 adopted (pin `v0.6.2`), nothing new to ask.*

- `mirror_status.total_bytes` / `total_files`: noted; the Lakehouse pane shows the per-lakehouse
  figures and the runtime page sizes the mirror directory itself, so no change was needed yet.
- `status.dropping` / `info.contexts[].dropping`: noted for a "closing…" state on the attached
  count; not surfaced yet.
- The `fs.defaultFS` note in the protocol doc: thank you.
- One small inconsistency found while building the pane: `mount_table(lakehouse, table)` builds
  `spark_catalog.<lakehouse>.<table>`, so a schema-enabled lakehouse's table has no spelling it
  accepts (`dbo/publicholidays` → `INVALID_SCHEMA_OR_RELATION_NAME`), while `preload`'s explicit
  form takes `schema/table` entries. Cobalt's "Clone now" uses a one-table `preload` instead,
  which is fine; accepting the entry form in `mount_table`/`mount_tables` too would make the
  three consistent.

## One regression to look at: `shadow_status` lists nothing in 0.6.2

Found while wiring the Lakehouse pane's clone state (sandbox, `persist_shadow: false`,
Windows, fabric-2.0, both the `test` lakehouse's layouts):

```
cell: spark.table("test.sales_import").count() → 20000
      notice: mounted test.sales_import in 10.1 s (shallow clone)
shadow_status → {"tables": [], "persistent": false, "write_mode": "sandbox",
                 "shadow_root": ".../state/sessions/34948-1791406131/shadow"}

preload {"lakehouses": {"test": ["r2e_stream"]}}            → done 1/1 in 4.8 s
shadow_status → {"tables": [], ...}
preload {"lakehouses": {"test": ["dbo/publicholidays"]}}    → done 1/1 in 10.3 s
shadow_status → {"tables": [], ...}
```

On 0.5.0 the same session shape listed every clone (`test.sales_import`,
`test__dbo.publicholidays`, … with `state`, `version`, and from 0.4.3 `cloned_at` and
`registered`), and the 0.4.2 report's "shadows: 8 clones" came from exactly this call. The
clones are real (the notice, the row counts, and `DESCRIBE DETAIL`'s `file:/…` location say so),
so the lister seems to look in a place the 0.6.0 `file:`-URI / per-session-root change moved
them away from — or it filters on something the new layout no longer has. `discard_shadow`
and `restore_shadow` presumably still find them; the listing is what the Shadows window and the
pane's "cloned / written" markers depend on, so until it is back those show nothing for a
sandbox session. Not blocking Cobalt 0.8, but the first thing the next release should fix.

With this the sessions slate's upstream list is closed and Cobalt 0.8 builds on 0.6.2: contexts
per notebook, lifecycle policies, lazy Files, and a Lakehouse pane (tables with clone state,
Files from OneLake with pull / remove-local through `sync_files` / `clear_mirror`). The next
asks, if any, will come from users once 0.8 is out.
