# Request for local-spark-mcp (after 0.6.6) — query tabs: listings, streaming results, DML counts

*From Cobalt SQL Works, 2026-10-08. Cobalt now has Spark SQL query tabs on the session
(`docs/design/spark_query_tabs.md`, slice A built): one context per tab, every statement runs
through `run_code` + `__cobalt_sql_all`, rows come back as `display` blobs. Three asks follow
from using it; none blocks the release.*

## 1. `SHOW TABLES` on a schema-enabled lakehouse lists only the tables touched so far

Context on `test` (schema-enabled, catalog `test`, schema `dbo`), 0.6.6:

```
SHOW TABLES                      → 0 rows            (fresh context)
SELECT … FROM dbo.publicholidays → mounts the clone
SHOW TABLES                      → dbo | publicholidays | false
                                    | v | false        (a temp view)
```

The catalog's `listTables` answers with what is mounted, not with what the lakehouse has.
Ask: list the lakehouse's tables from OneLake (`Tables/<schema>/<table>` folders), marking
nothing — mount still happens on first touch. Same for `SHOW SCHEMAS` / `SHOW NAMESPACES` if
they take the same path. Cobalt's Lakehouse pane lists from OneLake itself, so this is about
what the user's own SQL sees, and about tools like `spark.catalog.listTables()`.

## 2. Streaming result batches for a statement (`run_sql`, or `display` in batches)

A query tab's *Run to File* collects the whole frame today (`display(df, limit=2e9)`) and then
feeds the export writer; fine to a few hundred MB, not beyond. Ask, as you scoped it in the
0.6.6 reply: a `run_sql {context, sql, batch_rows}` whose reply streams event frames each
carrying an Arrow IPC batch (`{"event": "batch", "rows": n}` followed by the blob), ending
with the usual reply (`row_count`, elapsed), interruptible on the control socket. Cobalt would
then also offer "fetch more" on a capped grid the way it does for SQL Server.

## 3. Affected-row counts for DML

`INSERT`, `UPDATE`, `DELETE`, `MERGE` come back as a frame with no columns; the tab says
"Statement 2 completed (1.4 s)". Delta's commit metrics (`numOutputRows`, `numAffectedRows`,
`numTargetRowsInserted/Updated/Deleted`) would let it say "(1,204 rows affected)" like a T-SQL
tab. Ask: expose them on the reply of the statement that produced the commit (or as a notice).
