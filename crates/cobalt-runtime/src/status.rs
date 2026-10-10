//! What is installed: a snapshot for the settings page, and the record written after
//! provisioning.

use crate::detect;
use crate::manifest::{Engine, JdkVendor, Manifest, Platform};
use crate::RuntimeDirs;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Written to `runtime.json` after each successful step so the status survives restarts.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Installed {
    pub profile: Option<String>,
    pub uv: Option<PathBuf>,
    pub uv_version: Option<String>,
    pub python_version: Option<String>,
    pub env: Option<PathBuf>,
    pub package_version: Option<String>,
    pub jdk_home: Option<PathBuf>,
    pub jdk_major: Option<u32>,
    pub jdk_vendor: Option<JdkVendor>,
    /// The JDK was adopted from the machine rather than installed by Cobalt.
    pub jdk_adopted: bool,
    /// Spark started once and resolved its jars: the Ivy cache is warm.
    pub warmed: bool,
    pub spark_version: Option<String>,
    pub last_error: Option<String>,
    /// The LakeSail environment, when installed.
    pub sail_env: Option<PathBuf>,
    pub sail_version: Option<String>,
    pub sail_pyspark: Option<String>,
    pub sail_warmed: bool,
    pub sail_spark_version: Option<String>,
    /// The Fabric package roster installed into the LakeSail environment, and what failed.
    pub sail_roster: Option<String>,
    pub sail_roster_failed: Vec<String>,
    /// The profile whose roster went into the Local Spark environment, and what failed.
    pub profile_packages: Option<String>,
    pub profile_packages_failed: Vec<String>,
}

impl Installed {
    pub fn load(dirs: &RuntimeDirs) -> Self {
        std::fs::read_to_string(dirs.record_file()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }
    pub fn save(&self, dirs: &RuntimeDirs) -> std::io::Result<()> {
        std::fs::create_dir_all(&dirs.root)?;
        std::fs::write(dirs.record_file(), serde_json::to_string_pretty(self).unwrap_or_default())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ComponentState {
    /// Installed by Cobalt under the runtime folder.
    Managed { detail: String },
    /// Found on the machine and adopted.
    Adopted { detail: String },
    Missing { reason: String },
}

impl ComponentState {
    pub fn is_ready(&self) -> bool {
        !matches!(self, ComponentState::Missing { .. })
    }
    pub fn detail(&self) -> &str {
        match self {
            ComponentState::Managed { detail } | ComponentState::Adopted { detail } => detail,
            ComponentState::Missing { reason } => reason,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeStatus {
    /// Which engine this status describes.
    pub engine: Engine,
    pub profile: String,
    pub uv: ComponentState,
    pub python: ComponentState,
    pub env: ComponentState,
    pub jdk: ComponentState,
    pub warm: bool,
    pub spark_version: Option<String>,
    /// JDKs found on the machine that the profile accepts ("Use what I have").
    pub jdk_candidates: Vec<detect::JdkCandidate>,
    pub disk_bytes: u64,
    /// The lakehouse Files mirror under the state folder (lazily fetched files and pulled folders).
    pub mirror_bytes: u64,
    /// The installed local-spark-mcp version (to tell an update from a reinstall).
    pub package_version: Option<String>,
    pub last_error: Option<String>,
    /// `python -m local_spark_mcp.healthcheck --json` from the environment (None when it is not
    /// installed or the check could not run).
    pub health: Option<Health>,
    /// The Fabric package roster the settings ask for, against the environment's dist-info.
    pub roster: Option<RosterStatus>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RosterStatus {
    pub profile: String,
    /// Packages present at Fabric's version.
    pub installed: usize,
    pub total: usize,
    pub missing: Vec<String>,
    /// Present at another version: `name have (Fabric want)`.
    pub mismatched: Vec<String>,
    /// Packages the last install could not put in (no wheel for this platform, a conflict…).
    pub failed: Vec<String>,
}

impl RosterStatus {
    /// Everything is there at Fabric's version.
    pub fn complete(&self) -> bool {
        self.missing.is_empty() && self.mismatched.is_empty()
    }

    /// Read the environment: which of the roster's pins are in, at which version.
    pub fn read(env_dir: &Path, roster: &crate::manifest::Roster, failed: &[String]) -> Self {
        let mut missing = Vec::new();
        let mut mismatched = Vec::new();
        let mut installed = 0;
        for spec in &roster.packages {
            let name = crate::libraries::python_dist_name(spec);
            let want = spec.split_once("==").map(|(_, v)| v.trim()).unwrap_or("");
            match detect::installed_package_version(env_dir, &name) {
                None => missing.push(name),
                Some(have) if have == want || want.is_empty() => installed += 1,
                Some(have) => mismatched.push(format!("{name} {have} (Fabric {want})")),
            }
        }
        Self { profile: roster.profile.clone(), installed, total: roster.packages.len(), missing, mismatched, failed: failed.to_vec() }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Health {
    pub ok: bool,
    pub problems: Vec<String>,
    pub warnings: Vec<String>,
    pub profile: Option<String>,
    pub protocol_version: Option<u64>,
}

/// The Sail worker's own healthcheck: imports and versions, no server.
pub fn sail_healthcheck(dirs: &RuntimeDirs) -> Option<Health> {
    let python = dirs.sail_env_python();
    if !python.is_file() || !dirs.sail_worker_file().is_file() {
        return None;
    }
    let mut cmd = std::process::Command::new(python);
    cmd.args(["-m", crate::SAIL_WORKER_MODULE, "--healthcheck"]).env("PYTHONPATH", dirs.sail_env_dir()).env("PYTHONIOENCODING", "utf-8");
    run_healthcheck(cmd)
}

/// Run the package's own healthcheck (no Spark): versions, profile verdict, JDK and winutils
/// resolution, catalog jar. A couple of seconds; `java_home` is passed so the JDK verdict matches
/// what sessions will use.
pub fn healthcheck(python: &Path, java_home: Option<&Path>, profile: &str) -> Option<Health> {
    if !python.is_file() {
        return None;
    }
    let mut cmd = std::process::Command::new(python);
    cmd.args(["-m", "local_spark_mcp.healthcheck", "--json"]).env("LOCAL_SPARK_PROFILE", profile).env("PYTHONIOENCODING", "utf-8");
    if let Some(j) = java_home {
        cmd.env("JAVA_HOME", j).env("LOCAL_SPARK_JAVA_HOME", j);
    }
    run_healthcheck(cmd)
}

fn run_healthcheck(mut cmd: std::process::Command) -> Option<Health> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let out = cmd.output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let start = text.find('{')?;
    let v: serde_json::Value = serde_json::from_str(text[start..].trim()).ok()?;
    let strings = |k: &str| v.get(k).and_then(|a| a.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default();
    Some(Health { ok: v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false), problems: strings("problems"), warnings: strings("warnings"), profile: v.get("profile").and_then(|p| p.as_str()).map(str::to_string), protocol_version: v.get("protocol_version").and_then(|p| p.as_u64()) })
}

impl RuntimeStatus {
    pub fn is_ready(&self) -> bool {
        self.uv.is_ready() && self.python.is_ready() && self.env.is_ready() && (self.engine.is_sail() || self.jdk.is_ready())
    }

    /// The LakeSail engine's status: uv, Python, the Sail environment; no JDK. `roster` is the
    /// Fabric package roster the settings ask for (`none` = none).
    pub fn inspect_sail(dirs: &RuntimeDirs, manifest: &Manifest, roster: &str) -> Self {
        let platform = Platform::current();
        let record = Installed::load(dirs);
        let pins = &manifest.sail;
        let uv = match detect::find_uv(&dirs.uv_exe(), &manifest.uv.min_adopt) {
            Some(c) if c.source == "managed" => ComponentState::Managed { detail: format!("uv {}", c.version) },
            Some(c) => ComponentState::Adopted { detail: format!("uv {} at {}", c.version, c.exe.display()) },
            None => ComponentState::Missing { reason: format!("uv {} will be downloaded", manifest.uv.version) },
        };
        let env_dir = dirs.sail_env_dir();
        let env_python = dirs.sail_env_python();
        let want_py = pins.python_for(platform).to_string();
        let python = match detect::venv_python_version(&env_dir) {
            Some(v) if env_python.is_file() => {
                if v.starts_with(&want_py) {
                    ComponentState::Managed { detail: format!("Python {v}") }
                } else {
                    ComponentState::Missing { reason: format!("environment has Python {v}; LakeSail needs {want_py}") }
                }
            }
            _ => ComponentState::Missing { reason: format!("Python {want_py} will be installed by uv") },
        };
        let env = match detect::installed_package_version(&env_dir, "pysail") {
            Some(v) if env_python.is_file() && dirs.sail_worker_file().is_file() => {
                let client = detect::installed_package_version(&env_dir, "pyspark-client").or_else(|| detect::installed_package_version(&env_dir, "pyspark")).unwrap_or_default();
                if v == pins.version {
                    ComponentState::Managed { detail: format!("pysail {v} · pyspark-client {client}") }
                } else {
                    ComponentState::Missing { reason: format!("pysail {v} installed; {} pinned", pins.version) }
                }
            }
            Some(_) => ComponentState::Missing { reason: "the worker module is missing; reinstall".into() },
            _ => ComponentState::Missing { reason: format!("pysail {} + pyspark-client {} will be installed (about 250 MB)", pins.version, pins.pyspark_client) },
        };
        let health = if env.is_ready() { sail_healthcheck(dirs) } else { None };
        let roster = manifest.roster(roster).map(|r| RosterStatus::read(&env_dir, &r, &record.sail_roster_failed));
        Self {
            engine: Engine::Sail,
            profile: "sail".into(),
            uv,
            python,
            env,
            jdk: ComponentState::Managed { detail: "not needed".into() },
            warm: record.sail_warmed,
            spark_version: record.sail_spark_version.clone(),
            jdk_candidates: Vec::new(),
            disk_bytes: crate::dir_size(&env_dir),
            mirror_bytes: 0,
            package_version: record.sail_version.clone(),
            last_error: record.last_error.clone(),
            health,
            roster,
        }
    }

    /// Inspect the runtime folder and the machine. `jdk_override` is a user-chosen JDK home;
    /// `packages` says whether the settings ask for the profile's Fabric package roster.
    pub fn inspect(dirs: &RuntimeDirs, manifest: &Manifest, profile: &str, jdk_override: Option<&Path>, packages: bool) -> Self {
        let platform = Platform::current();
        let record = Installed::load(dirs);
        let prof = manifest.profile(profile).ok();
        let roster = if packages { manifest.roster(profile).map(|r| RosterStatus::read(&dirs.env_dir(profile), &r, &record.profile_packages_failed)) } else { None };
        let uv = match detect::find_uv(&dirs.uv_exe(), &manifest.uv.min_adopt) {
            Some(c) if c.source == "managed" => ComponentState::Managed { detail: format!("uv {}", c.version) },
            Some(c) => ComponentState::Adopted { detail: format!("uv {} at {}", c.version, c.exe.display()) },
            None => ComponentState::Missing { reason: format!("uv {} will be downloaded", manifest.uv.version) },
        };
        let env_dir = dirs.env_dir(profile);
        let env_python = dirs.env_python(profile);
        let want_py = prof.map(|p| p.python_for(platform).to_string()).unwrap_or_default();
        let python = match detect::venv_python_version(&env_dir) {
            Some(v) if env_python.is_file() => {
                if v.starts_with(&want_py) {
                    ComponentState::Managed { detail: format!("Python {v}") }
                } else {
                    ComponentState::Missing { reason: format!("environment has Python {v}; {profile} needs {want_py}") }
                }
            }
            _ => ComponentState::Missing { reason: format!("Python {want_py} will be installed by uv") },
        };
        let env = match detect::installed_package_version(&env_dir, "local-spark-mcp") {
            Some(v) if env_python.is_file() => {
                if v == manifest.local_spark_mcp.version {
                    ComponentState::Managed { detail: format!("local-spark-mcp {v} [{profile}]") }
                } else {
                    ComponentState::Missing { reason: format!("local-spark-mcp {v} installed; {} pinned", manifest.local_spark_mcp.version) }
                }
            }
            _ => ComponentState::Missing { reason: format!("local-spark-mcp {} [{profile}] will be installed", manifest.local_spark_mcp.version) },
        };
        let mut jdk_candidates = detect::find_jdks(prof, Some(&dirs.jdk_dir()));
        if let Some(over) = jdk_override {
            if let Ok(c) = detect::check_jdk(over, "settings") {
                jdk_candidates.retain(|x| x.home != c.home);
                jdk_candidates.insert(0, c);
            }
        }
        let chosen: Option<&detect::JdkCandidate> = if jdk_override.is_some() {
            jdk_candidates.iter().find(|c| c.source == "settings").filter(|c| prof.and_then(|p| c.major.map(|m| p.accepts_java(m))).unwrap_or(true)).or(None)
        } else if let Some(home) = record.jdk_home.as_ref() {
            jdk_candidates.iter().find(|c| &c.home == home)
        } else {
            None
        };
        let chosen = chosen.or_else(|| jdk_candidates.iter().find(|c| c.source == "managed"));
        let jdk = match chosen {
            Some(c) if c.source == "managed" => ComponentState::Managed { detail: c.label() },
            Some(c) => ComponentState::Adopted { detail: format!("{} [{}]", c.label(), c.source) },
            None => ComponentState::Missing {
                reason: match prof {
                    Some(p) => format!("Java {} will be downloaded ({})", p.java_preferred.first().copied().unwrap_or(21), record.jdk_vendor.unwrap_or_default().label()),
                    None => "no profile".into(),
                },
            },
        };
        let health = if env.is_ready() {
            let jdk_home = match &jdk {
                ComponentState::Missing { .. } => None,
                _ => jdk_candidates.first().map(|c| c.home.clone()),
            };
            healthcheck(&env_python, jdk_home.as_deref(), profile)
        } else {
            None
        };
        Self {
            engine: Engine::PySpark,
            profile: profile.to_string(),
            uv,
            python,
            env,
            jdk,
            warm: record.warmed && record.profile.as_deref() == Some(profile),
            spark_version: record.spark_version.clone(),
            jdk_candidates,
            disk_bytes: crate::dir_size(&dirs.root),
            mirror_bytes: crate::dir_size(&dirs.state_dir().join("lakehouses")),
            package_version: record.package_version.clone(),
            last_error: record.last_error.clone(),
            health,
            roster,
        }
    }

    /// The JDK home the worker should use, per the inspection rules above.
    pub fn jdk_home(&self) -> Option<PathBuf> {
        match &self.jdk {
            ComponentState::Missing { .. } => None,
            _ => {
                // the chosen candidate is first among managed, else the first candidate
                let detail = self.jdk.detail();
                self.jdk_candidates.iter().find(|c| detail.contains(&c.home.display().to_string())).or_else(|| self.jdk_candidates.first()).map(|c| c.home.clone())
            }
        }
    }
}
