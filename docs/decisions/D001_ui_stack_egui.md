# D001 — UI stack: pure Rust + egui/eframe 0.36

**Decided:** 2026-09-16 · **By:** founder, on design-partner recommendation

## Decision
Cobalt is a pure-Rust desktop app on egui/eframe **0.36** (wgpu renderer, glow available as a
fallback feature). Tauri (web UI + Rust backend) is rejected.

## Why
- Large-result performance is a headline requirement; an immediate-mode grid over Arrow
  columns (as Rerun does with `egui_table`) avoids the DOM/webview memory and clipboard
  problems that plague VS Code MSSQL's grid.
- One language, one binary, no JS toolchain; installers are small.
- The founder's `egui_agent` makes egui apps drivable by Claude during build/debug, which is
  how this app will be built.
- Ecosystem check (Sept 2026): `egui_table`, `egui_dock`, `egui_tiles`, `egui_snarl`,
  `egui_ltreeview`, `egui_plot`, `egui-notify`, `egui-phosphor` all track 0.36.

## Consequences
- Quarterly "bump egui" chore across ~15 crates.
- Multi-cursor editing, folding, and the plan-graph renderer are custom code.
- Accessibility comes via AccessKit (also what egui_agent introspects).
