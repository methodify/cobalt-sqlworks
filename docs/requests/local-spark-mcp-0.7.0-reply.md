# Cobalt's reply to local-spark-mcp 0.7.0

*From Cobalt SQL Works, 2026-10-08. Pin `v0.7.0`. Adopted the same day: Spark SQL query tabs run
their statements through `run_sql` with `stream: true` now (one call per statement, in the tab's
context); the `run_code` helper stays as the path on a worker without `sql_stream`.*

Verified live on `test` (schema-enabled, Windows, fabric-2.0):

```
SELECT * FROM dbo.publicholidays  (limit 10000, batch_rows 10000)
  → batches arrive as event frames with the blob; the grid fills as they land; the reply's
    row_count = 10000, batches = 1; Cobalt says "the first 10,000 rows … there may be more"
Run to File, no limit             → 7 batches, 69,557 rows into CSV (4.95 MB), the grid keeps 1,000
CREATE OR REPLACE TABLE … AS      → metrics {affected_rows: 50, source: history}
INSERT INTO … SELECT … LIMIT 5    → metrics {affected_rows: 5, source: history}
UPDATE … / DELETE …               → metrics {affected_rows: n, source: result} (the count frame is
                                    dropped from the grid; Messages says "(n rows affected)")
MERGE INTO …                      → metrics {affected_rows: 60, inserted: 10, updated: 50, deleted: 0}
SELECT * FROM dbo.nope            → ok:false, error "AnalysisException: [TABLE_OR_VIEW_NOT_FOUND] …"
                                    (one line in Cobalt, the statement number in front)
SHOW TABLES in a fresh context    → every Tables/dbo/<t> of the lakehouse, nothing mounted
```

Two small things, no ask attached:

- The interrupted shape of `run_sql` (`ok: false, interrupted: true, error: "KeyboardInterrupt:
  interrupted …"`) is recognised by the error text on our side, since the client discards the
  frame's `interrupted` flag with the error. Fine as is.
- `truncated` is always `false` on a streamed reply even when `limit` cut the result; Cobalt
  infers the cut from `row_count == limit`. If a later version can say `truncated: true` when
  the plan had more rows (the `limit + 1` probe you use on the collected path), the message
  would stop hedging with "there may be more".
