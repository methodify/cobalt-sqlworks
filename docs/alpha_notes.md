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

## Results: find, totals, profile

The results find bar (Ctrl+F while a grid has focus, the toolbar magnifier, or the grid's context menu) has match-case, whole-word and regex toggles, a match
count and Shift+Enter for the previous match; matches are highlighted in the grid. Right-click a
grid → *Totals row* for a sticky Sum/Avg/Min/Max/Count/Distinct row; *Profile columns…* (also on
the toolbar) opens per-column nulls, distinct, min/max/avg, top values and a distribution.

## Execution options, query shortcuts, keyboard shortcuts

Query → Execution options holds the full SET surface per tab (ANSI_NULLS … DEADLOCK_PRIORITY) as
tri-state choices; Settings → Execution sets the defaults for new tabs. Settings → Query shortcuts
binds keys to procedures (Alt+F1 = `sp_help` on the selection, `{sel}` is the placeholder).
Settings → Keyboard shortcuts edits every command's binding.

## Files sidebar, getting started

The Files icon on the left rail opens a folder of .sql files as a tree (remembered across runs).
A fresh empty query tab shows a getting-started pane on its right until you type; hide it there or
in Settings → Appearance, bring it back with Help → Welcome.

## Service principal certificates and managed identity

A service principal can authenticate with a PEM file (certificate + unencrypted RSA private key,
as `az ad sp create-for-rbac --create-cert` writes it) instead of a secret. *Managed identity* uses
the Azure instance metadata endpoint on a VM / VMSS / AKS node or the App Service identity
endpoint; leave the client ID empty for the system-assigned identity. Both are untested against a
live tenant from the dev box — please report.

## Results: Save as table, Open in Excel, selection summary

Right-click a grid (or use the results toolbar / Results menu): *Save as table…* loads the set or
the selected cells into a new or existing table through any connected tab's connection — the
Target connection combo lists every connected tab, so a query against one server can land on
another. *Open in Excel* writes a temporary .xlsx and opens it. Select two or more cells and the
status bar shows Count / Sum / Avg / Min / Max / Distinct / Null for the selection.

## Snippets

Type a prefix (`sel`, `selw`, `cte`, …), accept the suggestion, and Tab walks the placeholders
(Shift+Tab back, Escape leaves). Your own snippets go in `snippets.toml` next to `settings.toml`
(a commented template is created on first start; the file is reloaded when saved).

## Object explorer: filters and schema grouping

Right-click a folder (Tables, Views, …) → *Filter…* for a per-folder name filter; right-click the
server → *Group objects by schema* to nest objects under schema rows. Hovering a table shows its
columns plus the row count and reserved size.

## Editor: multiple cursors

The editor follows VS Code / Azure Data Studio: Ctrl+Alt+Up/Down adds a cursor above/below,
Alt+Click adds one (Alt+Click on an existing one removes it), Shift+Alt+drag selects a column,
Ctrl+D selects the word under the cursor and then each next occurrence, Ctrl+Shift+L selects all
occurrences, Shift+Alt+I puts a cursor at the end of every selected line, Escape keeps only the
primary. Every motion and edit applies to all cursors; one edit is one undo step. Alt+Up/Down
moves lines, Shift+Alt+Up/Down copies them, Ctrl+Shift+K deletes them. Double-click and hold
selects by whole words while you drag; triple-click and hold by whole lines. Details and the rules
copied from VS Code: `docs/design/editor_multicursor.md`.

## Verifying a download

Every release file has a GitHub artifact attestation (Sigstore, keyless): with the GitHub CLI,
`gh attestation verify cobalt_<version>_x64-setup.exe --repo methodify/cobalt-sqlworks` confirms
the file was built by the release workflow at the tagged commit. SHA256SUMS is published alongside.

## Leaving the app open for hours

Fabric and Azure gateways (and many NATs / VPNs) drop a TCP session after some idle time. Since
0.4.1 a tab notices: the socket has keepalive on, a tab idle for over a minute pings the server
before the next run, and a dead connection is replaced in place (an expired Entra token is
refreshed silently from the stored refresh token) before the batch goes out. Messages then starts
with "The connection had been closed while idle (…); reconnected to *db* as SPID *n*". A run that
was in flight when the session died fails with the transport error in Messages and the tab shows
disconnected; the next run reconnects to the same database. The object explorer's own connection
heals the same way, so "Refresh list" in the database picker works after an idle stretch.

Dev knobs: `COBALT_IDLE_PING_SECS=2` shortens the idle threshold; `COBALT_TEST_EXPIRED_TOKENS=1`
treats every Entra token as expired (forces the silent refresh); the agent verb `break_connection`
makes the active tab treat its connection as dead at the next run. A server-side `KILL <spid>` from
another session reproduces the idle drop against the Docker server.

## Running without a GPU (VMs, RDP)

Measured 2026-09-22 on a 1600×900 window: Microsoft's WARP software rasterizer (the only Direct3D
adapter on a GPU-less VM) draws an egui frame in ~280 ms — about 3.5 fps, "dog slow" over RDP.
Mesa's llvmpipe through OpenGL draws the same frame in ~5 ms on the same machine.

Cobalt therefore picks its renderer at start-up (`advanced.renderer = "auto"`):

- a real GPU → wgpu (Direct3D 12 / Metal / Vulkan), as before;
- no GPU and a Mesa `opengl32.dll` next to `cobalt.exe` → OpenGL via that DLL (fast software);
- no GPU and no Mesa DLL → wgpu on WARP, with a warning toast and a status-bar badge.

Since 0.4.0 the Windows installer and zip ship that DLL: Mesa llvmpipe 26.2.3 built as a single
statically linked `opengl32.dll` by <https://github.com/mmozeiko/build-mesa> (MIT + Apache-2.0
with LLVM exception; `MESA-LICENSE.rst` and `LLVM-LICENSE.txt` sit next to the exe). The release
workflow pins the archive URL and SHA256 and hashes the DLL. It is also published on its own as
`cobalt-software-rendering-<version>-windows-x86_64.zip`. Any other Mesa llvmpipe build works too
(mesa-dist-win needs `GALLIUM_DRIVER=llvmpipe` on machines that have a D3D12 device, because its
megadriver prefers GLon12; Defender flags mesa-dist-win's archives, a known false positive on the
archives rather than the DLLs).

Overrides: Settings → Advanced → Renderer, or `COBALT_RENDERER=wgpu|opengl`. Diagnostics with the
dev build: `COBALT_PERF=1` logs fps and frame cost every 2 s; the agent verbs `perf`, `spin {ms}`
and `viewport {w,h}` measure the real maximum frame rate; `COBALT_ADAPTER=<name substring>` forces an
adapter (`basic render` = WARP) to reproduce the VM locally.

## Notebooks (0.7.0)

File → New Notebook (Ctrl+Shift+N), the Files sidebar (`.ipynb` files, "New notebook here"), or
Open File. ADS SQL notebooks and Fabric notebooks open as they are; Fabric's Git form
(`notebook-content.py`) opens too and saves back to `.py` when you Save As with that extension.
A notebook tab has the same connection machinery as a query tab (the kernel button on the right
of the toolbar changes it); SQL cells run through it one at a time and a failing cell stops the
queue. Shift+Enter runs and moves on (a new cell at the end), Ctrl+Enter runs in place, Alt+Enter
runs and inserts, F5 runs everything. Esc leaves the cell editor: then ↑/↓ select, Enter edits,
A/B insert above/below, M/Y switch Markdown/code, D D deletes, Z restores the last deleted cell.
Grids under cells are the real results grid (filters, find, totals, profile, copy, exports, Save
as table, pop out to a tab). Saving writes the outputs into the file: an Arrow IPC payload (rows
capped by Settings → Notebooks) plus HTML/Markdown/text previews; reopening shows the saved grids
marked "saved with the notebook". Export as HTML or Markdown from the toolbar or the File menu.

Agent: `notebook {action: new|open|save|cells|set_cell|add_cell|delete_cell|move_cell|set_kind|select|run|cancel|clear_outputs|export|md_edit|set_kernel}`.

## PySpark cells on the local Spark kernel (0.7.0)

The kernel button on the notebook toolbar picks where code cells run: the tab's connection (SQL
cells) or **Local Spark** (PySpark and Spark SQL cells). New PySpark notebooks (Settings →
Notebooks → "New notebooks start as", or `notebook new {language: pyspark}`) and Fabric notebooks
default to Local Spark. The first cell starts the session on the runtime from Settings → Spark
runtime (≈25 s on this machine, longer on the very first run while Ivy resolves jars); the status
bar shows `Spark 4.1.1 · up 3m` and turns busy while a cell runs; click it for Restart / Stop /
the session log. Cells run one at a time in a persistent IPython namespace (the same session
across notebooks); a failing cell stops the queue.

What comes back: `print` → text under the cell; `display(df)`, a bare DataFrame or pandas
expression, and `%%sql` → the results grid. With local-spark-mcp 0.4.1 these are native: the
worker attaches each frame to its reply as an Arrow IPC blob (`displays` entries; a bare last
expression through `run_code`'s `capture_result`, whose `Out[n]:` repr Cobalt drops from the
text), and Cobalt's `%%sql` helper just runs the statements and hands the last frame to the
worker's `display`. On an older environment the legacy hook (IPC files under the runtime's
`state/outputs/` plus a marker line) is still installed and the session log says so. The row cap
for all of them is Settings → Notebooks → "Rows a Spark DataFrame brings back" (`init`'s
`default_sql_limit`); `display(df, limit=N)` overrides it. Stop (toolbar, the cell's
button, Alt+C) on a running cell sends `interrupt` on the worker's control socket
(local-spark-mcp 0.4.0+): Spark jobs are cancelled, the cell ends as *cancelled* with
"Interrupted (Spark jobs cancelled)" under what it printed so far, and the session and its
namespace survive. Stop again while that is pending kills the worker (the next cell starts a
new session) — the escape hatch for a cell blocked in pure Python, which Windows interrupts only
when the blocking call returns. On a pre-0.4.0 environment Stop kills the worker straight away
and the session log says so. Cell output streams while the cell runs (0.4.0's `stream: true`);
the final reply replaces the streamed lines. `%pip install x` works (IPython's pip magic against
the runtime's environment) but is not recorded anywhere. Agent: `kernel {action: status|start|stop|restart|interrupt|log}`;
`kernel` state carries `protocol_version`, `control` and `interrupting`.

**Contexts (local-spark-mcp 0.5.0, Cobalt 0.8).** Each notebook runs in its own context of the
one session: `create_context(<nb-id>, default_lakehouse)` on the notebook's first run,
`run_code(context=…)` for every cell, `drop_context` when the tab closes. Isolated per notebook:
variables and imports, temp views, SQL conf, current database (the notebook's default lakehouse
— `test__dbo` for a schema-enabled one), UDFs. Shared: the catalog and its tables, lakehouse
clones, cached data, jars. Execution is still one cell at a time across notebooks. A notebook
from a workspace the session did not start with is attached with `register_lakehouse` (its
lakehouses join the catalog; the chip shows "+N"); a notebook whose write mode differs, or a
lakehouse notebook on a session started plain, still gets the restart hint. Cells carry a
`job_description` (first line) so `status.cell.jobs` and the Spark UI name them. The session log
shows `cobalt: context nb-xxxx created (database …)` / `dropped` / `attached lakehouse …`.

**Lifecycle (0.8).** Settings → Notebooks & Spark → Session lifecycle. Start: *when a Spark
notebook opens* (default; `maybe_early_start` on install/open/Fabric open, quiet when the
Fabric panel is signed out), *when Cobalt starts* (a plain session a minute after launch), or
*only when a cell runs*. End: *after idle minutes* (default 60; the idle clock is
`KernelUi.last_activity`, reset by a cell submit/finish, a running preload, or start), *only
when I stop it*, or *when the last Spark notebook closes*. Per-lakehouse policies live in the
store (`pref:lakehouse:<id>`): preload none / `last` (persisted clones with `registered: false`
from `shadow_status` → `preload {lakehouses: {lh: [tables]}}` right after start) / all; keep
clones (`persist_shadow` for the session when any attached lakehouse asks). Set from the
lakehouse button; agent `notebook set_lakehouse {preload_policy, keep_clones}`.

**Files (0.6.1, `files_mode: lazy` by default).** Spark's `Files/…` resolves through the catalog
jar's `lakehouse://<ws>@<lh>.onelake…` filesystem to the context's default lakehouse on OneLake:
`spark.read.csv("Files/x")` streams, `df.write…("Files/out")` is refused under sandbox/readonly
with a message naming write-through, and `/lakehouse/default/Files/…` in Python fetches a file
into the mirror on first `open` (re-fetched when OneLake has a newer copy), lists from OneLake,
and writes locally (pushed on close only in writethrough). Two Spark facts: `delta.`<path>``
in SQL needs an absolute path (`lakehouse://…/Files/dt`, as on Fabric), and `DESCRIBE DETAIL` on a
shadow shows a `file:/…` location. Native readers (DuckDB, Arrow `OSFile`) bypass Python's `open`:
`sync_files(paths=[folder])` first, or `open()` the file once from Python. Settings → Notebooks &
Spark → Lakehouse Files: lazy / mirror; Settings → Spark runtime shows the mirror's size with
Clear (stop the session first). Agent: `kernel {action: call, method: mirror_status}`.

**Lakehouse pane (0.8).** Sidebar view `Lakehouse` (`ui/lakehouse.rs`, state
`LakehousePane`): follows the active notebook's binding (picker for the workspace's other
lakehouses; Make default rewrites the notebook's binding). Tables come from Cobalt's own OneLake
`Tables/` listing (schema folders → groups), clone state from `shadow_status` (`lh.t` /
`lh__schema.t`); Clone now = `mount_table`, Discard = `discard_shadow`, Rewind =
`restore_shadow(version 0)`. Files folders are listed on demand with the DFS API (sizes from
`contentLength`), "local" markers from `mirror_status` (`pulled`, `fetched`), Pull =
`sync_files(paths, pull)`, Remove local copy = `clear_mirror(paths)`. Double-click / Insert makes
a PySpark cell by extension (csv/parquet/json/… → `spark.read…` + `display`; txt → `open()`);
drag payload is the qualified name or `Files/<path>`. Agent: `lakehouse_pane {action: …}`.
Clone now = `mount_table(lakehouse, "schema/table")` (0.6.3 accepts the entry form). 0.6.0–0.6.2
had `shadow_status` empty on any state path with a space (the shadow root reached the JVM as a
percent-encoded `file:` URI, so clones landed under a `…%20…` sibling the lister never scanned —
on this box `AppData/Local/Cobalt/Cobalt%20SQL%20Works`); fixed in 0.6.3, and the stray folder
can be deleted. Token note: the OneLake data plane (the JVM's ABFS, Cobalt's DFS listings, the
pane) accepts **only the Azure Storage audience** (`https://storage.azure.com/.default`); a
Fabric API token is answered with `401 Authentication Failed with Audience validation failed for
audience 'https://api.fabric.microsoft.com'`, although the blob path the OneLake *export* uses
takes it. Seen live 2026-10-07 after an evening re-sign-in: the silent storage-scope refresh
stopped, the code fell back to the Fabric token, and every lakehouse first touch failed with
TABLE_OR_VIEW_NOT_FOUND while the worker log showed `AccessDeniedException: Unauthorized 401` from
`listOneLakeTables` and the pane said "Fabric rejected the token". Cobalt's token endpoint now
serves the storage audience only and says "sign in again on the Fabric panel" when it cannot;
the session start's token check still goes interactive. The OneLake client reports the 401 body.
`notebookutils.fs` (0.6.4) covers Fabric's surface: `ls/exists/mkdirs/rm/cp/mv/put/head/append`,
`mounts/getMountPath/refreshMounts`; `abfss://` paths hit OneLake (writes only in writethrough,
otherwise a PermissionError naming the mirror), `/lakehouse/…` paths hit the mirror through the
lazy hooks (`default` = the context's default lakehouse).
Cell headers are now always visible (no hover expansion).

## Spark runtime (Settings → Spark runtime)

Everything lands under `%LOCALAPPDATA%\Cobalt\Cobalt SQL Works\data\spark-runtime` (override
the folder in the same section): `uv/`, `python/`, `envs/<profile>/`, `jdk/`, `ivy/`, `state/`,
`runtime.json`, `provision.log`. Pick the profile (`fabric-2.0` by default) and the JDK vendor,
optionally a JDK already on the machine ("Java on this machine" lists JAVA_HOME, PATH, vfox,
SDKMAN, Program Files…; Oracle builds are refused), then **Install for me**. On this machine the
first install took 4½ minutes: uv adopted from `~/.local/bin`, Python 3.11.12, local-spark-mcp
0.3.4 with pyspark 4.1.1 + delta-spark 4.2.0, Microsoft JDK 21.0.8 (hash-verified), then a first
Spark session that pulled the Delta and hadoop-azure jars into `ivy/` and answered `SELECT 1`
(`[[1,"ok"]]`). Footprint after the uv cache is dropped: ≈1.3 GB. **Run smoke test** repeats the
session start (≈30 s warm). The Spark driver binds to 127.0.0.1 only, so Windows Firewall has
nothing to ask about java.exe.

The manifest (`crates/cobalt-runtime/manifest.json`) mirrors local-spark-mcp's `profiles.py`
until that project publishes a machine-readable one; the asks are in
`docs/requests/local-spark-mcp.md`. Agent: `runtime {action: status|install|smoke|cancel|remove|refresh}`.

## Libraries for the Spark environment (0.7.2)

Settings → Notebooks & Spark → **Libraries**. Two lists, saved with the settings:

- **Python packages**: PyPI requirement specs (`polars`, `dwlib==0.3.1`) or paths to wheels /
  sdists (Add wheel…). *Install libraries* runs `uv pip install --python <env> …` into the
  profile's environment (`envs/fabric-2.0`), so the packages are importable in Spark sessions;
  a running session needs a restart. Each row shows the installed version or "not installed".
- **Java libraries**: jar files on this machine (Add jar…) and Maven coordinates
  (`org.postgresql:postgresql:42.7.3`), which *Install libraries* fetches from Maven Central into
  the runtime's `jars/` folder (the artifact only — add transitive dependencies as further
  coordinates). At session start every present jar goes on `spark.driver.extraClassPath` and
  `spark.executor.extraClassPath` (one JVM under `local[*]`); missing ones are logged and skipped.
  Verified: `tabulate` imported and `org.postgresql.Driver` resolved in a fresh session.

Since local-spark-mcp 0.3.5 the session takes jars and Maven coordinates directly
(`extra_jars`, `extra_packages`): jars join `spark.jars` next to the catalog jar, and Ivy
resolves the Maven packages with their dependencies when the session starts (the first start
with a new package waits on the download; the session log shows Ivy's progress). "Install
Python packages" is the only install step left. The runtime page also runs the package's
`healthcheck` and shows its problems and warnings; a lakehouse binding has a "Preload …
tables at session start" option whose progress shows in Lakehouse shadows. Since 0.4.1 the
preload is the worker's: `init` carries `preload: [<lakehouse>]`, the worker lists `Tables/`
through the Hadoop filesystem it already authenticates (one level down for schema folders) and
clones in parallel, and every Azure token it needs comes from Cobalt's loopback endpoint —
`GET /token?scope=<scope>` now serves the Fabric API scope as well as OneLake storage (Key
Vault is refused with a 404). The Shadows window polls `preload_status` on the control socket,
so the progress bar moves while a cell runs. Between 0.3.5 and 0.4.0 Cobalt listed OneLake and
drove `mount_tables` itself because the worker's discovery needed a credential this process never
gives it and the Fabric REST tables endpoint refuses schema-enabled lakehouses; that code is gone.
**Schema-enabled lakehouses** (`Tables/<schema>/<table>`) get a V2 catalog named after the
lakehouse (`SHOW NAMESPACES IN test` → `dbo`; `test.dbo.publicholidays`, `SHOW TABLES IN
test.dbo`, `USE test`), top-level tables stay `test.<table>`, and an unqualified name resolves to
the default lakehouse's default schema (`spark_catalog.test__dbo` is current at start). 0.4.1
had every table of such a lakehouse fail to materialize because its catalog was made current and
the shallow clone's `delta.`path`` SQL then resolved under it (diagnosed live on `test`, see
`docs/requests/local-spark-mcp-0.4.1-reply.md`); 0.4.2 qualifies everything the worker runs
with `spark_catalog.` and leaves `USE test` to the user. In your own cells, with a V2 catalog
current, write `spark_catalog.delta.`path`` rather than `delta.`path``. Both fixtures are
verified: `test` (mixed layout) and `test_no_schema` (plain, empty — preload 0/0, sandbox
`saveAsTable`, `SHOW TABLES`, shadows).

## Permissions and re-consent (0.7.1)

A Microsoft sign-in carries the permissions you consented to at the time; a refresh token never
gains new ones. So when Cobalt starts needing a permission the app registration did not have when
you first signed in (Item.ReadWrite.All for notebooks was the first case), the token silently
lacks it and Fabric answers 403 InsufficientScopes. The Fabric panel now compares the token's
scopes with what Cobalt needs (`FABRIC_REQUIRED_SCOPES` in cobalt-auth) and shows a yellow banner
naming the missing ones; **Grant permissions…** runs the browser flow with `prompt=consent`, which
makes Entra show the permissions screen even though you are already signed in. A 403 from any
call raises the same banner. If your tenant requires admin approval, Entra says so in the browser
and the banner offers "Copy admin consent link" for your administrator. **Sign out** forgets the
account (refresh token in the keychain and the cached tokens; a saved connection that shared that
sign-in will ask again), and the next sign-in always goes through the browser with the account
picker. Agent: `fabric {action: sign_in|sign_out|grant|refresh}`; `fabric_scopes` lists what the
token has and what is missing.

## OneLake-bound sessions, shadows, Fabric notebooks (0.7.0)

On a Spark-kernel notebook the **lakehouse button** (toolbar, right) binds the session: workspace,
default lakehouse, write mode. The binding is written into the notebook's metadata
(`dependencies.lakehouse`), the same place Fabric keeps it, so a notebook opened from Fabric comes
pre-bound and a notebook saved here opens in Fabric with the same default lakehouse. The first
cell starts a session bound that way: Cobalt starts a loopback token endpoint that answers the
JVM's token requests with the Fabric account's OneLake token (`OneLake.ReadWrite.All` on the
Fabric API token, or an Azure Storage token when the registration has it), registers the
workspace's lakehouses as databases and selects the default one. Tested live on the test tenant
(2026-10-06): `SELECT COUNT(*) FROM test.sales_import` cloned the 20,000-row table in 12 s on
first touch, `spark.table` + `display` showed it in the grid, an INSERT in sandbox mode made the
clone "written" (version 1), Rewind took it back to version 0, Discard re-cloned on the next
query, and in write-through mode `CREATE TABLE test.cobalt_nb_writethrough` landed in OneLake —
a fresh sandbox session read it back from there (it is still in the lakehouse; drop it from the
portal or a write-through cell when you no longer want it).

**Shadows window** (kernel menu on the status bar, lakehouse button, Query → Lakehouse Shadows…):
the clones in the running session, read vs written, version; Discard all / Discard written /
per-table Discard and Rewind. A session keeps its binding until it is restarted; when the active
notebook's binding differs, a warning chip on the toolbar restarts the session with it.

**Fabric notebooks.** Workspaces in the Fabric panel now list their notebooks; double-click opens
one bound to the item, Ctrl+S saves back after a confirmation (Fabric keeps no history, so the
dialog says so), Save As… writes a local `.ipynb` and detaches. "Open a copy" opens it detached.
Needs the delegated permission `Item.ReadWrite.All` on the app registration (listing works
with `Item.Read.All`; without the write scope `getDefinition` and `updateDefinition` return 403
InsufficientScopes and the error says what to add). Added to the test tenant on 2026-10-06; the
next token refresh picked it up without a new sign-in. Verified live: your `Notebook_1` opened
from the Fabric test workspace bound to lakehouse `test`, ran a `%%sql` count over
`test.sales_import` and two sandbox writes, saved back through `updateDefinition`, and "Open a
copy" fetched the saved version with the new cells and their outputs. `fabric_scopes` (agent)
shows what the token has.
Agent: `notebook {action: set_lakehouse, workspace, lakehouse?, write_mode?}`, `shadows {action: status|discard|discard_written|restore, table?}`, `fabric_notebooks {workspace?}`, `notebook {action: open_fabric, item, copy?}`, `notebook {action: save_fabric}`.

## Spark SQL query tabs (0.8.2)

"Just run this query against the lakehouse" without a notebook. In the Servers sidebar the
**Local Spark** root shows the session's state and the lakehouses it knows: those bound to open
Spark notebooks and tabs, the ones pinned on the Fabric panel, and the ones used before.
Double-click a lakehouse (or right-click → New Spark SQL query; File → New Spark SQL Query
inherits the active tab's binding) for a tab named `SparkSQL_1 · test`. Its toolbar carries the
session chip (`Local Spark (fabric-2.0) · ready`, with Start / Restart / Stop / Interrupt /
Session log) and the lakehouse chip (`test (Fabric test)`: workspace, default lakehouse, write
mode — the same binding a notebook has; "Connect" or "Change connection" on such a tab opens it).
The session is the one the notebooks use (lifecycle, early start and idle stop apply); each tab
runs in its own context, created on the first run with the lakehouse as current catalog
(local-spark-mcp 0.6.6), so `publicholidays`, `dbo.publicholidays` and `test.dbo.publicholidays`
all resolve.

Run (F5), Run selection and Run current statement work as on a SQL Server tab: statements are
split on `;` and each runs through the worker's `run_sql` (local-spark-mcp 0.7.0) in the tab's
context. Every statement that returns rows is its own result set in the grid (filters, sort,
viewer, copy, exports, Save as table) and the rows stream in while the statement runs;
`INSERT` / `UPDATE` / `DELETE` / `MERGE` / `CREATE TABLE … AS` say "(n rows affected)" in
Messages (a merge adds "10 inserted, 50 updated, 0 deleted"), other statements without rows say
"Statement 3 completed (0.4 s)". A failing statement ends the run and shows Spark's analysis
message as one error line ("Statement 2: [TABLE_OR_VIEW_NOT_FOUND] …"); earlier result sets
stay. Cancel interrupts the Spark jobs (the session survives; Cancel again kills it). The row
cap is Settings → Notebooks → "Rows a Spark DataFrame brings back"; a capped result says "the
first 10,000 rows … there may be more" and there is no fetch-more (re-run with a `LIMIT`). Run
to File streams every row to the chosen file or lakehouse without the cap and keeps the first
1,000 in the grid as a preview. History lists the run under
`Local Spark (fabric-2.0)` with the lakehouse as database. The Lakehouse pane follows a Spark
tab: double-click a table to insert `SELECT * FROM test.dbo.publicholidays LIMIT 100` at the
caret; a file inserts its `Files/…` path. Hot exit restores the tab with its binding (the
session is not restarted until you run). `SHOW TABLES` lists every table of the lakehouse
(0.7.0), without mounting any. On a runtime older than 0.7.0 the tab runs its statements
through the `%%sql` helper instead: collected, not streamed, no DML counts.

**Est. plan** (Ctrl+L) runs `EXPLAIN EXTENDED` on the selection or the statement under the
caret and shows the Plan tab: parsed, analyzed, optimized and physical plan as collapsible
monospace sections (the physical plan open), Copy all. A statement that does not analyze
ends in Messages with Spark's message. **Parse** (Shift+Alt+P) analyzes every statement without
running anything: queries through the analyzer, everything else through `EXPLAIN EXTENDED`;
Messages says "Statement n: parsed and analyzed" or names the first failing one. Parse cannot
see tables an earlier statement of the same script would create, so an `INSERT` into a table the
script's own `CREATE TABLE` makes reports "not found" — that is the check working, not a bug.
**Completion** on a Spark tab knows the bound lakehouse: tables (with their schema when the
lakehouse has schemas), schemas and columns with types, read from each table's Delta log on
OneLake when the tab opens (no session, nothing mounted; the pane's Refresh reads them again),
plus Spark keywords and functions; names that need quoting get backticks. The editor highlights
Spark SQL: backtick identifiers, `"text"` strings, Spark keywords and functions; SQL cells on the
Spark kernel use the same. A capped result shows "Capped at 10,000 rows per statement · Run
again without the cap" above the grid. The Servers tree's "Choose a lakehouse…" and the tab's
lakehouse chip open a picker with workspaces and lakehouses side by side; a notebook SQL cell's
run menu (the caret next to Run) has "Open in a Spark SQL tab", which carries the notebook's
binding. Agent: `spark_query {workspace?, lakehouse?, write_mode?, text?}`; `state` reports
`kind: "spark"`, a `spark` object, `catalog_objects` / `catalog_columns`, `text_plan_sections`
and `capped` per tab; `complete {append}` works on Spark tabs.

## Import Data from File

Destination: a table in the tab's database, or *File or lakehouse…* to convert the file into any
export format (Parquet, Arrow, Delta, CSV, JSON, Excel…) on disk or in a OneLake lakehouse. The
column names, SQL types and exclusions you set in the dialog apply either way, so the same dialog
turns a raw CSV into a typed Parquet or Delta table without a database in between.

File → Import Data from File… (or right-click a database in Servers → Import data from file…).
The file is read on a background thread (`cobalt-import`: CSV/TSV via arrow-csv with schema
inference, Parquet, Arrow IPC) and streamed as Arrow batches to the tab's session actor, which
runs `CREATE TABLE` (new-table mode), `BEGIN TRANSACTION`, a TDS bulk insert
(`Connection::bulk_insert`, `mssql/bulk.rs`: batches are cast to each column's SQL type, then
encoded row by row), and `COMMIT` — or `ROLLBACK` on any error or Cancel. Types and nullability
are editable per column; existing-table mode reads the table's columns and locks them.

Known limits: one file per import, no column reordering or transforms, geography/hierarchyid/sql_variant
targets are not supported, and CSV row counts in the progress bar are estimates.
Agent: `import {path, table, schema?, existing?, delimiter?, header?, types?, exclude?}`, `import_state`.
