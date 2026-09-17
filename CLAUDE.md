# Cobalt SQL Works

A free, open-source, cross-platform desktop SQL client — the spiritual successor to
Azure Data Studio (retired by Microsoft, Feb 2026). Pure Rust + egui 0.36.

Primary audience: people who do everyday query work against SQL Server, Azure SQL, and
Microsoft Fabric (Data Warehouse / SQL analytics endpoint), and who want something fast
and light — not SSMS, not a VS Code extension.

## Status

**Phase: V1 (2026-09-17).** The app runs end to end on Windows against SQL Server (Docker),
Fabric Warehouse and SQL database in Fabric (Entra sign-in, routing redirects, TRACEID — see
`vendor/tiberius-ng/COBALT-PATCH.md`), and builds for Linux in Docker. Design corpus in `docs/` — start with `docs/reading_guide.md`; the spec is
`docs/product_design.md`, the build shape is `docs/architecture.md`, decisions in `docs/decisions/`,
founder-facing test notes in `docs/alpha_notes.md`.

## Workspace map

```
crates/cobalt-core      shared types (profiles, engines, SqlType<->Arrow, catalog, settings)
crates/cobalt-driver    Driver/Connection traits; mssql impl over vendor/tiberius-ng (patched, see D003)
crates/cobalt-results   Arrow chunk store: spill, display cache, sort/filter views, summaries
crates/cobalt-sql       T-SQL lexer, batches/statements, completion, format, snippets
crates/cobalt-plan      showplan XML -> model, layout, icons (fixtures from real SQL Server)
crates/cobalt-store     SQLite: connection library, history (FTS5), tab snapshots, settings TOML, ADS import
crates/cobalt-auth      Entra PKCE loopback / device code / az CLI / service principal, keyring
crates/cobalt-export    CSV/TSV/JSON/JSONL/XML/Markdown/Excel/Parquet/Arrow; cobalt-export-delta = delta-rs
crates/cobalt-app       the binary: session actors (tokio), state, ui/, ops, agent verbs
vendor/tiberius-ng      vendored TDS driver + COBALT-PATCH.md
assets/                 icon.svg / logo.svg (source of truth) + rendered PNGs and icon.ico (resvg + Pillow)
site/                   GitHub Pages homepage (static; lists the latest release via the GitHub API)
.github/workflows/      ci.yml (tests), release.yml (tag v* -> installers + GitHub release), pages.yml (site)
scripts/                launch.sh, agent-smoke.sh, agent-ui.sh, agent-verify.sh (egui-agent-cli drivers)
tests/seed.sql          test database for the Docker SQL Server
```

## Build, run, test

- `cargo build -p cobalt-app --features agent` (dev, with the egui_agent channel) · `cargo build --release -p cobalt-app`
  (release: no agent feature, ever) · full cold build ≈ 10 min.
- Dev binary: `target/debug/cobalt.exe`. Launch for agent driving: `bash scripts/launch.sh`
  (kills, builds, starts with `COBALT_AGENT=1`, pipe `cobalt.agent`). Then
  `egui-agent-cli --pipe cobalt.agent invoke state --quiet`, `... invoke connect --args '{"profile":"local"}'`, etc.
  Verbs are listed at the top of `crates/cobalt-app/src/agent.rs`.
- Test DB: Docker `cobalt-mssql` (SQL Server 2022, localhost:1433, sa / `Cobalt!Dev2026pw`,
  db `cobalt_test` from `tests/seed.sql`). Driver integration tests:
  `COBALT_TEST_MSSQL=1 cargo test -p cobalt-driver -- --test-threads=1`.
- Linux: `docker run --rm -v "$PWD:/src" -w /src -e CARGO_TARGET_DIR=/src/target-linux rust:1.95-bookworm
  bash -c "apt-get update && apt-get install -y pkg-config libssl-dev libxkbcommon-dev libwayland-dev
  libx11-dev libxcb1-dev libgl1-mesa-dev libfontconfig1-dev libdbus-1-dev cmake clang && cargo build --release -p cobalt-app"`.
- Windows GPU note: `main.rs` forces `WGPU_BACKEND=dx12` (default enumeration crashes on mixed AMD/NVIDIA boxes).

## Conventions

- Keep `cobalt-core` free of egui/tokio/driver types. UI never touches driver types; results travel as
  `Arc<ResultSet>` (created by the session actor), never as events.
- UI modules return actions or call `ops::*` with `(&mut AppState, &Ctx)`; effects live in `ops.rs`.
- Every interactive custom widget gets `widget_info` (a11y label) so agents can drive it by label
  (`"server: local"`, `"tab SQLQuery_1"`, `"column id"`, `"filter id"`).
- Decisions D001–D008 are settled; don't re-litigate. Founder's scope answers are in D002.

## Scope framing (from the founder, 2026-09-16)

- V1: connection library, object explorer, editor, streaming grid, exports incl. Parquet/Arrow/Delta,
  plan viewer, history, light/dark, egui_agent. V2: SQL-only notebooks, dashboards, Edit Data, column
  profiler, DataFusion local re-query, BYO-model AI, DuckDB/SQLite/PostgreSQL. Out: extension marketplace,
  source control, Jupyter kernels, Agent, dacpac, telemetry.
- Platform priority: Windows > Linux > macOS. macOS tuning later via an Aspen-bussed agent on a MacBook.
- Entra: founder registers the app; **stop and coordinate with him before live Fabric/Entra testing.**
