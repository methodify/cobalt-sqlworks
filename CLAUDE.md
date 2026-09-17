# Cobalt SQL Works

A free, open-source, cross-platform desktop SQL client — the spiritual successor to
Azure Data Studio (retired by Microsoft, Feb 2026). Built in pure Rust with egui.

Primary audience: people who do everyday query work against SQL Server, Azure SQL, and
Microsoft Fabric (Data Warehouse / SQL analytics endpoint), and who want something fast
and light — not SSMS, not a VS Code extension.

## Status

**Phase: Product Design (PDT) — Excavation / Research.** No application code yet.
Design corpus lives in `docs/`. Start with `docs/reading_guide.md`.

## Scope framing (from the founder, 2026-09-16)

- Reproduce the good parts of ADS: connection library, object explorer, query editor,
  results grid, exports (CSV/Excel/JSON/XML/Markdown), estimated/actual plans + viewer,
  light/dark themes.
- Add: Parquet, Arrow, and Delta Lake export of query results.
- Explicitly OUT (for now): ADS's extension marketplace model; source control integration.
- Then reach past ADS — a dedicated section of the design covers what a next-gen tool
  should do that ADS never did.

## Technical direction (decided in principle, to be confirmed in design)

- Pure Rust + egui/eframe (Tauri is the fallback only if egui proves inadequate).
- Must integrate `egui_agent` (https://github.com/methodify/egui_agent) so Claude agents
  can see and drive the UI during build/debug. It currently pins **egui/eframe 0.35**.
- Platform priority: Windows > Linux > macOS. Windows/Linux built from the founder's box;
  macOS tuning later via an Aspen-bussed agent on a MacBook.
