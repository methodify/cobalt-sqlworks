# Request for local-spark-mcp (after 0.6.4) — two-part names against the default lakehouse

*From Cobalt SQL Works, 2026-10-08. One ask, found while fixing `%%sql` cells on contexts.*

## 1. `schema.table` should resolve against the default schema-enabled lakehouse

A context created with `default_lakehouse: test` (schema-enabled, session database
`test__dbo`) runs this fine:

```sql
SELECT countryOrRegion, COUNT(*) FROM publicholidays GROUP BY 1
SELECT * FROM test.dbo.publicholidays LIMIT 3
```

but the form Fabric notebooks use when a schema-enabled lakehouse is the default fails:

```sql
SELECT * FROM dbo.publicholidays LIMIT 3
-- [TABLE_OR_VIEW_NOT_FOUND] The table or view `dbo`.`publicholidays` cannot be found.
```

In Fabric the default lakehouse is the current catalog, so `dbo.publicholidays` resolves as
`<current catalog>.dbo.publicholidays`. In the worker the V2 catalog (`OneLakeSchemaCatalog`)
is registered but not current since 0.4.2 (making it current broke `delta.`<path>`` reads,
your 0.4.1 → 0.4.2 fix). The two-part name therefore resolves as `spark_catalog.dbo.…`.

Ask: make `schema.table` resolve against the context's default schema-enabled lakehouse
without regressing the `delta.`path`` case. Options we can see from outside:

- a thin resolver rule (or a `CatalogExtension` that delegates to `spark_catalog` for
  everything it does not own) so the lakehouse catalog is current but path tables still work;
- or keep `spark_catalog` current and alias each schema `s` of the default lakehouse as a
  session database `s` (today it is `test__dbo`), so `dbo.publicholidays` hits the alias.

Either is fine for us. Cobalt ships notebooks written in Fabric unchanged, so the Fabric
spelling is the one people paste. (Cobalt-side we compact the analysis message to one line
now; the resolution itself has to be yours.)

## 2. Heads-up, not an ask yet: Spark SQL query tabs

Cobalt is proposing a query-tab mode on the Spark session (`docs/design/spark_query_tabs.md`):
each tab is a context, statements run through the existing `run_code` + `display` path. Two
things we may ask for later, once that ships: a `run_sql` that streams result batches (for
exports larger than memory and "fetch more"), and affected-row counts for DML statements.
Nothing needed now.
