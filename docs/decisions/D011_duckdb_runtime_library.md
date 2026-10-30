# D011 — DuckDB: the official library, downloaded on first use and loaded at run time

**Status:** decided by Claude while building slice E of the file-databases slate, 2026-10-30 ·
**Revisit when:** DuckDB's C API drops the functions Cobalt uses, or a platform appears without
a prebuilt library

## Context

`docs/design/file_databases.md` §3.11 put DuckDB after SQLite because of its cost as a
dependency: the `duckdb` crate bundles and compiles libduckdb (a long C++ build on every cold
compile, on every CI runner) and adds tens of megabytes to a binary whose file-analytics role the
local engine already fills. The founder asked for slice E once SQLite had proved the path.

## Decision

Cobalt does not link DuckDB. `crates/cobalt-duckdb` loads the official `libduckdb` shared
library with `libloading` and drives it through the C API, with hand-written bindings for the
functions Cobalt needs (open / connect / interrupt, extracted statements, streaming pending
results, data chunks to Arrow through the C data interface, Arrow streams registered as views,
error data). The library is downloaded from DuckDB's GitHub release on the first DuckDB
connection (about 20 MB, into the app's local data folder, one version per folder) and
verified by loading its symbols; a missing or wrong library is a clear connection error.

The version is pinned (1.5.6 today: the last release with prebuilt libraries for Windows,
Linux and macOS, x86-64 and arm64; 2.0.0 shipped nine days before this decision without them).
A pinned version makes the "unstable" Arrow functions of the C API safe to use, and a newer
file format than the pinned library can read surfaces as DuckDB's own message.

One database instance per file per process (DuckDB locks the file), keyed by a normalised path;
connections share it; a read-only instance refuses a writable open until its handles are gone.

## Consequences

- The installer and the build are unchanged in size and time; CI needs no C++ toolchain for
  DuckDB.
- The first DuckDB connection needs the network once; an offline first use fails with a
  message naming the URL. Settings can remove the library (`duckdb {action: remove}` for the
  agent).
- Results go through Arrow with DuckDB's own types; the grid shows nested values as text.
- The same shape (download, load, bind) is available to any future engine that ships a
  C-API library.
