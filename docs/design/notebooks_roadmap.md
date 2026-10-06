# Notebooks and local Spark: the next version roadmap

Status: draft for discussion, 2026-10-05. Written after v0.6.1 closed the V1.x backlog, and after
reading `methodify/local-spark-mcp` (v0.3.4) end to end. Companion decision: D009 (proposed).

## 1. The idea in one paragraph

A Fabric developer's loop today is: edit a notebook in the portal, start a Spark session, wait,
run, wait, and every write touches real lakehouse tables. local-spark-mcp already proved the
alternative on this very machine: a local Spark that matches the Fabric runtime version for
version, reads lakehouse tables straight from OneLake, and turns the first *write* to a table
into a local Delta **shallow clone** so nothing reaches OneLake unless you say so. Cobalt SQL
Works has the other half: the Entra sign-in, the Fabric explorer, the streaming grid, exports,
profiler and editor. Put together: open a notebook (local file, or straight from a Fabric
workspace), run its cells on local compute against live OneLake data in a sandbox, see
DataFrames in the same grid every query result uses, and save the notebook back to Fabric. Cobalt
provisions and owns the whole runtime, so none of this needs a Python, Java or Spark install on
the machine.

## 2. What local-spark-mcp already solved (and what Cobalt should reuse, not rewrite)

| Concern | How local-spark-mcp does it | Cobalt's take |
|---|---|---|
| Runtime parity | Profiles pin Fabric runtimes: `fabric-1.3` = PySpark 3.5.5 + Delta 3.2.0 + Python 3.11 + Java 17; `fabric-2.0` = PySpark 4.1.1 + Delta 4.2.0 + Python 3.13 + Java 17/21. Windows needs Python 3.11 for both (SPARK-53759). | Adopt the profiles as the runtime manifest. Ship 1.3 first. |
| Process model | Parent process spawns `python -m local_spark_mcp.worker --port N`; the worker holds an IPython shell with a live SparkSession and serves a length-prefixed JSON socket (`{"id","method","params"}` → `{"id","ok","result"}`); dead JVM detected and respawned. | Cobalt becomes the parent. A Rust `cobalt-kernel` crate speaks the same framing, so the engine is reused byte-for-byte and the MCP/stdio layer is simply not used. |
| OneLake auth | A loopback HTTP token endpoint with a per-start secret; the JVM-side `ch.fs.HttpTokenProvider` (a Hadoop `CustomTokenProviderAdaptee`, shipped as a jar) fetches a fresh storage token whenever ABFS needs one. | Cobalt serves that endpoint itself from its Entra refresh token (`onelake_token` exists today), so no `az login` and no second identity: the notebook runs as the signed-in user. |
| Lakehouse tables | `OneLakeCatalog` (Scala, extends `DeltaCatalog`): `lakehouse.table` resolves on first touch; sandbox mode materializes a `SHALLOW CLONE` under a shadow root, writethrough points at OneLake, readonly refuses writes. Confs `spark.localspark.{workspace_id,lakehouse.<name>,write_mode,shadow_root,onelake_host,dv_strategy}`. | The Fabric explorer already knows workspace and lakehouse GUIDs; Cobalt registers them at session init and offers the write mode in the notebook toolbar (sandbox by default). |
| Shadows | `shadow_status`, `discard_shadow(table, only)`, `restore_shadow(table, version)`; clones frozen at first-touch version; `persist_shadow` across sessions. | A "Shadows" panel: what is cloned, read vs written, discard, rewind. |
| Files | Default lakehouse `Files/` mirrored under `~/.local-spark/lakehouses/<ws>/<lh>/Files`, linked at `/lakehouse/default/Files` (`C:\lakehouse\default` junction on Windows); `sync_files` pulls subtrees. | Keep; surface the synced subtrees in settings. |
| Fabric notebooks | `run_notebook` runs a Git-format `.py` export with cell selection and parameters; `%pip`/`%run` reported, not executed; `notebookutils`/`mssparkutils` shim (secrets, variable library, fs, notebook.run/exit, runtime.context). | Cobalt opens `.ipynb` *and* the Git `.py` form, fetches notebooks from a workspace through the Fabric REST API, and saves back. |
| Host prerequisites | Java discovered via vfox → `JAVA_HOME` → PATH; winutils shipped in the package; libraries via `uvx --with`. | Cobalt detects what exists, and otherwise installs everything into its own app-data folder (§4). |

Two protocol additions Cobalt needs and should contribute upstream: **Arrow results** (a
DataFrame `display` or a `run_sql_arrow` method returning Arrow IPC bytes instead of JSON rows,
so a 100,000-row frame lands in the grid with types intact) and **interrupt** (cancel the running
cell: `sparkContext.cancelAllJobs()` plus a KeyboardInterrupt into the shell thread).

## 3. Product shape

### 3.1 The notebook document

- File format: `.ipynb` (nbformat 4) with Fabric's metadata conventions, so a notebook moves
  between Cobalt, the Fabric portal and Git without translation: `metadata.language_info`,
  `metadata.microsoft` (default lakehouse), cell `metadata.microsoft.language`, the
  `parameters` cell tag, `%%sql` cells inside a PySpark notebook. The Git `.py` form (what
  Fabric's Git integration writes) opens and saves too.
- Cell kinds: Markdown, SQL, PySpark/Python. Markdown renders in place (egui_commonmark) and
  edits on double-click.
- Each cell's editor is the Cobalt editor (multi-cursor, snippets, completion against the
  connection's catalog for SQL cells).
- Outputs are cached in the file: `stream`, `execute_result`, `display_data`, `error`, and a
  Cobalt tabular output carrying `application/vnd.apache.arrow.stream` (base64) next to a
  `text/html` preview of the first rows, so the Fabric portal and GitHub still render something.
  Reopening a notebook restores grids from the Arrow payload without running anything.
- Export to HTML and Markdown with results embedded (the thing ADS never had).

### 3.2 Two ways to run a cell

- **SQL against a connection**: the notebook has a connection like a query tab; SQL cells go
  through the session actor and come back as ordinary result sets (grid, filters, profiler,
  Save as table, Run to File). This is the whole of the first release and needs no runtime.
- **Local Spark**: PySpark cells and `%%sql` cells run in the managed runtime's worker. stdout,
  stderr and tracebacks show under the cell; `display(df)` and bare DataFrame expressions come
  back as Arrow and open in the grid; a cell can be interrupted; the session persists across
  cells and notebooks until reset. Spark SQL results use the same path.

The kernel picker lives in the notebook toolbar: a connection, or "Local Spark (Fabric 1.3)".

### 3.3 Fabric integration

- The Fabric explorer lists **Notebook** items. Open fetches the definition (`getDefinition`,
  ipynb format); Save pushes it back (`updateDefinition`) with a confirmation that names the
  workspace. "Open a copy" for the cautious.
- The notebook's default lakehouse (from its metadata, or picked from the explorer) is
  registered with the session; every lakehouse in the workspace is addressable as
  `lakehouse.table`, exactly as in Fabric.
- Write mode is visible at all times: **Sandbox** (default: clones, OneLake untouched),
  **Read only**, **Write through** (needs an explicit switch with a warning). The Shadows
  panel shows what has been cloned and lets you discard or rewind.
- "Run in Fabric" (job scheduler API) is a later item; the first goal is the local loop.

### 3.4 The managed runtime

Everything under the app-data folder (`…/Cobalt/Cobalt SQL Works/runtime/`), pinned by a
manifest shipped with each Cobalt release, hash-checked, resumable, removable:

| Component | Source | Notes |
|---|---|---|
| `uv` | GitHub release binary, pinned version + SHA-256 | the only bootstrap download that is Cobalt's own |
| Python 3.11 | `uv python install 3.11` with `UV_PYTHON_INSTALL_DIR` inside the runtime folder | python-build-standalone; 3.11 on Windows regardless of profile |
| Environment | `uv venv` + `uv pip install "local-spark-mcp[fabric-1.3]==<pin>"` | from PyPI once published (today: `git+…@vX`); brings pyspark, delta-spark, pyarrow, the token-provider jar and winutils |
| JDK 17 | Microsoft Build of OpenJDK 17 LTS (zip/tar, pinned URL + SHA-256); Eclipse Temurin as the alternate | never Oracle; Microsoft's build is what Fabric runs |
| Delta and hadoop-azure jars | pulled by `configure_spark_with_delta_pip` from Maven on first session | pre-warmed during provisioning so the first notebook does not wait on Maven |
| Hadoop on Windows | winutils + hadoop.dll shipped inside local-spark-mcp | nothing extra to fetch |

Behaviour: Settings → Spark runtime shows the state of each component (found on the machine /
installed by Cobalt / missing), with **Use what I have** (adopt an existing JDK, uv or Python
that meets the pins) and **Install for me** (download everything above). A smoke test runs
`SELECT 1` through the worker and reports the versions. Disk use and a Remove button sit next
to it. Downloads honour the system proxy and resume on retry. Estimated size: JDK ~180 MB,
Python ~30 MB, environment ~350 MB, jars ~60 MB.

## 4. Architecture additions

```
crates/cobalt-notebook   nbformat model (.ipynb + Fabric metadata + Git .py), outputs incl. Arrow,
                         HTML/Markdown export — no egui, no tokio
crates/cobalt-runtime    manifest, detection, downloads (reqwest, sha256, resume), uv/python/venv/JDK
                         provisioning, smoke test — no egui
crates/cobalt-kernel     the worker process manager and socket protocol client (init, run_code,
                         run_sql, run_sql_arrow, interrupt, session_info, shadow_*, sync_files),
                         plus the loopback OneLake token endpoint — tokio, no egui
crates/cobalt-app        notebook tabs (a new tab kind beside query tabs), cell UI, kernel picker,
                         Shadows panel, Settings → Spark runtime, Fabric notebook open/save
```

The session actor model stays: SQL cells reuse `Command::Run` with a sink per cell; Spark cells
go to a `kernel_actor` per runtime with the same event style (`CellStarted`, `CellOutput`,
`CellResultSet(Arc<ResultSet>)`, `CellDone`).

## 5. Phases

### 0.7 — SQL notebooks (first slate, proposed)

1. `cobalt-notebook`: nbformat read/write with Fabric metadata, Git `.py` import, output
   model, HTML/Markdown export; unit-tested against notebooks exported from the Fabric test
   workspace.
2. Notebook tab: cell list, add/move/delete/split cells, Markdown render/edit, SQL cells in the
   Cobalt editor, run cell / run all / run above, per-cell grids (collapsible, with the full
   results toolbar), execution counters and timings, cached outputs restored on open.
3. Files sidebar and File → Open handle `.ipynb`; New Notebook; Save / Save As; hot exit.
4. Settings → Spark runtime: detection and **Install for me** for uv, Python 3.11, the
   `fabric-1.3` environment and the JDK, with the smoke test (the kernel host speaking `init` +
   `run_sql`). This de-risks the plumbing a release early; PySpark cells stay disabled in the
   picker until 0.8.

### 0.8 — PySpark cells (shipped 2026-10-06 as slate 2; notes below)

Shipped: the kernel actor (`crates/cobalt-app/src/kernel.rs`, a thread owning one worker), the
kernel picker per notebook, Python + `%%sql` cells, stdout/stderr/tracebacks, stop (as a session
restart), status-bar session indicator, session log window. Arrow results use an interim path: a
`display`/pretty-printer hook installed at session start writes Arrow IPC files that Cobalt reads
(replaced by `run_sql_arrow` once upstream has it). Still open from the list below: inline images,
driver memory UI (the setting exists), per-worker environment variables, a true interrupt.

- Kernel actor, execution of Python and `%%sql` cells, stdout/stderr/tracebacks, interrupt,
  reset session, session info in the status bar (uptime, driver memory).
- Arrow results into grids (protocol addition upstream); `display()` of DataFrames and pandas
  frames; images (`display_data` PNG) rendered inline.
- Driver memory and extra Spark confs in settings; environment variables for workers.

### 0.9 — OneLake and Fabric

- Cobalt's own loopback token endpoint; lakehouse registration from the explorer; default
  lakehouse per notebook; write modes with the Shadows panel; Files mirror and sync.
- Fabric notebook items in the explorer: open, save back, open a copy; parameters cell and
  "Run with parameters".
- `notebookutils` coverage documented; `%pip` lines reported with a "add to runtime" action
  that installs the package into the Cobalt environment.

### Later

- Charts in cells (egui_plot) and the dashboard tiles from §5.6 of the product design.
- `cobalt run notebook.ipynb --param k=v --out report.html` for scheduled runs.
- `fabric-2.0` profile (Spark 4.1, Delta 4.2) as a second runtime.
- Run in Fabric through the job scheduler, with status in the tab.
- Notebook diff/versions in History; AI cell assist on top of the BYO-model work.

## 6. Risks and open questions

- **Size and time**: the full runtime is ~600 MB; provisioning is a one-time, progress-bar,
  resumable job, and nothing downloads until the user asks. Offline machines: provide a
  "bundle" zip for manual placement.
- **Maven at first session**: Delta and hadoop-azure jars come through Ivy; pre-warm during
  provisioning and cache under the runtime folder so later sessions are offline-safe.
- **Windows Python**: 3.11 is mandatory for workers; the manifest fixes it so users never see
  the issue.
- **Interrupt**: not in the worker today; needs the upstream change before 0.8.
- **Memory**: the default 8 GB driver is too much for small laptops; detect RAM and size the
  driver, expose it in settings.
- **Security**: notebooks run arbitrary code as the user, like every notebook tool. The token
  endpoint is loopback-only with a per-start secret; tokens never touch disk. Sandbox is the
  default write mode; writethrough needs an explicit switch.
- **Linux**: `/lakehouse` needs a one-time `sudo mkdir`; document, and fall back to the
  runtime folder when it is missing. macOS waits on the parked keychain issue.
- **Licensing**: local-spark-mcp (Apache-2.0), winutils (Apache-2.0), Microsoft Build of
  OpenJDK (GPLv2 + Classpath Exception), uv (MIT/Apache) — all fine to download and run; the
  licences get listed in About like Mesa's.

## 7. Decisions needed from the founder

1. **Reuse local-spark-mcp as the engine** (recommended) versus re-implementing the catalog and
   shim in Cobalt. Reuse means publishing it to PyPI with the two protocol additions, and Cobalt
   pins the version in its manifest.
2. **JDK vendor**: Microsoft Build of OpenJDK 17 (Fabric parity) or Eclipse Temurin 17 (widest
   platform coverage). Both are free of Oracle terms.
3. **Where notebooks live locally**: the Files sidebar root is the natural notebooks root.
4. **First slate**: SQL notebooks plus the runtime manager with its smoke test (§5, 0.7), or
   SQL notebooks alone with the runtime work pushed to 0.8.
