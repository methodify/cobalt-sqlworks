//! The local Spark kernel: one local-spark-mcp worker process, owned by a thread that speaks the
//! socket protocol, driven by commands from the UI and reporting events the app polls each frame.
//! Notebook cells on the "Local Spark" kernel run here (Python through `run_code`; SQL through a
//! `spark.sql` helper). DataFrames come back as Arrow IPC files written by a helper the kernel
//! installs into the IPython namespace at start (see `BOOTSTRAP`), until local-spark-mcp grows a
//! native Arrow method.

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
    /// Lakehouses whose tables the session clones in the background right after start
    /// (`["all"]` for every lakehouse in the workspace).
    pub preload: Vec<String>,
}

impl KernelFabric {
    pub fn label(&self) -> String {
        format!("{}{} · {}", self.default_lakehouse.clone().unwrap_or_else(|| "no default lakehouse".into()), if self.workspace_name.is_empty() { String::new() } else { format!(" ({})", self.workspace_name) }, self.write_mode)
    }
    /// Same binding (ignoring the account fields).
    pub fn same_binding(&self, other: &KernelFabric) -> bool {
        self.workspace_id == other.workspace_id && self.default_lakehouse == other.default_lakehouse && self.write_mode == other.write_mode && self.lakehouses == other.lakehouses && self.preload == other.preload
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
}

enum Cmd {
    Start { cfg: WorkerConfig, bootstrap: String },
    Run(RunReq),
    /// Any other worker method (shadow_status, discard_shadow, restore_shadow, info…).
    Call { tag: String, method: String, params: Value },
    Shutdown,
}

pub enum KernelEvent {
    Ready { info: Value, control: Option<ControlHandle> },
    Log(String),
    /// Streamed output of the running cell (protocol 2): `stream` is `stdout` or `stderr`.
    CellOutput { tab: TabId, cell_id: String, stream: String, text: String },
    /// The control socket answered an `interrupt`.
    Interrupted(Result<Value, String>),
    CallResult { tag: String, result: Result<Value, String> },
    /// A cell finished: the worker's `ExecResult` (ok, stdout, stderr, error, traceback…), or a
    /// transport-level error.
    Done { req: RunReq, result: Result<Value, String> },
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
    ev_tx: Option<Sender<KernelEvent>>,
    /// A preload driven from the app: table lists come from the Fabric REST API (the worker's
    /// own discovery needs an Azure credential this process does not give it), then
    /// `mount_tables` runs chunk by chunk so cells can interleave.
    pub preload: Option<Preload>,
}

#[derive(Clone, Debug, Default)]
pub struct Preload {
    /// `(lakehouse, tables)` chunks still to mount.
    pub pending: std::collections::VecDeque<(String, Vec<String>)>,
    /// A `mount_tables` call is in flight.
    pub in_flight: bool,
    pub total: usize,
    pub done: usize,
    pub failed: usize,
    pub errors: Vec<String>,
    /// Tables in schema folders the catalog cannot mount (not counted in `total`).
    pub skipped: usize,
    pub note: Option<String>,
    /// Lakehouses whose table lists are still being fetched.
    pub listing: usize,
    pub started: Option<Instant>,
    pub finished: Option<Instant>,
}

impl Preload {
    pub fn state(&self) -> &'static str {
        if self.listing > 0 || self.in_flight || !self.pending.is_empty() {
            "running"
        } else if self.started.is_some() {
            "done"
        } else {
            "idle"
        }
    }
    pub fn as_json(&self) -> Value {
        json!({"state": self.state(), "tables_total": self.total, "tables_done": self.done, "tables_failed": self.failed, "tables_skipped": self.skipped, "note": self.note, "errors": self.errors, "seconds": self.started.map(|s| self.finished.unwrap_or_else(Instant::now).duration_since(s).as_secs())})
    }
}

impl Default for KernelUi {
    fn default() -> Self {
        Self { state: KernelState::Stopped, busy: None, log: VecDeque::new(), log_open: false, profile: String::new(), waiting: Vec::new(), tx: None, rx: None, cancel: Arc::new(AtomicBool::new(false)), out_dir: None, fabric: None, pending_fabric_start: None, token_server: None, binding_warned: Default::default(), control: None, interrupting: None, ev_tx: None, preload: None }
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
        extra.insert("persist_shadow".into(), Value::Bool(false));
        k.preload = if fs.fabric.preload.is_empty() { None } else { Some(Preload::default()) };
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
    let mut cfg = install::worker_config(&dirs, &profile, jdk.as_deref(), &settings.spark.driver_memory, extra);
    // the control socket exists from local-spark-mcp 0.4.0 (protocol 2); an older worker would
    // reject the argument
    cfg.control = rec.package_version.as_deref().map(|v| version_at_least(v, 0, 4)).unwrap_or(false);
    if !cfg.control {
        k.log.push_back("cobalt: local-spark-mcp before 0.4.0 — Stop ends the session instead of interrupting the cell (update the runtime on the Spark runtime page)".into());
    }
    let bootstrap = python_bootstrap(&out_dir, settings.notebooks.spark_row_limit.max(1));
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
    let _ = ctx_tx.send(Cmd::Start { cfg, bootstrap });
    Ok(())
}

fn kernel_thread(rx: Receiver<Cmd>, tx: Sender<KernelEvent>, cancel: Arc<AtomicBool>, egui: egui::Context) {
    let mut worker: Option<Worker> = None;
    let log_tx = tx.clone();
    let log_egui = egui.clone();
    let log: cobalt_runtime::worker::LogFn = Arc::new(move |s: String| {
        let _ = log_tx.send(KernelEvent::Log(s));
        log_egui.request_repaint_after(Duration::from_millis(100));
    });
    while let Ok(cmd) = rx.recv() {
        match cmd {
            Cmd::Start { cfg, bootstrap } => {
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
                    let _ = tx.send(KernelEvent::Done { req, result: Err("the Spark kernel is not running".into()) });
                    egui.request_repaint();
                    continue;
                };
                cancel.store(false, Ordering::Relaxed);
                let stream = w.protocol_version >= 2;
                let (tab, cell_id) = (req.tab, req.cell_id.clone());
                let ev_tx = tx.clone();
                let ev_egui = egui.clone();
                let mut on_event = |ev: &str, text: &str| {
                    let _ = ev_tx.send(KernelEvent::CellOutput { tab, cell_id: cell_id.clone(), stream: ev.to_string(), text: text.to_string() });
                    ev_egui.request_repaint_after(Duration::from_millis(100));
                };
                let r = w.call_streaming("run_code", json!({"code": req.code, "stream": stream}), Duration::from_secs(60 * 60 * 24), &cancel, &mut on_event);
                match r {
                    Ok(v) => {
                        let _ = tx.send(KernelEvent::Done { req, result: Ok(v) });
                    }
                    Err(RuntimeError::Cancelled) => {
                        // the protocol is out of step after an abandoned call: the session restarts
                        if let Some(w) = worker.take() {
                            w.kill();
                        }
                        let _ = tx.send(KernelEvent::Done { req, result: Err("interrupted — the local Spark session was stopped (the next cell starts a new session)".into()) });
                        let _ = tx.send(KernelEvent::Stopped);
                    }
                    Err(RuntimeError::WorkerFatal(e)) => {
                        if let Some(w) = worker.take() {
                            w.kill();
                        }
                        let _ = tx.send(KernelEvent::Done { req, result: Err(e.clone()) });
                        let _ = tx.send(KernelEvent::Failed(e));
                    }
                    Err(e) => {
                        let fatal = !w.is_alive();
                        let _ = tx.send(KernelEvent::Done { req, result: Err(e.to_string()) });
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
    if k.state.is_ready() {
        if let Some(tx) = &k.tx {
            k.busy.get_or_insert((req.tab, req.cell_id.clone()));
            let _ = tx.send(Cmd::Run(req));
            return;
        }
    }
    k.waiting.push(req);
}

/// Send the next preload chunk when none is in flight.
pub fn preload_pump(k: &mut KernelUi) {
    let Some(p) = k.preload.as_mut() else { return };
    if p.in_flight || !k.state.is_ready() {
        return;
    }
    let Some((lakehouse, tables)) = p.pending.pop_front() else {
        if p.listing == 0 && p.finished.is_none() && p.started.is_some() {
            p.finished = Some(Instant::now());
        }
        return;
    };
    p.in_flight = true;
    let params = json!({"lakehouse": lakehouse, "tables": tables});
    if let Some(tx) = &k.tx {
        let _ = tx.send(Cmd::Call { tag: "preload-mount".into(), method: "mount_tables".into(), params });
    } else {
        p.in_flight = false;
    }
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

/// `v` (`major.minor[.patch]`) is at least `maj.min`.
pub fn version_at_least(v: &str, maj: u64, min: u64) -> bool {
    let mut it = v.trim().split(|c: char| c == '.' || c == '-' || c == '+').map(|p| p.chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse::<u64>().unwrap_or(0));
    let (a, b) = (it.next().unwrap_or(0), it.next().unwrap_or(0));
    (a, b) >= (maj, min)
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
    pub done: Vec<(RunReq, Result<Value, String>)>,
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
                k.state = KernelState::Ready { info, since: Instant::now() };
                k.control = control;
                ready_now = true;
            }
            KernelEvent::CellOutput { tab, cell_id, stream, text } => outputs.push((tab, cell_id, stream, text)),
            KernelEvent::Interrupted(r) => {
                k.log.push_back(match &r {
                    Ok(v) => format!("cobalt: interrupt → {}", v.get("state").and_then(Value::as_str).unwrap_or("?")),
                    Err(e) if e.contains("timed out") => "cobalt: interrupt sent; the worker's acknowledgement did not arrive in time (the cell is still being cancelled — Stop again to end the session)".to_string(),
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
            KernelEvent::Done { req, result } => {
                if k.busy.as_ref().map(|(t, c)| *t == req.tab && *c == req.cell_id).unwrap_or(false) {
                    k.busy = None;
                }
                k.interrupting = None;
                done.push((req, result));
            }
            KernelEvent::Stopped => {
                k.state = KernelState::Stopped;
                k.busy = None;
                k.tx = None;
                k.waiting.clear();
                k.token_server = None;
                k.fabric = None;
                k.preload = None;
                k.control = None;
                k.interrupting = None;
                broke = Some("the local Spark session stopped".to_string());
            }
            KernelEvent::Failed(e) => {
                k.state = KernelState::Failed(e.clone());
                k.busy = None;
                k.tx = None;
                k.waiting.clear();
                k.token_server = None;
                k.fabric = None;
                k.preload = None;
                k.control = None;
                k.interrupting = None;
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

pub fn outcome(result: &Result<Value, String>) -> CellOutcome {
    let mut out = CellOutcome { messages: Vec::new(), result_sets: Vec::new(), failed: false, interrupted: false };
    let msg = |text: String, is_error: bool| MessageLine { text, is_error, is_batch_header: false, line: None, at: Instant::now(), path: None };
    match result {
        Err(e) => {
            out.messages.push(msg(e.clone(), true));
            out.failed = true;
        }
        Ok(v) => {
            let stdout = v.get("stdout").and_then(Value::as_str).unwrap_or("");
            let interrupted = v.get("interrupted").and_then(Value::as_bool) == Some(true);
            let failed = !interrupted && (v.get("ok").and_then(Value::as_bool) == Some(false) || v.get("error").and_then(Value::as_str).map(|e| !e.is_empty()).unwrap_or(false));
            let mut in_traceback = false;
            for line in stdout.lines() {
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
    fn bootstrap_substitutes() {
        let s = python_bootstrap(std::path::Path::new("C:\\x\\out"), 500);
        assert!(s.contains("__COBALT_OUT = r\"C:\\x\\out\""));
        assert!(s.contains("__COBALT_LIMIT = 500"));
        assert!(!s.contains("{out_dir}"));
    }

    #[test]
    fn outcome_parses_exec_result() {
        let v = json!({"ok": false, "stdout": "hello\nworld\n", "stderr": "26/10/05 WARN SQLConf: x\nreal problem\n", "error": "ZeroDivisionError: division by zero", "traceback": "Traceback...\nZeroDivisionError: division by zero", "execution_count": 3, "notices": []});
        let o = outcome(&Ok(v));
        assert!(o.failed);
        let texts: Vec<&str> = o.messages.iter().map(|m| m.text.as_str()).collect();
        assert_eq!(texts[0], "hello");
        assert_eq!(texts[1], "world");
        assert_eq!(texts[2], "real problem");
        assert!(texts[3].starts_with("Traceback"));
        assert!(o.messages[3].is_error);
        // IPython's own traceback on stdout is used as the error, once
        let v = json!({"ok": false, "stdout": "x\n---------------------------------\nZeroDivisionError  Traceback\nCell In[6], line 1\n----> 1 1 / 0\n\nZeroDivisionError: division by zero\n", "stderr": "", "error": "ZeroDivisionError: division by zero", "traceback": "Traceback (most recent call last)...", "notices": []});
        let o = outcome(&Ok(v));
        assert_eq!(o.messages[0].text, "x");
        assert!(!o.messages[0].is_error);
        let errs: Vec<&str> = o.messages.iter().filter(|m| m.is_error).map(|m| m.text.as_str()).collect();
        assert_eq!(errs.last().copied(), Some("ZeroDivisionError: division by zero"));
        assert!(!o.messages.iter().any(|m| m.text.starts_with("Traceback (most recent")));
        // an "Out[n]: " prefix before a marker
        let v = json!({"ok": true, "stdout": "Out[8]: \u{1e}COBALT-ARROW-FILE:C:/nope/x.arrow\u{1e}\n", "stderr": "", "error": null, "traceback": null, "notices": []});
        let o = outcome(&Ok(v));
        assert!(o.messages.iter().any(|m| m.text.contains("Could not read the DataFrame file")));
        let o = outcome(&Err("interrupted".into()));
        assert!(o.failed && o.messages[0].is_error);
        // protocol 2: an interrupted cell is not a failure and its traceback is dropped
        let v = json!({"ok": false, "interrupted": true, "stdout": "step 1\n---------------------------------\nKeyboardInterrupt  Traceback\n", "stderr": "ERROR:root:Exception while sending command.\npy4j.protocol.Py4JNetworkError: x\n", "error": "KeyboardInterrupt: interrupted (Spark jobs cancelled)", "traceback": "Traceback...", "notices": []});
        let o = outcome(&Ok(v));
        assert!(o.interrupted && !o.failed);
        let texts: Vec<&str> = o.messages.iter().map(|m| m.text.as_str()).collect();
        assert_eq!(texts, vec!["step 1", "Interrupted (Spark jobs cancelled)"]);
        assert!(version_at_least("0.4.0", 0, 4) && version_at_least("1.0", 0, 4) && !version_at_least("0.3.5", 0, 4));
    }
}
