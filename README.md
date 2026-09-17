# Cobalt SQL Works

A free, open-source, cross-platform desktop SQL client for SQL Server, Azure SQL, and Microsoft
Fabric — the spiritual successor to Azure Data Studio. Pure Rust, egui.

**Status:** v0.1.0 released. Downloads and news at [cobaltsql.org](https://cobaltsql.org); installers on the
[releases page](https://github.com/methodify/cobalt-sqlworks/releases). See `docs/product_design.md` for what it is,
`docs/architecture.md` for how it's built, and `docs/alpha_notes.md` for what to try first.

## Build

Requires Rust 1.85+ (developed on 1.95). On Windows, Visual Studio Build Tools; on Linux,
`pkg-config libssl-dev libxkbcommon-dev libwayland-dev libx11-dev libxcb1-dev libgl1-mesa-dev libfontconfig1-dev libdbus-1-dev cmake clang`.

```
cargo run -p cobalt-app                 # dev build with the agent control channel compiled in
COBALT_AGENT=1 cargo run -p cobalt-app  # ...and serving it (Windows: named pipe `cobalt.agent`)
cargo build --release -p cobalt-app     # release (agent channel still compiled in but inert)
cargo build --release -p cobalt-app --no-default-features   # release without the agent channel
```

A Linux binary can be produced from any machine with Docker (see `CLAUDE.md`). The SQL Server
driver is vendored under `vendor/tiberius-ng` with a small documented patch.

## Testing

Unit tests run with `cargo test --workspace`. Driver integration tests need a SQL Server; the
easiest is Docker: `docker run -d --name cobalt-mssql -e ACCEPT_EULA=Y -e MSSQL_SA_PASSWORD='Cobalt!Dev2026pw'
-p 1433:1433 mcr.microsoft.com/mssql/server:2022-latest`, seed it with `tests/seed.sql`, then
`COBALT_TEST_MSSQL=1 cargo test -p cobalt-driver -- --test-threads=1`.

The app can be driven by an agent (or a script) through [egui_agent](https://github.com/methodify/egui_agent):
`bash scripts/launch.sh` then `egui-agent-cli --pipe cobalt.agent invoke state --quiet`.

## License

MIT OR Apache-2.0.
