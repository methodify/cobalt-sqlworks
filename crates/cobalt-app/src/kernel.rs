//! The local Spark kernel: one local-spark-mcp worker process, owned by a thread that speaks the
//! socket protocol, driven by commands from the UI and reporting events the app polls each frame.
//! Notebook cells on the "Local Spark" kernel run here (Python through `run_code`; SQL through a
//! `spark.sql` helper). DataFrames come back as Arrow: with local-spark-mcp 0.4.1+ natively
//! (`display(df)` and, through `capture_result`, a bare DataFrame as the cell's last expression
//! become `displays` entries whose IPC blobs follow the reply); on an older environment through
//! the legacy `BOOTSTRAP` hook that writes IPC files and prints a marker.

use crate::state::*;
use cobalt_core::{Settings, TabId};
use cobalt_runtime::install;
use cobalt_runtime::worker::{ControlHandle, Worker, WorkerConfig};
use cobalt_runtime::{Installed, RuntimeError};
use cobalt_store::AppPaths;
use crossbeam_channel::{Receiver, Sender};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Marks an Arrow IPC file the Python side wrote for a DataFrame (`\x1e` = record separator).
pub const ARROW_MARK: &str = "\u{1e}COBALT-ARROW-FILE:";
pub const MARK_END: char = '\u{1e}';

/// Installed into the worker's namespace once per session. `display(df)` and bare DataFrame
/// expressions write Arrow streams under `OUT_DIR`; `__cobalt_sql` runs `%%sql` cells.
const BOOTSTRAP: &str = r#"
import os as __os, uuid as __uuid, sys as __sys
__COBALT_OUT = r"{out_dir}"
__COBALT_LIMIT = {limit}
__COBALT_DISPLAY_LIMIT = 1000
__os.makedirs(__COBALT_OUT, exist_ok=True)

def __cobalt_to_table(obj, limit):
    import pyarrow as pa
    from pyspark.sql import DataFrame
    try:
        import pandas as pd
    except Exception:
        pd = None
    if isinstance(obj, DataFrame):
        if limit:
            obj = obj.limit(int(limit))
        try:
            return obj.toArrow()
        except AttributeError:
            batches = obj._collect_as_arrow()
            if batches:
                return pa.Table.from_batches(batches)
            from pyspark.sql.pandas.types import to_arrow_schema
            return pa.Table.from_batches([], schema=to_arrow_schema(obj.schema))
    if pd is not None and isinstance(obj, pd.DataFrame):
        t = pa.Table.from_pandas(obj, preserve_index=False)
        return t.slice(0, int(limit)) if limit else t
    if isinstance(obj, pa.Table):
        return obj.slice(0, int(limit)) if limit else obj
    return None

def __cobalt_emit(obj, limit=None):
    import pyarrow as pa
    tbl = __cobalt_to_table(obj, limit)
    if tbl is None:
        return None
    path = __os.path.join(__COBALT_OUT, __uuid.uuid4().hex + ".arrow")
    with pa.OSFile(path, "wb") as f:
        with pa.ipc.new_stream(f, tbl.schema) as w:
            w.write_table(tbl)
    return path

def display(obj, *args, **kwargs):
    limit = kwargs.get("limit", __COBALT_DISPLAY_LIMIT)
    try:
        p = __cobalt_emit(obj, limit)
    except Exception as exc:
        print(f"display(): could not convert to Arrow: {type(exc).__name__}: {exc}", file=__sys.stderr)
        p = None
    if p:
        print("\x1eCOBALT-ARROW-FILE:" + p + "\x1e")
    else:
        print(obj)

def __cobalt_split_sql(text):
    out, buf, q = [], [], None
    i = 0
    while i < len(text):
        c = text[i]
        if q:
            buf.append(c)
            if c == q:
                q = None
        elif c in ("'", '"', "`"):
            q = c
            buf.append(c)
        elif c == "-" and text[i:i+2] == "--":
            j = text.find("\n", i)
            j = len(text) if j < 0 else j
            buf.append(text[i:j])
            i = j
            continue
        elif c == ";":
            s = "".join(buf).strip()
            if s:
                out.append(s)
            buf = []
        else:
            buf.append(c)
        i += 1
    s = "".join(buf).strip()
    if s:
        out.append(s)
    return out

def __cobalt_sql(text, limit=None):
    stmts = __cobalt_split_sql(text)
    last = None
    for s in stmts:
        last = spark.sql(s)
    if last is not None:
        p = __cobalt_emit(last, limit or __COBALT_LIMIT)
        if p:
            print("\x1eCOBALT-ARROW-FILE:" + p + "\x1e")

def __cobalt_df_plain(df, p, cycle):
    try:
        path = __cobalt_emit(df, __COBALT_LIMIT)
    except Exception as exc:
        p.text(f"DataFrame (not shown: {type(exc).__name__}: {exc})")
        return
    p.text("\x1eCOBALT-ARROW-FILE:" + path + "\x1e")

# IPython's pretty printer walks the MRO but stops at the first class defining __repr__, so the
# concrete classes (Spark 4 splits classic/connect) are registered, not only the base.
__cobalt_hooked = 0
for __modname in ("pyspark.sql.dataframe", "pyspark.sql.classic.dataframe", "pyspark.sql.connect.dataframe", "pandas"):
    try:
        __mod = __import__(__modname, fromlist=["DataFrame"])
        get_ipython().display_formatter.formatters["text/plain"].for_type(__mod.DataFrame, __cobalt_df_plain)
        __cobalt_hooked += 1
    except Exception:
        pass
if not __cobalt_hooked:
    print("cobalt: display hook not installed", file=__sys.stderr)
"#;

/// A session bound to a Fabric workspace: lakehouses become Spark databases, OneLake is read
/// through the token endpoint, and writes follow `write_mode` (sandbox = shallow clones).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KernelFabric {
    pub workspace_id: String,
    pub workspace_name: String,
    /// `(name, id)` of every lakehouse in the workspace.
    pub lakehouses: Vec<(String, String)>,
    pub default_lakehouse: Option<String>,
    /// `sandbox`, `readonly` or `writethrough`.
    pub write_mode: String,
    pub slot: cobalt_core::ProfileId,
    pub tenant: Option<String>,
    /// The worker's `preload` argument built from the lakehouse policies: `Null` for none, else
    /// `{lakehouse: null | [tables]}` (null = every table).
    pub preload: Value,
    /// Lakehouses whose policy is "the tables I used last time": after start, `shadow_status`
    /// lists the persisted clones and those not yet registered are preloaded.
    pub preload_last: Vec<String>,
    /// Keep clones on disk between sessions (any attached lakehouse's policy asks for it).
    pub persist_shadow: bool,
    /// Workspaces attached after start through `register_lakehouse` (`(id, name)`).
    pub extra_workspaces: Vec<(String, String)>,
}

impl KernelFabric {
    pub fn label(&self) -> String {
        let ws = if self.workspace_name.is_empty() { String::new() } else if self.extra_workspaces.is_empty() { format!(" ({})", self.workspace_name) } else { format!(" ({} +{})", self.workspace_name, self.extra_workspaces.len()) };
        format!("{}{} · {}", self.default_lakehouse.clone().unwrap_or_else(|| "no default lakehouse".into()), ws, self.write_mode)
    }
    /// The session knows this workspace (started with it or attached later).
    pub fn knows_workspace(&self, id: &str) -> bool {
        self.workspace_id == id || self.extra_workspaces.iter().any(|(w, _)| w == id)
    }
}

#[derive(Clone, Debug)]
pub enum KernelState {
    Stopped,
    Starting { since: Instant },
    Ready { info: Value, since: Instant },
    Failed(String),
}

impl KernelState {
    pub fn is_ready(&self) -> bool {
        matches!(self, KernelState::Ready { .. })
    }
    pub fn is_starting(&self) -> bool {
        matches!(self, KernelState::Starting { .. })
    }
    pub fn label(&self) -> String {
        match self {
            KernelState::Stopped => "Spark: stopped".into(),
            KernelState::Starting { since } => format!("Spark: starting… {}s", since.elapsed().as_secs()),
            KernelState::Ready { info, since } => {
                let v = info.get("spark_version").and_then(Value::as_str).unwrap_or("");
                let s = since.elapsed().as_secs();
                let up = if s >= 3600 { format!("{}h {:02}m", s / 3600, (s % 3600) / 60) } else if s >= 60 { format!("{}m", s / 60) } else { format!("{s}s") };
                format!("Spark {v} · up {up}")
            }
            KernelState::Failed(_) => "Spark: failed".into(),
        }
    }
}

/// A cell submitted to the kernel.
#[derive(Clone, Debug)]
pub struct RunReq {
    pub tab: TabId,
    pub cell_id: String,
    pub code: String,
    /// The notebook's context (local-spark-mcp 0.5.0): its own namespace and SparkSession in the
    /// shared JVM, created on first use. `None` = the worker's default context.
    pub context: Option<String>,
    /// The context's default lakehouse (current database), set when it is created.
    pub context_lakehouse: Option<String>,
    /// The notebook's title: the context's display name and Spark job group description (0.5.1).
    pub context_name: Option<String>,
    /// The cell's first line, for `status.cell.jobs` and the Spark UI (0.4.3).
    pub job_description: Option<String>,
    /// Lakehouses to attach before the cell runs (`register_lakehouse`, 0.4.3) — a notebook from
    /// a workspace the session does not know yet.
    pub register: Vec<Value>,
}

/// The context id a notebook tab uses in the worker.
pub fn context_id(tab: TabId) -> String {
    let s = tab.to_string();
    format!("nb-{}", &s[..s.len().min(8)])
}

enum Cmd {
    /// `capture`: ask `run_code` for `capture_result` (0.4.1+).
    Start { cfg: WorkerConfig, bootstrap: String, capture: bool },
    Run(RunReq),
    /// Any other worker method (shadow_status, discard_shadow, restore_shadow, info…).
    Call { tag: String, method: String, params: Value },
    /// A notebook closed: release its context (`forced`: already requested on the control socket
    /// with `force: true`, only the bookkeeping remains).
    DropContext { tab: TabId, id: String, forced: bool },
    Shutdown,
}

pub enum KernelEvent {
    Ready { info: Value, control: Option<ControlHandle> },
    Log(String),
    /// Streamed output of the running cell (protocol 2): `stream` is `stdout` or `stderr`.
    CellOutput { tab: TabId, cell_id: String, stream: String, text: String },
    /// The control socket answered an `interrupt`.
    Interrupted(Result<Value, String>),
    /// A notebook's context was created in / dropped from the worker.
    ContextCreated(TabId),
    ContextDropped(TabId),
    CallResult { tag: String, result: Result<Value, String> },
    /// A cell finished: the worker's `ExecResult` (ok, stdout, stderr, error, traceback,
    /// displays…) with the Arrow blobs that followed it, or a transport-level error.
    Done { req: RunReq, result: Result<Value, String>, blobs: Vec<Vec<u8>> },
    Stopped,
    Failed(String),
}

pub struct KernelUi {
    pub state: KernelState,
    /// Cell currently executing on the worker.
    pub busy: Option<(TabId, String)>,
    pub log: VecDeque<String>,
    pub log_open: bool,
    pub profile: String,
    /// Cells to submit once the kernel is ready (filled while it starts).
    pub waiting: Vec<RunReq>,
    tx: Option<Sender<Cmd>>,
    rx: Option<Receiver<KernelEvent>>,
    cancel: Arc<AtomicBool>,
    /// Arrow files written by the worker, cleared when a cell's outputs are consumed.
    pub out_dir: Option<PathBuf>,
    /// The Fabric binding this session was started with (None = plain local session).
    pub fabric: Option<KernelFabric>,
    /// A Fabric-bound start waiting for its OneLake token (interactive sign-in may be running).
    pub pending_fabric_start: Option<KernelFabric>,
    token_server: Option<crate::onelake_tokens::TokenServer>,
    /// Notebooks already warned that their binding differs from the running session's.
    pub binding_warned: std::collections::HashSet<TabId>,
    /// The worker's control socket (protocol 2): interrupts go here while a cell runs.
    pub control: Option<ControlHandle>,
    /// An `interrupt` was sent for the running cell at this time; a second Stop kills the session.
    pub interrupting: Option<Instant>,
    /// When the last `interrupt` went out, and how the worker answered (seconds to the
    /// acknowledgement, its text) — for the session log and diagnostics.
    pub interrupt_sent_at: Option<Instant>,
    pub last_interrupt: Option<(f32, String)>,
    ev_tx: Option<Sender<KernelEvent>>,
    /// DataFrames arrive as native `displays` blobs (local-spark-mcp 0.4.1+) rather than through
    /// the legacy file-marker bootstrap.
    pub native_arrow: bool,
    /// The worker's additive capabilities (`init.features`, 0.5.0): `contexts`,
    /// `register_lakehouse`, `job_description`, … Empty on older workers.
    pub features: std::collections::HashSet<String>,
    /// Notebooks that have a context in the worker.
    pub contexts: std::collections::HashSet<TabId>,
    /// Last time a cell finished, a cell was submitted, a preload ran, or the session came up —
    /// the idle clock of the lifecycle policy.
    pub last_activity: Instant,
    /// The reply to the agent's last `kernel {action: call}` (diagnostics).
    pub last_call: Option<Value>,
}

impl KernelUi {
    pub fn has(&self, feature: &str) -> bool {
        self.features.contains(feature)
    }
    pub fn idle(&self) -> Duration {
        if self.busy.is_some() || !self.waiting.is_empty() {
            Duration::ZERO
        } else {
            self.last_activity.elapsed()
        }
    }
}

impl Default for KernelUi {
    fn default() -> Self {
        Self { state: KernelState::Stopped, busy: None, log: VecDeque::new(), log_open: false, profile: String::new(), waiting: Vec::new(), tx: None, rx: None, cancel: Arc::new(AtomicBool::new(false)), out_dir: None, fabric: None, pending_fabric_start: None, token_server: None, binding_warned: Default::default(), control: None, interrupting: None, interrupt_sent_at: None, last_interrupt: None, ev_tx: None, native_arrow: false, features: Default::default(), contexts: Default::default(), last_activity: Instant::now(), last_call: None }
    }
}

impl KernelUi {
    pub fn token_requests(&self) -> Option<u64> {
        self.token_server.as_ref().map(|t| t.served.load(Ordering::Relaxed))
    }
    pub fn token_error(&self) -> Option<String> {
        self.token_server.as_ref().and_then(|t| t.last_error.lock().clone())
    }
}

/// Why a start was refused.
pub enum StartError {
    NotProvisioned,
    /// The catalog/token-provider jar is missing from the environment.
    NoJar,
    TokenServer(String),
}

fn python_bootstrap(out_dir: &std::path::Path, limit: u64) -> String {
    BOOTSTRAP.replace("{out_dir}", &out_dir.to_string_lossy()).replace("{limit}", &limit.to_string())
}

/// The namespace helpers for local-spark-mcp 0.4.1+: `display()` is the worker's own and a bare
/// DataFrame is captured natively, so only the `%%sql` runner remains (statements split as in the
/// legacy bootstrap, the last statement's frame handed to `display` under the cell's limit).
const BOOTSTRAP_SQL: &str = r#"
def __cobalt_split_sql(text):
    out, buf, q = [], [], None
    i = 0
    while i < len(text):
        c = text[i]
        if q:
            buf.append(c)
            if c == q:
                q = None
        elif c in ("'", '"', "`"):
            q = c
            buf.append(c)
        elif c == "-" and text[i:i+2] == "--":
            j = text.find("\n", i)
            j = len(text) if j < 0 else j
            buf.append(text[i:j])
            i = j
            continue
        elif c == ";":
            s = "".join(buf).strip()
            if s:
                out.append(s)
            buf = []
        else:
            buf.append(c)
        i += 1
    s = "".join(buf).strip()
    if s:
        out.append(s)
    return out

def __cobalt_sql(text, limit=None):
    last = None
    for s in __cobalt_split_sql(text):
        last = spark.sql(s)
    if last is not None:
        display(last, limit=limit)
"#;

/// Everything a Fabric-bound start needs from the app besides the settings.
pub struct FabricStart {
    pub fabric: KernelFabric,
    pub resolver: Arc<cobalt_auth::CredentialResolver>,
    pub handle: tokio::runtime::Handle,
}

/// Start the worker on its thread (no-op when starting or ready).
pub fn start(k: &mut KernelUi, settings: &Settings, paths: &AppPaths, egui: &egui::Context, fabric: Option<FabricStart>) -> Result<(), StartError> {
    if k.state.is_starting() || k.state.is_ready() {
        return Ok(());
    }
    let dirs = crate::runtime::dirs(settings, paths);
    let profile = settings.spark.profile.clone();
    if !dirs.env_python(&profile).is_file() {
        return Err(StartError::NotProvisioned);
    }
    let rec = Installed::load(&dirs);
    let jdk = settings.spark.java_home.clone().filter(|s| !s.trim().is_empty()).map(PathBuf::from).or(rec.jdk_home);
    let out_dir = dirs.state_dir().join("outputs");
    let _ = std::fs::create_dir_all(&out_dir);
    let mut extra = serde_json::Map::new();
    k.token_server = None;
    k.fabric = None;
    if let Some(fs) = fabric {
        let scala = cobalt_runtime::Manifest::embedded().profile(&profile).map(|p| p.scala.clone()).unwrap_or_else(|_| "2.13".into());
        let jar = cobalt_runtime::detect::package_jar(&dirs.env_dir(&profile), &scala).ok_or(StartError::NoJar)?;
        let ts = crate::onelake_tokens::TokenServer::start(fs.resolver.clone(), fs.fabric.slot, fs.fabric.tenant.clone(), fs.handle.clone()).map_err(|e| StartError::TokenServer(e.to_string()))?;
        extra.insert("onelake".into(), json!({"endpoint": ts.url, "secret": ts.secret, "jar_path": jar.to_string_lossy()}));
        extra.insert("lakehouses".into(), Value::Array(fs.fabric.lakehouses.iter().map(|(name, id)| json!({"name": name, "id": id, "workspace_id": fs.fabric.workspace_id})).collect()));
        if let Some(d) = &fs.fabric.default_lakehouse {
            extra.insert("default_lakehouse".into(), Value::String(d.clone()));
        }
        extra.insert("write_mode".into(), Value::String(fs.fabric.write_mode.clone()));
        extra.insert("persist_shadow".into(), Value::Bool(fs.fabric.persist_shadow));
        // the worker's own preload (0.4.1 lists OneLake through the JVM and takes every token
        // from Cobalt's endpoint, so it needs no credential of its own)
        if !fs.fabric.preload.is_null() {
            extra.insert("preload".into(), fs.fabric.preload.clone());
        }
        extra.insert("mirror_root".into(), Value::String(dirs.state_dir().join("lakehouses").to_string_lossy().to_string()));
        k.token_server = Some(ts);
        k.fabric = Some(fs.fabric);
    }
    k.binding_warned.clear();
    // the worker checks the installed stack against the profile and refuses a mismatch
    extra.insert("profile".into(), Value::String(profile.clone()));
    // user libraries: jar files and Maven packages are the worker's (local-spark-mcp 0.3.5:
    // `extra_jars` join spark.jars, `extra_packages` are resolved by Ivy with their dependencies)
    let libs = cobalt_runtime::libraries::for_session(&crate::runtime::libraries(settings));
    for m in &libs.skipped {
        k.log.push_back(format!("cobalt: library skipped: {m}"));
    }
    if !libs.jars.is_empty() {
        extra.insert("extra_jars".into(), Value::Array(libs.jars.iter().map(|p| Value::String(p.to_string_lossy().to_string())).collect()));
        k.log.push_back(format!("cobalt: {} user jar{} for the session", libs.jars.len(), if libs.jars.len() == 1 { "" } else { "s" }));
    }
    if !libs.packages.is_empty() {
        extra.insert("extra_packages".into(), Value::Array(libs.packages.iter().map(|p| Value::String(p.clone())).collect()));
        k.log.push_back(format!("cobalt: {} Maven package{} for the session (Ivy resolves them at start)", libs.packages.len(), if libs.packages.len() == 1 { "" } else { "s" }));
    }
    let limit = settings.notebooks.spark_row_limit.max(1);
    // the row cap for display(df), a captured bare DataFrame and run_sql
    extra.insert("default_sql_limit".into(), json!(limit));
    let mut cfg = install::worker_config(&dirs, &profile, jdk.as_deref(), &settings.spark.driver_memory, extra);
    // the control socket exists from local-spark-mcp 0.4.0 (protocol 2); an older worker would
    // reject the argument. Native Arrow displays and capture_result from 0.4.1.
    let ver = rec.package_version.as_deref().map(version_tuple).unwrap_or((0, 0, 0));
    // lakehouse Files: lazy (Spark streams from OneLake, Python fetches on first open) from 0.6.x
    if ver >= (0, 6, 0) {
        cfg.init["files_mode"] = Value::String(if settings.spark.files_mode == "mirror" { "mirror".into() } else { "lazy".into() });
    }
    cfg.control = ver >= (0, 4, 0);
    let native = ver >= (0, 4, 1);
    if !cfg.control {
        k.log.push_back("cobalt: local-spark-mcp before 0.4.0 — Stop ends the session instead of interrupting the cell (update the runtime on the Spark runtime page)".into());
    }
    if !native {
        k.log.push_back("cobalt: local-spark-mcp before 0.4.1 — DataFrames come back through the legacy file hook (update the runtime on the Spark runtime page)".into());
    }
    k.native_arrow = native;
    let bootstrap = if native { BOOTSTRAP_SQL.to_string() } else { python_bootstrap(&out_dir, limit) };
    let (ctx_tx, ctx_rx) = crossbeam_channel::unbounded::<Cmd>();
    let (ev_tx, ev_rx) = crossbeam_channel::unbounded::<KernelEvent>();
    let cancel = Arc::new(AtomicBool::new(false));
    k.cancel = cancel.clone();
    k.tx = Some(ctx_tx.clone());
    k.rx = Some(ev_rx);
    k.ev_tx = Some(ev_tx.clone());
    k.control = None;
    k.interrupting = None;
    k.out_dir = Some(out_dir);
    k.profile = profile;
    k.state = KernelState::Starting { since: Instant::now() };
    k.log.clear();
    let egui2 = egui.clone();
    std::thread::Builder::new()
        .name("spark-kernel".into())
        .spawn(move || kernel_thread(ctx_rx, ev_tx, cancel, egui2))
        .ok();
    let _ = ctx_tx.send(Cmd::Start { cfg, bootstrap, capture: native });
    Ok(())
}

fn kernel_thread(rx: Receiver<Cmd>, tx: Sender<KernelEvent>, cancel: Arc<AtomicBool>, egui: egui::Context) {
    let mut worker: Option<Worker> = None;
    let mut capture = false;
    // contexts created in this worker (0.5.0), by id
    let mut created: std::collections::HashSet<String> = Default::default();
    let log_tx = tx.clone();
    let log_egui = egui.clone();
    let log: cobalt_runtime::worker::LogFn = Arc::new(move |s: String| {
        let _ = log_tx.send(KernelEvent::Log(s));
        log_egui.request_repaint_after(Duration::from_millis(100));
    });
    while let Ok(cmd) = rx.recv() {
        match cmd {
            Cmd::Start { cfg, bootstrap, capture: cap } => {
                capture = cap;
                created.clear();
                cancel.store(false, Ordering::Relaxed);
                match Worker::start(&cfg, log.clone(), &cancel) {
                    Ok(mut w) => {
                        match w.call_cancellable("run_code", json!({"code": bootstrap}), Duration::from_secs(120), &cancel) {
                            Ok(r) => {
                                if r.get("ok").and_then(Value::as_bool) == Some(false) {
                                    let _ = tx.send(KernelEvent::Log(format!("cobalt: bootstrap failed: {}", r.get("error").and_then(Value::as_str).unwrap_or(""))));
                                }
                            }
                            Err(e) => {
                                let _ = tx.send(KernelEvent::Failed(format!("kernel bootstrap failed: {e}")));
                                w.kill();
                                egui.request_repaint();
                                continue;
                            }
                        }
                        if let Some(pw) = w.info.get("profile_warnings").and_then(Value::as_array) {
                            for x in pw.iter().filter_map(Value::as_str) {
                                let _ = tx.send(KernelEvent::Log(format!("cobalt: profile warning: {x}")));
                            }
                        }
                        let _ = tx.send(KernelEvent::Ready { info: w.info.clone(), control: w.control() });
                        worker = Some(w);
                    }
                    Err(RuntimeError::Cancelled) => {
                        let _ = tx.send(KernelEvent::Stopped);
                    }
                    Err(e) => {
                        let _ = tx.send(KernelEvent::Failed(e.to_string()));
                    }
                }
                egui.request_repaint();
            }
            Cmd::Run(req) => {
                let Some(w) = worker.as_mut() else {
                    let _ = tx.send(KernelEvent::Done { req, result: Err("the Spark kernel is not running".into()), blobs: Vec::new() });
                    egui.request_repaint();
                    continue;
                };
                cancel.store(false, Ordering::Relaxed);
                // COBALT_SPARK_STREAM=0 turns streamed output off (diagnostics)
                let stream = w.protocol_version >= 2 && std::env::var("COBALT_SPARK_STREAM").map(|v| v != "0").unwrap_or(true);
                let (tab, cell_id) = (req.tab, req.cell_id.clone());
                let ev_tx = tx.clone();
                let ev_egui = egui.clone();
                let mut on_event = |ev: &str, text: &str| {
                    let _ = ev_tx.send(KernelEvent::CellOutput { tab, cell_id: cell_id.clone(), stream: ev.to_string(), text: text.to_string() });
                    ev_egui.request_repaint_after(Duration::from_millis(100));
                };
                // lakehouses from a workspace the session did not start with
                for lh in &req.register {
                    let name = lh.get("name").and_then(Value::as_str).unwrap_or("?").to_string();
                    match w.call("register_lakehouse", json!({"lakehouse": lh}), Duration::from_secs(300)) {
                        Ok(_) => {
                            let _ = tx.send(KernelEvent::Log(format!("cobalt: attached lakehouse {name}")));
                        }
                        Err(e) => {
                            let _ = tx.send(KernelEvent::Log(format!("cobalt: could not attach lakehouse {name}: {e}")));
                        }
                    }
                }
                // the notebook's own context, created on first use
                let mut context = req.context.clone();
                if let Some(id) = context.clone() {
                    if !created.contains(&id) {
                        let mut p = serde_json::Map::new();
                        p.insert("id".into(), Value::String(id.clone()));
                        if let Some(lh) = &req.context_lakehouse {
                            p.insert("default_lakehouse".into(), Value::String(lh.clone()));
                        }
                        if let Some(n) = &req.context_name {
                            p.insert("name".into(), Value::String(n.clone()));
                        }
                        match w.call("create_context", Value::Object(p), Duration::from_secs(120)) {
                            Ok(v) => {
                                created.insert(id.clone());
                                let _ = tx.send(KernelEvent::ContextCreated(req.tab));
                                let _ = tx.send(KernelEvent::Log(format!("cobalt: context {id} created (database {})", v.get("current_database").and_then(Value::as_str).unwrap_or("default"))));
                            }
                            Err(e) => {
                                let _ = tx.send(KernelEvent::Log(format!("cobalt: context {id} could not be created, running in the shared context: {e}")));
                                context = None;
                            }
                        }
                    }
                }
                let mut params = serde_json::Map::new();
                params.insert("code".into(), Value::String(req.code.clone()));
                params.insert("stream".into(), Value::Bool(stream));
                params.insert("capture_result".into(), Value::Bool(capture));
                if let Some(c) = &context {
                    params.insert("context".into(), Value::String(c.clone()));
                }
                if let Some(d) = &req.job_description {
                    params.insert("job_description".into(), Value::String(d.clone()));
                }
                let r = w.call_streaming("run_code", Value::Object(params), Duration::from_secs(60 * 60 * 24), &cancel, &mut on_event);
                match r {
                    Ok(v) => {
                        let blobs = std::mem::take(&mut w.last_blobs);
                        let _ = tx.send(KernelEvent::Done { req, result: Ok(v), blobs });
                    }
                    Err(RuntimeError::Cancelled) => {
                        // the protocol is out of step after an abandoned call: the session restarts
                        if let Some(w) = worker.take() {
                            w.kill();
                        }
                        let _ = tx.send(KernelEvent::Done { req, result: Err("interrupted — the local Spark session was stopped (the next cell starts a new session)".into()), blobs: Vec::new() });
                        let _ = tx.send(KernelEvent::Stopped);
                    }
                    Err(RuntimeError::WorkerFatal(e)) => {
                        if let Some(w) = worker.take() {
                            w.kill();
                        }
                        let _ = tx.send(KernelEvent::Done { req, result: Err(e.clone()), blobs: Vec::new() });
                        let _ = tx.send(KernelEvent::Failed(e));
                    }
                    Err(e) => {
                        let fatal = !w.is_alive();
                        let _ = tx.send(KernelEvent::Done { req, result: Err(e.to_string()), blobs: Vec::new() });
                        if fatal {
                            if let Some(w) = worker.take() {
                                w.kill();
                            }
                            let _ = tx.send(KernelEvent::Failed(e.to_string()));
                        }
                    }
                }
                egui.request_repaint();
            }
            Cmd::DropContext { tab, id, forced } => {
                if let Some(w) = worker.as_mut() {
                    if created.remove(&id) && !forced {
                        match w.call("drop_context", json!({"id": id}), Duration::from_secs(60)) {
                            Ok(_) => {
                                let _ = tx.send(KernelEvent::Log(format!("cobalt: context {id} dropped")));
                            }
                            Err(e) => {
                                let _ = tx.send(KernelEvent::Log(format!("cobalt: context {id} could not be dropped: {e}")));
                            }
                        }
                    }
                }
                let _ = tx.send(KernelEvent::ContextDropped(tab));
                egui.request_repaint();
            }
            Cmd::Call { tag, method, params } => {
                let result = match worker.as_mut() {
                    Some(w) => w.call(&method, params, Duration::from_secs(600)).map_err(|e| e.to_string()),
                    None => Err("the Spark kernel is not running".into()),
                };
                let _ = tx.send(KernelEvent::CallResult { tag, result });
                egui.request_repaint();
            }
            Cmd::Shutdown => {
                if let Some(w) = worker.take() {
                    w.shutdown();
                }
                let _ = tx.send(KernelEvent::Stopped);
                egui.request_repaint();
                break;
            }
        }
    }
    if let Some(w) = worker.take() {
        w.kill();
    }
}

/// Submit a cell. While the kernel starts, the request waits in `waiting`.
pub fn run(k: &mut KernelUi, req: RunReq) {
    k.last_activity = Instant::now();
    if k.state.is_ready() {
        if let Some(tx) = &k.tx {
            k.busy.get_or_insert((req.tab, req.cell_id.clone()));
            let _ = tx.send(Cmd::Run(req));
            return;
        }
    }
    k.waiting.push(req);
}

/// A notebook closed: release its context in the worker (no-op without one). When the
/// notebook's cell is running, the control socket interrupts it and drops the context as soon as
/// it ends (`force`, 0.5.1); otherwise the request goes behind the queue on the data socket.
pub fn drop_context(k: &mut KernelUi, tab: TabId) {
    if !k.contexts.contains(&tab) {
        return;
    }
    let id = context_id(tab);
    let running_here = k.busy.as_ref().map(|(t, _)| *t == tab).unwrap_or(false);
    let mut forced = false;
    if running_here {
        if let (Some(ctl), Some(ev)) = (k.control.clone(), k.ev_tx.clone()) {
            forced = true;
            let id2 = id.clone();
            std::thread::Builder::new()
                .name("spark-drop".into())
                .spawn(move || {
                    let r = ctl.call("drop_context", json!({"id": id2, "force": true}), Duration::from_secs(30));
                    let _ = ev.send(KernelEvent::Log(match r {
                        Ok(v) => format!("cobalt: context {id2} drop requested while its cell runs → {v}"),
                        Err(e) => format!("cobalt: context {id2} forced drop failed: {e}"),
                    }));
                })
                .ok();
        }
    }
    if let Some(tx) = &k.tx {
        let _ = tx.send(Cmd::DropContext { tab, id, forced });
    }
}

/// A request on the control socket (`status`, `preload_status`, `ping`), answered while a cell
/// runs; the reply arrives as `PollOut::calls` under `tag`. Falls back to the data socket when
/// the worker has no control socket.
pub fn control_call(k: &mut KernelUi, tag: &str, method: &str, params: Value) -> bool {
    if !k.state.is_ready() {
        return false;
    }
    let (Some(ctl), Some(ev)) = (k.control.clone(), k.ev_tx.clone()) else { return call(k, tag, method, params) };
    let tag = tag.to_string();
    let method = method.to_string();
    std::thread::Builder::new()
        .name("spark-control".into())
        .spawn(move || {
            let result = ctl.call(&method, params, Duration::from_secs(20)).map_err(|e| e.to_string());
            let _ = ev.send(KernelEvent::CallResult { tag, result });
        })
        .is_ok()
}

/// Any worker method; the answer arrives as `PollOut::calls` under `tag`.
pub fn call(k: &mut KernelUi, tag: &str, method: &str, params: Value) -> bool {
    match (&k.tx, k.state.is_ready()) {
        (Some(tx), true) => {
            let _ = tx.send(Cmd::Call { tag: tag.into(), method: method.into(), params });
            true
        }
        _ => false,
    }
}

/// What `interrupt` did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InterruptAction {
    /// `interrupt` went out on the control socket; the cell returns `interrupted` shortly.
    Sent,
    /// The worker is being killed (no control socket, a second Stop, or a start in progress).
    Killed,
    Nothing,
}

/// Stop the running cell. With a control socket (local-spark-mcp 0.4.0+) the first call sends
/// `interrupt` — Spark jobs are cancelled and the cell raises KeyboardInterrupt, the session
/// survives; a second call while that is pending kills the worker. Without one, or during a
/// start, the worker is killed and the next cell starts a new session.
pub fn interrupt(k: &mut KernelUi) -> InterruptAction {
    k.waiting.clear();
    if k.state.is_starting() {
        k.cancel.store(true, Ordering::Relaxed);
        return InterruptAction::Killed;
    }
    if k.busy.is_none() {
        return InterruptAction::Nothing;
    }
    match (&k.control, k.interrupting) {
        (Some(ctl), None) => {
            k.interrupting = Some(Instant::now());
            k.interrupt_sent_at = k.interrupting;
            k.last_interrupt = None;
            let ctl = ctl.clone();
            let ev = k.ev_tx.clone();
            std::thread::Builder::new()
                .name("spark-interrupt".into())
                .spawn(move || {
                    let r = ctl.interrupt().map_err(|e| e.to_string());
                    if let Some(ev) = ev {
                        let _ = ev.send(KernelEvent::Interrupted(r));
                    }
                })
                .ok();
            InterruptAction::Sent
        }
        _ => {
            k.interrupting = None;
            k.cancel.store(true, Ordering::Relaxed);
            InterruptAction::Killed
        }
    }
}

/// `major.minor.patch` of a version string (missing parts are 0, suffixes ignored).
pub fn version_tuple(v: &str) -> (u64, u64, u64) {
    let mut it = v.trim().split(|c: char| c == '.' || c == '-' || c == '+').map(|p| p.chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse::<u64>().unwrap_or(0));
    (it.next().unwrap_or(0), it.next().unwrap_or(0), it.next().unwrap_or(0))
}

pub fn stop(k: &mut KernelUi) {
    k.waiting.clear();
    if k.busy.is_some() || k.state.is_starting() {
        k.cancel.store(true, Ordering::Relaxed);
    }
    if let Some(tx) = &k.tx {
        let _ = tx.send(Cmd::Shutdown);
    }
    if !k.busy.is_some() && !k.state.is_starting() {
        k.state = KernelState::Stopped;
    }
}

pub struct PollOut {
    pub done: Vec<(RunReq, Result<Value, String>, Vec<Vec<u8>>)>,
    /// Streamed output for running cells: `(tab, cell id, stream, text)`.
    pub outputs: Vec<(TabId, String, String, String)>,
    pub ready_now: bool,
    /// The session ended (stopped or failed) — cells still marked running must be failed.
    pub broke: Option<String>,
    pub calls: Vec<(String, Result<Value, String>)>,
}

/// Drain events. Finished cells go back to the notebook layer; when the kernel just became
/// ready the waiting cells are submitted.
pub fn poll(k: &mut KernelUi) -> PollOut {
    let mut done = Vec::new();
    let mut ready_now = false;
    let mut broke = None;
    let mut calls = Vec::new();
    let mut outputs = Vec::new();
    let Some(rx) = &k.rx else { return PollOut { done, outputs, ready_now, broke, calls } };
    let mut events = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        events.push(ev);
    }
    for ev in events {
        match ev {
            KernelEvent::Ready { info, control } => {
                k.features = info.get("features").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default();
                k.contexts.clear();
                k.state = KernelState::Ready { info, since: Instant::now() };
                k.control = control;
                k.last_activity = Instant::now();
                ready_now = true;
            }
            KernelEvent::ContextCreated(tab) => {
                k.contexts.insert(tab);
            }
            KernelEvent::ContextDropped(tab) => {
                k.contexts.remove(&tab);
            }
            KernelEvent::CellOutput { tab, cell_id, stream, text } => outputs.push((tab, cell_id, stream, text)),
            KernelEvent::Interrupted(r) => {
                let secs = k.interrupt_sent_at.map(|t| t.elapsed().as_secs_f32()).unwrap_or(0.0);
                k.last_interrupt = Some((secs, match &r { Ok(v) => v.to_string(), Err(e) => e.clone() }));
                k.log.push_back(match &r {
                    Ok(v) => format!("cobalt: interrupt → {} after {secs:.1} s{}", v.get("state").and_then(Value::as_str).unwrap_or("?"), v.get("detail").and_then(Value::as_str).map(|d| format!(" ({d})")).unwrap_or_default()),
                    Err(e) if e.contains("timed out") => format!("cobalt: interrupt sent; the worker's acknowledgement did not arrive in time (the cell is still being cancelled — Stop again to end the session): {e}"),
                    Err(e) => format!("cobalt: interrupt failed: {e}"),
                });
            }
            KernelEvent::Log(s) => {
                k.log.push_back(s);
                if k.log.len() > 3000 {
                    k.log.drain(..1000);
                }
            }
            KernelEvent::CallResult { tag, result } => calls.push((tag, result)),
            KernelEvent::Done { req, result, blobs } => {
                if k.busy.as_ref().map(|(t, c)| *t == req.tab && *c == req.cell_id).unwrap_or(false) {
                    k.busy = None;
                }
                k.interrupting = None;
                k.last_activity = Instant::now();
                done.push((req, result, blobs));
            }
            KernelEvent::Stopped => {
                k.state = KernelState::Stopped;
                k.busy = None;
                k.tx = None;
                k.waiting.clear();
                k.token_server = None;
                k.fabric = None;
                k.control = None;
                k.interrupting = None;
                k.contexts.clear();
                k.features.clear();
                broke = Some("the local Spark session stopped".to_string());
            }
            KernelEvent::Failed(e) => {
                k.state = KernelState::Failed(e.clone());
                k.busy = None;
                k.tx = None;
                k.waiting.clear();
                k.token_server = None;
                k.fabric = None;
                k.control = None;
                k.interrupting = None;
                k.contexts.clear();
                k.features.clear();
                broke = Some(e);
            }
        }
    }
    if ready_now {
        let waiting = std::mem::take(&mut k.waiting);
        for req in waiting {
            run(k, req);
        }
    }
    PollOut { done, outputs, ready_now, broke, calls }
}

/// Split a worker `ExecResult` into messages and Arrow result sets for a cell.
pub struct CellOutcome {
    pub messages: Vec<MessageLine>,
    pub result_sets: Vec<Arc<cobalt_results::ResultSet>>,
    pub failed: bool,
    /// The cell was stopped by `interrupt` (protocol 2): not a failure, outputs so far kept.
    pub interrupted: bool,
}

pub fn outcome(result: &Result<Value, String>, blobs: &[Vec<u8>]) -> CellOutcome {
    let mut out = CellOutcome { messages: Vec::new(), result_sets: Vec::new(), failed: false, interrupted: false };
    let msg = |text: String, is_error: bool| MessageLine { text, is_error, is_batch_header: false, line: None, path: None };
    match result {
        Err(e) => {
            out.messages.push(msg(e.clone(), true));
            out.failed = true;
        }
        Ok(v) => {
            let stdout = v.get("stdout").and_then(Value::as_str).unwrap_or("");
            let interrupted = v.get("interrupted").and_then(Value::as_bool) == Some(true);
            let failed = !interrupted && (v.get("ok").and_then(Value::as_bool) == Some(false) || v.get("error").and_then(Value::as_str).map(|e| !e.is_empty()).unwrap_or(false));
            // native displays (0.4.1+): one Arrow IPC blob per entry, in order; a captured bare
            // expression (`source: "result"`) also left IPython's `Out[n]:` repr on stdout
            let displays = v.get("displays").and_then(Value::as_array).cloned().unwrap_or_default();
            let mut captured_result = false;
            for (i, d) in displays.iter().enumerate() {
                if d.get("kind").and_then(Value::as_str) != Some("arrow") {
                    continue;
                }
                if d.get("source").and_then(Value::as_str) == Some("result") {
                    captured_result = true;
                }
                match blobs.get(i) {
                    Some(bytes) => match crate::notebook::result_set_from_ipc(bytes, out.result_sets.len()) {
                        Ok(rs) => out.result_sets.push(rs),
                        Err(e) => out.messages.push(msg(format!("Could not read the DataFrame: {e}"), true)),
                    },
                    None => out.messages.push(msg("The worker announced a DataFrame but sent no data for it.".into(), true)),
                }
            }
            let stdout_lines: Vec<&str> = stdout.lines().collect();
            let cut = if captured_result { stdout_lines.iter().rposition(|l| l.starts_with("Out[")).unwrap_or(stdout_lines.len()) } else { stdout_lines.len() };
            let mut in_traceback = false;
            for line in stdout_lines.into_iter().take(cut) {
                // an interrupted cell's KeyboardInterrupt traceback is noise: keep what it printed before
                if interrupted && line.starts_with("-----") {
                    break;
                }
                // a DataFrame marker may follow IPython's "Out[n]: " prefix
                if let Some(pos) = line.find(ARROW_MARK) {
                    let rest = &line[pos + ARROW_MARK.len()..];
                    let path = rest.split(MARK_END).next().unwrap_or("").trim();
                    match std::fs::read(path) {
                        Ok(bytes) => {
                            let _ = std::fs::remove_file(path);
                            match crate::notebook::result_set_from_ipc(&bytes, out.result_sets.len()) {
                                Ok(rs) => out.result_sets.push(rs),
                                Err(e) => out.messages.push(msg(format!("Could not read the DataFrame: {e}"), true)),
                            }
                        }
                        Err(e) => out.messages.push(msg(format!("Could not read the DataFrame file {path}: {e}"), true)),
                    }
                    continue;
                }
                // IPython prints its formatted traceback on stdout; that becomes the error text
                if failed && line.starts_with("-----") && !in_traceback {
                    in_traceback = true;
                    continue;
                }
                if !line.trim().is_empty() || !out.messages.is_empty() {
                    out.messages.push(msg(line.to_string(), in_traceback));
                }
            }
            // an interrupted cell's stderr is the cancellation itself (py4j errors from the
            // cancelled jobs), not the user's output
            let stderr = if interrupted { "" } else { v.get("stderr").and_then(Value::as_str).unwrap_or("") };
            for line in stderr.lines() {
                let t = line.trim_end();
                if t.is_empty() || t.contains(" WARN ") || t.contains(" INFO ") || t.starts_with('[') && t.contains("Stage ") {
                    continue;
                }
                out.messages.push(msg(t.to_string(), false));
            }
            if failed {
                out.failed = true;
                if !in_traceback && !out.messages.iter().any(|m| m.is_error) {
                    let err = v.get("error").and_then(Value::as_str).unwrap_or("error").to_string();
                    let tb = v.get("traceback").and_then(Value::as_str).map(cobalt_notebook::strip_ansi).unwrap_or_default();
                    out.messages.push(msg(if tb.trim().is_empty() { err } else { tb }, true));
                }
            }
            if let Some(n) = v.get("notices").and_then(Value::as_array) {
                for x in n.iter().filter_map(Value::as_str) {
                    out.messages.push(msg(format!("notice: {x}"), false));
                }
            }
            if interrupted {
                out.interrupted = true;
                let err = v.get("error").and_then(Value::as_str).unwrap_or("");
                let detail = if err.contains("Spark jobs cancelled") { " (Spark jobs cancelled)" } else { "" };
                out.messages.push(msg(format!("Interrupted{detail}"), false));
            }
        }
    }
    // trailing blank stdout lines
    while out.messages.last().map(|m| !m.is_error && m.text.trim().is_empty()).unwrap_or(false) {
        out.messages.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_reads_native_displays() {
        // an Arrow IPC stream with one int column
        let schema = std::sync::Arc::new(arrow::datatypes::Schema::new(vec![arrow::datatypes::Field::new("n", arrow::datatypes::DataType::Int64, false)]));
        let batch = arrow::record_batch::RecordBatch::try_new(schema.clone(), vec![std::sync::Arc::new(arrow::array::Int64Array::from(vec![1, 2, 3]))]).unwrap();
        let mut buf = Vec::new();
        {
            let mut w = arrow::ipc::writer::StreamWriter::try_new(&mut buf, &schema).unwrap();
            w.write(&batch).unwrap();
            w.finish().unwrap();
        }
        let v = json!({"ok": true, "stdout": "hello\nOut[3]: DataFrame[n: bigint]\n", "stderr": "", "error": null, "traceback": null, "notices": [], "displays": [{"kind": "arrow", "source": "result", "columns": ["n"], "row_count": 3, "truncated": false, "limit": 100, "arrow_bytes": buf.len()}]});
        let o = outcome(&Ok(v), &[buf]);
        assert!(!o.failed);
        assert_eq!(o.result_sets.len(), 1);
        assert_eq!(o.result_sets[0].row_count(), 3);
        // the captured value's Out[n] repr is dropped from the text
        let texts: Vec<&str> = o.messages.iter().map(|m| m.text.as_str()).collect();
        assert_eq!(texts, vec!["hello"]);
        assert!(version_tuple("0.4.1") >= (0, 4, 1) && version_tuple("0.4.0") < (0, 4, 1) && version_tuple("1.0") >= (0, 4, 1));
    }

    #[test]
    fn bootstrap_substitutes() {
        let s = python_bootstrap(std::path::Path::new("C:\\x\\out"), 500);
        assert!(s.contains("__COBALT_OUT = r\"C:\\x\\out\""));
        assert!(s.contains("__COBALT_LIMIT = 500"));
        assert!(!s.contains("{out_dir}"));
    }

    #[test]
    fn outcome_parses_exec_result() {
        let v = json!({"ok": false, "stdout": "hello\nworld\n", "stderr": "26/10/05 WARN SQLConf: x\nreal problem\n", "error": "ZeroDivisionError: division by zero", "traceback": "Traceback...\nZeroDivisionError: division by zero", "execution_count": 3, "notices": []});
        let o = outcome(&Ok(v), &[]);
        assert!(o.failed);
        let texts: Vec<&str> = o.messages.iter().map(|m| m.text.as_str()).collect();
        assert_eq!(texts[0], "hello");
        assert_eq!(texts[1], "world");
        assert_eq!(texts[2], "real problem");
        assert!(texts[3].starts_with("Traceback"));
        assert!(o.messages[3].is_error);
        // IPython's own traceback on stdout is used as the error, once
        let v = json!({"ok": false, "stdout": "x\n---------------------------------\nZeroDivisionError  Traceback\nCell In[6], line 1\n----> 1 1 / 0\n\nZeroDivisionError: division by zero\n", "stderr": "", "error": "ZeroDivisionError: division by zero", "traceback": "Traceback (most recent call last)...", "notices": []});
        let o = outcome(&Ok(v), &[]);
        assert_eq!(o.messages[0].text, "x");
        assert!(!o.messages[0].is_error);
        let errs: Vec<&str> = o.messages.iter().filter(|m| m.is_error).map(|m| m.text.as_str()).collect();
        assert_eq!(errs.last().copied(), Some("ZeroDivisionError: division by zero"));
        assert!(!o.messages.iter().any(|m| m.text.starts_with("Traceback (most recent")));
        // an "Out[n]: " prefix before a marker
        let v = json!({"ok": true, "stdout": "Out[8]: \u{1e}COBALT-ARROW-FILE:C:/nope/x.arrow\u{1e}\n", "stderr": "", "error": null, "traceback": null, "notices": []});
        let o = outcome(&Ok(v), &[]);
        assert!(o.messages.iter().any(|m| m.text.contains("Could not read the DataFrame file")));
        let o = outcome(&Err("interrupted".into()), &[]);
        assert!(o.failed && o.messages[0].is_error);
        // protocol 2: an interrupted cell is not a failure and its traceback is dropped
        let v = json!({"ok": false, "interrupted": true, "stdout": "step 1\n---------------------------------\nKeyboardInterrupt  Traceback\n", "stderr": "ERROR:root:Exception while sending command.\npy4j.protocol.Py4JNetworkError: x\n", "error": "KeyboardInterrupt: interrupted (Spark jobs cancelled)", "traceback": "Traceback...", "notices": []});
        let o = outcome(&Ok(v), &[]);
        assert!(o.interrupted && !o.failed);
        let texts: Vec<&str> = o.messages.iter().map(|m| m.text.as_str()).collect();
        assert_eq!(texts, vec!["step 1", "Interrupted (Spark jobs cancelled)"]);
        assert!(version_tuple("0.4.0") >= (0, 4, 0) && version_tuple("1.0") >= (0, 4, 0) && version_tuple("0.3.5") < (0, 4, 0));
    }
}
