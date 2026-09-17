# D007 — egui_agent v0.3.0 integrated from the first commit

**Decided:** 2026-09-16

## Decision
The app depends on `egui_agent = { git = "https://github.com/methodify/egui_agent", tag = "v0.3.0",
features = ["pipe", "accesskit-introspection", "screenshot"] }` behind an `agent` cargo feature
(on in dev, off in release). Windows uses `Transport::Pipe("cobalt.agent")`, Unix uses UDS.
Enabled only when `COBALT_AGENT=1`. Key surfaces (connect, run query, grid, export) expose
semantic `dispatch` verbs so Claude can drive the app end to end during the build.
`egui-agent-cli` / `egui-agent-mcp` v0.3.0 are installed on the founder's box.

## Why
The founder built egui_agent for exactly this; it turned two prior egui builds "into a piece of
cake." It also gives an in-process test harness (`TestHarness` over egui_kittest) for CI.
