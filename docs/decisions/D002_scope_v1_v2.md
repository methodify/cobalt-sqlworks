# D002 — Scope: V1 / V2 / Later / Out

**Decided:** 2026-09-16 · **By:** founder

Authoritative table: `docs/feature_inventory.md`. Summary of the founder's answers:

- **V1** = the query-runner surface: connection library (server groups, SQL/Entra-MFA/az-login/
  Windows auth), object explorer, editor, streaming results grid, exports incl. Parquet/Arrow/
  **Delta (local)**, estimated/actual plan viewer, query history, light/dark, egui_agent.
- **V2**: SQL-only notebooks (`.ipynb`-compatible, no Jupyter kernels), lightweight dashboards
  ("pin a query as a tile"), Edit Data with staged/safe-mode edits, column profiler, DataFusion
  local re-query, BYO-model AI, additional engines (DuckDB + SQLite first, then PostgreSQL),
  Fabric workspace browsing, Delta/Parquet export to OneLake/ADLS.
- **Later**: Table Designer, Schema Compare, Profiler, Query Store view, Backup/Restore.
- **Out**: extension marketplace, source control, Jupyter/Python/PowerShell kernels, SQL Agent,
  dacpac/bacpac, SQL projects, migration/Arc/BDC/ML, integrated terminal, telemetry, settings sync.

Founder usage signal: never used Edit Data, dashboards, or (recently) the import wizard;
charts rarely; notebooks sometimes. "Haven't used anything beyond the short list in 8 years."
Fabric data movement is done in Spark notebooks, so import matters less.
