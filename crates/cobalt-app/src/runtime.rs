//! Settings → Spark runtime: status inspection and provisioning jobs on background threads,
//! polled by the app each frame.

use cobalt_core::Settings;
use cobalt_runtime::install::{self, Plan, Progress, Step};
use cobalt_runtime::{Installed, JdkVendor, Manifest, RuntimeDirs, RuntimeStatus};
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobKind {
    Install,
    SmokeTest,
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
}

pub fn dirs(settings: &Settings, paths: &AppPaths) -> RuntimeDirs {
    match settings.spark.runtime_dir.as_deref().filter(|s| !s.trim().is_empty()) {
        Some(d) => RuntimeDirs::new(PathBuf::from(d)),
        None => RuntimeDirs::new(paths.runtime_dir()),
    }
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
    let jdk = settings.spark.java_home.clone().filter(|s| !s.trim().is_empty()).map(PathBuf::from);
    let (tx, rx) = crossbeam_channel::bounded(1);
    let ctx = egui.clone();
    ui.status_pending = true;
    ui.status_rx = Some(rx);
    std::thread::Builder::new()
        .name("runtime-status".into())
        .spawn(move || {
            let m = Manifest::embedded();
            let st = RuntimeStatus::inspect(&dirs, &m, &profile, jdk.as_deref());
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

pub fn action(ui: &mut RuntimeUi, settings: &Settings, paths: &AppPaths, egui: &egui::Context, a: RuntimeAction) -> Option<String> {
    match a {
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
            match install::remove_all(&d) {
                Ok(()) => {
                    ui.last_result = Some(Ok(format!("Removed {}", d.root.display())));
                    ui.last_smoke = None;
                    refresh_status(ui, settings, paths, egui);
                }
                Err(e) => return Some(format!("Could not remove the runtime: {e}")),
            }
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
                JobEvent::Done(r) => {
                    let secs = job.started.elapsed().as_secs();
                    ui.last_result = Some(match &r {
                        Ok(rec) => {
                            if job.kind == JobKind::SmokeTest {
                                ui.last_smoke = job.log.iter().rev().find(|l| l.contains("SELECT 1 returned")).cloned();
                            }
                            Ok(format!("{} finished in {}s{}", if job.kind == JobKind::SmokeTest { "Smoke test" } else { "Install" }, secs, rec.spark_version.as_ref().map(|v| format!(" — Spark {v}")).unwrap_or_default()))
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
