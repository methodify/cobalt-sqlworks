# Cobalt SQL Works

A free, open-source, cross-platform desktop SQL client for SQL Server, Azure SQL, and Microsoft
Fabric — the spiritual successor to Azure Data Studio. Pure Rust, egui.

**Status:** pre-alpha, under construction. See `docs/product_design.md` for what it is and
`docs/architecture.md` for how it's built.

## Build

```
cargo run -p cobalt-app                 # dev build with the agent control channel compiled in
COBALT_AGENT=1 cargo run -p cobalt-app  # ...and serving it (Windows: named pipe `cobalt.agent`)
cargo build --release -p cobalt-app --no-default-features
```

## License

MIT OR Apache-2.0.
