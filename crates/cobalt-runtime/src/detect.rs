//! Find what the machine already has: JDKs (JAVA_HOME, PATH, vfox, vendor folders) and uv.

use crate::manifest::{version_tuple, Profile};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JdkCandidate {
    pub home: PathBuf,
    pub major: Option<u32>,
    /// Where it was found: "JAVA_HOME", "PATH", "vfox", "Program Files", "managed"…
    pub source: String,
    pub vendor: Option<String>,
}

impl JdkCandidate {
    pub fn label(&self) -> String {
        let v = self.major.map(|m| format!("Java {m}")).unwrap_or_else(|| "Java ?".into());
        match &self.vendor {
            Some(vendor) => format!("{v} ({vendor}) — {}", self.home.display()),
            None => format!("{v} — {}", self.home.display()),
        }
    }
}

fn launcher(home: &Path) -> Option<PathBuf> {
    for name in ["java", "java.exe"] {
        let p = home.join("bin").join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// `realpath`, `.../bin/java[.exe]` → the home, and macOS bundles → `Contents/Home`.
pub fn normalize_home(path: &Path) -> PathBuf {
    let p = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let p = if p.file_name().map(|n| n.eq_ignore_ascii_case("java") || n.eq_ignore_ascii_case("java.exe")).unwrap_or(false) && p.parent().and_then(|b| b.file_name()).map(|n| n == "bin").unwrap_or(false) {
        p.parent().and_then(|b| b.parent()).map(Path::to_path_buf).unwrap_or(p)
    } else {
        p
    };
    let contents = p.join("Contents").join("Home");
    if launcher(&p).is_none() && launcher(&contents).is_some() {
        return contents;
    }
    // Windows: strip the \\?\ prefix canonicalize adds, Java handles plain paths better
    #[cfg(windows)]
    {
        let s = p.to_string_lossy();
        if let Some(rest) = s.strip_prefix(r"\\?\") {
            return PathBuf::from(rest);
        }
    }
    p
}

/// `(major, vendor)` from the JDK's `release` file.
pub fn release_info(home: &Path) -> (Option<u32>, Option<String>) {
    let Ok(text) = std::fs::read_to_string(home.join("release")) else { return (None, None) };
    let mut major = None;
    let mut vendor = None;
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("JAVA_VERSION=") {
            let v = v.trim_matches('"');
            let parts = version_tuple(v);
            major = match parts.as_slice() {
                [1, m, ..] => Some(*m),
                [m, ..] => Some(*m),
                _ => None,
            };
        } else if let Some(v) = line.strip_prefix("IMPLEMENTOR=") {
            vendor = Some(v.trim_matches('"').to_string());
        }
    }
    (major, vendor)
}

/// A usable JDK at `path`, or why not.
pub fn check_jdk(path: &Path, source: &str) -> std::result::Result<JdkCandidate, String> {
    let home = normalize_home(path);
    if !home.is_dir() {
        return Err(format!("{} does not exist", path.display()));
    }
    if launcher(&home).is_none() {
        return Err(format!("{} is not a JDK (no bin/java)", home.display()));
    }
    let (major, vendor) = release_info(&home);
    if vendor.as_deref().map(|v| v.to_ascii_lowercase().contains("oracle")).unwrap_or(false) {
        return Err(format!("{} is an Oracle JDK; Cobalt does not use Oracle builds", home.display()));
    }
    Ok(JdkCandidate { home, major, source: source.to_string(), vendor })
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from)
}

fn glob_dirs(parent: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(parent).map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect()).unwrap_or_default()
}

/// Every JDK the machine offers, best first for `profile` (accepted majors only when a profile
/// is given). Managed JDKs under `managed_dir` are included with source "managed".
pub fn find_jdks(profile: Option<&Profile>, managed_dir: Option<&Path>) -> Vec<JdkCandidate> {
    let mut found: Vec<JdkCandidate> = Vec::new();
    let mut push = |cand: std::result::Result<JdkCandidate, String>| {
        if let Ok(c) = cand {
            if !found.iter().any(|f| f.home == c.home) {
                found.push(c);
            }
        }
    };
    if let Some(dir) = managed_dir {
        for vendor_dir in glob_dirs(dir) {
            push(check_jdk(&vendor_dir, "managed"));
            for inner in glob_dirs(&vendor_dir) {
                push(check_jdk(&inner, "managed"));
            }
        }
    }
    if let Some(jh) = std::env::var_os("JAVA_HOME") {
        push(check_jdk(Path::new(&jh), "JAVA_HOME"));
    }
    if let Some(exe) = which("java") {
        push(check_jdk(&exe, "PATH"));
    }
    if let Some(home) = home_dir() {
        let vfox = home.join(".version-fox").join("cache").join("java");
        for v in glob_dirs(&vfox) {
            push(check_jdk(&v, "vfox"));
            for inner in glob_dirs(&v) {
                push(check_jdk(&inner, "vfox"));
            }
        }
        // SDKMAN, jabba, and ~/.jdks (IntelliJ)
        for sub in [".sdkman/candidates/java", ".jdks", ".jabba/jdk"] {
            for v in glob_dirs(&home.join(sub)) {
                push(check_jdk(&v, sub));
            }
        }
    }
    #[cfg(windows)]
    {
        for pf in ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"] {
            if let Some(root) = std::env::var_os(pf) {
                let root = PathBuf::from(root);
                for vendor in ["Microsoft", "Eclipse Adoptium", "Eclipse Foundation", "Amazon Corretto", "Zulu", "OpenJDK", "Java", "BellSoft", "Azul"] {
                    for v in glob_dirs(&root.join(vendor)) {
                        push(check_jdk(&v, "Program Files"));
                    }
                }
            }
        }
    }
    #[cfg(target_os = "linux")]
    {
        for v in glob_dirs(Path::new("/usr/lib/jvm")) {
            push(check_jdk(&v, "/usr/lib/jvm"));
        }
    }
    #[cfg(target_os = "macos")]
    {
        for v in glob_dirs(Path::new("/Library/Java/JavaVirtualMachines")) {
            push(check_jdk(&v, "JavaVirtualMachines"));
        }
        for v in glob_dirs(Path::new("/opt/homebrew/opt")) {
            if v.file_name().map(|n| n.to_string_lossy().starts_with("openjdk")).unwrap_or(false) {
                push(check_jdk(&v.join("libexec").join("openjdk.jdk"), "homebrew"));
            }
        }
    }
    if let Some(p) = profile {
        found.retain(|c| c.major.map(|m| p.accepts_java(m)).unwrap_or(false));
        found.sort_by_key(|c| (c.major.and_then(|m| p.java_rank(m)).unwrap_or(usize::MAX), c.source != "managed"));
    }
    found
}

/// First match of an executable on PATH.
pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let exts: Vec<String> = if cfg!(windows) { vec![".exe".into(), ".cmd".into(), ".bat".into(), String::new()] } else { vec![String::new()] };
    for dir in std::env::split_paths(&path) {
        for ext in &exts {
            let p = dir.join(format!("{name}{ext}"));
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// `uv --version` → "0.12.23".
pub fn uv_version(exe: &Path) -> Option<String> {
    let out = Command::new(exe).arg("--version").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    s.split_whitespace().nth(1).map(str::to_string)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UvCandidate {
    pub exe: PathBuf,
    pub version: String,
    pub source: String,
}

/// The managed uv if present, else one on PATH that is at least `min_adopt`.
pub fn find_uv(managed_exe: &Path, min_adopt: &str) -> Option<UvCandidate> {
    if managed_exe.is_file() {
        if let Some(v) = uv_version(managed_exe) {
            return Some(UvCandidate { exe: managed_exe.to_path_buf(), version: v, source: "managed".into() });
        }
    }
    let exe = which("uv")?;
    let version = uv_version(&exe)?;
    if version_tuple(&version) >= version_tuple(min_adopt) {
        Some(UvCandidate { exe, version, source: "PATH".into() })
    } else {
        None
    }
}

/// Version of the interpreter inside a venv, from `pyvenv.cfg`.
pub fn venv_python_version(env_dir: &Path) -> Option<String> {
    let cfg = std::fs::read_to_string(env_dir.join("pyvenv.cfg")).ok()?;
    cfg.lines().find_map(|l| {
        let (k, v) = l.split_once('=')?;
        let k = k.trim();
        (k == "version_info" || k == "version").then(|| v.trim().to_string())
    })
}

/// Installed `local-spark-mcp` version inside a venv (from the dist-info folder name).
pub fn installed_package_version(env_dir: &Path, dist: &str) -> Option<String> {
    let site = if cfg!(windows) {
        vec![env_dir.join("Lib").join("site-packages")]
    } else {
        glob_dirs(&env_dir.join("lib")).into_iter().map(|p| p.join("site-packages")).collect()
    };
    let needle = format!("{}-", dist.replace('-', "_"));
    for sp in site {
        for d in glob_dirs(&sp) {
            let name = d.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            if let Some(rest) = name.strip_prefix(&needle) {
                if let Some(v) = rest.strip_suffix(".dist-info") {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_jdk(dir: &Path, version: &str, implementor: &str) {
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(dir.join("bin").join(if cfg!(windows) { "java.exe" } else { "java" }), b"").unwrap();
        std::fs::write(dir.join("release"), format!("IMPLEMENTOR=\"{implementor}\"\nJAVA_VERSION=\"{version}\"\n")).unwrap();
    }

    #[test]
    fn reads_release_file_and_rejects_oracle() {
        let tmp = std::env::temp_dir().join(format!("cobalt-rt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        fake_jdk(&tmp.join("ms21"), "21.0.8", "Microsoft");
        fake_jdk(&tmp.join("j8"), "1.8.0_392", "Temurin");
        fake_jdk(&tmp.join("oracle"), "21.0.1", "Oracle Corporation");
        let c = check_jdk(&tmp.join("ms21"), "test").unwrap();
        assert_eq!(c.major, Some(21));
        assert_eq!(c.vendor.as_deref(), Some("Microsoft"));
        assert_eq!(check_jdk(&tmp.join("j8"), "test").unwrap().major, Some(8));
        assert!(check_jdk(&tmp.join("oracle"), "test").unwrap_err().contains("Oracle"));
        assert!(check_jdk(&tmp.join("nope"), "test").is_err());
        // the launcher path normalizes to the home
        let via_bin = check_jdk(&tmp.join("ms21").join("bin").join(if cfg!(windows) { "java.exe" } else { "java" }), "PATH").unwrap();
        assert_eq!(via_bin.home, c.home);
        // managed-dir scan ranks by the profile's preference
        let m = crate::Manifest::embedded();
        let p = m.profile("fabric-2.0").unwrap();
        fake_jdk(&tmp.join("managed").join("microsoft-17").join("jdk-17"), "17.0.16", "Microsoft");
        let found = find_jdks(Some(p), Some(&tmp.join("managed")));
        assert!(found.iter().any(|c| c.major == Some(17) && c.source == "managed"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn venv_cfg() {
        let tmp = std::env::temp_dir().join(format!("cobalt-venv-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("pyvenv.cfg"), "home = x\nversion_info = 3.11.9\n").unwrap();
        assert_eq!(venv_python_version(&tmp).as_deref(), Some("3.11.9"));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
