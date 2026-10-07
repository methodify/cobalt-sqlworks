# Spark sessions: lifecycle, lakehouses, Files, warm start — an expansion slate

*Status: decided 2026-10-06 (§11); upstream asks 0, 1, 2, 4, 5 shipped in local-spark-mcp
0.4.3/0.5.0 on 2026-10-07 and adopted in Cobalt 0.8 (contexts per notebook, attach without
restart, job descriptions); ask 3 (lazy Files) proposed upstream as 0.6.0. Follows `notebooks_roadmap.md` (slates 1–3, shipped in 0.7.x) and
D009. Covers the founder's questions: can one session serve many notebooks, what happens on
close/open, different workspaces, default lakehouse and Files, and how a user opts into the
worker's warm start and eager mounting.*

## 1. How people actually use this on Fabric

The mental model Cobalt should match, because it is the one our users carry over:

- **A session belongs to a notebook, until it doesn't.** Each notebook starts its own Spark
  session (30–90 s on a starter pool); the session dies on an idle timeout (20 min default,
  configurable) or when the user stops it. Closing the browser tab does *not* stop it.
- **High-concurrency mode** lets several notebooks share one session when they match on user,
  workspace, Spark configuration (environment) and default lakehouse. People turn it on exactly
  because waiting for a session per notebook is the worst part of the loop.
- **Default lakehouse is a property of the notebook**, not of the session: it decides what
  `Files/` and an unqualified `table` mean, and it is shown in the left pane. Other lakehouses are
  reached by `workspace.lakehouse.table` (or the `abfss://` path) — any lakehouse in the tenant
  the user can read, not only those in the notebook's workspace.
- **Files live in the lakehouse explorer pane** next to Tables. The notebook mounts
  `/lakehouse/default/Files` as a local path (the Fabric VM's own mount, lazily fetched; nothing is
  "synced" ahead of time). People use `Files/raw/…` for landing data, `spark.read.csv("Files/…")`
  and `open("/lakehouse/default/Files/…")` interchangeably, and `notebookutils.fs.mount` for
  other lakehouses.
- **Environments** carry the libraries and Spark settings; switching a notebook's environment
  restarts its session. Clusters are sized by the capacity; nobody thinks about driver memory.
- Nobody expects a notebook to "own" data on close. The session is a workbench; the lakehouse is
  the thing that persists.

Cobalt's 0.7 model is narrower than that: one worker per app, bound at start to the workspace
and default lakehouse of whichever notebook ran first, a warning when another notebook's binding
differs, and "restart to rebind". No Files story. Preload is a per-notebook checkbox for the
default lakehouse only. Nothing starts before the first cell.

## 2. Where the worker already is (0.4.2)

What local-spark-mcp gives us without any upstream change:

- `init` takes a **list of lakehouses, each with its own `workspace_id`** — one session can
  address lakehouses from several workspaces of the same tenant; the token endpoint is
  per-account, not per-workspace. `default_lakehouse` and `write_mode` are session-wide.
- **Files mirror**: `<mirror_root>/<ws>/<lh>/Files` with *selective* pulls (`files_sync` subtrees
  at init, `sync_files(paths, direction, lakehouse)` later; unchanged files skipped by size and
  mtime; push only in writethrough). `/lakehouse/default` is a junction to the default lakehouse's
  mirror, one per machine, lock-protected. `Files/` relative paths in Spark resolve against the
  mirror. `notebookutils.fs.ls/exists` work on `abfss://` directly through the host token — no
  download. `Tables/` is never mirrored.
- **Warm**: `python -m local_spark_mcp.warm` (Ivy cache; Cobalt runs it at install),
  `preload` at init or on demand (`{lakehouse: [tables]}` form, 32 workers, ~1.5–2 s per table),
  `persist_shadow` (clones survive across sessions; frozen at first-touch version), `shadow_status`
  / `discard_shadow` / `restore_shadow`.
- `status` on the control socket (cell running, active jobs, preload), `info.current_catalog`,
  `info.lakehouse_schemas`.

What it does not do (yet): change `default_lakehouse` or `write_mode` after `init`, register a
lakehouse after `init`, or serve `Files/` straight from OneLake without a mirror.

## 3. The proposal in one paragraph

Make the session an **account-scoped workbench that notebooks attach to**, not something a
notebook starts and owns. A session's identity is (Entra account, runtime profile, write mode,
libraries, memory). Workspace, default lakehouse, preload and Files are **per-notebook context
applied on attach**, never reasons to restart. Opening or closing a notebook never starts or
stops a session; running a cell attaches; a policy (keep alive / idle timeout / stop with the
last notebook) decides when it ends. Files are never pulled wholesale: Spark reads them from
OneLake, and a local mirror exists only for the folders the user explicitly pulls, with the
footprint visible. Warm start and eager mounting become **remembered per-lakehouse policies plus
a "start the session early" setting**, not a checkbox you re-tick per notebook.

## 4. Lifecycle

### 4.1 Session identity and what triggers a restart

| Property | Scope | Changing it means |
|---|---|---|
| Entra account (token endpoint) | session | restart (different user, different tokens) |
| Runtime profile (fabric-1.3 / 2.0), JDK, driver memory | session | restart |
| Libraries (Python, jars, Maven) | session | restart ("Apply libraries" already warns) |
| Write mode (sandbox / readonly / writethrough) | session | restart — the clones are different objects; keep it a session property and say so in the chip |
| Workspace(s) and lakehouses registered | session, **growable** | today restart; with an upstream `register_lakehouse` no restart (ask 1) |
| Default lakehouse | **notebook** | nothing — Cobalt sets the current database per cell run (`USE spark_catalog.<lh>` or `<lh>__<schema>`), cheap and exact; a `run_code` context parameter would be cleaner (ask 2) |
| Preload policy, Files pulls | **lakehouse** (remembered) | nothing — applied when the lakehouse is first attached |

Until ask 1 lands, a notebook from a workspace the session does not know gets the same prompt as
today, worded as a choice: *"This session knows workspace A. Attach workspace B too (restart,
~40 s, keeps clones) · Use the session as is · Cancel."* With ask 1 it just attaches.

### 4.2 Attach, detach, stop

- **Open** a notebook: nothing happens. The kernel chip shows the session's state and whether
  this notebook's context (workspace, default lakehouse, write mode) matches it.
- **Run** a cell: attach — register missing lakehouses (or prompt, see above), set the default
  lakehouse for this notebook's cells, apply the lakehouse's remembered preload policy once.
- **Close** a notebook: detach. The session keeps running. The status bar shows "Spark · 2
  notebooks attached" so the user knows what is holding it.
- **End**: by policy (Settings → Notebooks & Spark → Session lifecycle):
  `Keep running until I stop it` (default) · `Stop after N minutes idle` (idle = no cell and no
  preload for N minutes; default 30) · `Stop when the last Spark notebook closes`. Hot exit
  keeps the policy; app exit always stops the worker (as today).
- **Stop / Restart / Interrupt**: unchanged (0.7.4), plus "Restart with this notebook's
  context" when the chip shows a mismatch.

### 4.3 Isolation between notebooks: contexts, not JVMs

Sharing one IPython namespace between notebooks is crosstalk by construction: notebook A's
`df`, `spark.conf.set`, `USE`, temp views, UDF registrations and cached frames are notebook B's
too. It is the Python side that bites (variables and imports); on the Spark side the leaks are
temp views, the current database and session confs. Fabric's high-concurrency mode solves
exactly this: **one Spark application, one isolated REPL context per notebook**.

The same shape fits the worker: one JVM and one `SparkContext`, and per attached notebook a
**context** = its own Python namespace (its own IPython shell or `exec` globals) plus its own
`spark.newSession()` — which gives isolated temp views, SQL conf, current database and UDF
registry while sharing the catalog, the clones, cached data and the Ivy-resolved jars. A
context is created on attach (`create_context`), every `run_code`/`run_sql` names it, and it is
dropped on detach. The default lakehouse becomes a property of the context (current database and
the meaning of `Files/`), which retires the per-cell `USE` from 4.1. Cells from different
contexts still run one at a time at first; concurrent contexts (FAIR scheduler, one thread each)
can come later. This is upstream ask 0 and the keystone of the slate.

Until contexts exist, Cobalt has two honest modes:

- **Shared session (default today)** with the crosstalk stated in the chip — "variables and
  temp views are shared with the other N attached notebooks" — and `USE` per cell for the
  default lakehouse.
- **Isolated: one session per notebook** (opt-in in Settings → Session lifecycle): one worker
  process per Spark notebook, hard isolation, at the cost of one JVM each (driver memory ×
  notebooks, 20–60 s start each, separate shadow roots so clones do not collide). `KernelUi`
  becomes `sessions: Vec<Session>` keyed by notebook, which is the structure the chip's
  "Sessions…" list and a second account's session need anyway. With a driver at 2 GB this is a
  realistic choice on a 32 GB laptop for two or three notebooks; the setting says what it
  costs.

Once contexts ship, *isolated per notebook* becomes the default behaviour of the single shared
JVM, and "one session per notebook" stays as the heavy option for people who want separate
drivers (different write modes, different accounts).

## 5. Default lakehouse and Files

### 5.1 Default lakehouse

Per notebook, stored in the notebook's Fabric metadata as today (`dependencies.lakehouse`), shown
in the lakehouse chip; an unqualified `table` and `Files/` mean this lakehouse. Applied per cell
run (4.1), so two notebooks with different defaults share one session without either restarting
it. The chip's popup becomes a **lakehouse pane** (5.3) rather than a menu.

### 5.2 Files without syncing the world

The founder's objection stands: a lakehouse's `Files/` can be hundreds of GB; mirroring it is
never the default. Three tiers, cheapest first:

1. **Remote by default for Spark.** `spark.read.*("Files/…")`, `df.write.*("Files/…")` and
   `abfss://` go straight to OneLake through hadoop-azure with the host token — streaming, no
   local copy. This needs the worker to resolve relative `Files/` to the `abfss://` of the
   default lakehouse instead of the mirror (**ask 3: `files_mode`**); until then Cobalt
   rewrites `Files/` to the `abfss://` form in `%%sql` and documents the explicit path for
   Python. `notebookutils.fs.ls/exists` already work remotely.

   **And lazy for Python.** The Fabric path `/lakehouse/default/Files/<x>` must still work for
   `open()`, `pandas.read_csv`, `pathlib`, `os.listdir` — that was the hard part last time.
   The answer is not a sync and not a filesystem driver (FUSE/WinFsp would be a heavy install):
   the worker already shims `notebookutils`; the same technique can shim Python's file access
   under `/lakehouse/<name>/Files` — `builtins.open`, `os.listdir/scandir/stat/path.exists`,
   `pathlib` — so a *single file* is fetched into the mirror on first access and a directory
   listing comes from OneLake. Python-level IO (which is most of pandas, json, csv, PIL, zip)
   then works without pulling anything else. Native readers that `fopen` directly (DuckDB,
   Arrow's `OSFile`, some ML loaders) do not go through Python's `open`; for those the pane's
   explicit *Pull to local* (tier 2) is the path, and the error text should say so. Writes
   under the path land in the mirror and push only in writethrough, as today. This is ask 3's
   `"lazy"` mode, the default under a host.
2. **Explicit pulls.** From the lakehouse pane's Files tree (listed live from OneLake with the
   DFS API Cobalt already has, sizes shown), the user right-clicks a folder → *Pull to local*
   (`sync_files(paths=[folder])`). Only then does `/lakehouse/default/Files/<folder>` exist for
   `open()`, subprocesses and native libraries. Pulled folders are remembered per lakehouse and
   shown with their local size; *Refresh* re-syncs (size+mtime skip), *Remove local copy* drops
   it. *Push* appears only in writethrough and only for pulled folders.
3. **Footprint and cleanup.** Settings → Spark runtime shows the mirror's total size per
   lakehouse with "Clear"; the runtime's "Remove everything" includes it. A pull larger than a
   threshold (1 GB default) asks first and shows progress in the session log.

Shadows (table clones) stay as they are: sandbox clones are *shallow* (Delta log only), so the
"sync" concern does not apply to Tables.

### 5.3 The lakehouse pane

The Fabric notebook's left pane, inside the lakehouse chip's popup (and later dockable):

- **Lakehouses** of the attached workspaces, default one bold; pick another as default; "Attach
  workspace…".
- **Tables**, grouped by schema for schema-enabled lakehouses, with state: *not touched* ·
  *cloned (read)* · *written* · *external (writethrough)*; per-table *Preload* / *Discard* /
  *Rewind*; drag a name into a cell (the explorer's drag-to-editor already exists).
- **Files** tree from OneLake with sizes; *Pull to local* / *Refresh* / *Remove local copy*;
  pulled folders marked. Double-click a CSV/Parquet → a `spark.read` cell with the right path.
- **Policies** for this lakehouse (remembered in the store, keyed by lakehouse id): preload
  (none / all tables / these tables), keep clones between sessions, pulled Files folders.

## 6. Warm start and eager mounting, as user choices

Four layers; each is a setting or a remembered policy, none is a per-notebook checkbox:

1. **Ivy warm at install** — done (0.7.0).
2. **Start the session early** (Settings → Session lifecycle): *when a Spark notebook opens*
   (default on) · *when Cobalt starts* · *only when a cell runs*. A background start with the
   last-used context, so the first cell is instant; the status bar shows it starting; memory
   is the trade-off and the setting says so.
3. **Keep clones between sessions** (per lakehouse, `persist_shadow`): the shallow clones made
   last time are reused, so a restart is 20 s instead of 20 s + one clone per table. Clones are
   frozen at their first-touch version, so the pane shows *cloned at <time>* with *Refresh
   clone* (discard + re-mount) and a global *Refresh all clones* on the chip.
4. **Preload** (per lakehouse): *none* · *the tables I used last time* (default when clones
   are kept — Cobalt knows them from `shadow_status`) · *all tables* · *these tables*. Runs in
   the background on attach with the Shadows window's progress bar; a cell that touches a table
   mid-preload simply waits for that table.

"Warm start" as the user sees it: open the notebook, the chip says *Spark · ready · 8 tables
mounted* before they have finished reading their first cell.

## 7. Session presets (later)

Fabric's Environment, locally: a named bundle of profile, memory, write mode, libraries, and
default policies, selectable in the kernel chip ("Session: Default ▾"). The current
Settings pages become the editor of the *Default* preset. Useful once people keep a sandbox
preset and a writethrough preset side by side; not needed for the lifecycle work.

## 8. What Cobalt builds vs what we ask upstream

Cobalt-side (no upstream dependency): attach/detach model, lifecycle policy, per-cell default
lakehouse via `USE`, remembered per-lakehouse policies (store), lakehouse pane with Tables and a
live Files tree (OneLake DFS listing exists), explicit pulls through `sync_files`, mirror
footprint and cleanup, early start, keep-clones and preload policies, "Attach workspace" via
restart-with-union.

Asks for local-spark-mcp (written up in `docs/requests/local-spark-mcp-sessions.md`):

0. **Contexts** (§4.3) — *shipped in 0.5.0*: `create_context(id, default_lakehouse)` / `drop_context(id)`;
   `run_code` / `run_sql` / `interrupt` / `status` take `context`; each context is an isolated
   namespace plus `spark.newSession()`; `info.contexts`. Sequential execution across contexts
   is fine for a first version.
1. *Shipped in 0.4.3.* `register_lakehouse` / `unregister_lakehouse` after `init`, so a session
   grows without a restart; `info.lakehouses` reflects it.
2. *Shipped in 0.4.3.* `job_description` on `run_code` / `run_sql` (the cell's first line, for
   `status.cell.jobs` and the Stop tooltip).
3. *Proposed upstream as 0.6.0 (after contexts).* `files_mode: "lazy" | "mirror"` (default lazy under a host): Spark's `Files/` resolves to
   the default lakehouse's `abfss://…/Files/` (no mirror involved); `/lakehouse/<name>/Files`
   is served by Python-level hooks that fetch single files on first access and list directories
   from OneLake; explicit `sync_files` pulls whole subtrees for native readers. Plus
   `mirror_status` (per lakehouse: pulled subtrees, bytes) and `clear_mirror`.
4. `set_default_lakehouse` as a cheaper alternative to 2 if per-call context is unwelcome.
5. *Shipped in 0.4.3.* `shadow_status` entries carry `cloned_at` and `registered`; "the tables I
   used last time" = entries with `registered: false`. `status.idle_s` / `last_activity` too.

## 9. Phasing

- **Slate 4 — Sticky sessions (0.8.0)** — *built 2026-10-07 (contexts instead of the shared-session notice and the per-notebook-worker mode, which contexts made unnecessary).* Attach/detach, lifecycle policy with idle timeout,
  per-cell default lakehouse, early start setting (default on, remembered), keep-clones +
  preload policies per lakehouse (remembered), "Attach workspace" through restart-with-union,
  status bar "N notebooks attached", the crosstalk notice, and the opt-in *one session per
  notebook* mode on `sessions: Vec<Session>`. All Cobalt-side; ships against worker 0.4.2.
- **Slate 5 — Lakehouse pane and Files (0.8.x).** Tables with state and per-table actions,
  live Files tree, explicit pulls with footprint and cleanup, remote `Files/` once ask 3 ships
  (rewrite in `%%sql` meanwhile).
- **Slate 6 — Contexts and growable sessions (0.9).** Per-notebook contexts in one JVM
  (ask 0) become the default isolation; attach without restart (ask 1); lazy Files (ask 3);
  session presets; the "Sessions…" list for second accounts or write modes.

## 10. Decisions needed

1. One session as the product default, structured for several later — agree?
2. Lifecycle default: *keep running until I stop it* vs *stop after 30 min idle*.
3. Early start default: *when a Spark notebook opens* (costs memory while you read) or *only
   when a cell runs*.
4. Files default: *remote, pull on request* (recommended) — any case for a default mirror of
   small folders (say under 100 MB) automatically?
5. Keep clones between sessions: default off (fresh view of OneLake each session) or on (fast
   restarts, stale until refreshed)?
6. Cross-workspace: one session spanning workspaces (needs ask 1) vs "one session per
   workspace" as a simpler rule. The worker's model allows the former; the question is whether
   users expect it.

## 11. Decisions (founder, 2026-10-06)

1. **Isolation.** Each user-opened notebook should run isolated "if possible", to avoid
   crosstalk. Resolution: isolation is the goal, *contexts in one JVM* (§4.3, ask 0) is the way
   to get it without a JVM per notebook; until the worker has them, the shared session states
   its crosstalk plainly and an opt-in *one session per notebook* mode gives hard isolation to
   those who want it now.
2. **Lifecycle default** (delegated): *stop after 60 minutes idle*, remembered; "keep running
   until I stop it" and "stop with the last notebook" remain choices. A JVM that nobody has
   used for an hour on a laptop is not what the user wants to find.
3. **Early start**: default on (*when a Spark notebook opens*), a remembered preference.
4. **Files**: remote by default for Spark; the Fabric path for Python is served lazily (ask 3),
   never by syncing the tree; explicit pulls for native readers.
5. **Keep clones between sessions**: default off.
6. **Cross-workspace** (delegated): one account-scoped session may span workspaces — with
   contexts, a workspace is just more lakehouses in the catalog; the chip shows which
   workspaces are attached and "Attach workspace…" adds one (restart-with-union until ask 1).
