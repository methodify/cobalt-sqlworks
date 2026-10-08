//! User libraries for the Spark environment: Python packages (PyPI requirement specs or wheel
//! files) installed into the profile's virtual environment with uv, and Java libraries (jar
//! files, or Maven coordinates fetched from Maven Central into the runtime folder) that go on the
//! Spark classpath when a session starts.

use crate::detect;
use crate::install::{run_tool, uv_env, Context, Progress, Step};
use crate::{Result, RuntimeDirs, RuntimeError};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Libraries {
    /// PyPI requirement specs (`polars==1.9`, `dwlib`) or paths to wheels / sdists.
    pub python: Vec<String>,
    /// Jar files on this machine.
    pub jars: Vec<PathBuf>,
    /// Maven coordinates `group:artifact:version` (the artifact's own jar; no transitive deps).
    pub maven: Vec<String>,
}

impl Libraries {
    pub fn is_empty(&self) -> bool {
        self.python.is_empty() && self.jars.is_empty() && self.maven.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MavenCoord {
    pub group: String,
    pub artifact: String,
    pub version: String,
}

impl MavenCoord {
    /// `group:artifact:version`.
    pub fn parse(s: &str) -> Option<Self> {
        let parts: Vec<&str> = s.trim().split(':').collect();
        if parts.len() != 3 || parts.iter().any(|p| p.is_empty() || p.contains('/') || p.contains(char::is_whitespace)) {
            return None;
        }
        Some(Self { group: parts[0].into(), artifact: parts[1].into(), version: parts[2].into() })
    }
    pub fn url(&self) -> String {
        format!("https://repo1.maven.org/maven2/{}/{}/{}/{}-{}.jar", self.group.replace('.', "/"), self.artifact, self.version, self.artifact, self.version)
    }
    pub fn file_name(&self) -> String {
        format!("{}_{}-{}.jar", self.group, self.artifact, self.version)
    }
}

pub fn maven_jar_path(dirs: &RuntimeDirs, c: &MavenCoord) -> PathBuf {
    dirs.root.join("jars").join(c.file_name())
}

/// What the worker's `init` gets: jar files that exist (`extra_jars`), Maven coordinates that
/// parse (`extra_packages`, resolved by Ivy with their dependencies at session start), and the
/// entries that are skipped with the reason.
pub struct SessionLibraries {
    pub jars: Vec<PathBuf>,
    pub packages: Vec<String>,
    pub skipped: Vec<String>,
}

pub fn for_session(libs: &Libraries) -> SessionLibraries {
    let mut out = SessionLibraries { jars: Vec::new(), packages: Vec::new(), skipped: Vec::new() };
    for j in &libs.jars {
        if j.is_file() {
            out.jars.push(j.clone());
        } else {
            out.skipped.push(format!("{} (file not found)", j.display()));
        }
    }
    for m in &libs.maven {
        if MavenCoord::parse(m).is_some() {
            out.packages.push(m.trim().to_string());
        } else {
            out.skipped.push(format!("{m} (not a group:artifact:version coordinate)"));
        }
    }
    out
}

/// Kept for callers that still want a classpath string (local[*] driver and executors share it).
pub fn classpath(_dirs: &RuntimeDirs, libs: &Libraries) -> (Vec<PathBuf>, Vec<String>) {
    let s = for_session(libs);
    (s.jars, s.skipped)
}

/// Classpath string for `spark.driver.extraClassPath` on this platform.
pub fn classpath_string(jars: &[PathBuf]) -> String {
    let sep = if cfg!(windows) { ";" } else { ":" };
    jars.iter().map(|p| p.to_string_lossy().to_string()).collect::<Vec<_>>().join(sep)
}

/// The distribution name a Python spec refers to (`polars==1.9` → `polars`, `C:\x\dwlib-0.3-py3-none-any.whl` → `dwlib`).
pub fn python_dist_name(spec: &str) -> String {
    let s = spec.trim();
    let looks_like_path = s.ends_with(".whl") || s.ends_with(".tar.gz") || s.ends_with(".zip") || s.contains('\\') || s.contains('/');
    if looks_like_path {
        // a Windows path may be typed on any platform, so both separators count
        let file = s.rsplit(['/', '\\']).next().unwrap_or(s);
        return file.split('-').next().unwrap_or("").replace('_', "-").to_ascii_lowercase();
    }
    let end = s.find(['=', '<', '>', '!', '~', '[', ';', '@', ' ']).unwrap_or(s.len());
    s[..end].trim().replace('_', "-").to_ascii_lowercase()
}

/// Installed version per spec (None = not installed), from the environment's dist-info folders.
pub fn python_status(env_dir: &Path, specs: &[String]) -> Vec<(String, Option<String>)> {
    specs.iter().map(|s| (s.clone(), detect::installed_package_version(env_dir, &python_dist_name(s)))).collect()
}

/// Install the Python specs with uv into the profile's environment. Jars and Maven packages need
/// no install step: the worker takes them at session start (`extra_jars` / `extra_packages`,
/// Ivy resolves the packages). Missing jar files are reported, not fatal.
pub fn install(cx: &Context, profile: &str, libs: &Libraries) -> Result<String> {
    let mut notes = Vec::new();
    if !libs.python.is_empty() {
        (cx.progress)(Progress::Step { step: Step::Libraries, label: format!("Installing {} Python package{}", libs.python.len(), if libs.python.len() == 1 { "" } else { "s" }) });
        let uv = detect::find_uv(&cx.dirs.uv_exe(), &cx.manifest.uv.min_adopt).map(|c| c.exe).ok_or_else(|| RuntimeError::Manifest("uv is not installed — install the runtime first".into()))?;
        let py = cx.dirs.env_python(profile);
        if !py.is_file() {
            return Err(RuntimeError::Manifest(format!("the {profile} environment is not installed — install the runtime first")));
        }
        let py_s = py.to_string_lossy().to_string();
        let mut args: Vec<&str> = vec!["pip", "install", "--python", &py_s];
        let specs: Vec<String> = libs.python.iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
        for s in &specs {
            args.push(s);
        }
        run_tool(cx, &uv, &args, &uv_env(cx.dirs))?;
        notes.push(format!("{} Python package{} installed", specs.len(), if specs.len() == 1 { "" } else { "s" }));
    }
    for m in &libs.maven {
        if MavenCoord::parse(m).is_none() {
            notes.push(format!("skipped {m}: not a group:artifact:version coordinate"));
        }
    }
    let n_pk = libs.maven.iter().filter(|m| MavenCoord::parse(m).is_some()).count();
    if n_pk > 0 {
        notes.push(format!("{n_pk} Maven package{} will be resolved by the session (with dependencies)", if n_pk == 1 { "" } else { "s" }));
    }
    for j in &libs.jars {
        if !j.is_file() {
            notes.push(format!("jar not found: {}", j.display()));
        }
    }
    if notes.is_empty() {
        notes.push("nothing to do".into());
    }
    Ok(notes.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maven_coords() {
        let c = MavenCoord::parse("org.postgresql:postgresql:42.7.3").unwrap();
        assert_eq!(c.url(), "https://repo1.maven.org/maven2/org/postgresql/postgresql/42.7.3/postgresql-42.7.3.jar");
        assert_eq!(c.file_name(), "org.postgresql_postgresql-42.7.3.jar");
        assert!(MavenCoord::parse("org.postgresql:postgresql").is_none());
        assert!(MavenCoord::parse("a:b:c:d").is_none());
    }

    #[test]
    fn dist_names() {
        assert_eq!(python_dist_name("polars==1.9.0"), "polars");
        assert_eq!(python_dist_name("Dw_Lib>=0.3 ; python_version>'3'"), "dw-lib");
        assert_eq!(python_dist_name(r"C:\libs\dwlib-0.3.1-py3-none-any.whl"), "dwlib");
        assert_eq!(python_dist_name("/tmp/my_pkg-1.0.tar.gz"), "my-pkg");
    }

    #[test]
    fn session_libraries_split() {
        let libs = Libraries { python: vec![], jars: vec![PathBuf::from("Z:/nope.jar")], maven: vec!["a:b:1".into(), "bad".into()] };
        let s = for_session(&libs);
        assert!(s.jars.is_empty());
        assert_eq!(s.packages, vec!["a:b:1".to_string()]);
        assert_eq!(s.skipped.len(), 2);
        assert!(classpath_string(&[PathBuf::from("a.jar"), PathBuf::from("b.jar")]).contains("a.jar"));
    }
}
