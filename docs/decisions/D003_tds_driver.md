# D003 — TDS driver: `tiberius-ng` behind an internal `Driver` trait

**Decided:** 2026-09-16

## Decision
SQL Server connectivity goes through an internal `Driver`/`Connection` trait. The first
implementation is **`tiberius-ng`** (0.13.x, rustls). Microsoft's `mssql-tds` (0.1.0, Sept 2026)
is the planned migration target once it has a few releases and a rustls path. `odbc-api` +
`arrow-odbc` may become an opt-in "system ODBC" connection type later.

## Why
Original `tiberius` is unmaintained with open RustSec advisories. `tiberius-ng` adds exactly what
a client needs: attention-packet **cancellation**, **MARS**, TDS 8 strict encryption, **info
messages (PRINT/RAISERROR) exposed to the app**, bulk insert. It is a drop-in continuation
(library name `tiberius`), so migrating to the Microsoft bridge later is mechanical.

## Consequences
- Re-evaluate the driver quarterly; CI integration tests run against SQL Server 2022 in Docker.
- Kerberos on Linux/macOS via `integrated-auth-gssapi` is deferred; Windows SSPI works via
  the driver.
