# D012 — egui 0.37 taken up (supersedes D010)

**Status:** decided by Claude at the founder's request, 2026-10-30 ("take a look at egui 0.37
and take it up") · **Supersedes:** D010

## Context

D010 deferred egui 0.37 on 2026-10-27 because egui-notify, egui-phosphor and egui_agent were
not on it. By 2026-10-30: egui-notify 0.24, egui_table 0.11, egui_commonmark 0.26, egui_plot
0.38 and egui_kittest 0.37 are on 0.37; egui-phosphor 0.14 still pins egui 0.36; egui_agent is
Cobalt's own. The founder saw box glyphs ("tofu") in a tab title and recalled 0.37's font work.

## Decision

Cobalt builds on egui / eframe 0.37 with eframe's `system_fonts` feature: 0.37's font provider
(`fontique`) finds system fonts at run time, so any glyph a system font has renders — the tofu
problem that Cobalt's font-pack mechanism only partly covered. egui-phosphor 0.14 is vendored
under `vendor/egui-phosphor` with only its egui pin bumped (a `[patch.crates-io]` entry).
egui_agent 0.4.0 is the 0.37 port (screenshots through `Context::request_screenshot`,
`Id::unique`, kittest 0.37's harness without the accessible-name check, DuckDB-era role names).

Migration in Cobalt: `WidgetType` → `accesskit::Role` at every `widget_info` site (Button,
TextInput, CheckBox, ComboBox, Label, SpinButton, Unknown), `Ui::id` → `Ui::scope_id`,
`Id::new` → `Id::unique`, `FontData::from_owned`, `MouseWheelSource` on injected wheel events,
the chart capture through the screenshot callback.

## Consequences

- Cobalt's font packs stay as the explicit, offline fallback for machines without the glyphs;
  the system provider answers first.
- egui-phosphor is a vendored copy until upstream publishes a 0.37 release; the diff is one
  line of `Cargo.toml`.
- `default_fonts` is no longer egui's default: Cobalt and egui_agent's test harness name it.

*Postscript (0.15.1):* eframe 0.37 also made the system clipboard and the browser opener
explicit features (`clipboard`, `links`) in its default set. Cobalt lists eframe's features by
name (`default-features = false`), so 0.14.0 and 0.15.0 shipped without either: copy and paste
worked inside the app and nowhere else, links opened nothing. When bumping eframe, diff its
`default` feature list against ours.
