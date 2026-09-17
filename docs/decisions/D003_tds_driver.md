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

## Update 2026-09-17 — vendored with a patch

Implementation found that `tiberius-ng` 0.13.1's *public* `QueryStream` silently drops INFO,
ERROR and DONE tokens (PRINT/RAISERROR messages, row counts and error placement are
unreachable), and its `cancel_query` resynchronises at the token level, which can hang after a
dropped stream. The crate is therefore **vendored at `vendor/tiberius-ng/`** (`[patch.crates-io]`
in the root `Cargo.toml`) with a ~120-line additive patch documented in
`vendor/tiberius-ng/COBALT-PATCH.md`: raw token pull (`simple_query_send` + `next_token`),
re-exported token types, packet-level attention resync, and lossless `money` decoding.
The patch should be offered upstream; until then, upgrades of tiberius-ng require re-applying it.

## Addendum 2026-09-17 — what Fabric needed from the driver

Live testing against a Fabric Warehouse and a SQL database in Fabric surfaced three protocol gaps
in tiberius, all fixed additively in the vendored copy (see `vendor/tiberius-ng/COBALT-PATCH.md`):

1. Routing redirects to `gateway\instance:port` targets must connect to the bare gateway host and
   send the full routed name (`gateway\instance,port`) in LOGIN7 — SqlClient behaviour, confirmed
   by reflection on a live `System.Data.SqlClient` connection (`TdsParser._server`).
2. PRELOGIN TRACEID is mandatory on the routed Fabric gateway (18456 "system update" otherwise) and
   is 36 bytes on the wire, not tiberius's 20. Cobalt sends one on every connection.
3. Servers may echo TRACEID with a zero-length payload; the decoder must honour the option length.

Method worth keeping: a fake TDS server (`crates/cobalt-driver/examples/tds_capture.rs`) that
terminates TLS and dumps LOGIN7/PRELOGIN lets us diff any driver against SqlClient byte for byte.
`fabric_probe.rs` connects with a bearer token and tries routing-name variants. Both are dev-only.
These fixes are candidates for upstream PRs to tiberius-ng.
