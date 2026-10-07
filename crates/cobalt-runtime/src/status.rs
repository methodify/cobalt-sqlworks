//! What is installed: a snapshot for the settings page, and the record written after
//! provisioning.

use crate::detect;
use crate::manifest::{JdkVendor, Manifest, Platform};
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
    pub last_error: Option<String>,
    /// `python -m local_spark_mcp.healthcheck --json` from the environment (None when it is not
    /// installed or the check could not run).
    pub health: Option<Health>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Health {
    pub ok: bool,
    pub problems: Vec<String>,
    pub warnings: Vec<String>,
    pub profile: Option<String>,
    pub protocol_version: Option<u64>,
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
        self.uv.is_ready() && self.python.is_ready() && self.env.is_ready() && self.jdk.is_ready()
    }

    /// Inspect the runtime folder and the machine. `jdk_override` is a user-chosen JDK home.
    pub fn inspect(dirs: &RuntimeDirs, manifest: &Manifest, profile: &str, jdk_override: Option<&Path>) -> Self {
        let platform = Platform::current();
        let record = Installed::load(dirs);
        let prof = manifest.profile(profile).ok();
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
            last_error: record.last_error.clone(),
            health,
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
