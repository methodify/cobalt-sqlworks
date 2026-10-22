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

## LakeSail engine (0.9.0, experimental)

A second engine behind the same Spark session. Settings › Notebooks & Spark › Spark runtime has
an **Engine** row: *Local Spark (JVM)* is everything from 0.7–0.8; *LakeSail* is
[Sail](https://github.com/lakehq/sail) 0.7.2 — a Rust Spark Connect server, no Java — installed
as one uv environment (`envs/sail`: pysail, pyspark-client 4.1.3, IPython; about 250 MB, ~90 s)
with "Install LakeSail for me", a smoke test and "Remove LakeSail". The Spark menu › Engine
submenu and the notebook kernel picker switch too; a running session restarts on the new engine
(variables and temp views are gone). Everything names the engine: status bar "Sail 0.7.2 · up
2m", chip "LakeSail (0.7.2) · ready", history source "LakeSail (0.7.2)", the session log title.

What to try: a Spark SQL tab on `test` (the session is up in ~3 s): `SELECT … FROM
publicholidays`, `dbo.t`, `test.dbo.t`, `test_no_schema.dbo.t`, `SHOW TABLES` (instant: the
catalog is Fabric's own table listing, served to Sail by Cobalt on loopback, cached per
session; the first touch of a table costs one metadata call, ~0.5 s), Est. plan (Sail's
DataFusion plan under the usual section headers), Parse, Run to File, Cancel. The agent's
`kernel` JSON has a `catalog` block (requests served, upstream calls, last error). Tables with
struct / array / map columns read on both engines (nested values show as text in the grid; the
LakeSail catalog takes the exact schema from the table's Delta log). Settings › Spark runtime ›
**Fabric packages**: the Python packages a Fabric runtime ships that a notebook imports (pandas,
scikit-learn, plotly, the azure-* clients… 56 for fabric-2.0, 57 for fabric-1.3, at Fabric's
versions; pyspark, notebookutils, synapseml and semantic-link are not on PyPI for us). The list
is local-spark-mcp's own since its 0.8.0 (its `profiles.json`, curated from Microsoft's published
environment files), so both engines offer the same packages: on Local Spark a checkbox installs
the profile's roster into its environment, on LakeSail the picker offers *None* / *fabric-2.0* /
*fabric-1.3*. Opt-in, a few hundred MB; the status line counts the packages at Fabric's version
and names any at another version or that would not install here (on Windows, fabric-2.0's
`scipy==1.18.0` has no wheel for Python 3.11, so the manifest's platform fallback `scipy>=1.15,<1.18`
goes in and the status line says "platform fallback"; on LakeSail protobuf is not taken from the
roster because the PySpark Connect client needs its own newer line, so that engine shows 55 of
the 56). Lakehouse Files for Python on Sail (0.9.5): `open('/lakehouse/default/Files/x.csv')`,
`os.listdir`, `notebookutils.fs.ls / head / put / exists` on `/lakehouse/...` and `abfss://` paths,
`notebookutils.runtime.context`, `notebookutils.notebook.exit`, and the pane's Pull / Remove
local copy, all as on Local Spark (the same mirror folder, lazy by default); a read-only session
refuses `df.write.saveAsTable("dbo.t")` and `df.write.save("abfss://…")`, write-through writes
them (saveAsTable on a new lakehouse table lands as a Delta write at the table's path). A notebook on the Spark kernel: `df = spark.sql(...)`, `display(df)`, a bare
`df`, `%%sql`, Stop. The agent: `settings {set: {"spark.engine": "sail"}}`, `runtime
{action: install}`, `kernel` JSON carries `engine` / `engine_version`.

**Charts (0.9.5, slice A of `docs/design/charts.md`).** On any result set, the Chart button
(next to Maximize) replaces the grid with a chart; the strip above it picks the kind (Bar,
Histogram, Line, Area, Scatter, Pie), X, Y (several numeric columns = several series; none =
count of rows), the aggregation, Top N (+ other), Stacked, Horizontal. Histogram: pick the
numeric or date column to bin; *Auto bins* sizes the bins from the data (Freedman–Diaconis, 5 to
200), untick it to type a count; hover a bin for its range and count; click one to jump to its
first row in the grid. Try `SELECT type_desc, object_id, create_date
FROM sys.all_objects`: count by `type_desc` as a bar, then Pie with Top 6; Line with X =
`create_date` and max `object_id`; Scatter `object_id` over `schema_id` with aggregation none.
Hover for values, click a bar to select its rows (the grid shows them when you switch back),
drag to zoom, double-click to reset. Copy image → paste into Teams; Save PNG… → a file. The chart
follows the grid's sort/filter and a re-run; there is no row budget (1.3 M rows aggregate in
~0.2 s; a 1.3 M-point scatter draws, slowly). The agent: `grid {action: chart, kind, x, y: [..],
agg, top_n, stacked, horizontal}` / `{action: chart_state}` / `{action: chart, off: true}`;
labels `chart toggle`, `chart kind <k>`, `chart x`, `chart y <col>`, `chart agg`, `chart top`,
`chart copy image`, `chart save png`, `chart close`, `chart status`.

**Dashboards (0.9.5, slice C of `docs/design/charts.md`).** File › New Dashboard, or right-click
a server › Dashboard for its home page. Add tile… takes a title, a connection, a database, a
query, how to show it (Number = the first cell, Chart, Grid = the first rows), the refresh
interval and the size; tiles connect on their own and refresh on their clock, with the age in
the header, Refresh now per tile and Refresh all on the toolbar. On a chart tile the sliders
button opens the chart strip (kind, X, Y, aggregation, top-N, bins…) and the settings are the
tile's. The quickest way in: run a query in a tab, chart it if you like, right-click the result
› Pin to dashboard… Save writes `<name>.cobalt-dash.json` (shareable; a reader resolves the
connections from their own library by profile id, then name, then server); a server's home page
remembers its file and opens from the server menu from then on. The agent: `dashboard {action:
new|open|home|pin|save|add_tile|update_tile|remove_tile|refresh|state, …}`; labels `dashboard
add tile`, `dashboard refresh all`, `tile N refresh|configure|edit|remove|status|value`.

**Charts in cells (0.9.5, slice B).** The same chart button sits on a cell's result header (SQL
and Spark kernels alike). The chart's settings are saved in the cell, so it is back when the
notebook opens and after you re-run the cell; *Keep in notebook* in the strip captures it as a
picture into the outputs, which is what Fabric, VS Code and GitHub show for that cell (Cobalt
shows the live chart again). Pictures already in a notebook's outputs (matplotlib from Fabric)
now render under the cell. The agent: `notebook {action: chart, index, set?, kind, x, y, …,
keep?}`; `cells` carry `charts` and `saved_charts`.

**Parameters and `%pip` (0.9.5).** A Python cell's run menu (the caret next to its play button)
has *Parameters cell*: the cell gets Fabric's `parameters` tag and a badge, and the toolbar's
*Run with parameters…* lists its `name = value` lines for editing; Run appends the changed
values to that cell for the run only and runs every cell (Fabric's Run with parameters, and what
`notebookutils.notebook.run(path, args)` does). A cell with `%pip install x` / `!pip install x`
lines runs the rest of its code and reports the packages under the cell with *Add to runtime*,
which puts them in Settings › Notebooks & Spark › Libraries and starts the install into every
engine environment. The agent: `notebook {action: set_parameters, index, on}`, `{action:
parameters}`, `{action: run_with_parameters, params: {name: "expr"}}`, `{action: add_pip_packages,
index}`; cells carry `parameters` and `pip_packages`.

What LakeSail does not do (said in the chip, the pane and the errors): no sandbox — the write
mode is *read only* (default; writes are refused with "write_mode is 'readonly'") or *write
through* (INSERT / UPDATE / DELETE / MERGE / INSERT OVERWRITE go to OneLake; DROP TABLE only
unmounts); no shadows or preload; `notebookutils.notebook.run` is Local Spark only; `USE db` is rewritten
to `USE DATABASE`; `DESCRIBE HISTORY` and `SET` are not supported by Sail; `DROP TABLE` on a
lakehouse table is refused (Fabric would delete the folder); legacy top-level tables of a
schema-enabled lakehouse show up as schemas in Fabric's listing and are reached by path.
Sail's own OneLake catalog is not used (it rejects tables with integer / decimal columns;
reported in `docs/requests/lakesail-0.7.2.md`); Cobalt's loopback catalog normalises the
types itself.

## Spark SQL query tabs (0.8.2)

"Just run this query against the lakehouse" without a notebook. In the Servers sidebar the
**Local Spark** root shows the session's state and the lakehouses it knows: those bound to open
Spark notebooks and tabs, the ones pinned on the Fabric panel, and the ones used before.
Double-click a lakehouse (or right-click › New Spark SQL query; File › New Spark SQL Query
inherits the active tab's binding and otherwise asks for the workspace and lakehouse; a
lakehouse on the Fabric panel has "New Spark SQL query" in its context menu) for a tab named
`SparkSQL_1 · test`. A fresh tab shows its toolbar and a Spark getting-started pane (choose
or change the lakehouse, lakehouses used before, the session state). The **Spark** menu on the
menu bar starts, restarts, stops and interrupts the session, opens the log and the shadows,
and reaches the runtime settings, from any tab; the status bar's Spark entry is a readout. Its toolbar carries the
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

## Semantic models: DAX query tabs and the model explorer (0.10.0)

Power BI / Fabric semantic models show up in the Fabric panel under their workspace, with a
cube icon. Click the row for the **model explorer** (Tables with their columns, measures and
hierarchies; Relationships with cardinality, direction and active state; calculation groups
when the model has them; hidden objects on request through the Tables context menu).
Double-click a model (or right-click › New DAX query) for a tab named `DAX_1 · <model>`. The
tab is a query tab: DAX highlighting, completion from the model (`'` offers tables, `'Table'[`
that table's columns, `[` measures then columns, otherwise functions, keywords, tables,
measures and snippets like `topn`, `summarize`, `define`), F5 runs the text, Ctrl+Enter runs
the `EVALUATE` under the caret together with the `DEFINE` block, several `EVALUATE`s give
several result sets, and the grid, charts, totals, find, Copy, Export, Run to File, history and
Pin to dashboard work as on any other tab. The row cap (Settings › Execution) applies per
`EVALUATE`; a capped set offers "Run again without the cap". Stop ends the run locally (the
service finishes the query on its own; the command timeout is sent along as the query timeout).
The model rows' context menus run quick queries into the tab: Top 100 rows, Count rows,
Distinct values, a column summary (rows, distinct, blanks, min, max), Evaluate measure,
Evaluate by a column, Define measure (the expression pasted into a DEFINE block to edit),
Define all measures, and the INFO.VIEW metadata as result sets. Double-click a column or
measure to insert `'Table'[Column]` / `[Measure]` at the caret. Hover a measure for its
format string and expression (the service hides expressions from Build-only users; the tree
says so).

Queries run through Microsoft's **Arrow query API** (`executeDaxQueries`) as the account the
Fabric panel is signed in with. Prerequisites: the model on a Fabric or Premium capacity (Pro
workspaces have no query endpoint; the run says so), **Build** permission on the model, the
tenant settings "Dataset Execute Queries REST API" and "Allow XMLA endpoints and Analyze in
Excel" (both on by default), and the delegated Power BI permission **`Dataset.Read.All`** on
Cobalt's app registration — a sign-in without it is refused before anything is sent, with the
reason; after the permission is added, **Grant permissions** on the Fabric panel picks it up.
The API answers at most 120 requests per minute per user, which matters only for dashboards
with many model tiles on short intervals. "Copy connection string" on a model row gives the
`powerbi://` data source for Excel, SSMS, DAX Studio or Tabular Editor. Azure Analysis Services,
SSAS and MDX / DMV queries come with the XMLA transport (slice C of the design).

Agent: `fabric_state` lists `semantic_models` per expanded workspace; `dax_query {workspace,
model, text?, run?}` opens a tab; `model {workspace, model, action: tree|refresh|functions}`
returns the catalog; `state` tabs carry `kind: "dax"` and a `tabular` block.

## Explore: the pivot builder on semantic models (0.10.0)

For the people who want "rows, columns, values" without writing DAX. Right-click a semantic
model on the Fabric panel › **Explore** (a measure's context menu has "Explore with this
measure"; a model explorer's Tables row has Explore too). The tab shows the model's tables on
the left with their columns and measures (search box on top; hidden objects stay hidden) and
four wells on the right: **Rows**, **Columns**, **Values**, **Filters**. Click a field to add
it (a measure goes to Values; a numeric column to Values as a Sum, since that is what it was
modelled for; any other column to Rows); right-click for Add to Rows / Columns / Values and
Filter on…. Chips in the wells: ✕ removes; right-click a Rows or Columns chip to sort by it,
move it left or right, move it to the other well, or filter on it; right-click a Values chip to
change the aggregation (Sum, Count, Count distinct, Average, Min, Max for columns) or sort by
it. **Top** on the toolbar keeps the first n rows by the sort (or by the first value);
**Subtotals** adds a subtotal row per grouping level (the DAX uses `ROLLUPADDISSUBTOTAL`; the
matrix shows them as "Total" rows, set off). A filter opens a small window: pick the operator
(is one of / is not one of with a checklist of the column's values, loaded from the model;
between; contains; starts with; in the last n days, months or years for dates; blank; not
blank) and Apply.

With **Auto** on (the default) every change runs the query; otherwise Run does. The result
lands in the usual results area — grid, charts, totals, find, Copy, Export, history — and with a
field in Columns it shows as a **matrix** (row keys down the left, column keys across the top,
values in the cells; right-click › Copy matrix for tab-separated text) with **Flat** a click
away. **DAX** on the toolbar shows the generated query; Copy copies it; **Open in query tab**
puts it in a DAX tab to edit and run, which is the quickest way to learn what a well does.
**Save** writes a `.cobalt-explore.json` file (File › Open and the command line open them;
the title in the toolbar names the tab). **Pin to dashboard** makes a tile from the current
query: dashboard tiles can now run on a semantic model (through the Fabric panel's sign-in,
no connection to pick), and a tile pinned from Explore has an Open in Explore button.

Limits worth knowing: the matrix keeps the first 10,000 row keys and 500 column keys; the
value picker loads the first 500 distinct values; a non-additive measure in a matrix shows
the model's own subtotal (not a local sum), which is why subtotals come from the query. The
spec is engine-agnostic, so the same builder can later produce SQL for a warehouse table.

## Analysis Services over XMLA (0.10.0)

The Servers tree has an **Analysis Services** root for the engines that speak XMLA but are not
Fabric semantic models: right-click it (or the "Add an endpoint…" row) for the dialog. Three
kinds: **Azure Analysis Services** (region + server name, signed in as the Fabric panel's
account; the cluster is resolved on connect), **HTTP (msmdpump)** (the URL of SQL Server
Analysis Services' IIS endpoint, basic or anonymous authentication; the password goes to the
keyring), and **Power BI workspace** over XMLA (workspace name and id; experimental, see
below). **Test** connects and lists the models before you save. Expand an endpoint for its
models, click a model for the model explorer (tables, columns, measures, hierarchies,
relationships, read from the `TMSCHEMA` DMVs or INFO.VIEW), double-click for a query tab. The
tab says "DAX / MDX": a DAX query, an MDX `SELECT … ON COLUMNS … FROM [cube]`, or a DMV
(`SELECT * FROM $SYSTEM.DISCOVER_SESSIONS`) all run, every result lands in the grid (MDX comes
back flattened, with the members' unique names as column headers). Explore and dashboard
tiles work on these models like on Fabric ones.

How it talks: plain SOAP over HTTPS as MS-SSAS specifies, from Rust, no .NET. Cobalt asks for
text XML (the first request negotiates with the server, binary XML and compression are never
advertised), opens a session (`BeginSession`), sends `Execute` / `Discover` with the session
id, and `Cancel` on Stop. A session the service moved or expired is reopened once. The
rowset's inline schema becomes the grid's columns; `_x0020_`-style escaped names are decoded.

Power BI over XMLA: Settings › Connections › **Semantic models** chooses the transport for
Fabric models (0.10.1): **Auto** sends DAX through the Arrow query API and MDX or `$SYSTEM` DMV
text through the XMLA endpoint, **XMLA** sends everything there, **Arrow query API** never
uses XMLA (MDX and DMVs are then refused). Cobalt does what the official client library does
(found by decompiling it): it exchanges the Entra token for a workload token at the Power BI
service, resolves the capacity to its cluster, and talks XMLA to that cluster. One transport
limit to know: a date outside 1899-12-30 … 9999-12-31 23:59:59 in the result (the
`ValidTo = 9999-12-31 23:59:59.999…` sentinel that slowly changing dimensions use) faults the
whole query over XMLA, exactly as it does in SSMS; the Arrow query API carries such values, so
use the API path (the Auto or Arrow setting) for those tables, or project the columns you need. Not supported: SQL Server Analysis Services over its native TCP
port (that is not HTTP; use the IIS HTTP endpoint), and Windows integrated authentication on
the HTTP endpoint (basic over HTTPS or anonymous for now).

## Semantic models: chooser, connection controls, drag and drop (0.10.1)

Your first-pass notes on 0.10.0, addressed:

- **The transport is a setting**, not a toggle on the query: Settings › Connections ›
  Semantic models, Auto / Arrow query API / XMLA, for every query on a model (DAX tabs,
  Explore, the model explorer, tiles). Auto is the rational default: DAX goes through the Arrow
  API (fast, lossless), MDX and `$SYSTEM` DMVs through XMLA because the API cannot run them.
  The status bar names the transport each run used.
- **New DAX query** is in the + menu (and "A click on + opens"), in File, and in the command
  palette. With a DAX or Explore tab active it inherits that model; otherwise the chooser
  opens: Fabric workspaces and saved Analysis Services endpoints on the left, models on the
  right, a filter, a Recent models entry. The chooser is the same dialog Connect / Change
  Connection open on a DAX tab, and what the model button on the toolbar opens — on a DAX tab
  and on an Explore tab (where switching models clears the wells, since the fields belonged
  to the old model). The getting-started pane on an empty DAX tab is now about DAX: the model,
  Choose another model, Explore this model, starting-point queries, recent models.
- **Drag and drop in Explore**: fields from the list onto any well, chips between wells and
  within a well to reorder (a chip lands before the one you drop it on), a chip back onto the
  field list to remove it. A label follows the pointer; the wells that take the field are
  outlined and the one under the pointer lights up. Clicking still works as before.

Agent labels: `tab model` (the DAX toolbar's model button), `explore model`, `model source
<workspace or endpoint>`, `model source recent`, `model <name>`, `model filter`, `recent model
<name>`, `semantic transport auto|arrow|xmla` in Settings; verbs `command {id:
"file.new_dax_query"}`, `explore {action: "drop", …}`, `settings {set: {"semantic.transport":
"xmla"}}`.

## Drop files on the window (0.10.2)

Drag files from Explorer (or a folder) onto the Cobalt window. While they hover, a card lists
each file and what the drop will do; the drop does it. The routing: a `.sql` file is read and
opened in a tab of the dialect it looks like — T-SQL, Spark SQL or DAX / MDX — and when the
text does not say (a plain `SELECT id FROM t`), in the kind "A click on + opens" names (the +
menu's setting). `.dax`, `.msdax`, `.mdx`, `.hql`, `.sparksql` and `.sqlplan` say what they are
by name; `.txt` is sniffed too (a query opens, otherwise it is a delimited data file). T-SQL
lands in the empty active tab, or a new tab on the active connection; Spark SQL on the active
lakehouse; DAX on the active model, or the model chooser opens and the file loads once you pick
one. Data files (`.csv .tsv .parquet .arrow .feather .jsonl .xlsx`, JSON arrays) open the
Import dialog on the active tab's connection. `.ipynb` and Fabric `notebook-content.py` open as
notebooks; any other `.py` becomes a PySpark notebook with the file as its first cell. A
`cobalt-connections.json` export imports the connections; an Azure Data Studio `settings.json`
opens the ADS importer. A folder with a `_delta_log` opens a Spark SQL tab reading the Delta
table; any other folder opens in the Files sidebar. Dashboards and Explores open as themselves.
A file that is already open comes to the front instead of opening twice. The command line
(`cobalt file.sql …`) still opens files the File › Open way (no dialect sniffing yet).

Agent: `drop {paths: [...]}` does what a drop does; `classify: true` reports the plan without
opening; `hover: true` shows the overlay for those paths (`hover: false` clears it).

## Non-Latin text: system fonts on demand (0.10.3)

Your Japanese names drew as tofu boxes because egui ships Latin / Greek / Cyrillic fonts
only. Rather than bundling tens of megabytes of CJK fonts, Cobalt now watches what it draws
(grid cells and headers, the matrix, editor lines, tree rows, messages, anything typed or
pasted) with a byte scan, and the first time a script appears it loads that script's font
from the OS on a worker thread and adds it as a fallback to both families: the next frame
draws it. Windows: Yu Gothic (Japanese), Microsoft YaHei (Simplified), Microsoft JhengHei
(Traditional), Malgun Gothic (Korean), Segoe UI (Arabic, Hebrew, Georgian, Armenian…),
Nirmala UI (Indic), Leelawadee UI (Thai, Lao, Khmer), Ebrima, Myanmar Text, Microsoft
Himalaya, Mongolian Baiti, Gadugi, Segoe UI Symbol. Han-only text (no kana, bopomofo or
hangul) is ambiguous between the three CJK glyph styles, so the user locale decides: `ja` →
Japanese, `zh-TW` / `zh-HK` / `Hant` → Traditional, otherwise Simplified. Emoji stay on
egui's bundled Noto Emoji (monochrome). macOS and Linux paths are best effort (Hiragino,
PingFang, Apple SD Gothic Neo; Noto CJK or `fc-match`) and untested so far. The agent `state`
has `font_packs`, the packs loaded this session; the log line `system font loaded for glyph
fallback` names the file.

## Fonts: the manager, downloadable packs, your own font (0.10.4)

The stylised names (𝓚𝓲𝔂𝓪 𝓑✰, 𝐬𝐚𝐫𝐚𝐡❀, ✕NAHO) boxed because the mathematical-alphanumeric and
dingbat blocks were not mapped to any pack; they now load Segoe UI Symbol on Windows, which
covers them all, including the emoji modifiers.

Your proposal is in as **Settings › Fonts**. The page has three parts. *Interface font* and
*Editor and grid font*: the bundled defaults (Ubuntu Light, Hack), an installed pack from the
list (Noto Sans, JetBrains Mono), or any `.ttf` / `.otf` / `.ttc` on the computer; they apply
on Save. *Font packs for other scripts*: the registry — Noto Sans CJK (one 19 MB collection
with the Japanese, Korean, Simplified and Traditional faces; the locale orders them), Symbols
and Math (2.5 MB), Arabic / Hebrew and neighbours, Indic, Thai / Lao / Khmer / Myanmar, and
Ethiopic / Tibetan / Mongolian / Cherokee — each with Download / Remove and a progress bar;
files land in the app data folder (`…\Cobalt SQL Works\data\fonts`) and load at startup
before any system font, so a downloaded pack also gives consistent glyph style across
machines. *Refresh the list from cobaltsql.org* fetches `site/fonts.json` (the same file ships
in the binary as the fallback), so new packs or moved URLs need no release.

The suggestion: when a script shows up that no font on the computer draws (on Windows that is
rare; on Linux and macOS it is the common case), a card at the bottom right names the script
and the pack that covers it with its size, and offers Download / Settings › Fonts / Not now /
Don't suggest again (the last is the `fonts.suggest_downloads` setting). Only one suggestion
is up at a time, and a declined pack is not offered again that session.

Agent: `fonts` (state: registry entries with installed flags, system fonts loaded, the
requested packs, the chosen fonts, downloads in flight, the current suggestion), `fonts
{action: download|remove, id}`, `{action: refresh}`, `{action: set_ui|set_mono, value}`,
`{action: suggest, pack: "MiddleEast"}` (shows the card for a pack as if it had no system
font). Labels: `settings tab Fonts`, `settings ui-font`, `settings mono-font`, `font pack
download <id>`, `font pack remove <id>`, `font suggestion download`, `font suggestion later`.

## Local query: SQL over your results, in-process (0.11.0, slice A)

The V2 bet from the product design, built: a DataFusion engine inside the app. Run any query
in any tab (SQL Server, Fabric, Spark, DAX, Explore) and its result is a **local table** named
after the tab: `sqlquery_1`, `sqlquery_1_2` for a second result set, and `_` for whatever
arrived last. Nothing is copied or written: the engine reads the chunks the grid already holds
(spilled ones through the same cache), and the table disappears when the tab closes or runs
again, so a session that never opens a Local tab pays nothing.

**Local query tab**: + › New local query (or File, or "A click on + opens"). The editor knows
the local tables, their columns and the engine's functions (Ctrl+Space). The dialect is
DataFusion's, PostgreSQL-flavoured, with the T-SQL habits rewritten for you and reported in
Messages: `[brackets]`, `TOP n`, `ISNULL`, `GETDATE()`, `LEN` (Settings › Query execution turns
that off). F5 runs every statement, Ctrl+Enter the one under the caret, Cancel stops, the row
cap pauses exactly at the cap and "fetch more" continues, Est. plan (Ctrl+L) prints the
logical and physical plan in Messages. Results land in the normal grid with everything it
does (sort, filter, find, totals, charts, profile, export, Run to File, pin), and each result
is a table too, so a chain of Local tabs refines step by step.

**Query this result**: the funnel button on any result set's toolbar opens a Local tab with
`SELECT * FROM <its table> LIMIT 100` already run. **The Servers tree has a Local root** with
the tables (rows, columns, the producing tab, "arriving" while it streams); expand one for
its columns, double-click to query it, right-click for Query / Reveal the tab / Rename. A
renamed table keeps its name until its tab runs again.

The demo from the design: run a query on production in one tab and the same on development
in another, then in a Local tab:

```sql
SELECT p.id, p.amount AS prod, d.amount AS dev
FROM prod_orders p FULL OUTER JOIN dev_orders d ON p.id = d.id
WHERE p.amount IS DISTINCT FROM d.amount
```

Performance was measured before and after on a million-row stream: 4.2 s both times, with
streaming and scrolling frame times within noise, because registering a result is one Arc
clone. The engine's memory pool is the app's memory budget (Settings › Advanced); its sorts,
joins and aggregates spill to the spill directory past it. The debug binary grew by about 45
MB with DataFusion in it; release builds will show the real number at the next tag.

Not in this slice (B–D in the design): files and Delta folders as tables, Diff, Explore over
local tables, chained notebook cells, scratch persistence, OneLake reads.

Agent: `local_query {text?, run?}`, `local_tables {action: list | rename | query, name, to}`,
`state.local_tables`, per-tab `local {running, notes}`; labels `query locally <set>`, `local
table <name>`, `local column <table>.<col>`, `tab local tables`, `local table new name`,
`settings local tsql sugar`, `local welcome table <name>`.

## Local query, slice B: drop a file, query it (0.11.0)

The thing you said you would have loved a year ago: drop a Parquet, CSV, TSV, JSON-lines or
Arrow file (or a Delta folder) on the window and it is a table. The drop card now says *Open
as a local table*; the tab that opens is a Local tab bound to the file, with `SELECT * FROM
<name> LIMIT 1000` already run, so the grid, profiler, charts, exports and Run to File are
there without any connection. The file is read on every scan (nothing is loaded into memory
up front): a 5 GB Parquet costs nothing until you query it, and the row cap and fetch-more
apply as usual. CSV delimiters and headers are sniffed, types inferred from the first 5,000
rows; the Parquet footer gives the row count in the Local root; CSV and JSON show "—" until
queried. The toolbar has the file chip and **Import into a connection…**, which takes the file
to the Import dialog on a connected tab (or opens one). The Files sidebar lists data files now
and offers *Query as a local table*; a folder with a `_delta_log` offers *Query the Delta
table locally*. Delta reads go through delta-rs (deletion vectors, column mapping, the current
version); when that fails the drop falls back to the Spark route as before. `CREATE EXTERNAL
TABLE t STORED AS PARQUET LOCATION '…'` and `CREATE TABLE x AS SELECT …` work in a Local tab
and the tables appear in the Local root after the run. Excel files and JSON arrays still go
to the Import dialog (the engine reads neither).

Agent: `local_tables {action: register_file, path}`; `drop` reports `LocalTable` and
`DeltaFolder` for these paths. Labels: `tab local import`.

## Local query, slice C: Diff, and Explore without SQL (0.11.0)

**Diff.** Run the same query twice (or on two servers, or drop two versions of a file), then
*Diff with…* on either result's toolbar (the arrows icon), or right-click a table under the
Local root. The dialog takes A (the earlier side, "was") and B ("now"), guesses the key (a
primary key, `id`, a column ending in `_id`, `key` or `code`, else the first shared column) and
lets you tick the keys and the columns to compare (none ticked = every other shared column).
The Diff tab is a Local tab: its editor holds the generated query (`FULL OUTER JOIN` on the
keys, `IS DISTINCT FROM` across the compared columns, so nulls equal nulls), so you can keep or
edit it; the grid tints rows green (added: only in B), red (removed: only in A), amber (changed)
with the differing cells lit, and shows each compared column next to its `<column>__was` value;
the toolbar has a chip per class with its count, and a click shows only that class (it is a
filter on the `__change` column, so the funnel can clear it too). Export, copy, Run to File
and Save as table work on the diff like on any result.

**Explore over local tables.** The pivot builder from semantic models now works on any local
table: *Explore this result* on a result set's toolbar (the rows icon), or *Explore* on a table
under the Local root. Same wells, drag and drop, aggregations (Sum / Count / Count distinct /
Average / Min / Max on columns; a local table has no measures), filters (the "is one of" picker
loads the column's distinct values from the engine), Top N, subtotals (`GROUP BY ROLLUP`) and
the matrix or flat view; the DAX button shows the generated SQL and *Open in query tab* hands
it to a Local tab. Pin to dashboard is not offered here (local tables are not stable across
sessions); it comes with scratch tables in slice D.

Agent: `diff {a, b, keys?: [...], compare?: [...]}` (keys guessed when absent; the tab's
`local.diff` carries the counts and the class filter), `explore {action: new, table}` then
the usual `add`, `drop`, `filter`, `top_n`, `subtotals`. Labels: `diff locally <set>`,
`explore locally <set>`, `diff filter all|added|removed|changed|same`, the dialog's `diff-a`,
`diff-b`, `diff key <col>`, `diff go`.

## Local query, slice D: scratch tables, cells, lakehouse tables, tiles (0.11.0)

**Scratch tables.** Result tables go with their tab. To keep one, *Save as local table…* (the
box icon on the result set's toolbar) asks for a name and writes the rows as Parquet to
`data/local/scratch` under the app data folder; the table is `scratch.<name>` from then on,
loaded at every start, listed in the Local root with a "scratch (kept across sessions)" badge
and *Delete the scratch table* in its menu. The same happens for `CREATE TABLE scratch.<name>
AS SELECT …` in a Local tab, and `DROP TABLE scratch.<name>` deletes the file. Nothing else is
persisted: a scratch table exists only because you asked for it.

**Notebook cells.** In a SQL notebook (a connection kernel), each cell's result registers as
`cell_1`, `cell_2`, … while the notebook is open; re-running a cell replaces that cell's table
only, so a Local tab can `SELECT … FROM cell_1 JOIN cell_2`, and Diff or Explore take cells
too. A "Local SQL" cell kind (a cell that runs on the engine inside the notebook) is not in
this slice.

**Lakehouse tables from OneLake.** In the Lakehouse pane, right-click a table: *Query locally
(no Spark)* reads the Delta table from OneLake with your Fabric sign-in (the storage-scope
token the exports use) in the local engine and opens a Local tab with the first 1,000 rows;
the table is listed in the Local root with its `abfss://` URL. Read-only; Spark remains the
way to write. Verified live on the "Fabric test" workspace: `dbo/publicholidays` in the `test`
lakehouse opened in a Local tab (1,000-row preview in 0.4 s) and a `count(*)` over the whole
table ran from a second Local tab. Column names there are camelCase and the engine folds bare
names to lower case, so the T-SQL sugar now resolves names case-insensitively against the
local tables (`countryorregion` → `"countryOrRegion"`, noted in Messages); quote a name
yourself to bypass it, or turn the sugar off in Settings › Query execution.

**Tiles on the local engine.** *Pin to dashboard* is now offered on Local tabs and on Explore
tabs over local tables; the tile runs the SQL on the local engine with no connection. Pin
queries over scratch tables or files: a tile over a result table (`sqlquery_1`) errors after a
restart until that tab runs again.

Agent: `local_tables {action: save_scratch, name, set?}` (active tab), `{action:
remove_scratch, name: "scratch.x"}`, `{action: register_onelake, workspace_id, lakehouse_id,
table: "schema/name" | "name"}`; the results toolbar label `save locally <set>`; dialog
`save_scratch` with `scratch name` and `scratch save`; `dashboard {action: pin}` on a Local
tab.

## Local tables sidebar, Explore any table, completion quoting (0.11.1)

**The Local sidebar.** The lightning bolt on the rail (Ctrl+Shift+L) lists everything the
in-process engine knows, grouped by origin: *Results* (live with their tabs), *Files and Delta*,
*Scratch* (kept across sessions), *OneLake*, *Made by statements*. Each row shows rows, columns
and the cost (memory a result holds — it is the tab's own memory, nothing is copied — or a
file's size); the summary line totals the memory. Filter by name or column; expand a table for
its columns and types; double-click to query it. Right-click: Query, Explore, Diff with…, Save
as scratch table… (works for any table, so a file or a result you want to keep is one click
away from `scratch.<name>`), Rename…, Reveal the tab, Copy name, Insert name into editor, and
Forget / Delete with a confirmation that spells out what happens. The header's buttons open a
new Local query and register a file or a Delta folder through a picker. The Local root in the
Servers tree stays as a compact view of the same thing.

**Explore any table.** Right-click a table or view under a connection in the Servers tree:
*Explore (pivot builder, no SQL)*. The builder you know from semantic models and local tables
now writes T-SQL and runs it on that connection: `[db].[schema].[table]` so the tab's database
does not matter, `TOP (n)` for Top N, `GROUP BY ROLLUP` and `GROUPING` for subtotals, `LIKE …
ESCAPE` for contains / starts with, `DATEADD` for "last n days". The "is one of" picker lists
the column's distinct values from the server (top 500). *Open in query tab* hands the T-SQL to
a query tab on the same connection; *Pin to dashboard* makes a tile that runs there; the tab
comes back after a restart (it reconnects on the first run). Works on SQL Server, Azure SQL,
Fabric Warehouse and the SQL endpoint. Not yet: Spark tables, joins across tables (one table
per Explore, as for local tables).

**Scratch tables by name (0.11.2).** The same schema-and-name rule now holds everywhere a
table name becomes SQL: Query and Insert name from the Local sidebar or the Local root write
`scratch.name`; Explore, Diff, the filter picker and Save as scratch table over a scratch table
write `scratch."name"`. Before 0.11.2 those produced `"scratch.name"`, a table the engine does
not have.

**Completion in Local tabs.** Typing `scratch.` offers the scratch tables and accepting inserts
the name after the dot; from the top level a scratch table inserts as `scratch.name`. Names are
quoted only when the engine would otherwise fold them (`"Mixed Case"`, `"countryOrRegion"`),
with the engine's double quotes, never a bracketed `[schema.name]`.

Agent: `explore {action: new, profile: "idletest", table: "dbo.big", database: "cobalt_test"}`,
`explore {action: distinct, field: "big[category]"}` then `state.explore.distinct`;
`local_tables {action: save_scratch, name: "x", from: "orders"}`, `{action: forget, name}`;
`click` on `sidebar Local`, `local table: <name>`, `local filter`, `local new query`, `local
register`; `forget local go` in the confirmation.
