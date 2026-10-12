# Sail 0.7.2 — a SELECT that mixes a plain column with an aggregate and has no GROUP BY fails with an internal column id instead of Spark's MISSING_AGGREGATION

*For the Sail issue tracker (github.com/lakehq/sail/issues). Found 2026-10-12 in Cobalt SQL
Works (Sail 0.7.2 in-process via `pysail`, `pyspark-client` 4.1.3, Windows 11 x64, Python 3.11,
a Delta table `lh.dbo.t` with a string column `k` on Microsoft Fabric OneLake; any table reproduces it). Not a Cobalt defect: the query is wrong, and adding the
GROUP BY fixes it on both engines. Filed because the message sent a user looking for a planner bug
instead of the missing clause.*

## Repro

```sql
SELECT ss.k, count(*) AS n
FROM (SELECT k FROM lh.dbo.t LIMIT 100) ss
WHERE ss.k IS NOT NULL
```

Sail 0.7.2:

```
No field named ss."#8".
Valid fields are "count(Int64(1))".
```

The same shape with a `sum` and a two-level nested subquery over joins gives

```
No field named ss."#151". Did you mean '"sum(ss.#153)"'?
Valid fields are "sum(ss.#153)".
```

and with the plain column only in `ORDER BY`
(`SELECT count(*) AS n FROM (...) ss ORDER BY ss.k`):

```
No field named ss."#8". Did you mean '"#9"'?
Valid fields are "#9".
```

Spark's own message for this shape (error class `MISSING_AGGREGATION` in Spark 4's error
conditions; not re-run on the JVM engine for this note):

```
[MISSING_AGGREGATION] The non-aggregating expression "k" is based on columns which are not
participating in the GROUP BY clause. Add the columns or the expression to the GROUP BY, aggregate
the expression, or use "any_value(k)" if you do not care which of the values within a group
is returned.
```

With `GROUP BY ss.k` the query runs on Sail as expected (5 rows, 3.9 s).

## Ask

Raise the Spark `MISSING_AGGREGATION` error (or at least name the user's column) at analysis
time, before the projection is resolved against the aggregate's output schema. The `#n` names are
the analyzer's internal column ids and mean nothing to the person who wrote the query; the "Did you
mean" suggestion pointing at `sum(ss.#153)` makes it worse.
