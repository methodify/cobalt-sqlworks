# LakeSail / Sail 0.7.2 — findings from Cobalt SQL Works (2026-10-09)

*For the Sail issue tracker (github.com/lakehq/sail/issues). Cobalt SQL Works embeds Sail as an
optional "LakeSail" engine beside a JVM Spark: `pysail` 0.7.2 in-process
(`SparkConnectServer`), `pyspark-client` 4.1.3, Windows 11 x64, Python 3.11, Microsoft Fabric
OneLake with Entra bearer tokens. Everything below was reproduced on 2026-10-09 against a Fabric
workspace with a schema-enabled lakehouse (`test`) and a plain one (`test_no_schema`).*

## 1. OneLake catalog (`api="delta"`): tables with `integer` / `long` / `decimal` columns cannot be loaded

Config: `SAIL_CATALOG__LIST='[{type="onelake", name="fabric", url="Fabric test/test.Lakehouse", api="delta"}]'`,
`AZURE_STORAGE_TOKEN=<Entra token for https://storage.azure.com>`.

`SHOW DATABASES IN fabric` and `SHOW TABLES IN fabric.dbo` are correct. `SELECT * FROM
fabric.dbo.publicholidays` works (columns: string, boolean, timestamp). Every other table fails:

```
AnalysisException: Failed to get table: unknown error: status code 200 OK
```

Fabric's table endpoint (`https://onelake.table.fabric.microsoft.com/delta/<ws>/<lh>.Lakehouse/api/2.1/unity-catalog/tables/<lh>.Lakehouse.dbo.<t>`)
answers 200 with lowercase Spark type names and no `type_text` / `type_json`:

```json
{"name":"cobalt_export_test","catalog_name":"test.Lakehouse","schema_name":"dbo","table_type":null,
 "data_source_format":"DELTA",
 "columns":[{"name":"sale_id","type_text":null,"type_json":null,"type_name":"integer",...},
            {"name":"amount","type_name":"decimal(18,2)",...},
            {"name":"sold_on","type_name":"date",...},
            {"name":"sold_at","type_name":"timestamp_ntz",...},
            {"name":"ratio","type_name":"double",...}],
 "storage_location":"https://onelake.dfs.fabric.microsoft.com/Fabric test/test.Lakehouse/Tables/dbo/cobalt_export_test",
 "comment":null,"properties":null,"owner":null,"created_at":1789686243000,"created_by":null,
 "updated_at":1789686243000,"updated_by":null,"table_id":"656ed4aa-a370-4e4b-8e67-906eadf54c08"}
```

A table whose columns are only `string` / `boolean` / `timestamp` loads; one with `integer`,
`long` or `decimal(18,2)` (a table Sail itself created with `id INT, name STRING` included) does
not. The Unity client seems to accept only the Unity spellings (`INT`, `LONG`, `DECIMAL`) and the
error swallows the parse detail ("unknown error: status code 200 OK"). Ask: accept Spark type
names in `type_name` (case-insensitive; `decimal(p,s)` with the precision inline), and surface the
deserialization error in the message.

Also noted: `CREATE TABLE` through this catalog returns Fabric's 405 for `staging-tables` (Fabric has
no staging API; creating the Delta table at `Tables/<schema>/<name>` by path is what works), and
`DROP TABLE` through the catalog deletes the OneLake folder — expected for a managed catalog, but
worth a line in the OneLake page.

## 2. Two or more catalogs without `catalog.default_catalog`: sessions die without a message

`SAIL_CATALOG__LIST='[{type="memory", name="m1", initial_database=["default"]}, {type="memory", name="m2", initial_database=["default"]}]'`
(any two catalogs, OneLake included) and no `SAIL_CATALOG__DEFAULT_CATALOG`: every request after
session creation fails with

```
IllegalArgumentException: invalid argument: session <id> is not running
```

and nothing is logged at `warn`. With `SAIL_CATALOG__DEFAULT_CATALOG=m1` everything works. Ask: fail
at configuration load with "catalog.default_catalog is required when catalog.list has more than
one entry", or default to the first entry.

## 3. Spark SQL syntax users paste

- `USE dbo` → `found dbo at 4:7 expected 'DATABASE', 'SCHEMA', 'NAMESPACE', or 'CATALOG'`. Spark
  accepts the bare form (`USE db`), and notebooks copied from Fabric use it. (Cobalt rewrites it
  to `USE DATABASE` for now.)
- `DESCRIBE HISTORY delta.\`path\`` → parse error. Delta users run it reflexively; even a
  read-only projection of the commit log (`version`, `timestamp`, `operation`,
  `operationMetrics`) would be enough.
- `SET key=value` → `Could not find config namespace "spark"`; `SET` → `list all properties` is
  unsupported. A no-op with a warning would keep pasted notebooks running.

## 4. Nice to have

- A way to attach a catalog to a running server (per session or server-wide) so a client can
  add a lakehouse without restarting; Cobalt restarts the embedded server today when a
  schema-enabled lakehouse is attached after start.
- `DESCRIBE TABLE` on an external Delta table created with `USING delta LOCATION '…'` and no
  column list returns 0 rows (the schema was inferred and queries work).
- `CREATE TABLE … USING parquet AS SELECT` returns a `count` column typed `uint64`, which the
  PySpark client cannot convert ("uint64 is not supported in conversion to Arrow"); `int64`
  would round-trip.

## What worked well (for the record)

In-process server start 80 ms; abfss OneLake reads and Delta writes (INSERT / UPDATE / DELETE /
MERGE / INSERT OVERWRITE / time travel) through external tables by path; the Fabric token
service provider (`AZURE_FABRIC_TOKEN_SERVICE_URL` + `AZURE_FABRIC_SESSION_TOKEN` +
`AZURE_ALLOW_HTTP=true`) against a loopback endpoint that returns a raw JWT, with refresh;
per-session isolation of temp views / current database / conf; `interruptAll`; Arrow batches
through the client; Python UDFs; PySpark 4.1.3 and 4.2.0 clients.
