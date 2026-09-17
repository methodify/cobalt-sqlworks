# Using egui_agent from GitHub

The crates aren't on crates.io (yet); everything below consumes them straight from
`https://github.com/methodify/egui_agent` — no local clone needed. Cargo fetches git
dependencies itself, and `cargo install --git` builds the binaries directly from the repo.

## 1. Add the crate to your egui/eframe app

In your app's `Cargo.toml`:

```toml
[dependencies]
egui_agent = { git = "https://github.com/methodify/egui_agent", features = ["glow"] }

# Optional: #[derive(Snapshot)] for exposing app-state structs
egui_agent_macros = { git = "https://github.com/methodify/egui_agent" }
```

Cargo locates each crate inside the workspace by package name automatically. The first
build records the exact commit in your `Cargo.lock`; later `cargo update` will move it.
To pin explicitly, use a release tag (see `CHANGELOG.md` for versions):

```toml
egui_agent = { git = "https://github.com/methodify/egui_agent", tag = "v0.2.1", features = ["glow"] }
```

The wire protocol can change between 0.x versions (e.g. 0.2.0 changed node-id
encoding) — keep the app's crate and the installed tools on the same version.

Two version notes that matter:

- **Your app must use egui/eframe 0.35.** egui types (`egui::Context`, `egui::Ui`) cross
  the API boundary, so a mismatched egui version fails to compile with confusing
  "expected `egui::Context`, found `egui::Context`" errors.
- **The `glow` feature must match your renderer.** eframe's default renderer is glow, and
  eframe's `glow` feature changes the `App::on_exit` signature — so with a default eframe
  setup, enable `egui_agent/glow` (as shown above). If you run eframe with
  `default-features = false` and wgpu instead, omit it.

Useful optional features:

| Feature | What you get |
|---|---|
| `uds` *(default)* | Unix-socket transport (Linux/macOS) |
| `accesskit-introspection` | Auto-snapshot of every labeled widget + generic click/type on plain egui widgets, zero per-widget code |
| `screenshot` | The `screenshot` command (PNG of the current frame) |
| `pipe` | Windows named-pipe / cross-platform local-socket transport |
| `ws` | WebSocket transport on 127.0.0.1 |
| `test-harness` | In-process `TestHarness` for integration tests (put it in `dev-dependencies`) |

## 2. Wire it up (one trait + one wrap call)

```rust
use egui_agent::{Action, ActionResult, AgentApp};

#[derive(Default)]
struct MyApp {
    query: String,
    results: Vec<String>,
}

impl MyApp {
    fn run_search(&mut self) { /* the same method your Search button calls */ }
}

impl AgentApp for MyApp {
    // Optional: route app-level verbs to the same methods your UI calls.
    // Widgets drawn with egui_agent::widgets::* (or, with the
    // accesskit-introspection feature, ANY labeled egui widget) already work
    // without writing anything here — the defaults cover them.
    fn dispatch(&mut self, action: &Action, _ctx: &egui::Context) -> ActionResult {
        match action.name() {
            "run_search" => {
                self.run_search();
                ActionResult::with(&self.results.len())
            }
            _ => ActionResult::Unhandled, // falls through to automatic actuation
        }
    }
}

impl eframe::App for MyApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Registered widget: agent-visible under the stable id "query".
        egui_agent::widgets::text_edit_singleline(ui, "query", &mut self.query);
        if egui_agent::widgets::button(ui, "search", "Search").clicked() {
            self.run_search();
        }
        for r in &self.results {
            ui.label(r);
        }
    }
}

fn main() -> eframe::Result {
    let control = egui_agent::Control::spawn(egui_agent::Config {
        transport: egui_agent::Transport::UnixSocket("/tmp/myapp.agent.sock".into()),
        auth: egui_agent::Auth::from_env("MYAPP_AGENT_TOKEN"), // optional token
        enabled: std::env::var("MYAPP_AGENT").is_ok(),         // inert unless opted in
        ..Default::default()
    });

    eframe::run_native("MyApp", Default::default(), Box::new(move |cc| {
        control.bind_context(cc.egui_ctx.clone());
        Ok(Box::new(egui_agent::wrap(MyApp::default(), control)))
    }))
}
```

Run it opted-in:

```console
$ MYAPP_AGENT=1 cargo run
```

The app behaves identically for human users; the socket only exists when `MYAPP_AGENT` is set.

**Hardening for release builds** (recommended, design doc §13): make the dependency
optional and gate every `egui_agent::` call behind your own cargo feature, so release
builds compile the control channel out entirely:

```toml
[features]
agent = ["dep:egui_agent"]

[dependencies]
egui_agent = { git = "https://github.com/methodify/egui_agent", features = ["glow"], optional = true }
```

## 3. Install the MCP server and CLI (no clone needed)

```console
$ cargo install --git https://github.com/methodify/egui_agent egui_agent_mcp
```

(Add `--tag v0.2.1` to install a pinned release matching your app's dependency.)

This installs two binaries into `~/.cargo/bin`:

- `egui-agent-mcp` — MCP server (stdio) that bridges an MCP host to your app's socket
- `egui-agent-cli` — command-line/REPL driver for scripts and quick pokes

## 4. Connect an MCP host

The server speaks MCP over stdio and forwards each tool call to the running app over its
Unix socket. Point any MCP host at it:

**Claude Code:**

```console
$ claude mcp add myapp -- egui-agent-mcp --socket /tmp/myapp.agent.sock
```

**Generic JSON config** (Claude Desktop and most other hosts):

```json
{
  "mcpServers": {
    "myapp": {
      "command": "egui-agent-mcp",
      "args": ["--socket", "/tmp/myapp.agent.sock"],
      "env": { "EGUI_AGENT_TOKEN": "your-token-if-configured" }
    }
  }
}
```

**On Windows**, connect to the app's named pipe instead of a socket:

```json
{
  "mcpServers": {
    "myapp": {
      "command": "egui-agent-mcp",
      "args": ["--pipe", "myapp.agent"]
    }
  }
}
```

The connection to the app is lazy: the MCP server can start before the app does, and it
reconnects once after an app restart mid-session.

The host gets seven tools: `snapshot`, `query`, `invoke`, `set_value`, `wait_for`,
`screenshot`, `custom`. A typical agent session: call `snapshot` once to discover node
ids/labels/actions, then act on stable ids:

```
snapshot                                       → see {"id": "query"}, {"id": "search"}, ...
set_value {"target": {"id": "query"}, "value": "rust"}
invoke    {"target": {"id": "search"}, "action": "click"}
wait_for  {"predicate": {"exists": {"label": "results: 10"}}, "timeout_ms": 3000}
```

Notes for hosts/agents: node ids in snapshots and `{"node": ...}` selectors are
**strings** (they exceed JSON's safe integer range); `args`/`value` must be typed
JSON values, not JSON-encoded strings (structured args arriving as strings are
tolerated and re-parsed, except `set_value.value`); pass `"quiet": true` on
`invoke`/`set_value`/`wait_for` to omit the echoed snapshot when context is tight.

## 5. Drive it from the shell

```console
$ export EGUI_AGENT_SOCKET=/tmp/myapp.agent.sock   # or pass --socket each time

$ egui-agent-cli snapshot
$ egui-agent-cli set-value '{"id":"query"}' '"rust"'
$ egui-agent-cli invoke click --target '{"id":"search"}'
$ egui-agent-cli invoke run_search                       # app-level dispatch verb
$ egui-agent-cli wait-for '{"exists":{"label":"Done"}}' --timeout 3000
$ egui-agent-cli screenshot
$ egui-agent-cli repl                                    # raw Command JSON, line by line
```

Responses are pretty-printed JSON; errors are structured (`target_not_found`,
`ambiguous_selector`, `action_rejected`, `timeout`, `unauthorized`, `read_only`, ...),
and the process exits nonzero on error, so the CLI scripts cleanly.

## 6. Use it in your app's own tests

The same command layer runs in-process under `egui_kittest` — no socket, no display
server, CI-friendly:

```toml
[dev-dependencies]
egui_agent = { git = "https://github.com/methodify/egui_agent", features = ["glow", "test-harness"] }
```

```rust
#[test]
fn search_flow() {
    let mut h = egui_agent::TestHarness::new(MyApp::default(), |app, ui| {
        // your per-frame UI, same code as eframe::App::ui
    });
    h.send(egui_agent::Command::SetValue {
        target: egui_agent::Selector::Id("query".into()),
        value: "rust".into(),
    })
    .unwrap();
    h.send(egui_agent::Command::Invoke {
        target: Some(egui_agent::Selector::Id("search".into())),
        action: "click".into(),
        args: None,
    })
    .unwrap();
    assert!(!h.app().results.is_empty());
}
```

## 7. Windows

There is no UDS on Windows; use the `pipe` transport (a Windows named pipe under the
hood, via the `interprocess` crate) on both sides instead.

App side:

```toml
egui_agent = { git = "https://github.com/methodify/egui_agent", features = ["glow", "pipe"] }
```

```rust
transport: egui_agent::Transport::Pipe("myapp.agent".into()),
```

Tool side — everything accepts `--pipe <NAME>` (or `EGUI_AGENT_PIPE`) in place of
`--socket`:

```console
$ egui-agent-cli --pipe myapp.agent snapshot
$ egui-agent-mcp --pipe myapp.agent          # for MCP host configs, see §4
```

The pipe name is just a name (no `\\.\pipe\` prefix, no filesystem path) and must
match between `Transport::Pipe("...")` and `--pipe ...`. A cross-platform app can use
UDS on Linux/macOS and the pipe on Windows:

```rust
#[cfg(unix)]
let transport = egui_agent::Transport::UnixSocket("/tmp/myapp.agent.sock".into());
#[cfg(not(unix))]
let transport = egui_agent::Transport::Pipe("myapp.agent".into());
```

The `pipe` transport also works on Linux/macOS (as a namespaced Unix socket), so the
whole flow is testable on either platform.

## Troubleshooting

- **"cannot reach app at ..."** — the app isn't running, wasn't started with its
  opt-in env var set, or the socket path / pipe name differs.
- **"--socket ... is not available on Windows"** — Unix domain sockets don't exist
  there; serve `Transport::Pipe("name")` in the app and pass `--pipe name` (see §7).
- **`unauthorized`** — the app was configured with an auth token; pass `--token` /
  `EGUI_AGENT_TOKEN` to the CLI or set `env` in the MCP config.
- **`read_only`** — the app enabled `Config::read_only`; observation commands still work.
- **Empty snapshot right after launch** — snapshots describe the last *completed* frame;
  issue a `wait_for { exists: ... }` first (the MCP tool description says the same).
- **on_exit / trait mismatch compile errors** — your eframe has `glow` enabled (the
  default) but `egui_agent/glow` isn't; add the feature (see §1).
