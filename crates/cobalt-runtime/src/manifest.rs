//! Version pins. Cobalt's own (`manifest.json`: the local-spark-mcp release, Sail, uv, the JDKs)
//! plus local-spark-mcp's machine-readable manifest (`local-spark-mcp-profiles.json`, a verbatim
//! copy of `profiles.json` at the pinned tag): the runtime profiles and, since 0.8.0, each
//! profile's Fabric Python package roster. Bumping the pin means copying that file again; a test
//! keeps the two versions in step.

use crate::{Result, RuntimeError};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const EMBEDDED: &str = include_str!("../manifest.json");
/// `profiles.json` of the pinned local-spark-mcp release.
pub const LSM_PROFILES: &str = include_str!("../local-spark-mcp-profiles.json");

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Manifest {
    pub schema: u32,
    pub local_spark_mcp: Package,
    /// The LakeSail engine (Sail): pysail + the PySpark Connect client, no JVM.
    #[serde(default)]
    pub sail: SailPins,
    pub default_profile: String,
    /// The runtime profiles, from local-spark-mcp's manifest (`manifest.json` may override).
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
    /// Packages of Fabric's environment files that no roster carries, with the reason
    /// (pyspark, Fabric-only wheels, the torch stack…), from local-spark-mcp's manifest.
    #[serde(default)]
    pub python_packages_excluded: BTreeMap<String, String>,
    pub uv: UvPins,
    pub jdk: JdkPins,
}

/// The shape of local-spark-mcp's `profiles.json`.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct LsmManifest {
    pub schema: u32,
    pub package: String,
    pub version: String,
    #[serde(default)]
    pub protocol_version: Option<u64>,
    pub default_profile: String,
    #[serde(default)]
    pub python_packages_excluded: BTreeMap<String, String>,
    pub profiles: BTreeMap<String, Profile>,
}

impl LsmManifest {
    pub fn embedded() -> Self {
        serde_json::from_str(LSM_PROFILES).expect("embedded local-spark-mcp profiles parse")
    }
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
    /// Other packages of the environment (IPython for cells, pandas/pyarrow for results, the
    /// protobuf line the Connect client's generated code needs).
    pub packages: Vec<String>,
    /// Roster packages the engine owns (name → why): left out of a Fabric roster on this engine
    /// and re-asserted from `packages` after a roster install.
    pub reserved: BTreeMap<String, String>,
}

/// A Fabric runtime's notebook-facing Python packages at Fabric's versions, as pip
/// requirements (`name==version`), with where the list came from. Built from a profile's
/// `python_packages`; installed opt-in into either engine's environment.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Default)]
#[serde(default)]
pub struct Roster {
    /// The profile the roster belongs to (`fabric-2.0`).
    pub profile: String,
    /// The Fabric runtime version (`2.0`).
    pub fabric_runtime: String,
    /// URL of the environment file the versions were taken from.
    pub source: String,
    pub note: String,
    /// Fabric's pins, `name==version`.
    pub packages: Vec<String>,
    /// Per-platform replacements for pins that cannot install everywhere (name → fallback).
    pub fallbacks: BTreeMap<String, Fallback>,
}

/// A platform fallback for one roster pin: when `marker` holds for the environment's Python,
/// `requirement` is installed instead of Fabric's exact version (`python_packages_fallbacks`).
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Default)]
#[serde(default)]
pub struct Fallback {
    /// `python_version <op> 'X.Y'` — the only shape local-spark-mcp uses.
    pub marker: String,
    pub requirement: String,
    pub reason: String,
}

impl Roster {
    /// What to install for an environment running `python` (major, minor): Fabric's pin, or
    /// the fallback requirement where its marker applies.
    pub fn requirements_for(&self, python: Option<(u32, u32)>) -> Vec<String> {
        self.packages
            .iter()
            .map(|spec| {
                let name = crate::libraries::python_dist_name(spec);
                match self.fallbacks.get(&name) {
                    Some(fb) if marker_applies(&fb.marker, python) => fb.requirement.clone(),
                    _ => spec.clone(),
                }
            })
            .collect()
    }

    /// The fallback that applies to `name` on `python`, if any.
    pub fn fallback_for(&self, name: &str, python: Option<(u32, u32)>) -> Option<&Fallback> {
        self.fallbacks.get(name).filter(|fb| marker_applies(&fb.marker, python))
    }

    /// The roster without the named packages (an engine's own), with the note saying so.
    pub fn without(&self, reserved: &BTreeMap<String, String>) -> Roster {
        if reserved.is_empty() {
            return self.clone();
        }
        let mut r = self.clone();
        let dropped: Vec<String> = r.packages.iter().filter(|p| reserved.contains_key(&crate::libraries::python_dist_name(p))).cloned().collect();
        if dropped.is_empty() {
            return r;
        }
        r.packages.retain(|p| !reserved.contains_key(&crate::libraries::python_dist_name(p)));
        for name in dropped.iter().map(|p| crate::libraries::python_dist_name(p)) {
            r.fallbacks.remove(&name);
            if let Some(why) = reserved.get(&name) {
                r.note.push_str(&format!(" Not from the roster on this engine: {name} ({why})."));
            }
        }
        r
    }
}

/// Evaluate `python_version <op> 'X.Y'` for a Python (major, minor); false when the marker has
/// another shape or the Python is unknown.
pub fn marker_applies(marker: &str, python: Option<(u32, u32)>) -> bool {
    let Some((major, minor)) = python else { return false };
    let rest = marker.trim().strip_prefix("python_version").map(str::trim);
    let Some(rest) = rest else { return false };
    let found = ["<=", ">=", "==", "!=", "<", ">"].iter().find_map(|op| rest.strip_prefix(op).map(|v| (*op, v.trim().trim_matches(|c| c == '\'' || c == '"'))));
    let Some((op, ver)) = found else { return false };
    let want = version_tuple(ver);
    if want.len() < 2 {
        return false;
    }
    let have = vec![major, minor];
    let want = want[..2].to_vec();
    match op {
        "<" => have < want,
        "<=" => have <= want,
        ">" => have > want,
        ">=" => have >= want,
        "==" => have == want,
        "!=" => have != want,
        _ => false,
    }
}

/// Does an installed version satisfy a pip specifier set such as `scipy>=1.15,<1.18`?
/// Numeric comparison on the first three components; unknown operators fail closed.
pub fn spec_satisfied(have: &str, spec: &str) -> bool {
    let have_t = version_tuple(have);
    let body = spec.trim();
    let start = body.find(['<', '>', '=', '!', '~']).unwrap_or(body.len());
    let clauses = body[start..].split(',').map(str::trim).filter(|c| !c.is_empty());
    let mut any = false;
    for c in clauses {
        any = true;
        let (op, ver) = ["<=", ">=", "==", "!=", "~=", "<", ">"].iter().find_map(|op| c.strip_prefix(op).map(|v| (*op, v.trim()))).unwrap_or(("", c));
        let want = version_tuple(ver);
        let ok = match op {
            "<" => have_t < want,
            "<=" => have_t <= want,
            ">" => have_t > want,
            ">=" | "~=" => have_t >= want,
            "==" => have_t == want,
            "!=" => have_t != want,
            _ => false,
        };
        if !ok {
            return false;
        }
    }
    any
}

/// Where a profile's package roster was taken from (`python_packages_source`).
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Default)]
#[serde(default)]
pub struct PackageSource {
    pub repo: String,
    pub commit: String,
    pub date: String,
    pub file: String,
    pub url: String,
}

impl Default for SailPins {
    fn default() -> Self {
        Self { version: "0.7.2".into(), pyspark_client: "4.1.3".into(), python: "3.13".into(), python_windows: "3.11".into(), packages: vec!["ipython>=8.18".into(), "pandas>=2.0,<3".into(), "pyarrow>=15".into(), "protobuf>=6.33,<7".into()], reserved: BTreeMap::new() }
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
    /// Fabric's notebook-facing Python packages at the runtime's versions (name → version),
    /// curated by local-spark-mcp from Microsoft's `synapse-spark-runtime` environment file.
    #[serde(default)]
    pub python_packages: BTreeMap<String, String>,
    #[serde(default)]
    pub python_packages_source: Option<PackageSource>,
    /// Pins that cannot install on every Python, with the requirement to use instead.
    #[serde(default)]
    pub python_packages_fallbacks: BTreeMap<String, Fallback>,
}

impl Profile {
    /// The profile's Fabric package roster as pip requirements, or None when the manifest has
    /// no packages for it.
    pub fn roster(&self, name: &str) -> Option<Roster> {
        if self.python_packages.is_empty() {
            return None;
        }
        let src = self.python_packages_source.clone().unwrap_or_default();
        let commit = src.commit.chars().take(7).collect::<String>();
        Some(Roster {
            profile: name.to_string(),
            fabric_runtime: self.fabric_runtime.clone(),
            source: if src.url.is_empty() { src.repo.clone() } else { src.url.clone() },
            note: format!(
                "The Python packages Fabric Runtime {} ships that a notebook can import, at Fabric's versions, as listed in Microsoft's synapse-spark-runtime repository ({}{}). Left out: pyspark and delta-spark (the engine's own), notebookutils, synapseml and semantic-link (Fabric-only wheels), the torch stack and packages with no PyPI release.",
                self.fabric_runtime,
                if src.file.is_empty() { "environment file".to_string() } else { src.file.clone() },
                if commit.is_empty() { String::new() } else { format!(", commit {commit}, {}", src.date) }
            ),
            packages: self.python_packages.iter().map(|(n, v)| format!("{n}=={v}")).collect(),
            fallbacks: self.python_packages_fallbacks.clone(),
        })
    }
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
    /// Cobalt's manifest with local-spark-mcp's profiles folded in: the profiles and the
    /// exclusion list come from the package's own manifest unless `manifest.json` carries its own.
    pub fn embedded() -> Self {
        let mut m: Manifest = serde_json::from_str(EMBEDDED).expect("embedded manifest parses");
        let lsm = LsmManifest::embedded();
        if m.profiles.is_empty() {
            m.profiles = lsm.profiles;
        }
        if m.python_packages_excluded.is_empty() {
            m.python_packages_excluded = lsm.python_packages_excluded;
        }
        m
    }

    pub fn profile(&self, name: &str) -> Result<&Profile> {
        self.profiles.get(name).ok_or_else(|| RuntimeError::Manifest(format!("unknown runtime profile {name:?}; known: {}", self.profiles.keys().cloned().collect::<Vec<_>>().join(", "))))
    }

    /// The Fabric package rosters, by profile name (only profiles that carry packages).
    pub fn rosters(&self) -> BTreeMap<String, Roster> {
        self.profiles.iter().filter_map(|(n, p)| p.roster(n).map(|r| (n.clone(), r))).collect()
    }

    /// The Fabric package roster of a profile (`none` or an unknown name → None).
    pub fn roster(&self, name: &str) -> Option<Roster> {
        self.profiles.get(name).and_then(|p| p.roster(name))
    }

    /// The roster as installed on an engine: LakeSail leaves out the packages it owns
    /// (`sail.reserved`), Local Spark takes it whole (local-spark-mcp validated it).
    pub fn engine_roster(&self, engine: Engine, name: &str) -> Option<Roster> {
        let r = self.roster(name)?;
        Some(if engine.is_sail() { r.without(&self.sail.reserved) } else { r })
    }

    /// Every roster as an engine installs it.
    pub fn engine_rosters(&self, engine: Engine) -> BTreeMap<String, Roster> {
        self.rosters().into_keys().filter_map(|n| self.engine_roster(engine, &n).map(|r| (n, r))).collect()
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

    /// The base package without a profile extra (no pyspark, no delta-spark): what the LakeSail
    /// environment installs for the pure-Python halves (lazy Files, the notebookutils shim, the
    /// host credential).
    pub fn base_requirement(&self) -> String {
        let src = &self.local_spark_mcp.source;
        if src.starts_with("http") || src.starts_with("git+") {
            format!("local-spark-mcp @ {src}")
        } else {
            format!("local-spark-mcp=={}", self.local_spark_mcp.version)
        }
    }

    /// Everything `uv pip install` gets for the LakeSail environment: Sail's own pins plus the
    /// base local-spark-mcp package.
    pub fn sail_requirements(&self) -> Vec<String> {
        let mut v = self.sail.requirements();
        v.push(self.base_requirement());
        v
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
    fn local_spark_mcp_profiles_follow_the_pin() {
        let m = Manifest::embedded();
        let lsm = LsmManifest::embedded();
        // the copied profiles.json is the pinned release's
        assert_eq!(lsm.version, m.local_spark_mcp.version, "copy profiles.json from the pinned local-spark-mcp tag");
        assert!(m.local_spark_mcp.source.contains(&format!("v{}", m.local_spark_mcp.version)));
        assert_eq!(lsm.protocol_version, Some(2));
        assert!(m.profiles.contains_key("fabric-1.3") && m.profiles.contains_key("fabric-2.0"));
        // every profile carries a roster at Fabric's versions, with its source
        let rosters = m.rosters();
        assert_eq!(rosters.len(), 2);
        let r = m.roster("fabric-2.0").unwrap();
        assert!(r.packages.len() >= 50, "{}", r.packages.len());
        assert!(r.packages.contains(&"pandas==2.3.3".to_string()));
        assert!(r.packages.iter().all(|p| p.contains("==")));
        assert!(r.source.starts_with("https://github.com/microsoft/synapse-spark-runtime/blob/"));
        assert!(r.note.contains("Fabric Runtime 2.0"));
        assert_eq!(r.fabric_runtime, "2.0");
        assert!(m.roster("fabric-1.3").unwrap().packages.contains(&"pandas==2.1.4".to_string()));
        assert!(m.roster("none").is_none());
        // the engine's own pins never come through the roster
        for r in rosters.values() {
            assert!(!r.packages.iter().any(|p| p.starts_with("pyspark==") || p.starts_with("delta-spark==") || p.starts_with("notebookutils")));
        }
        assert!(m.python_packages_excluded.contains_key("pyspark"));
        // 0.8.1: the per-platform fallback is data; a 3.11 environment gets the alternative
        let fb = r.fallbacks.get("scipy").expect("scipy fallback on fabric-2.0");
        assert_eq!(fb.marker, "python_version < '3.12'");
        let on_311 = r.requirements_for(Some((3, 11)));
        assert!(on_311.contains(&fb.requirement) && !on_311.contains(&"scipy==1.18.0".to_string()));
        let on_313 = r.requirements_for(Some((3, 13)));
        assert!(on_313.contains(&"scipy==1.18.0".to_string()));
        assert_eq!(on_311.len(), r.packages.len());
        assert!(r.fallback_for("scipy", Some((3, 11))).is_some() && r.fallback_for("scipy", Some((3, 12))).is_none() && r.fallback_for("scipy", None).is_none());
        // LakeSail keeps its own protobuf line: the roster's pin would break the Connect client
        assert!(m.sail.reserved.contains_key("protobuf"));
        assert!(m.sail.packages.iter().any(|p| p.starts_with("protobuf>=6.33")));
        let sail = m.engine_roster(Engine::Sail, "fabric-2.0").unwrap();
        assert_eq!(sail.packages.len(), r.packages.len() - 1);
        assert!(!sail.packages.iter().any(|p| p.starts_with("protobuf==")) && sail.note.contains("protobuf"));
        assert_eq!(m.engine_roster(Engine::PySpark, "fabric-2.0").unwrap().packages.len(), r.packages.len());
        assert_eq!(m.engine_rosters(Engine::Sail).len(), 2);
    }

    #[test]
    fn markers_and_specs() {
        assert!(marker_applies("python_version < '3.12'", Some((3, 11))));
        assert!(!marker_applies("python_version < '3.12'", Some((3, 12))));
        assert!(marker_applies("python_version >= \"3.12\"", Some((3, 13))));
        assert!(marker_applies("python_version == '3.11'", Some((3, 11))));
        assert!(!marker_applies("python_version < '3.12'", None));
        assert!(!marker_applies("sys_platform == 'win32'", Some((3, 11))));
        assert!(spec_satisfied("1.17.1", "scipy>=1.15,<1.18"));
        assert!(!spec_satisfied("1.18.0", "scipy>=1.15,<1.18"));
        assert!(!spec_satisfied("1.14.0", "scipy>=1.15,<1.18"));
        assert!(spec_satisfied("2.3.3", "pandas==2.3.3"));
        assert!(!spec_satisfied("2.3.3", "pandas"));
        assert!(spec_satisfied("1.2.3", ">=1.2"));
    }

    #[test]
    fn versions() {
        assert_eq!(version_tuple("uv 0.12.23 (abc)"), vec![0, 12, 23]);
        assert!(version_tuple("0.7.3") < version_tuple("0.12.0"));
        assert_eq!(version_tuple("21.0.8+9"), vec![21, 0, 8]);
    }
}
