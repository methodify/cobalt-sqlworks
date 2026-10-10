//! Settings → Spark runtime: status inspection and provisioning jobs on background threads,
//! polled by the app each frame.

use cobalt_core::Settings;
use cobalt_runtime::install::{self, Plan, Progress, Step};
use cobalt_runtime::{Engine, Installed, JdkVendor, Manifest, RuntimeDirs, RuntimeStatus};
use cobalt_store::AppPaths;
use crossbeam_channel::{Receiver, Sender};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

#[derive(Default)]
pub struct RuntimeUi {
    pub status: Option<RuntimeStatus>,
    pub status_pending: bool,
    status_rx: Option<Receiver<RuntimeStatus>>,
    pub job: Option<RuntimeJob>,
    pub log_open: bool,
    /// Outcome of the last job, for the settings page.
    pub last_result: Option<Result<String, String>>,
    /// The smoke test's `SELECT 1` answer.
    pub last_smoke: Option<String>,
    /// Library status for the settings page, keyed by the spec list it was computed for.
    pub lib_cache: Option<(String, Vec<(String, Option<String>)>, Vec<(String, bool)>)>,
}

impl RuntimeUi {
    /// Python package versions and jar presence for the settings page (cached per spec list).
    pub fn library_status(&mut self, settings: &Settings, paths: &AppPaths) -> (Vec<(String, Option<String>)>, Vec<(String, bool)>) {
        let key = format!("{:?}|{:?}|{:?}|{}", settings.spark.python_packages, settings.spark.jars, settings.spark.maven, settings.spark.profile);
        if let Some((k, p, j)) = &self.lib_cache {
            if *k == key {
                return (p.clone(), j.clone());
            }
        }
        let d = dirs(settings, paths);
        let libs = libraries(settings);
        // the selected engine's environment: packages go into every installed one, so either answers
        let env_dir = if engine(settings).is_sail() { d.sail_env_dir() } else { d.env_dir(&settings.spark.profile) };
        let py = cobalt_runtime::libraries::python_status(&env_dir, &libs.python);
        let mut jars: Vec<(String, bool)> = libs.jars.iter().map(|j| (j.to_string_lossy().to_string(), j.is_file())).collect();
        for m in &libs.maven {
            let ok = cobalt_runtime::libraries::MavenCoord::parse(m).map(|c| cobalt_runtime::libraries::maven_jar_path(&d, &c).is_file()).unwrap_or(false);
            jars.push((m.clone(), ok));
        }
        self.lib_cache = Some((key, py.clone(), jars.clone()));
        (py, jars)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobKind {
    Install,
    SmokeTest,
    Libraries,
}

pub struct RuntimeJob {
    rx: Receiver<JobEvent>,
    cancel: Arc<AtomicBool>,
    pub kind: JobKind,
    pub step: Option<(Step, String)>,
    pub bytes: Option<(u64, Option<u64>)>,
    pub log: Vec<String>,
    pub started: Instant,
}

enum JobEvent {
    Progress(Progress),
    Done(Result<Installed, String>),
    DoneLibraries(Result<(Installed, String), String>),
}

/// What the settings page asks for.
#[derive(Clone, Debug)]
pub enum RuntimeAction {
    Refresh,
    Install,
    SmokeTest,
    Cancel,
    Remove,
    OpenFolder,
    OpenLog,
    /// Install the configured Python packages and fetch Maven jars.
    InstallLibraries,
    /// Delete the lakehouse Files mirror (lazily fetched files and pulled folders).
    ClearMirror,
}

/// The configured libraries as the runtime crate sees them.
pub fn libraries(settings: &Settings) -> cobalt_runtime::libraries::Libraries {
    cobalt_runtime::libraries::Libraries {
        python: settings.spark.python_packages.iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
        jars: settings.spark.jars.iter().map(|s| s.trim()).filter(|s| !s.is_empty()).map(PathBuf::from).collect(),
        maven: settings.spark.maven.iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
    }
}

pub fn dirs(settings: &Settings, paths: &AppPaths) -> RuntimeDirs {
    match settings.spark.runtime_dir.as_deref().filter(|s| !s.trim().is_empty()) {
        Some(d) => RuntimeDirs::new(PathBuf::from(d)),
        None => RuntimeDirs::new(paths.runtime_dir()),
    }
}

/// The engine the settings choose for the next session.
pub fn engine(settings: &Settings) -> Engine {
    Engine::parse(&settings.spark.engine)
}

pub fn jdk_vendor(settings: &Settings) -> JdkVendor {
    if settings.spark.jdk_vendor.eq_ignore_ascii_case("temurin") {
        JdkVendor::Temurin
    } else {
        JdkVendor::Microsoft
    }
}

/// Inspect the runtime folder and the machine on a thread; the result lands in `status`.
pub fn refresh_status(ui: &mut RuntimeUi, settings: &Settings, paths: &AppPaths, egui: &egui::Context) {
    if ui.status_pending {
        return;
    }
    let dirs = dirs(settings, paths);
    let profile = settings.spark.profile.clone();
    let engine = engine(settings);
    let jdk = settings.spark.java_home.clone().filter(|s| !s.trim().is_empty()).map(PathBuf::from);
    let (tx, rx) = crossbeam_channel::bounded(1);
    let ctx = egui.clone();
    ui.status_pending = true;
    ui.status_rx = Some(rx);
    std::thread::Builder::new()
        .name("runtime-status".into())
        .spawn(move || {
            let m = Manifest::embedded();
            let st = if engine.is_sail() { RuntimeStatus::inspect_sail(&dirs, &m) } else { RuntimeStatus::inspect(&dirs, &m, &profile, jdk.as_deref()) };
            let _ = tx.send(st);
            ctx.request_repaint();
        })
        .ok();
}

fn spawn_job(ui: &mut RuntimeUi, settings: &Settings, paths: &AppPaths, egui: &egui::Context, kind: JobKind, steps: Vec<Step>) {
    if ui.job.is_some() {
        return;
    }
    let dirs = dirs(settings, paths);
    let plan = Plan {
        engine: engine(settings),
        profile: settings.spark.profile.clone(),
        jdk_vendor: jdk_vendor(settings),
        adopt_jdk: settings.spark.java_home.clone().filter(|s| !s.trim().is_empty()).map(PathBuf::from),
        steps,
        driver_memory: settings.spark.driver_memory.clone(),
    };
    let cancel = Arc::new(AtomicBool::new(false));
    let (tx, rx) = crossbeam_channel::unbounded::<JobEvent>();
    let ctx = egui.clone();
    let c2 = cancel.clone();
    std::thread::Builder::new()
        .name("runtime-install".into())
        .spawn(move || {
            let m = Manifest::embedded();
            let tx2: Sender<JobEvent> = tx.clone();
            let ctx2 = ctx.clone();
            let progress = move |p: Progress| {
                let _ = tx2.send(JobEvent::Progress(p));
                ctx2.request_repaint();
            };
            let cx = install::Context { dirs: &dirs, manifest: &m, progress: &progress, cancel: &c2 };
            let r = install::provision(&cx, &plan).map_err(|e| e.to_string());
            let _ = tx.send(JobEvent::Done(r));
            ctx.request_repaint();
        })
        .ok();
    ui.job = Some(RuntimeJob { rx, cancel, kind, step: None, bytes: None, log: Vec::new(), started: Instant::now() });
    ui.last_result = None;
    ui.log_open = true;
}

fn spawn_libraries_job(ui: &mut RuntimeUi, settings: &Settings, paths: &AppPaths, egui: &egui::Context) {
    if ui.job.is_some() {
        return;
    }
    let dirs = dirs(settings, paths);
    let profile = settings.spark.profile.clone();
    let sail = engine(settings).is_sail();
    let libs = libraries(settings);
    let cancel = Arc::new(AtomicBool::new(false));
    let (tx, rx) = crossbeam_channel::unbounded::<JobEvent>();
    let ctx = egui.clone();
    let c2 = cancel.clone();
    std::thread::Builder::new()
        .name("runtime-libraries".into())
        .spawn(move || {
            let m = Manifest::embedded();
            let tx2: Sender<JobEvent> = tx.clone();
            let ctx2 = ctx.clone();
            let progress = move |p: Progress| {
                let _ = tx2.send(JobEvent::Progress(p));
                ctx2.request_repaint();
            };
            let cx = install::Context { dirs: &dirs, manifest: &m, progress: &progress, cancel: &c2 };
            // every provisioned engine environment gets the packages, so switching engines keeps them
            let mut targets: Vec<String> = Vec::new();
            if dirs.env_python(&profile).is_file() {
                targets.push(profile.clone());
            }
            if dirs.sail_env_python().is_file() {
                targets.push(cobalt_runtime::SAIL_ENV.to_string());
            }
            let wanted = if sail { cobalt_runtime::SAIL_ENV.to_string() } else { profile.clone() };
            let r = if !targets.contains(&wanted) {
                Err(cobalt_runtime::RuntimeError::Manifest(format!("the {} environment is not installed — install it first (Settings › Spark runtime)", if sail { "LakeSail" } else { "Spark" })))
            } else {
                let mut summaries = Vec::new();
                let mut outcome = Ok(());
                for t in &targets {
                    match cobalt_runtime::libraries::install(&cx, t, &libs) {
                        Ok(summary) => summaries.push(format!("{}: {summary}", if t == cobalt_runtime::SAIL_ENV { "LakeSail" } else { t.as_str() })),
                        Err(e) => {
                            outcome = Err(e);
                            break;
                        }
                    }
                }
                outcome.map(|_| summaries.join(" · "))
            }
            .map(|summary| {
                let mut rec = Installed::load(&dirs);
                rec.last_error = None;
                let _ = rec.save(&dirs);
                (rec, summary)
            });
            let _ = tx.send(match r {
                Ok((rec, summary)) => JobEvent::DoneLibraries(Ok((rec, summary))),
                Err(e) => JobEvent::DoneLibraries(Err(e.to_string())),
            });
            ctx.request_repaint();
        })
        .ok();
    ui.job = Some(RuntimeJob { rx, cancel, kind: JobKind::Libraries, step: None, bytes: None, log: Vec::new(), started: Instant::now() });
    ui.last_result = None;
    ui.log_open = true;
    ui.lib_cache = None;
}

pub fn action(ui: &mut RuntimeUi, settings: &Settings, paths: &AppPaths, egui: &egui::Context, a: RuntimeAction) -> Option<String> {
    match a {
        RuntimeAction::InstallLibraries => spawn_libraries_job(ui, settings, paths, egui),
        RuntimeAction::Refresh => refresh_status(ui, settings, paths, egui),
        RuntimeAction::Install => spawn_job(ui, settings, paths, egui, JobKind::Install, Step::ALL.to_vec()),
        RuntimeAction::SmokeTest => spawn_job(ui, settings, paths, egui, JobKind::SmokeTest, vec![Step::Warm]),
        RuntimeAction::Cancel => {
            if let Some(j) = &ui.job {
                j.cancel.store(true, Ordering::Relaxed);
            }
        }
        RuntimeAction::Remove => {
            if ui.job.is_some() {
                return Some("Cancel the running job first.".into());
            }
            let d = dirs(settings, paths);
            let r = if engine(settings).is_sail() { install::remove_sail(&d) } else { install::remove_all(&d) };
            match r {
                Ok(()) => {
                    ui.last_result = Some(Ok(if engine(settings).is_sail() { format!("Removed the LakeSail environment under {}", d.root.display()) } else { format!("Removed {}", d.root.display()) }));
                    ui.last_smoke = None;
                    refresh_status(ui, settings, paths, egui);
                }
                Err(e) => return Some(format!("Could not remove the runtime: {e}")),
            }
        }
        RuntimeAction::ClearMirror => {
            let d = dirs(settings, paths);
            let m = d.state_dir().join("lakehouses");
            if m.is_dir() {
                if let Err(e) = std::fs::remove_dir_all(&m) {
                    return Some(format!("Could not clear the Files mirror: {e} (stop the Spark session first if it is running)"));
                }
            }
            ui.last_result = Some(Ok("Lakehouse Files mirror cleared".into()));
            refresh_status(ui, settings, paths, egui);
        }
        RuntimeAction::OpenFolder => {
            let d = dirs(settings, paths);
            let _ = std::fs::create_dir_all(&d.root);
            let _ = open::that(&d.root);
        }
        RuntimeAction::OpenLog => {
            let d = dirs(settings, paths);
            if d.log_file().exists() {
                let _ = open::that(d.log_file());
            } else {
                return Some("No provisioning log yet.".into());
            }
        }
    }
    None
}

/// Drain job and status events. Call once per frame.
pub fn poll(ui: &mut RuntimeUi, settings: &Settings, paths: &AppPaths, egui: &egui::Context) {
    if let Some(rx) = &ui.status_rx {
        if let Ok(st) = rx.try_recv() {
            ui.status = Some(st);
            ui.status_pending = false;
            ui.status_rx = None;
        }
    }
    let mut finished = false;
    if let Some(job) = ui.job.as_mut() {
        while let Ok(ev) = job.rx.try_recv() {
            match ev {
                JobEvent::Progress(Progress::Step { step, label }) => {
                    job.step = Some((step, label));
                    job.bytes = None;
                }
                JobEvent::Progress(Progress::Bytes { done, total }) => job.bytes = Some((done, total)),
                JobEvent::Progress(Progress::Log(s)) => {
                    job.log.push(s);
                    if job.log.len() > 2000 {
                        job.log.drain(..500);
                    }
                }
                JobEvent::DoneLibraries(r) => {
                    let secs = job.started.elapsed().as_secs();
                    ui.last_result = Some(match r {
                        Ok((_, summary)) => Ok(format!("Libraries: {summary} ({secs}s). Jars apply at the next Spark session start; a running session needs a restart to import new Python packages.")),
                        Err(e) => Err(e),
                    });
                    ui.lib_cache = None;
                    finished = true;
                }
                JobEvent::Done(r) => {
                    let secs = job.started.elapsed().as_secs();
                    ui.last_result = Some(match &r {
                        Ok(rec) => {
                            if job.kind == JobKind::SmokeTest {
                                ui.last_smoke = job.log.iter().rev().find(|l| l.contains("SELECT 1 returned")).cloned();
                            }
                            let version = if engine(settings).is_sail() { rec.sail_version.as_ref().map(|v| format!(" — Sail {v} (PySpark Connect client {})", rec.sail_spark_version.clone().unwrap_or_default())) } else { rec.spark_version.as_ref().map(|v| format!(" — Spark {v}")) };
                            Ok(format!("{} finished in {}s{}", if job.kind == JobKind::SmokeTest { "Smoke test" } else { "Install" }, secs, version.unwrap_or_default()))
                        }
                        Err(e) => Err(e.clone()),
                    });
                    finished = true;
                }
            }
        }
        if !finished {
            egui.request_repaint_after(std::time::Duration::from_millis(250));
        }
    }
    if finished {
        ui.job = None;
        refresh_status(ui, settings, paths, egui);
    }
}
