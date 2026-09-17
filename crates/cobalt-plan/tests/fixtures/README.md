# Showplan fixtures

Captured 2026-09-16 from the `cobalt-mssql` container (SQL Server 2022, build
16.0.4295.3, showplan schema version 1.564) against the seeded `cobalt_test` database
(`tests/seed.sql` at the repo root: `dbo.big` has 2M rows with only a clustered PK;
`dbo.child` is empty). Each file is one `<ShowPlanXML>` document on a single line.

## How they were captured

Each query was written to a `.sql` script, copied into the container, and run with:

```
docker cp <name>.sql cobalt-mssql:/tmp/<name>.sql
MSYS_NO_PATHCONV=1 docker exec cobalt-mssql /opt/mssql-tools18/bin/sqlcmd \
  -S localhost -U sa -P '<sa password>' -C -d cobalt_test -y 0 -w 65535 -i /tmp/<name>.sql
```

`-y 0 -w 65535` keeps each XML document on one line (`:XML ON` is not available in the
Linux tools). The output was filtered to lines starting with `<ShowPlanXML`, each validated
with Python's `xml.etree`, and written as `<name>.sqlplan`. `SET SHOWPLAN_XML ON` must be
alone in its batch, so every script is:

```
SET NOCOUNT ON;
GO
SET SHOWPLAN_XML ON;      -- or SET STATISTICS XML ON for actual plans
GO
<query>
GO
```

## Fixtures

| File | Kind | Query | Shape / why it is here |
|---|---|---|---|
| `simple_seek.sqlplan` | estimated | `SELECT id, amount FROM dbo.big WHERE id = 42;` | Single Clustered Index Seek, seek predicate, auto-parameterised (`@1`) |
| `hash_join_aggregate.sqlplan` | estimated | `SELECT b.category, s.cnt, COUNT(*) AS n, SUM(b.amount) AS total FROM dbo.big b JOIN dbo.v_big_summary s ON s.category = b.category WHERE b.amount > 14000 GROUP BY b.category, s.cnt OPTION (MAXDOP 1);` | Hash Match join over two Hash Match aggregates, 7 operators, a missing-index hint |
| `nested_loops_seek.sqlplan` | estimated | `SELECT TOP (50) b.id, b.amount, b2.amount AS next_amount FROM dbo.big b JOIN dbo.big b2 ON b2.id = b.id + 1 WHERE b.id BETWEEN 1 AND 100 OPTION (MAXDOP 1);` | Top → Nested Loops with a range seek (outer) and a prefix seek (inner), OuterReferences |
| `key_lookup_actual.sqlplan` | **actual** | `BEGIN TRAN; CREATE INDEX IX_tmp_big_cat ON dbo.big(category);` `GO` `SET STATISTICS XML ON;` `GO` `SELECT TOP (20) id, amount, note FROM dbo.big WITH (INDEX(IX_tmp_big_cat)) WHERE category = 'CAT3' AND amount > 14000 OPTION (MAXDOP 1);` `GO` `SET STATISTICS XML OFF;` `GO` `ROLLBACK;` | Index Seek + Key Lookup (`IndexScan/@Lookup="1"`) under Nested Loops. The temporary index lives only inside the rolled-back transaction, so the seeded database is unchanged |
| `parallel.sqlplan` | estimated | `SELECT category, SUM(amount) AS total, COUNT(*) AS cnt FROM dbo.big GROUP BY category;` | Gather Streams over a parallel hash aggregate, `DegreeOfParallelism="8"`, MemoryFractions |
| `batch_three.sqlplan` | estimated | `SELECT id, amount FROM dbo.big WHERE id = 42;` `SELECT TOP (5) id, big_id, qty FROM dbo.child ORDER BY qty;` `SELECT category, cnt, total FROM dbo.v_big_summary WHERE cnt > 100;` | One document, three `StmtSimple` statements (8 operators total) |
| `actual_sort.sqlplan` | **actual** | `SELECT TOP (1000) b.id, b.amount, b.note FROM dbo.big b WHERE b.category = 'CAT3' ORDER BY b.amount DESC;` | Parallel (DOP 16) TopN Sort; `RunTimeInformation` with 17 threads per operator, `WaitStats`, `QueryTimeStats`, `MemoryGrantInfo`, a missing-index hint |
| `missing_index.sqlplan` | estimated | `SELECT id, amount FROM dbo.big WHERE amount = 1234.5 AND created > '2021-01-01';` | Missing index with EQUALITY (`amount`) and INEQUALITY (`created`) column groups |
| `implicit_convert.sqlplan` | estimated | `SELECT id FROM dbo.big WHERE category = 5;` | Two `PlanAffectingConvert` warnings (Cardinality Estimate, Seek Plan) at `QueryPlan/Warnings` |
| `sort_spill_actual.sqlplan` | **actual** | `SELECT MAX(rn) AS last_rn FROM (SELECT ROW_NUMBER() OVER (ORDER BY note) AS rn FROM dbo.big) x OPTION (MAXDOP 1, MAX_GRANT_PERCENT = 0.01);` | `SpillToTempDb` + `SortSpillDetails` on the Sort operator, `MemoryGrantWarning` at statement level, Window Aggregate |
| `exec_p_multi.sqlplan` | estimated | `EXEC dbo.p_multi;` | `StmtSimple` (EXECUTE PROC) containing `StoredProc/Statements` with the procedure's six statements (SET, PRINT, SELECT, RAISERROR, SELECT, PRINT); two have query plans |
| `stmt_cond.sqlplan` | estimated | `IF (SELECT COUNT(*) FROM dbo.child) > 0 SELECT TOP (1) id, big_id, qty FROM dbo.child; ELSE SELECT TOP (1) id, category FROM dbo.big ORDER BY id;` | `StmtCond` with `Condition/QueryPlan`, `Then/Statements`, `Else/Statements` |
| `cursor.sqlplan` | estimated | `DECLARE c CURSOR FAST_FORWARD FOR SELECT id, amount FROM dbo.big WHERE id < 10;` `OPEN c;` `FETCH NEXT FROM c;` `CLOSE c;` `DEALLOCATE c;` | Five `StmtCursor` statements; only DECLARE has a `CursorPlan/Operation/QueryPlan` |

For actual plans, `sqlcmd` also prints the query's result rows; those lines were dropped
(only `<ShowPlanXML…` lines were kept).
