//! Version pins. Mirrors `profiles.py` in local-spark-mcp until that project ships a
//! machine-readable manifest (see `docs/requests/local-spark-mcp.md`).

use crate::{Result, RuntimeError};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const EMBEDDED: &str = include_str!("../manifest.json");

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Manifest {
    pub schema: u32,
    pub local_spark_mcp: Package,
    /// The LakeSail engine (Sail): pysail + the PySpark Connect client, no JVM.
    #[serde(default)]
    pub sail: SailPins,
    pub default_profile: String,
    pub profiles: BTreeMap<String, Profile>,
    pub uv: UvPins,
    pub jdk: JdkPins,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Package {
    pub version: String,
    /// What pip installs: a tarball URL (no git needed) or, later, a PyPI version.
    pub source: String,
}

/// Which local Spark engine a session runs on.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    /// local-spark-mcp: a JVM Spark matching a Fabric runtime profile.
    #[default]
    PySpark,
    /// LakeSail's Sail: a Rust Spark Connect server, no Java.
    Sail,
}

impl Engine {
    pub fn parse(s: &str) -> Engine {
        if s.trim().eq_ignore_ascii_case("sail") || s.trim().eq_ignore_ascii_case("lakesail") {
            Engine::Sail
        } else {
            Engine::PySpark
        }
    }
    pub fn key(self) -> &'static str {
        match self {
            Engine::PySpark => "pyspark",
            Engine::Sail => "sail",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Engine::PySpark => "Local Spark (JVM)",
            Engine::Sail => "LakeSail (experimental)",
        }
    }
    pub fn is_sail(self) -> bool {
        matches!(self, Engine::Sail)
    }
}

/// Pins of the LakeSail engine's environment.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct SailPins {
    /// The `pysail` wheel.
    pub version: String,
    /// The `pyspark-client` (Spark Connect, no jars) the worker talks with.
    pub pyspark_client: String,
    pub python: String,
    pub python_windows: String,
    /// Other packages of the environment (IPython for cells, pandas/pyarrow for results).
    pub packages: Vec<String>,
}

impl Default for SailPins {
    fn default() -> Self {
        Self { version: "0.7.2".into(), pyspark_client: "4.1.3".into(), python: "3.13".into(), python_windows: "3.11".into(), packages: vec!["ipython>=8.18".into(), "pandas>=2.0,<3".into(), "pyarrow>=15".into()] }
    }
}

impl SailPins {
    pub fn python_for(&self, platform: Platform) -> &str {
        if platform.is_windows() {
            &self.python_windows
        } else {
            &self.python
        }
    }
    /// What `uv pip install` gets.
    pub fn requirements(&self) -> Vec<String> {
        let mut v = vec![format!("pysail=={}", self.version), format!("pyspark-client=={}", self.pyspark_client)];
        v.extend(self.packages.iter().cloned());
        v
    }
    pub fn describe(&self) -> String {
        format!("Sail {}: pysail {}, pyspark-client {}, Python {}, no Java", self.version, self.version, self.pyspark_client, self.python)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Profile {
    pub fabric_runtime: String,
    pub extra: String,
    pub pyspark: String,
    pub delta: String,
    pub python: String,
    /// SPARK-53759: PySpark workers crash on Windows under 3.12+ for these pyspark versions.
    pub python_windows: String,
    pub java_majors: Vec<u32>,
    pub java_preferred: Vec<u32>,
    pub scala: String,
    pub hadoop_azure: String,
    pub spark_major: u32,
}

impl Profile {
    /// The Python version to install for this platform.
    pub fn python_for(&self, platform: Platform) -> &str {
        if platform.is_windows() {
            &self.python_windows
        } else {
            &self.python
        }
    }
    pub fn accepts_java(&self, major: u32) -> bool {
        self.java_majors.contains(&major)
    }
    /// Rank of a Java major in the preference order (lower is better); None = not accepted.
    pub fn java_rank(&self, major: u32) -> Option<usize> {
        self.java_preferred.iter().position(|m| *m == major).or_else(|| self.java_majors.contains(&major).then_some(self.java_preferred.len()))
    }
    pub fn describe(&self) -> String {
        format!("Fabric Runtime {}: pyspark {}, delta-spark {}, Python {}, Java {}", self.fabric_runtime, self.pyspark, self.delta, self.python, self.java_majors.iter().map(|m| m.to_string()).collect::<Vec<_>>().join("/"))
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Asset {
    pub url: String,
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct UvPins {
    pub version: String,
    /// An existing uv at least this new is adopted instead of downloading.
    pub min_adopt: String,
    pub assets: BTreeMap<String, Asset>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct JdkRelease {
    pub version: String,
    pub assets: BTreeMap<String, Asset>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct JdkPins {
    /// Microsoft Build of OpenJDK, by major.
    pub microsoft: BTreeMap<String, JdkRelease>,
    pub temurin: TemurinApi,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct TemurinApi {
    pub api: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum JdkVendor {
    #[default]
    Microsoft,
    Temurin,
}

impl JdkVendor {
    pub fn label(self) -> &'static str {
        match self {
            JdkVendor::Microsoft => "Microsoft Build of OpenJDK",
            JdkVendor::Temurin => "Eclipse Temurin",
        }
    }
    pub const ALL: [JdkVendor; 2] = [JdkVendor::Microsoft, JdkVendor::Temurin];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    WindowsX64,
    LinuxX64,
    LinuxArm64,
    MacArm64,
    MacX64,
}

impl Platform {
    pub fn current() -> Self {
        match (std::env::consts::OS, std::env::consts::ARCH) {
            ("windows", _) => Platform::WindowsX64,
            ("linux", "aarch64") => Platform::LinuxArm64,
            ("linux", _) => Platform::LinuxX64,
            ("macos", "aarch64") => Platform::MacArm64,
            ("macos", _) => Platform::MacX64,
            _ => Platform::LinuxX64,
        }
    }
    pub fn is_windows(self) -> bool {
        matches!(self, Platform::WindowsX64)
    }
    /// Key into the manifest asset maps.
    pub fn key(self) -> &'static str {
        match self {
            Platform::WindowsX64 => "windows-x86_64",
            Platform::LinuxX64 => "linux-x86_64",
            Platform::LinuxArm64 => "linux-aarch64",
            Platform::MacArm64 => "macos-aarch64",
            Platform::MacX64 => "macos-x86_64",
        }
    }
    /// Adoptium API `os` / `architecture` values.
    pub fn adoptium(self) -> (&'static str, &'static str) {
        match self {
            Platform::WindowsX64 => ("windows", "x64"),
            Platform::LinuxX64 => ("linux", "x64"),
            Platform::LinuxArm64 => ("linux", "aarch64"),
            Platform::MacArm64 => ("mac", "aarch64"),
            Platform::MacX64 => ("mac", "x64"),
        }
    }
}

impl Manifest {
    pub fn embedded() -> Self {
        serde_json::from_str(EMBEDDED).expect("embedded manifest parses")
    }

    pub fn profile(&self, name: &str) -> Result<&Profile> {
        self.profiles.get(name).ok_or_else(|| RuntimeError::Manifest(format!("unknown runtime profile {name:?}; known: {}", self.profiles.keys().cloned().collect::<Vec<_>>().join(", "))))
    }

    pub fn uv_asset(&self, platform: Platform) -> Result<&Asset> {
        self.uv.assets.get(platform.key()).ok_or_else(|| RuntimeError::Manifest(format!("no uv build pinned for {}", platform.key())))
    }

    /// The Microsoft JDK pin for a major.
    pub fn microsoft_jdk(&self, major: u32, platform: Platform) -> Result<(&JdkRelease, &Asset)> {
        let rel = self.jdk.microsoft.get(&major.to_string()).ok_or_else(|| RuntimeError::Manifest(format!("no Microsoft JDK {major} pinned")))?;
        let asset = rel.assets.get(platform.key()).ok_or_else(|| RuntimeError::Manifest(format!("no Microsoft JDK {major} build pinned for {}", platform.key())))?;
        Ok((rel, asset))
    }

    pub fn temurin_url(&self, major: u32, platform: Platform) -> String {
        let (os, arch) = platform.adoptium();
        self.jdk.temurin.api.replace("{major}", &major.to_string()).replace("{os}", os).replace("{arch}", arch)
    }

    /// The pip requirement for a profile: `local-spark-mcp[fabric-2.0] @ <tarball>` or `==version`.
    pub fn requirement(&self, profile: &Profile) -> String {
        let src = &self.local_spark_mcp.source;
        if src.starts_with("http") || src.starts_with("git+") {
            format!("local-spark-mcp[{}] @ {src}", profile.extra)
        } else {
            format!("local-spark-mcp[{}]=={}", profile.extra, self.local_spark_mcp.version)
        }
    }
}

/// `"0.12.3"` → `[0, 12, 3]` for comparisons.
pub fn version_tuple(v: &str) -> Vec<u32> {
    v.trim().trim_start_matches('v').split(|c: char| !c.is_ascii_digit()).filter(|s| !s.is_empty()).filter_map(|s| s.parse().ok()).take(3).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_manifest_is_consistent() {
        let m = Manifest::embedded();
        assert!(m.profiles.contains_key(&m.default_profile));
        let p = m.profile("fabric-2.0").unwrap();
        assert_eq!(p.python_for(Platform::WindowsX64), "3.11");
        assert_eq!(p.python_for(Platform::LinuxX64), "3.13");
        assert!(p.accepts_java(21) && p.accepts_java(17) && !p.accepts_java(11));
        assert_eq!(p.java_rank(21), Some(0));
        assert_eq!(p.java_rank(11), None);
        for plat in [Platform::WindowsX64, Platform::LinuxX64, Platform::MacArm64] {
            m.uv_asset(plat).unwrap();
            for major in p.java_preferred.iter() {
                let (_, a) = m.microsoft_jdk(*major, plat).unwrap();
                assert_eq!(a.sha256.len(), 64);
            }
        }
        assert!(m.requirement(p).starts_with("local-spark-mcp[fabric-2.0] @ https://"));
        assert!(m.temurin_url(21, Platform::WindowsX64).contains("/21/"));
    }

    #[test]
    fn versions() {
        assert_eq!(version_tuple("uv 0.12.23 (abc)"), vec![0, 12, 23]);
        assert!(version_tuple("0.7.3") < version_tuple("0.12.0"));
        assert_eq!(version_tuple("21.0.8+9"), vec![21, 0, 8]);
    }
}
