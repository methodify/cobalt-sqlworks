# Request for local-spark-mcp: take a host-supplied table listing (2026-10-10)

*From Cobalt SQL Works. Pin `v0.8.1`. One ask, small, with the payload Cobalt already has.*

## What Cobalt does now

Cobalt lists a lakehouse's tables through Fabric's OneLake table API
(`https://onelake.table.fabric.microsoft.com/delta/<workspace>/<lakehouse>/api/2.1/unity-catalog`,
the storage-scope token): one call for the schemas, one per schema for its tables, one per table
for its columns. The Lakehouse pane and the Spark SQL completion catalog read it (with the DFS
crawl of `Tables/` as the fallback when the API is not available for a lakehouse), and the
LakeSail engine's catalog is served from it. On a lakehouse with a few hundred plain tables that
is 2 calls instead of a DFS listing per top-level folder.

The JVM worker still discovers tables on its own: `OneLakeCatalog.listOneLakeTables` crawls
`Tables/` over DFS at session start and on `SHOW TABLES`, once per schema folder, and the
first-touch clone reads the Delta log again. For big catalogs this is the part of session start
that users wait on.

## The ask

Accept the listing from the host and skip the crawl when it is given:

1. `init` and `register_lakehouse`: an optional `tables` array per lakehouse:

   ```json
   {"name": "test", "id": "…", "workspace_id": "…",
    "tables": [{"schema": "dbo", "name": "publicholidays"}, {"schema": null, "name": "legacy_top_level"}]}
   ```

   `schema: null` is a plain (non-schema) lakehouse's table at `Tables/<name>`; a schema-enabled
   lakehouse always carries the schema. The list is the host's view at that moment; the worker
   may still crawl for anything it is asked for that is not in the list (a table created after
   the listing), as it does today for an unknown name.
2. A `set_tables {lakehouse, tables}` method so the host can refresh the list after a create
   without re-registering (Cobalt invalidates its own cache on a `CREATE TABLE` it ran; it would
   push the new list then).
3. `info.lakehouses[name].tables_source`: `"host"` or `"crawl"`, so Cobalt can say where the
   listing came from, as it does for its own readers.

Not asked: the columns. The worker reads the Delta log when it clones; the host's column list is
for completion only.

## Why not the Unity connector

Pointing Spark at Cobalt's loopback Unity endpoint through the Unity connector would give up the
sandbox clones that are the JVM engine's point; the listing is the only part worth sharing.
