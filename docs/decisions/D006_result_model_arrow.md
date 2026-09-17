# D006 — Result model: Arrow RecordBatches with disk spill; Delta export via delta-rs

**Decided:** 2026-09-16

## Decision
Query results are held as chunks of Arrow `RecordBatch` built from the streaming TDS row
stream. Beyond a configurable memory budget, batches spill to Arrow IPC files in a temp dir and
are memory-mapped back. The grid, filters, sorts, and every exporter (CSV, Excel, JSON, XML,
Markdown, Parquet, Arrow IPC, Delta) consume the same batches. Delta tables are written with
**`deltalake` (delta-rs)** — the founder has verified its output is readable by Fabric Spark and
SQL endpoints as-is. Polars is not a core dependency (it has its own Arrow); DataFusion is the
planned local query engine for V2.

## Why
Columnar memory is several times smaller than row-of-values; exporters become trivial; the
"query this result locally" V2 feature falls out for free; Rerun proves the grid pattern.

## Consequences
- Pin `arrow` to the version `deltalake` supports and upgrade quarterly; isolate `deltalake` in
  its own crate so its dependency tree doesn't slow the main build.
- Type mapping TDS → Arrow is ours (decimal, datetime2/datetimeoffset, uniqueidentifier,
  varbinary, sql_variant, xml, geography as WKB/text).
