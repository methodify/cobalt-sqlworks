//! The Cobalt-managed local Spark runtime.
//!
//! Everything lives under one folder in Cobalt's app data (`RuntimeDirs`): a pinned `uv`, a
//! uv-managed Python, a virtual environment holding `local-spark-mcp` with the chosen Fabric
//! runtime profile (pyspark + delta-spark), a non-Oracle JDK, the Ivy cache Spark fills on its
//! first session, and local-spark-mcp's state root (shadows, mirrors, warehouses). The manifest
//! (`manifest.json`, embedded) pins versions and checksums; `detect` finds what the machine
//! already has; `install` provisions the rest with resumable, hash-checked downloads; `worker`
//! speaks local-spark-mcp's socket protocol to a worker process (the smoke test and, from 0.8,
//! PySpark cells).
//!
//! No egui, no tokio: callers run this on a plain thread and receive `Progress` events.

pub mod detect;
pub mod install;
pub mod libraries;
pub mod manifest;
pub mod status;
pub mod worker;

pub use manifest::{Engine, Fallback, JdkVendor, LsmManifest, Manifest, PackageSource, Platform, Profile, Roster, SailPins};

/// Cobalt's LakeSail worker (`python -m cobalt_sail_worker`), written into the Sail
/// environment at install time.
pub const SAIL_WORKER: &str = include_str!("../python/cobalt_sail_worker.py");
pub const SAIL_WORKER_MODULE: &str = "cobalt_sail_worker";
/// The environment name under `envs/` for the LakeSail engine.
pub const SAIL_ENV: &str = "sail";
pub use status::{ComponentState, Installed, RuntimeStatus};

use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("download failed: {0}")]
    Http(String),
    #[error("checksum mismatch for {name}: expected {expected}, got {got}")]
    Checksum { name: String, expected: String, got: String },
    #[error("{cmd} failed ({status}): {stderr}")]
    Tool { cmd: String, status: String, stderr: String },
    #[error("worker: {0}")]
    Worker(String),
    /// The worker or its JVM is gone; the process must be restarted.
    #[error("worker (fatal): {0}")]
    WorkerFatal(String),
    #[error("cancelled")]
    Cancelled,
    #[error("{0}")]
    Manifest(String),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, RuntimeError>;

/// Layout of the runtime folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeDirs {
    pub root: PathBuf,
}

impl RuntimeDirs {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
    pub fn uv_dir(&self) -> PathBuf {
        self.root.join("uv")
    }
    pub fn uv_exe(&self) -> PathBuf {
        self.uv_dir().join(if cfg!(windows) { "uv.exe" } else { "uv" })
    }
    pub fn python_dir(&self) -> PathBuf {
        self.root.join("python")
    }
    pub fn cache_dir(&self) -> PathBuf {
        self.root.join("cache")
    }
    pub fn envs_dir(&self) -> PathBuf {
        self.root.join("envs")
    }
    pub fn env_dir(&self, profile: &str) -> PathBuf {
        self.envs_dir().join(profile)
    }
    /// The interpreter inside a profile's environment.
    pub fn env_python(&self, profile: &str) -> PathBuf {
        let env = self.env_dir(profile);
        if cfg!(windows) {
            env.join("Scripts").join("python.exe")
        } else {
            env.join("bin").join("python")
        }
    }
    pub fn jdk_dir(&self) -> PathBuf {
        self.root.join("jdk")
    }
    /// The LakeSail environment (pysail + pyspark-client + the worker module).
    pub fn sail_env_dir(&self) -> PathBuf {
        self.env_dir(SAIL_ENV)
    }
    pub fn sail_env_python(&self) -> PathBuf {
        self.env_python(SAIL_ENV)
    }
    /// Where the worker module lives (on `PYTHONPATH` for the worker process).
    pub fn sail_worker_file(&self) -> PathBuf {
        self.sail_env_dir().join(format!("{SAIL_WORKER_MODULE}.py"))
    }
    pub fn ivy_dir(&self) -> PathBuf {
        self.root.join("ivy")
    }
    pub fn state_dir(&self) -> PathBuf {
        self.root.join("state")
    }
    pub fn downloads_dir(&self) -> PathBuf {
        self.root.join("downloads")
    }
    pub fn record_file(&self) -> PathBuf {
        self.root.join("runtime.json")
    }
    pub fn log_file(&self) -> PathBuf {
        self.root.join("provision.log")
    }
}

/// Bytes used under a folder (best effort; unreadable entries count as zero).
pub fn dir_size(path: &Path) -> u64 {
    let mut total = 0;
    let Ok(rd) = std::fs::read_dir(path) else { return 0 };
    for e in rd.flatten() {
        let Ok(md) = e.metadata() else { continue };
        if md.is_dir() {
            total += dir_size(&e.path());
        } else {
            total += md.len();
        }
    }
    total
}

pub fn fmt_bytes(b: u64) -> String {
    const K: f64 = 1024.0;
    let b = b as f64;
    if b >= K * K * K {
        format!("{:.1} GB", b / (K * K * K))
    } else if b >= K * K {
        format!("{:.0} MB", b / (K * K))
    } else if b >= K {
        format!("{:.0} KB", b / K)
    } else {
        format!("{b:.0} B")
    }
}
