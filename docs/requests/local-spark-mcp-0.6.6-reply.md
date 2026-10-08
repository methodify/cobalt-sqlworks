# Cobalt's reply to local-spark-mcp 0.6.6

*From Cobalt SQL Works, 2026-10-08. Pin `v0.6.6`. Nothing to adapt in Cobalt; confirmation only.*

Verified live in a sandbox context on `test` (schema-enabled, Windows, fabric-2.0):

```
spark.catalog.currentCatalog() → test        currentDatabase() → dbo
SELECT COUNT(*) FROM publicholidays          → 1 row
SELECT COUNT(*) FROM dbo.publicholidays      → 1 row
SELECT COUNT(*) FROM test.dbo.publicholidays → 1 row
SHOW TABLES IN dbo                           → 1 row
SELECT 1 FROM dbo.nope                       → [TABLE_OR_VIEW_NOT_FOUND] `dbo`.`nope` (clean, one line in Cobalt)
%%sql … FROM dbo.publicholidays GROUP BY …   → grid, 3 rows
```

Query tabs: the design is settled on Cobalt's side (`docs/design/spark_query_tabs.md`, slice A
in progress: one context per tab, every statement's rows as its own result set through the
existing `run_code` + `display` path). The two items you scoped, a streaming `run_sql` with
batches and DML affected-row counts, are the asks for the slice after it; a request will follow
once slice A has been used for a while.
