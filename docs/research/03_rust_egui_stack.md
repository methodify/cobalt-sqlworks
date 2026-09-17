# Rust ecosystem survey for a pure-Rust egui SQL client (state as of September 2026)

Scope: a cross-platform (Windows first, then Linux, macOS) desktop SQL client in Rust + egui, replacing Azure Data Studio for SQL Server / Azure SQL / Microsoft Fabric DW, with PostgreSQL/MySQL/DuckDB/SQLite later. Everything below was checked against crates.io / lib.rs / docs.rs / GitHub during this session unless tagged **[memory]** (recalled, not verified this session). Version numbers are "latest at time of writing" and will drift.

Headline findings:

1. The SQL Server driver landscape changed drastically in August/September 2026. The original `tiberius` (0.12.3, last release July 2024) is effectively unmaintained and carries open RustSec advisories; three successors exist: `tiberius-ng` (community, feature-rich), `mssql` (community, security-patch-focused), and Microsoft's official `mssql-tds` 0.1.0 (published 10 Sep 2026) with a tiberius-compatible shim `mssql-tiberius-bridge`.
2. `azure_identity` went GA (1.0) in 2026 but ships **no** interactive-browser or device-code credential, so an MFA-capable desktop app must implement the user-facing OAuth flows itself (or use the unofficial `azure-identity-helpers`).
3. egui is on 0.36.x with a large, mostly up-to-date third-party ecosystem: `egui_table` (Rerun) for millions of rows, `egui_dock`/`egui_tiles` for layout, `egui-snarl` for node graphs, `egui_ltreeview` for trees, all tracking egui 0.36.
4. The Arrow stack is healthy but version-skewed: `arrow`/`parquet` 60.0, `datafusion` 55.1 (arrow 59.2), `deltalake` 0.32.4 (arrow 58). Pin carefully.

---

## 1. TDS / SQL Server drivers

### 1.1 `tiberius` (original, Prisma lineage)

- Latest: **0.12.3**, released 19 July 2024 (lib.rs shows no release since). ~235k downloads/month. Dual MIT/Apache-2.0. https://lib.rs/crates/tiberius, https://docs.rs/tiberius
- Repo moved from `prisma/tiberius` to a `tiberius-rs/tiberius` org (105 open issues, 39 open PRs at time of check). https://github.com/tiberius-rs/tiberius
- Runtime-agnostic (any `AsyncRead + AsyncWrite`), works with tokio/async-std/smol.
- Auth (`AuthMethod`): `SqlServer` (user/password), `Windows` (NTLM; on Windows via `winauth`, on Unix via optional `sspi-rs` feature), `Integrated` (SSPI on Windows; Kerberos via `integrated-auth-gssapi` feature on Unix, which needs system GSSAPI libs; on macOS uses Apple GSS framework), `AADToken` via `AuthMethod::aad_token(jwt)`. https://docs.rs/tiberius/latest/tiberius/enum.AuthMethod.html
- TLS: `native-tls` (default) or `rustls` feature; `vendored-openssl`. TDS 7.2-7.4; **no TDS 8.0 strict encryption** in 0.12.3.
- Multiple result sets: yes (`QueryStream` yields `QueryItem::Metadata`/`Row` per result set; `into_results()`). Row streaming: yes.
- Bulk insert: `BulkLoadRequest` exists.
- **MARS: not supported** (issue #142 open since 2021). https://github.com/prisma/tiberius/issues/142
- **Info messages (PRINT/RAISERROR severity <= 10)**: parsed as TDS INFO tokens but only emitted through `log`; not surfaced to callers as a typed event **[memory]**.
- **Query cancellation (attention packet)**: not implemented in 0.12.3 **[memory]**; added in forks (below).
- Maintenance: the `tiberius-ng` announcement (31 Aug 2026) states upstream has unanswered issues/PRs and open, unpatched cert-verification advisories (RUSTSEC-2026-0098 / -0099 / -0104) plus an `h2` advisory. https://users.rust-lang.org/t/tiberius-ng-a-maintained-continuation-of-the-tiberius-sql-server-driver/142157
- Pooling: `bb8-tiberius` 0.16.0, `deadpool-tiberius` 0.1.9. https://docs.rs/bb8-tiberius, https://docs.rs/deadpool-tiberius

**Recommendation:** do not start a new project on `tiberius` 0.12.3 itself.

### 1.2 `tiberius-ng` (community continuation, Matt Jackson)

- Latest: **0.13.1**, 30 Aug 2026. Crate name `tiberius-ng`, library name still `tiberius` (drop-in). MIT/Apache-2.0. ~1.2k downloads/month (very new). https://lib.rs/crates/tiberius-ng, https://github.com/MattJackson/tiberius-ng
- Adds over 0.12.3: **query cancellation (attention)**, **TDS 8.0 strict encryption**, MultiSubnetFailover, **MARS**, bulk insert (whole-table and column-list), **info-message exposure (PRINT/RAISERROR to application code)**, connection reset, `sql_variant`/UDT/ALTMETADATA decoding, transactions; `cargo audit`/`cargo-deny` clean. CI against SQL Server 2017/2019/2022/2025 and Azure SQL Edge.
- Features: `tds73`, `tds80`, `native-tls`, `rustls`, `vendored-openssl`, `chrono`/`time`, `rust_decimal`/`bigdecimal`, `serde`, `integrated-auth-gssapi`, `sspi-rs`.
- Limitations: single maintainer, weeks old; full commit history preserved from upstream, so the codebase is mature even if the fork is not.

### 1.3 `mssql` (mssql-rust org fork)

- Latest: **1.0.2**, 1 Sep 2026, MIT/Apache-2.0, ~100 downloads/month. Explicit goal: "ongoing maintenance and security updates over large rewrites", small focused commits. Features mirror tiberius (`tds73`, `winauth`, `native-tls`, `rustls`, etc.). https://lib.rs/crates/mssql, https://github.com/mssql-rust/mssql-rust
- Does not claim MARS/cancellation/info-message additions.

### 1.4 Microsoft official: `mssql-tds` (microsoft/mssql-rs)

- Latest: **0.1.0**, 10 Sep 2026, MIT, owner `microsoft-oss-releases`. Repo: https://github.com/microsoft/mssql-rs (workspace: `mssql-tds` core, `mssql-odbc` cdylib implementing the msodbcsql18 interface, `mssql-js` experimental NAPI bindings, `mssql-py-core` for mssql-python, mock TDS server, CLI). https://lib.rs/crates/mssql-tds, https://docs.rs/mssql-tds
- Tokio-only, Rust 2024 edition, `native-tls` with ALPN (no rustls option listed). Features: `integrated-auth` (default; `sspi` on Windows, `gssapi` via dlopen on Unix).
- Documented capabilities: SQL auth, **Entra ID access tokens**, integrated auth; `CancelHandle` for **query cancellation**; **MARS**; bulk copy; multiple result sets with `next_row()` streaming; **info messages / PRINT exposed**; Always Encrypted crypto module.
- Maturity: first public release; Microsoft intends it to be the shared TDS core for `mssql-python` and a Rust-based ODBC driver, so investment is likely. API is low-level (`TdsClient`, `ClientContext`, `TdsConnectionProvider`).

### 1.5 `mssql-tiberius-bridge`

- Latest: **0.1.0**, 11 Sep 2026, MIT, ~6.7k downloads/month. A tiberius-compatible API over `mssql-tds` 0.1.0 so tiberius code migrates with minimal changes. Features: `arrow` (Arrow array/schema integration), `jiff`, `serde`, `time`; deadpool pooling; AAD/Entra federated auth; spatial types; connection health checks. https://lib.rs/crates/mssql-tiberius-bridge, https://github.com/saurabh500/mssql-tiberius-bridge
- Not under the `microsoft` org; author's affiliation unverified. Tabularis's SQL Server plugin uses it and drives SHOWPLAN_XML/STATISTICS XML through it (see section 7).

### 1.6 `mssql-client` (praxiomlabs/rust-mssql-driver)

- Latest: **0.20.2**, 7 Jul 2026, MIT/Apache-2.0, ~600 downloads/month, single maintainer, 850+ commits, integration tests against real SQL Server in CI. https://lib.rs/crates/mssql-client, https://github.com/praxiomlabs/rust-mssql-driver
- Tokio-only, pure-Rust rustls, TDS 7.3-8.0, built-in pool, type-state connections, streaming rows and blob sub-streams, transactions/savepoints, OUTPUT params, TVPs, bulk insert, Always Encrypted read/write, OpenTelemetry; optional `sspi-auth`, `azure-identity` (Managed Identity, service principal, default chain), `always-encrypted`.
- **No MARS**; parameterized queries go through `sp_executesql` by default; Kerberos not production-validated.

### 1.7 ODBC fallback: `odbc-api` + `arrow-odbc`

- `odbc-api` **29.0.0** (19 Jul 2026), MIT; tested against SQL Server, PostgreSQL, MariaDB, SQLite; needs the platform driver manager (built-in on Windows, unixODBC on Linux/macOS). https://lib.rs/crates/odbc-api
- `arrow-odbc` **25.3.0** (20 Jul 2026): ODBC result sets straight into Arrow `RecordBatch`es and back; depends on `arrow >=29,<60` and `odbc-api >=27,<30`. https://docs.rs/crate/arrow-odbc/latest
- Value: gives you Microsoft's own `msodbcsql18` and therefore every auth mode (Entra interactive with MFA via the driver's own browser/WAM flow, Kerberos, Always Encrypted) for free, plus a path to any ODBC source. Cost: a native dependency users must install, and worse streaming/cancellation control than a native TDS driver.

### 1.8 Execution plans, messages, cancellation - what the driver has to provide

- `SET SHOWPLAN_XML ON` / `SET STATISTICS XML ON` are ordinary T-SQL; the plan arrives as an extra result set with a single `nvarchar(max)` XML column (SHOWPLAN_XML returns only plans; STATISTICS XML returns data result sets followed by a plan result set per statement). Any driver that handles **multiple result sets on one batch** and large NVARCHAR streaming works. **[memory]**, corroborated by Tabularis doing exactly this with tiberius/`mssql-tiberius-bridge`. https://tabularis.dev/roadmap/sql-server
- MARS is **not** required for plans, but you want it (or a second connection) for "browse object explorer while a query runs".
- INFO tokens (PRINT, RAISERROR severity <= 10, `SET STATISTICS IO/TIME` output) must be surfaced as events: available in `tiberius-ng` and `mssql-tds`; not in `tiberius` 0.12.3.
- Cancellation needs an attention packet plus draining the `DONE` with attention-ack: `tiberius-ng` and `mssql-tds` implement it.
- Fabric DW / SQL analytics endpoint and Synapse speak standard TDS with Entra tokens; Fabric requires TLS and Entra (no SQL auth). Any of the above drivers with `aad_token` works provided the token audience is `https://database.windows.net/`. https://learn.microsoft.com/en-us/fabric/data-warehouse/entra-id-authentication

**Driver recommendation:** abstract the driver behind your own trait from day one. Start on **`tiberius-ng`** (drop-in, has every needed TDS feature today, rustls option, runtime-agnostic) and keep **`mssql-tds` / `mssql-tiberius-bridge`** as the planned migration target once it has a few releases and a rustls path; keep `odbc-api`/`arrow-odbc` as an opt-in "use system ODBC driver" connection type for edge cases (Always Encrypted enclaves, exotic auth).

---

## 2. Entra ID / Azure AD and Windows integrated auth

### 2.1 `azure_identity`

- GA: Azure SDK for Rust reached stable 1.0 for core, identity, Key Vault, Blob, Queue (announcement May 2026); `azure_identity` **1.0.0** on docs.rs dated 12 Jul 2026. MIT. https://devblogs.microsoft.com/azure-sdk/from-beta-to-stable-announcing-the-azure-sdk-for-rust-ga/, https://docs.rs/azure_identity
- Credentials shipped: `AzureCliCredential`, `AzureDeveloperCliCredential`, `DeveloperToolsCredential` (dev-time chain), `ClientSecretCredential`, `ClientCertificateCredential`, `ClientAssertionCredential`, `ManagedIdentityCredential`, `WorkloadIdentityCredential`, `AzurePipelinesCredential`.
- **Missing: `InteractiveBrowserCredential`, `DeviceCodeCredential`, `DefaultAzureCredential`** (the Rust SDK deliberately replaced the latter with `DeveloperToolsCredential`; Microsoft's guidance is `AzureCliCredential` for local dev). https://learn.microsoft.com/en-us/azure/developer/rust/sdk/authentication/overview
- Token acquisition is `TokenCredential::get_token(&["https://database.windows.net/.default"], None)`; the resulting access token string goes to `AuthMethod::aad_token`.

### 2.2 Filling the interactive gap

- `azure-identity-helpers` **0.2.0** (31 Jul 2026, author demoray, MIT, "unofficial"): device-code flow, env-var service principal, credential chaining, AzureAuth CLI integration, refresh-token handling, a Go-style `DefaultAzureCredential`. Builds on `azure_identity ^1.0`. https://docs.rs/azure-identity-helpers
- There is no Rust MSAL **[memory]**; no WAM/broker integration on Windows from Rust **[memory]**. For an ADS-class experience ("Azure MFA" login), implement OAuth 2.0 **authorization-code + PKCE with a loopback redirect** (open the system browser via `open`, listen on `http://localhost:<port>`) using the `oauth2` crate **[memory]** and the v2 endpoints `https://login.microsoftonline.com/{tenant}/oauth2/v2.0/...`. Register a public-client app (or reuse a well-known one - ADS/SSMS use Microsoft's first-party client IDs, which you should not rely on **[memory]**). Persist the refresh token in the OS keychain (section 6). Device-code flow is the fallback for headless/remote-desktop scenarios. Tokens for SQL are audience `https://database.windows.net/` with `.default` scope; Fabric accepts the same audience.
- `AzureCliCredential` is a pragmatic v1 path for developers who already run `az login`.

### 2.3 Windows integrated / Kerberos / NTLM

- On Windows: `tiberius`/`tiberius-ng` `AuthMethod::Integrated` uses SSPI through the `winauth` crate; Microsoft's `mssql-tds` uses SSPI directly (feature `sspi`). Works out of the box for domain-joined machines.
- Cross-platform pure Rust: `sspi` (Devolutions sspi-rs) **0.22.0**, 15 Sep 2026 - NTLM, Kerberos, PKU2U, Negotiate; uses native SSPI on Windows. Note the 0.22.0 SPNEGO mechListMIC regression (issue #748) - pin carefully. https://lib.rs/crates/sspi, https://github.com/Devolutions/sspi-rs
- Linux/macOS Kerberos via `integrated-auth-gssapi` needs MIT/Heimdal GSSAPI and a TGT (`kinit`). Fine for Linux servers; on macOS the Apple GSS framework is used. For most Windows-first users, Entra tokens will replace this anyway.

---

## 3. egui ecosystem

### 3.1 Core

- `egui` / `eframe` / `egui_extras` **0.36.2** (8 Sep 2026); 0.34.0 on 26 Mar 2026, 0.35.0 on 25 Jun 2026, 0.36.0 Aug 2026. MIT/Apache-2.0. https://github.com/emilk/egui/releases, https://docs.rs/crate/eframe/latest
- Notable recent changes: 0.34 switched text rendering to `skrifa` with hinting (sharper text) and a "more `Ui`, less `Context`" API refactor plus unified panel API; 0.35 added harfbuzz kerning/ligatures, IME composition underlines, CSS-like widget "classes", and the `egui_inspection` protocol (drive the AccessKit tree remotely on port 5719 with `EGUI_INSPECTION=1` - useful for UI tests); 0.36 improved mobile IME/autocomplete, drag-to-reopen panels, window decorations following app theme, `TextEdit::event_filter`.
- Breaking changes every ~3 months; third-party crates typically lag a few weeks. Budget for a quarterly "bump egui" chore.
- Renderer: `eframe` **defaults to wgpu**; `glow` is opt-in and reduces binary size (issue #5889 tracked the switch). wgpu gives DX12 on Windows, Metal on macOS, Vulkan on Linux, and a WebGL fallback since 0.34.1. Use wgpu; keep `glow` feature available for VM/RDP fallback where GPU drivers are poor **[memory]**. https://github.com/emilk/egui/issues/5889, https://docs.rs/eframe/latest/eframe/enum.Renderer.html
- Accessibility: `accesskit` **0.25.0** (29 Aug 2026) - Windows UIA, macOS NSAccessibility, Linux AT-SPI, Android; enabled by default in eframe. https://lib.rs/crates/accesskit
- Clipboard: `arboard` **3.6.1** (Aug 2025), text + image; Wayland via `wayland-data-control` feature. egui_winit uses it. https://lib.rs/crates/arboard
- HiDPI: winit-driven, per-monitor DPI works on all three platforms; IME works natively on desktop (issue #248 long closed) and got the composition-underline treatment in 0.35 **[memory + release notes]**.
- Modals: egui has a built-in `egui::Modal` since 0.30 **[memory]**; the third-party `egui-modal` is at 0.6.0 (~10 months old) and no longer necessary.

### 3.2 Tables / grids (the critical piece)

- **`egui_table` 0.10.0** (5 Aug 2026, egui 0.36, MSRV 1.95, MIT/Apache, Rerun). Virtualized: "support for millions of rows", auto-sized/resizable columns, hierarchical headers, sticky columns and header, heterogeneous row heights, `scroll_to_row`, `stick_to_bottom`, `row_ui` interaction. Delegate-based: you render only the cells egui asks for, so the data source can be an Arrow batch store. https://github.com/rerun-io/egui_table, https://docs.rs/crate/egui_table/latest
- `egui_extras::TableBuilder` 0.36.2: simpler; virtualizes homogeneous-height rows via `body.rows(height, count, |row| ..)` **[memory]**; adequate for small grids (object explorer detail panes), not for the main results grid.
- `egui-data-table` 0.11.0 (Aug 2026, egui 0.35, custom license): editable table with undo/redo, column hide/reorder, clipboard; not the right fit for read-mostly million-row grids.
- Proof point: Rerun's viewer (egui + wgpu) renders its dataframe view with `egui_table` over Arrow chunks; Rerun 0.18+ handles datasets with millions of time points after a 100x ingestion / 35x memory-overhead rework. https://rerun.io/blog/column-chunks, https://github.com/rerun-io/rerun/releases/tag/0.19.0

### 3.3 Layout / docking

- **`egui_dock` 0.21.1** (6 Aug 2026, egui 0.36, MIT, ~74k dl/mo): tabs, drag tabs between nodes, tear-out into separate windows, resizable splits (binary splits only). https://lib.rs/crates/egui_dock
- **`egui_tiles` 0.17.1** (18 Aug 2026, egui 0.36, MIT/Apache, Rerun/emilk, ~197k dl/mo): horizontal/vertical/grid containers + tabs, drag-and-drop; behavior via a `Behavior` trait; used by Rerun. https://lib.rs/crates/egui_tiles
- Both are viable. `egui_dock` has the more IDE-like tab UX (tear-out windows); `egui_tiles` has more flexible layouts and the same maintainer as egui.

### 3.4 Other widgets

| Crate | Version / date | egui | License | Notes |
|---|---|---|---|---|
| `egui_code_editor` | 0.4.1, 18 Aug 2026 | 0.36 | MIT | numbered lines, keyword-set highlighting, autocomplete from syntax dict + user words, themes. https://lib.rs/crates/egui_code_editor |
| `egui_commonmark` | 0.25.0, 5 Aug 2026 | 0.36 | MIT/Apache | GFM tables, syntect highlighting feature, images. https://lib.rs/crates/egui_commonmark |
| `egui-notify` | 0.23.0, 4 Sep 2026 | 0.36 | MIT | toasts. https://lib.rs/crates/egui-notify |
| `egui_plot` | 0.37.0, 5 Aug 2026 | 0.36 | MIT/Apache | 2D plots (statistics charts). https://lib.rs/crates/egui_plot |
| `egui-snarl` | 0.12.0, 26 Aug 2026 | 0.36 | MIT/Apache | node graph: typed nodes, 5-region node layout, wires, context menus, serde. https://lib.rs/crates/egui-snarl |
| `egui_graphs` | 0.32.0, 31 Aug 2026 | 0.36 | MIT | petgraph 0.8 viewer with hierarchical + force layouts; README says "not in active development, fork it". https://lib.rs/crates/egui_graphs |
| `egui_node_graph2` | 0.7.0, Nov 2024 | 0.29 | MIT | stale; avoid. https://lib.rs/crates/egui_node_graph2 |
| `egui_virtual_list` | 0.12.0, 6 Aug 2026 | 0.36 | MIT | variable-height virtual list (messages pane, history). https://lib.rs/crates/egui_virtual_list |
| `egui_inbox` | 0.13.0, 6 Aug 2026 | 0.36 | MIT | channel that auto-`request_repaint`s. https://lib.rs/crates/egui_inbox |
| `egui_dnd` | 0.17.0, 6 Aug 2026 | 0.36 | MIT | drag-and-drop lists (column reordering). https://lib.rs/crates/egui_dnd |
| `egui_ltreeview` | 0.9.0, 23 Aug 2026 | 0.36 | MIT | tree view with dir/leaf nodes, multi-select, keyboard nav, drag-and-drop; stores minimal state. No built-in lazy loading, but since it is immediate-mode you populate children on expand yourself. https://lib.rs/crates/egui_ltreeview |
| `egui-file-dialog` | 0.15.0, 8 Aug 2026 | 0.36 | MIT | in-egui dialog (consistent, no native deps). https://lib.rs/crates/egui-file-dialog |
| `rfd` | 0.17.2, 12 Jan 2026 | n/a | MIT | native dialogs; Linux via XDG portal (default) or GTK3. Prefer native on desktop. https://lib.rs/crates/rfd |
| `egui-phosphor` | 0.14.0, 10 Sep 2026 | 0.36 | MIT/Apache | icon font, subsetting. https://lib.rs/crates/egui-phosphor |
| `egui_material_icons` | 0.8.0, 6 Aug 2026 | 0.36 | MIT | Material Symbols. https://lib.rs/crates/egui_material_icons |
| `catppuccin-egui` | 5.7.0, 15 Nov 2025 | 0.26-0.33 only | MIT | lags egui; treat as a palette source, not a dependency. https://lib.rs/crates/catppuccin-egui |
| `egui-modal` | 0.6.0, ~Nov 2025 | older | MIT | superseded by `egui::Modal`. |

**Execution plan viewer:** `egui-snarl` is the healthiest node-graph crate, but its model is "editable graph with pins/wires"; a SQL plan is a read-only tree with auto-layout (Sugiyama/Reingold-Tilford), cost-weighted edges, tooltips and zoom/pan. Most teams end up writing a custom `egui::Painter`-based renderer (~1-2k lines) with a layered layout from `petgraph` or `layout-rs` **[memory]**; `egui_graphs`' hierarchical layout is a reference implementation but the crate is unmaintained. Plan for custom.

### 3.5 Async + egui pattern

egui is synchronous and repaints only on input unless asked. Standard pattern: spawn a `tokio` runtime on a background thread (`Runtime::new()` + `Handle`), send UI-bound events through a channel (`egui_inbox` or `std::sync::mpsc` + `ctx.request_repaint()`), and never block the UI thread. Discussions #521/#484/#4415 in the egui repo document the pattern; `egui-async` and `egui_inbox` package it. https://github.com/emilk/egui/discussions/521, https://docs.rs/egui-async

---

## 4. Code editor

### 4.1 Options

1. **`egui_code_editor` 0.4.1**: quickest path; keyword-set highlighting is fine for a first T-SQL mode and it already has a simple autocomplete popup. Limits: not a real lexer (strings/comments handled heuristically), single-cursor, no folding, no diagnostics gutter.
2. **`egui::TextEdit` + custom `layouter`**: the egui demo's code editor does this. You hand egui a `LayoutJob` per frame from your own tokenizer. Highlighter choices:
   - `syntect` **5.3.0** (27 Sep 2025, MIT): Sublime grammars; default syntax set includes SQL/T-SQL packages **[memory]**; `onig` (C) vs `fancy-regex` (pure Rust, ~half speed). Per-frame re-highlighting of large scripts needs caching by line. https://lib.rs/crates/syntect
   - `tree-sitter` with **`tree-sitter-mssql` 0.1.6** (meloncholic, MIT, tree-sitter 0.27) or `tree-sitter-sequel-tsql`; `tree-sitter-sql` (DerekStride) is PostgreSQL-flavoured. Incremental parsing gives structure (statement boundaries, aliases in scope) for autocomplete, not just colours. Coverage of T-SQL grammars is unproven - expect gaps. https://docs.rs/tree-sitter-mssql
3. **LSP client to an external server**: `async-lsp` (works as client and server, tower-based) or hand-rolled JSON-RPC over stdio with `lsp-types` (LSP 3.16). Microsoft's **SqlToolsService** (the engine behind ADS and the VS Code mssql extension) speaks LSP-style JSON-RPC over stdio and provides IntelliSense, connection/query services, and even plan retrieval; it is .NET, MIT, and ~100 MB self-contained per platform **[memory]**. It would give ADS-grade IntelliSense immediately at the cost of "pure Rust" and a large bundle. https://github.com/microsoft/sqltoolsservice, https://lib.rs/crates/async-lsp

### 4.2 Parsing/formatting

- `sqlparser` **0.63.0** (13 Sep 2026, Apache-2.0, DataFusion project). `MsSqlDialect` supports `[bracketed]` identifiers, `@`-identifiers, `CONVERT`/`TRY_CONVERT` argument order, `$money`, nested block comments, temporal `FOR SYSTEM_TIME`, `server..table`, `SET` without operator, `BEGIN/END TRY|CATCH`. It is a syntax-only parser without error recovery; procedural T-SQL (`DECLARE`/`WHILE`/`IF` blocks, `GO` batch separators, `EXEC` with named params) coverage is partial **[memory]** - good for statement splitting, alias/scope extraction for completion, and formatting; not a full T-SQL front end. https://docs.rs/sqlparser/latest/sqlparser/dialect/struct.MsSqlDialect.html
- `sqlformat` **0.5.0** (~Nov 2025, MIT): port of sql-formatter; dialect-agnostic, good enough for "Format document". https://lib.rs/crates/sqlformat

**Recommendation:** ship v1 on `TextEdit` + custom lexer-based `layouter` (own T-SQL tokenizer; ~300 lines, deterministic, fast), plus your own completion popup fed by (a) cached catalog metadata and (b) `sqlparser`/`tree-sitter-mssql` for alias resolution. Keep an LSP client behind a feature flag for optional SqlToolsService integration later.

---

## 5. Data and export stack

| Crate | Latest | License | Notes |
|---|---|---|---|
| `arrow` / `parquet` | **60.0.0**, 15 Sep 2026 | Apache-2.0 | monthly releases, major bump each quarter (planned 60.1 Sep 2026). https://lib.rs/crates/arrow, https://github.com/apache/arrow-rs |
| `datafusion` | **55.1.0**, 11 Sep 2026 | Apache-2.0 | depends on `arrow 59.2` (not 60 yet). SQL over RecordBatches, Parquet/CSV/JSON, joins across sources. https://lib.rs/crates/datafusion |
| `deltalake` | **0.32.4**, 7 Jun 2026 | Apache-2.0 | features `azure`, `s3`, `gcs`, `datafusion`, `rustls`; via `buoyant_kernel` (renamed delta_kernel) with **arrow-58**. Writes Delta tables to local paths and `abfss://` (ADLS Gen2 / **OneLake** uses the same ABFS endpoint) using `object_store`. https://lib.rs/crates/deltalake, https://delta-io.github.io/delta-rs/usage/writing/ |
| `object_store` | **0.14.1** | Apache-2.0 | local/Azure/S3/GCS; Azure backend takes bearer tokens, so the same Entra token flow can authorize OneLake writes **[memory]**. https://docs.rs/object_store/latest/object_store/azure/ |
| `rust_xlsxwriter` | **0.97.1**, 4 Aug 2026 | MIT/Apache | streaming-friendly, constant-memory mode, formats, autofilter. https://docs.rs/rust_xlsxwriter |
| `csv` | 1.4.0, Oct 2025 | Unlicense/MIT | |
| `serde_json` | 1.0.x | MIT/Apache | use streaming `Serializer` for large exports **[memory]** |
| `quick-xml` | 0.41.0 (0.42 announced) | MIT | writer API for XML export; also for parsing SHOWPLAN XML. https://github.com/tafia/quick-xml/blob/master/Changelog.md |
| `polars` | **0.55.2**, 6 Aug 2026 | MIT | own `polars-arrow`; conversion to/from arrow-rs exists but is a copy or a feature-gated bridge **[memory]**. Do not mix with arrow-rs in the core. |
| `duckdb` | **1.10505.0**, 22 Jul 2026 | MIT | `bundled` compiles DuckDB C++ (slow builds, needs MSVC/clang); `vtab-arrow` for Arrow in/out; can also read Parquet/Delta itself. https://lib.rs/crates/duckdb |

### 5.1 In-memory model for very large result sets

Hold results as a `Vec<RecordBatch>` (Arrow) built from the driver's row stream in chunks of e.g. 8-64k rows, with per-column builders (`StringBuilder`, `Decimal128Builder`, etc.). Advantages: columnar memory is 3-10x smaller than `Vec<Vec<Value>>` for typical SQL results **[memory]**, every exporter (Parquet, Arrow IPC, Delta, DataFusion, Excel, CSV) consumes it directly, and `egui_table` cell callbacks can index `batch.column(c).as_string::<i32>().value(r)` in O(1). Rerun does exactly this (Arrow chunks behind `egui_table`).

For "millions of rows" beyond RAM: write Arrow IPC (Feather) files to a temp dir with `arrow-ipc` `FileWriter`, then `mmap` them (`memmap2`) and read batches lazily; Arrow IPC is zero-copy mappable **[memory]**. Alternatively spill to DuckDB or Parquet and page through with DataFusion. Keep a "row cap with continue" UI like ADS/SSMS as the default to avoid surprising users.

`mssql-tiberius-bridge` already exposes an `arrow` feature that converts TDS rows to Arrow arrays; `arrow-odbc` does the same for ODBC. Either saves writing type mapping for the SQL Server path.

### 5.2 Version-skew warning

At the moment: `arrow` 60 vs `datafusion` 55.1 (arrow 59.2) vs `deltalake` 0.32.4 (arrow 58) vs `arrow-odbc` (<60). You cannot have one `arrow` version in the graph today unless you pin `arrow = 58` and wait for delta-rs to catch up. Pin to whatever `deltalake` supports and upgrade quarterly; isolate exporters in their own crates so a stale dependency does not block the main app.

---

## 6. Other infrastructure

- **Credentials:** `keyring` **4.2.0** (29 Aug 2026, MIT/Apache; 4.x split into `keyring-core` + per-platform store crates: `apple-native`, `windows-native`, `dbus-secret-service`, `linux-keyutils`; `v1` default feature keeps the 3.x API). Store SQL passwords and Entra refresh tokens here; on Linux require a Secret Service provider or fall back to encrypted file. https://lib.rs/crates/keyring
- **Paths:** `directories` (ProjectDirs: Known Folders on Windows, XDG on Linux, Standard Directories on macOS). https://docs.rs/directories
- **Local metadata DB:** `rusqlite` **0.40.2** (8 Aug 2026, MIT, bundled SQLite 3.53.2) for connection profiles, query history, snippets, catalog cache; use `bundled` feature. https://lib.rs/crates/rusqlite
- **Async:** `tokio` **1.53.1** (Jul 2026). **Logging:** `tracing` 0.1.44 + `tracing-subscriber` with a rolling file appender.
- **Files/OS:** `notify` 8.2.0 (9.0.0-rc available) for watching external edits of open scripts; `open` crate to launch the browser/OS handlers **[memory]**.
- **Packaging:**
  - `cargo-dist` **0.33.0** (10 Sep 2026): merged Astral's fork in 0.29, dropped Axo Releases, added **Azure Artifact Signing** for Windows binaries/installers, macOS signing options, GitHub attestations, MSI, shell/PowerShell/Homebrew/npm installers, CI generation. Best for CLI-style distribution; GUI bundles (.app/DMG/AppImage/deb) are thinner. https://github.com/axodotdev/cargo-dist/blob/main/CHANGELOG.md
  - `cargo-packager` **0.11.8** (Nov 2025, CrabNebula, MIT/Apache): NSIS + WiX MSI, DMG/.app, AppImage/deb/pacman, separate updater; no signing/notarization. https://lib.rs/crates/cargo-packager
  - `tauri-bundler` works without Tauri but drags Tauri config conventions **[memory]**.
  - Signing: Windows via Azure Trusted Signing (now "Azure Artifact Signing", ~USD 10/month, OIDC from GitHub Actions, `signtool` auto-selects it); macOS requires an Apple Developer ID cert + `notarytool`, which means a macOS runner. https://melatonin.dev/blog/code-signing-on-windows-with-azure-trusted-signing/, https://www.hanselman.com/blog/automatically-signing-a-windows-exe-with-azure-trusted-signing-dotnet-sign-and-github-actions
- **Cross-compilation:** `cargo-zigbuild` (0.8.x) links Linux gnu/musl and Windows-gnu targets from any host, with a selectable glibc floor - practical for Windows -> Linux CI. eframe on Linux needs X11/Wayland dev headers only at link time for some backends; wgpu is pure Rust. There is no supported Windows -> macOS cross-compile with signing; use `macos-latest` GitHub runners. `cross` (Docker) remains the alternative. https://github.com/rust-cross/cargo-zigbuild
- **DuckDB caveat:** the `bundled` C++ build adds several minutes to CI and needs a C++ toolchain on every target; consider `DUCKDB_DOWNLOAD_LIB=1` prebuilt libs or make DuckDB a later, optional feature.

---

## 7. Existing projects to learn from

- **DBX** (`t8y2/dbx`, 19.8k stars, Apache-2.0): Rust backend (sqlx / **tiberius** / redis-rs / mongodb) with Tauri 2 + Vue 3 + CodeMirror 6 front end; 90+ databases, visual explain plan, virtual-scrolled grid with CSV/JSON/Markdown/XLSX export, built-in MCP server. Not egui, but its Rust driver layer and feature list are a useful reference. https://github.com/t8y2/dbx
- **Tabularis** (`tabularis.dev`, ~5k stars, Apache-2.0): Rust (Tauri v2) + React; SQL Server support is a JSON-RPC plugin built on tiberius / `mssql-tiberius-bridge` with deadpool, schema introspection, CRUD/DDL, and **SHOWPLAN_XML / STATISTICS XML rendered in a Visual EXPLAIN**. Entra ID and Windows integrated auth are still on its roadmap - evidence that the auth gap is real and a differentiator. https://tabularis.dev/roadmap/sql-server, https://github.com/TabularisDB/tabularis-sqlserver-plugin
- **egui-based DB clients:** `omni-devel/rs-postgres` (PostgreSQL, egui) and `ggreco/postgres-e-gui` (PostgreSQL, egui + tokio-postgres, Sequel-Pro-inspired). Both small, single-database; useful for seeing how they wire tokio to egui. https://github.com/omni-devel/rs-postgres, https://github.com/ggreco/postgres-e-gui
- **TUIs:** `rainfrog` (Rust/ratatui, PostgreSQL primary, MySQL/SQLite unstable) https://github.com/achristmascarl/rainfrog; `gobang` (Rust TUI, mostly dormant **[memory]**); `lazysql` is Go (supports MSSQL). https://terminaltrove.com/lazysql/
- **Rerun** (`rerun-io/rerun`): the largest production egui + wgpu app; its dataframe view (0.19+) sits on `egui_table` over Arrow chunks; the team maintains `egui_table`, `egui_tiles`, `egui_plot` and co-maintains egui itself. Strong evidence egui can carry a data-heavy desktop tool. https://rerun.io/blog/dataframe

No existing open-source egui client targets SQL Server/Fabric with Entra MFA, plan visualisation, and Arrow-native export - that combination is open.

---

## 8. Recommended stack

### Core choices

| Concern | Choice | Why |
|---|---|---|
| GUI | `eframe`/`egui` 0.36 + wgpu (glow fallback feature) | maturity, Rerun proof, AccessKit built-in, fast iteration |
| Layout | `egui_dock` 0.21 (or `egui_tiles` if grid layouts matter more) | IDE-style tabs, tear-out windows |
| Results grid | `egui_table` 0.10 over Arrow `RecordBatch` chunks | virtualized to millions of rows, sticky headers/columns, heterogeneous heights |
| Object explorer | `egui_ltreeview` 0.9 with expand-on-demand loading | multi-select, keyboard nav, DnD |
| Editor | `TextEdit` + custom T-SQL lexer `layouter`; completion from catalog cache + `sqlparser` 0.63 `MsSqlDialect`; `sqlformat` for formatting; optional LSP client (`async-lsp`) to SqlToolsService later | full control, no C deps, incremental path to IntelliSense |
| Plan viewer | custom `Painter` renderer with layered layout (petgraph); parse plan XML with `quick-xml` | node-graph crates target editable graphs, not read-only trees |
| SQL Server driver | `tiberius-ng` 0.13 behind an internal `Driver` trait; migrate to `mssql-tds` / `mssql-tiberius-bridge` when it matures; `odbc-api` + `arrow-odbc` as optional connection type | cancellation, MARS, TDS 8, info messages, rustls today; official driver tomorrow |
| Entra auth | `azure_identity` 1.0 (`AzureCliCredential`, service principal, managed identity) + own auth-code+PKCE loopback flow via `oauth2` + device code; refresh tokens in `keyring` | SDK has no interactive credential; this is the ADS parity feature |
| Windows auth | driver SSPI on Windows; `sspi` crate NTLM / GSSAPI Kerberos elsewhere | |
| Data model | arrow-rs `RecordBatch` chunks; Arrow IPC spill files (`memmap2`) beyond a row threshold | shared by grid and all exporters |
| Exports | `csv`, `serde_json`, `quick-xml`, `rust_xlsxwriter`, `parquet`, `arrow-ipc`, `deltalake` (+ `object_store` Azure for OneLake/ADLS) | all Arrow-native except Excel/CSV/XML which stream from batches |
| Local analytics | `datafusion` 55 (SQL over exported results, cross-source joins); `duckdb` optional later | |
| Metadata/history | `rusqlite` 0.40 bundled | |
| Secrets/paths | `keyring` 4.2, `directories` | |
| Async/logging | `tokio` 1.53 on a background runtime + `egui_inbox`; `tracing` | |
| Icons/theme | `egui-phosphor`; own light/dark `Visuals` (Catppuccin palettes copied, crate not depended on) | |
| Packaging | `cargo-dist` 0.33 for CI + signed Windows MSI/installers (Azure Artifact Signing); `cargo-packager` for DMG/AppImage/deb; GitHub Actions matrix with macOS runners for notarization; `cargo-zigbuild` for Linux from Windows dev boxes | |

### Architecture notes

- Workspace crates: `core-driver` (trait + tiberius-ng impl), `core-arrow` (row -> RecordBatch, spill), `auth` (Entra flows, keyring), `export-*`, `ui`. Isolating `deltalake`/`datafusion` in their own crates contains the arrow version skew.
- One connection per editor tab plus a dedicated metadata connection for the object explorer; MARS only as an optimisation.
- All driver I/O on tokio; the UI thread only drains channels.

### Known risks

1. **Driver churn.** `tiberius-ng` is weeks old with one maintainer; `mssql-tds` is 0.1.0 and native-tls-only; the original `tiberius` has open advisories. Mitigation: driver trait, integration tests against SQL Server 2022 + Azure SQL + Fabric in CI, re-evaluate quarterly.
2. **Interactive Entra auth is on you.** No MSAL, no WAM broker, no `InteractiveBrowserCredential` in Rust. Conditional-access policies that require broker/device compliance may block a plain PKCE public client; device-code and `az login` fallbacks are essential.
3. **egui breaking releases every quarter** ripple through 15+ crates; some (catppuccin-egui, egui_node_graph2) already lag. Budget maintenance time; vendor small crates if they stall.
4. **Arrow version skew** across `arrow`/`datafusion`/`deltalake`/`arrow-odbc`.
5. **Text editing depth.** egui's `TextEdit` is single-cursor with basic selection; multi-cursor, folding, minimap and diagnostics squiggles are custom work. If ADS-level IntelliSense is a hard requirement, the SqlToolsService route (large .NET payload) is the only short-cut.
6. **Plan viewer is custom code** (layout, rendering, tooltips, cost heat-mapping); no crate solves it.
7. **DuckDB/`deltalake` build cost** (C++ for DuckDB; heavy dependency tree for delta-rs) - keep them optional features to protect compile times.
8. **Linux desktop variance** (Wayland clipboard needs the `wayland-data-control` feature, Secret Service may be absent, file dialogs via portals) - test on GNOME and KDE.
9. **macOS signing/notarization** needs Apple Developer membership and a macOS runner; do not promise macOS builds until that pipeline exists.

Sources are linked inline; claims marked **[memory]** were not verified against a live page in this session.
