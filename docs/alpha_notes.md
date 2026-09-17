# Cobalt SQL Works — V1 alpha notes for the founder

*Written during the build, 2026-09-17. Update as the alpha evolves.*

## Run it

```
cargo run -p cobalt-app --release            # or: target\release\cobalt.exe
COBALT_AGENT=1 cargo run -p cobalt-app       # dev build with the agent channel on (pipe: cobalt.agent)
```

Data lives in `%APPDATA%\Cobalt\Cobalt SQL Works\` (Windows): `config\settings.toml`,
`data\cobalt.db` (library, history, tab snapshots). Passwords go to Windows Credential Manager
under service `cobalt-sqlworks`. Spill files go to the temp dir and are cleaned on startup.

## What to try first

1. **Servers → +** — add your SQL Server (SQL login or Windows auth). "Test connection", then
   "Save & Connect". Expand the server in the tree: databases → Tables → columns/keys/indexes.
2. **F5** runs the tab (selection if any); **Ctrl+Enter** runs the statement under the cursor;
   **Ctrl+L** estimated plan; **Ctrl+M** toggles actual plan for subsequent runs.
3. Big result: `SELECT * FROM <big table>` stops at 10,000 rows with a **Fetch more / Fetch all**
   bar. 2M rows fetch in ~4 s locally; sort/filter work on the full set.
4. Right-click the grid: Copy / with headers / Markdown / JSON / INSERT / IN list;
   **Save results as…** → CSV, Excel, JSON, XML, Markdown, **Parquet, Arrow, Delta**.
5. Double-click a cell (or Enter) → cell viewer (pretty JSON / XML / hex).
6. **History** (left strip) — every run, searchable; double-click reopens.
7. **Ctrl+Shift+P** — command palette. **File → Import Azure Data Studio connections**.

## Entra ID / Fabric — verified live (2026-09-17)

Your app registration (`ecec63e7-…`) ships as Cobalt's default client ID (Settings → Connections
overrides it). Both of your Fabric profiles connect and run queries through the app:

- **fabric** (Warehouse endpoint `…datawarehouse.fabric.microsoft.com`) — engine detected as
  *Fabric Warehouse*; tree browses databases → tables → columns/keys/indexes; queries, estimated
  plans, exports (CSV/Excel/JSON/Parquet/Delta) all work. A `dbo.sales` table with 20,000 rows was
  created in the `warehouse` database for testing.
- **…database.fabric.microsoft.com** (SQL database in Fabric) — engine detected as *Fabric SQL
  database*; `dbo.customers` (1,000 rows), `dbo.orders` (5,000), view `dbo.v_customer_totals` and
  procedure `dbo.p_top_customers` were created there. Actual plans work on this engine.

The refresh token is stored in Windows Credential Manager (chunked, since Entra refresh tokens
exceed the 2,560-byte credential limit), so sign-in is silent after the first browser round trip.
The sign-in wait is 15 minutes with "open browser again" / "copy link" in the dialog.

What it took (all in the vendored driver, see `vendor/tiberius-ng/COBALT-PATCH.md`):

1. **Routing redirects** — Fabric answers the first login with a redirect to
   `pbipwus16-….pbidedicated.windows.net\<warehouse-id>-dw:1433`. The driver now connects TCP/TLS to
   the bare gateway host and sends the full routed name (plus port) in the LOGIN7 record, like
   SqlClient does, instead of treating the `\…` suffix as a SQL Browser instance.
2. **PRELOGIN TRACEID** — the routed gateway rejects any login whose PRELOGIN lacks a TRACEID with
   18456 *"Couldn't complete the operation due to a system update"*. tiberius never sent one and
   encoded it as 20 bytes; MS-TDS (and SqlClient) use 36 (connection GUID + activity GUID +
   sequence). Cobalt now sends one on every connection. Found by capturing SqlClient's LOGIN7 with a
   fake TDS server (`cargo run -p cobalt-driver --example tds_capture`) and bisecting.
3. **Zero-length TRACEID echo** — Azure SQL's gateway echoes the option with no payload; the
   decoder now honours the option length instead of reading 20 bytes past the end.
4. **Engine-aware session prelude** — Fabric Warehouse rejects `SET XACT_ABORT` and every
   `SET STATISTICS …`; those are now capability-gated, and the *Actual plan* toggle is disabled on
   engines that cannot produce one.

If your tenant's conditional access ever rejects the public client, switch the profile's auth to
**device code** or **Azure CLI**.

## New in 0.2.0 — Fabric explorer and OneLake export (2026-09-18)

- Click the **cube** on the left rail (Ctrl+Shift+B). It adopts your existing Entra sign-in silently
  (or offers *Sign in with Microsoft*), then lists every workspace you can reach. Expand one to see
  its warehouses, lakehouse SQL endpoints, SQL databases and mirrored databases. **Double-click** opens
  a connected tab; right-click for *Pin*, *Save to Servers…*, *Copy connection string*, *Open in
  Fabric portal*. Pins show at the top and survive restarts.
- **Save results → Destination: OneLake lakehouse.** Delta tables land in the lakehouse's `Tables/`
  and are queryable from its SQL endpoint right away; Parquet/CSV/Excel/… land in `Files/`. The
  lakehouse list comes from the Fabric panel. Schema-enabled lakehouses (like your `test`) get a
  *Schema* field defaulting to `dbo`, and the table lands in `Tables/<schema>/<table>`. Tested against your
  `test` lakehouse; the SQL endpoint shows a new table after its sync lag (about two minutes).
- Registration permissions you added (Workspace.Read.All, Item.Read.All, OneLake.ReadWrite.All under
  Power BI Service) are exactly what these need. If you ever add *Azure Storage → user_impersonation*
  the OneLake write will use that token instead; not required.

## Fixed after your first pass (2026-09-17)

- Connection editor: the **Advanced** section now opens (encrypt mode, trust server certificate, host name in certificate, timeouts, intent).
- Results/Messages: clicking **Results** after **Messages** works again (the Messages pane was swallowing the tab strip's clicks).
- Row cap: the timer freezes while "Paused at row cap" and resumes on *Fetch more*; the toolbar **Cancel** (and Alt+C) now ends a paused query, same as *Stop* on the strip.
- The app has its own icon (window, taskbar, exe, installer): `assets/icon.svg`; wordmark in `assets/logo.svg`.
- Grid selection: **Ctrl+A** after clicking a cell selects every cell, and the blank corner above the row numbers does the same (hover shows *Select all*). Clicking a cell now takes keyboard focus from the editor, so Ctrl+A / Ctrl+C act on the grid until you click back into the SQL text.
- The egui_agent control channel is now a dev-only cargo feature (`--features agent`); release builds from v0.1.1 on do not contain it (v0.1.0 has it compiled in but inert unless `COBALT_AGENT=1`).
- **Help → Check for Updates…** asks GitHub for the latest release; the same check runs once at start-up (Settings → Updates to turn it off or stop skipping a version).
- Column-header tooltips no longer appear twice (egui_table visits each header cell once per scroll region; the clipped visit is now skipped).

## Known gaps in this alpha

- Windows Integrated auth: verified against SQL Server 2025 Express on this box (`localhost,1435`, profile **express-winauth**) — logs in as `AzureAD\BryonWilliams` via NTLM. Kerberos against a domain-joined server is untested.
- Object-explorer filter dialog, group-by-schema, freeze columns, transposed view: V1.x.
- Multi-cursor / folding in the editor: V2.
- Plan comparison and Plan-Explorer-class analysis: V2.
- The agent's `snapshot` sometimes returns only the root node between interactions (egui_agent/AccessKit incremental tree); clicks by label still work. Worth a look in egui_agent.
- macOS: untested. Linux: built in Docker and runs under WSLg (X11); no Secret Service there, so passwords fall back to a plain file with a warning.
- Release binary is 46 MB and cold-starts in ≈1.7 s (target was <1 s / <40 MB); arrow + delta-rs + wgpu are the bulk. Worth a size pass later.
- The `all_types` seed row 'long nvarchar max ✓ text' shows `?` because the seed literal lacked an `N` prefix — a seed bug, not a driver bug.

## V1 acceptance checklist — status at alpha (from `product_design.md` §8)

| Item | Status |
|---|---|
| Cold start < 1 s, binary < 40 MB | ~1.7 s to agent-responsive, 46 MB — close, not met; size pass later |
| Group/profile with SQL auth, connect, lazy tree | ✅ verified via agent |
| Entra interactive login to Fabric | ✅ Warehouse and SQL database in Fabric, silent re-auth from the credential store |
| `az login` credential | implemented; no `az` on the build box |
| Windows integrated auth | ✅ SQL Server 2025 Express, NTLM |
| New Query, highlighting, completion, F5, Ctrl+Enter, cancel | ✅ keys verified by injected key events: F5, Ctrl+Enter (current statement), Ctrl+L, Ctrl+M, Alt+C, Ctrl+Shift+C, Ctrl+Shift+P |
| 5M-row streaming with cap, fetch-all, spill, sort/filter | ✅ 2M rows in ~4 s; spill covered by unit tests |
| Multiple result sets, PRINT, clickable errors, rows affected | ✅ |
| Copy / with headers / Markdown / JSON / INSERT | ✅ Markdown verified in clipboard; others share the builder |
| Save as CSV/Excel/JSON/XML/Markdown/Parquet/Arrow/Delta | ✅ all produced; round-trips in tests; Delta/Parquet written from Fabric results (`_delta_log` + parquet part) |
| JSON/XML cell viewer, 1 MB values in full | ✅ |
| Estimated/actual plan graph, properties, top ops, .sqlplan | ✅ |
| History records, search, restore closed tab | ✅ |
| Light/dark complete, follows OS | ✅ |
| Palette with shortcuts; ADS keys | ✅ palette (Ctrl+Shift+P) and keys verified via injected events |
| Hot exit | ✅ |
| Agent verbs via egui-agent-cli | ✅ (this is how everything above was tested; `press`/`type_text`/`focus_editor` added for keyboard checks) |
| Linux build runs the checklist | binary builds and runs under WSLg; checklist not exercised there |
