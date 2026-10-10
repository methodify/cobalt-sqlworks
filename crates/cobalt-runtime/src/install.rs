//! Provisioning: hash-checked resumable downloads, archive extraction, the uv steps (Python,
//! environment, package), the JDK, and the warm-up smoke test through the worker.

use crate::detect;
use crate::manifest::{Engine, JdkVendor, Manifest, Platform, Profile};
use crate::status::Installed;
use crate::worker::{Worker, WorkerConfig};
use crate::{Result, RuntimeDirs, RuntimeError};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Step {
    Uv,
    Python,
    Env,
    Jdk,
    Warm,
    /// User libraries (Python packages, Maven jars).
    Libraries,
}

impl Step {
    pub const ALL: [Step; 5] = [Step::Uv, Step::Python, Step::Env, Step::Jdk, Step::Warm];
    pub fn label(self) -> &'static str {
        match self {
            Step::Uv => "uv",
            Step::Python => "Python",
            Step::Env => "Spark environment",
            Step::Jdk => "Java",
            Step::Warm => "First Spark session",
            Step::Libraries => "Libraries",
        }
    }
}

#[derive(Clone, Debug)]
pub enum Progress {
    Step { step: Step, label: String },
    /// Download progress for the current step.
    Bytes { done: u64, total: Option<u64> },
    Log(String),
}

/// What to provision.
#[derive(Clone, Debug)]
pub struct Plan {
    /// Which engine's runtime: the JVM profile below, or the LakeSail environment.
    pub engine: Engine,
    /// LakeSail: the Fabric package roster to install (`none` for none).
    pub sail_profile: String,
    /// Local Spark: install the profile's own Fabric package roster into its environment.
    pub profile_packages: bool,
    pub profile: String,
    pub jdk_vendor: JdkVendor,
    /// Adopt this JDK instead of downloading one.
    pub adopt_jdk: Option<PathBuf>,
    /// Skip steps that are already satisfied.
    pub steps: Vec<Step>,
    pub driver_memory: String,
}

pub struct Context<'a> {
    pub dirs: &'a RuntimeDirs,
    pub manifest: &'a Manifest,
    pub progress: &'a (dyn Fn(Progress) + Sync),
    pub cancel: &'a AtomicBool,
}

impl Context<'_> {
    fn log(&self, s: impl Into<String>) {
        let s = s.into();
        tracing::info!(target: "cobalt_runtime", "{s}");
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(self.dirs.log_file()) {
            let _ = writeln!(f, "{s}");
        }
        (self.progress)(Progress::Log(s));
    }
    fn check_cancel(&self) -> Result<()> {
        if self.cancel.load(Ordering::Relaxed) {
            Err(RuntimeError::Cancelled)
        } else {
            Ok(())
        }
    }
}

fn client() -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder().user_agent(concat!("cobalt-sqlworks/", env!("CARGO_PKG_VERSION"))).timeout(Duration::from_secs(60 * 30)).connect_timeout(Duration::from_secs(30)).build().map_err(|e| RuntimeError::Http(e.to_string()))
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(format!("{:x}", h.finalize()))
}

/// Download `url` to `dest`, resuming a partial `.part` file, then verify the SHA-256 when
/// given. Already-complete files with the right hash are kept.
pub fn download(cx: &Context, url: &str, dest: &Path, expected_sha256: Option<&str>) -> Result<()> {
    if dest.is_file() {
        match expected_sha256 {
            Some(exp) if sha256_file(dest)?.eq_ignore_ascii_case(exp) => {
                cx.log(format!("{} already downloaded", dest.file_name().unwrap_or_default().to_string_lossy()));
                return Ok(());
            }
            _ => {
                let _ = std::fs::remove_file(dest);
            }
        }
    }
    std::fs::create_dir_all(dest.parent().unwrap_or(Path::new(".")))?;
    let part = dest.with_extension(format!("{}.part", dest.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default()));
    let have = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    let client = client()?;
    let mut req = client.get(url);
    if have > 0 {
        req = req.header(reqwest::header::RANGE, format!("bytes={have}-"));
    }
    let resp = req.send().map_err(|e| RuntimeError::Http(format!("{url}: {e}")))?;
    let status = resp.status();
    let (mut file, mut done) = if status == reqwest::StatusCode::PARTIAL_CONTENT && have > 0 {
        (std::fs::OpenOptions::new().append(true).open(&part)?, have)
    } else if status.is_success() {
        (std::fs::File::create(&part)?, 0)
    } else {
        return Err(RuntimeError::Http(format!("{url}: HTTP {status}")));
    };
    let total = resp.content_length().map(|l| l + done);
    cx.log(format!("downloading {url}{}", total.map(|t| format!(" ({})", crate::fmt_bytes(t))).unwrap_or_default()));
    let mut resp = resp;
    let mut buf = vec![0u8; 256 * 1024];
    let mut last_report = std::time::Instant::now();
    loop {
        cx.check_cancel()?;
        let n = resp.read(&mut buf).map_err(|e| RuntimeError::Http(format!("{url}: {e}")))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        done += n as u64;
        if last_report.elapsed() > Duration::from_millis(100) {
            (cx.progress)(Progress::Bytes { done, total });
            last_report = std::time::Instant::now();
        }
    }
    file.flush()?;
    drop(file);
    (cx.progress)(Progress::Bytes { done, total });
    if let Some(exp) = expected_sha256 {
        let got = sha256_file(&part)?;
        if !got.eq_ignore_ascii_case(exp) {
            let _ = std::fs::remove_file(&part);
            return Err(RuntimeError::Checksum { name: dest.file_name().unwrap_or_default().to_string_lossy().to_string(), expected: exp.to_string(), got });
        }
    }
    std::fs::rename(&part, dest)?;
    Ok(())
}

/// Extract a `.zip` or `.tar.gz` into `into` (created). Returns the single top-level directory
/// when the archive has one, else `into`.
pub fn extract(cx: &Context, archive: &Path, into: &Path) -> Result<PathBuf> {
    cx.check_cancel()?;
    if into.exists() {
        std::fs::remove_dir_all(into)?;
    }
    std::fs::create_dir_all(into)?;
    let name = archive.file_name().unwrap_or_default().to_string_lossy().to_string();
    cx.log(format!("extracting {name}"));
    if name.ends_with(".zip") {
        let f = std::fs::File::open(archive)?;
        let mut z = zip::ZipArchive::new(f).map_err(|e| RuntimeError::Http(format!("{name}: {e}")))?;
        for i in 0..z.len() {
            if i % 200 == 0 {
                cx.check_cancel()?;
            }
            let mut entry = z.by_index(i).map_err(|e| RuntimeError::Http(format!("{name}: {e}")))?;
            let Some(rel) = entry.enclosed_name() else { continue };
            let out = into.join(rel);
            if entry.is_dir() {
                std::fs::create_dir_all(&out)?;
                continue;
            }
            if let Some(p) = out.parent() {
                std::fs::create_dir_all(p)?;
            }
            let mut w = std::fs::File::create(&out)?;
            std::io::copy(&mut entry, &mut w)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Some(mode) = entry.unix_mode() {
                    let _ = std::fs::set_permissions(&out, std::fs::Permissions::from_mode(mode));
                }
            }
        }
    } else if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        let f = std::fs::File::open(archive)?;
        let gz = flate2::read::GzDecoder::new(f);
        let mut tar = tar::Archive::new(gz);
        tar.set_preserve_permissions(true);
        tar.unpack(into)?;
    } else {
        return Err(RuntimeError::Manifest(format!("unknown archive type: {name}")));
    }
    let entries: Vec<PathBuf> = std::fs::read_dir(into)?.flatten().map(|e| e.path()).collect();
    if entries.len() == 1 && entries[0].is_dir() {
        Ok(entries[0].clone())
    } else {
        Ok(into.to_path_buf())
    }
}

/// Find a file by name anywhere under `root` (shallow-first).
fn find_file(root: &Path, names: &[&str], depth: usize) -> Option<PathBuf> {
    for n in names {
        let p = root.join(n);
        if p.is_file() {
            return Some(p);
        }
    }
    if depth == 0 {
        return None;
    }
    for e in std::fs::read_dir(root).ok()?.flatten() {
        let p = e.path();
        if p.is_dir() {
            if let Some(f) = find_file(&p, names, depth - 1) {
                return Some(f);
            }
        }
    }
    None
}

pub(crate) fn uv_env(dirs: &RuntimeDirs) -> Vec<(String, String)> {
    vec![
        ("UV_PYTHON_INSTALL_DIR".into(), dirs.python_dir().to_string_lossy().to_string()),
        ("UV_CACHE_DIR".into(), dirs.cache_dir().to_string_lossy().to_string()),
        ("UV_PYTHON_PREFERENCE".into(), "only-managed".into()),
        ("UV_NO_PROGRESS".into(), "1".into()),
        ("UV_LINK_MODE".into(), "copy".into()),
    ]
}

/// Run a tool, streaming its stderr lines to the log. Fails on a non-zero exit.
pub(crate) fn run_tool(cx: &Context, exe: &Path, args: &[&str], env: &[(String, String)]) -> Result<String> {
    cx.check_cancel()?;
    let pretty = format!("{} {}", exe.file_name().unwrap_or_default().to_string_lossy(), args.join(" "));
    cx.log(format!("$ {pretty}"));
    let mut cmd = Command::new(exe);
    cmd.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = cmd.spawn().map_err(|e| RuntimeError::Tool { cmd: pretty.clone(), status: "spawn".into(), stderr: e.to_string() })?;
    let stderr = child.stderr.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let collected = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let c2 = collected.clone();
    let err_lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let e2 = err_lines.clone();
    let t_out = std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::BufReader::new(stdout).lines().map_while(std::result::Result::ok) {
            c2.lock().unwrap().push_str(&line);
            c2.lock().unwrap().push('\n');
        }
    });
    {
        use std::io::BufRead;
        for line in std::io::BufReader::new(stderr).lines().map_while(std::result::Result::ok) {
            if cx.cancel.load(Ordering::Relaxed) {
                let _ = child.kill();
            }
            let trimmed = line.trim_end().to_string();
            if !trimmed.is_empty() {
                e2.lock().unwrap().push(trimmed.clone());
                cx.log(format!("  {trimmed}"));
            }
        }
    }
    let _ = t_out.join();
    let status = child.wait()?;
    cx.check_cancel()?;
    if !status.success() {
        let tail: Vec<String> = err_lines.lock().unwrap().iter().rev().take(8).cloned().collect::<Vec<_>>().into_iter().rev().collect();
        return Err(RuntimeError::Tool { cmd: pretty, status: status.to_string(), stderr: tail.join("\n") });
    }
    let out = collected.lock().unwrap().clone();
    Ok(out)
}

fn step_uv(cx: &Context, rec: &mut Installed) -> Result<PathBuf> {
    (cx.progress)(Progress::Step { step: Step::Uv, label: "Checking uv".into() });
    let m = cx.manifest;
    if let Some(c) = detect::find_uv(&cx.dirs.uv_exe(), &m.uv.min_adopt) {
        cx.log(format!("using uv {} ({})", c.version, c.source));
        rec.uv = Some(c.exe.clone());
        rec.uv_version = Some(c.version);
        return Ok(c.exe);
    }
    let asset = m.uv_asset(Platform::current())?;
    let file = cx.dirs.downloads_dir().join(asset.url.rsplit('/').next().unwrap_or("uv-archive"));
    (cx.progress)(Progress::Step { step: Step::Uv, label: format!("Downloading uv {}", m.uv.version) });
    download(cx, &asset.url, &file, Some(&asset.sha256))?;
    let staging = cx.dirs.downloads_dir().join("uv-extract");
    let top = extract(cx, &file, &staging)?;
    let exe = find_file(&top, &["uv.exe", "uv"], 2).ok_or_else(|| RuntimeError::Manifest("uv archive has no uv executable".into()))?;
    std::fs::create_dir_all(cx.dirs.uv_dir())?;
    std::fs::copy(&exe, cx.dirs.uv_exe())?;
    if let Some(uvx) = find_file(&top, &["uvx.exe", "uvx"], 2) {
        let _ = std::fs::copy(&uvx, cx.dirs.uv_dir().join(uvx.file_name().unwrap()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(cx.dirs.uv_exe(), std::fs::Permissions::from_mode(0o755));
    }
    let _ = std::fs::remove_dir_all(&staging);
    let _ = std::fs::remove_file(&file);
    let v = detect::uv_version(&cx.dirs.uv_exe()).unwrap_or_else(|| m.uv.version.clone());
    cx.log(format!("installed uv {v}"));
    rec.uv = Some(cx.dirs.uv_exe());
    rec.uv_version = Some(v);
    Ok(cx.dirs.uv_exe())
}

fn step_python(cx: &Context, uv: &Path, profile: &Profile, rec: &mut Installed) -> Result<String> {
    let want = profile.python_for(Platform::current()).to_string();
    (cx.progress)(Progress::Step { step: Step::Python, label: format!("Checking Python {want} (installed only if missing)") });
    let env = uv_env(cx.dirs);
    std::fs::create_dir_all(cx.dirs.python_dir())?;
    run_tool(cx, uv, &["python", "install", &want], &env)?;
    let found = run_tool(cx, uv, &["python", "find", &want], &env)?;
    let found = found.trim().to_string();
    cx.log(format!("Python {want}: {found}"));
    rec.python_version = Some(want.clone());
    Ok(want)
}

fn step_env(cx: &Context, uv: &Path, profile_name: &str, profile: &Profile, python: &str, rec: &mut Installed) -> Result<PathBuf> {
    (cx.progress)(Progress::Step { step: Step::Env, label: format!("Checking the {profile_name} environment (created only if missing)") });
    let env = uv_env(cx.dirs);
    let env_dir = cx.dirs.env_dir(profile_name);
    let env_dir_s = env_dir.to_string_lossy().to_string();
    if !cx.dirs.env_python(profile_name).is_file() {
        run_tool(cx, uv, &["venv", &env_dir_s, "--python", python, "--seed"], &env)?;
    }
    let req = cx.manifest.requirement(profile);
    (cx.progress)(Progress::Step { step: Step::Env, label: format!("Updating local-spark-mcp to {} (pyspark {} + delta-spark {} kept when already there)", cx.manifest.local_spark_mcp.version, profile.pyspark, profile.delta) });
    let py = cx.dirs.env_python(profile_name).to_string_lossy().to_string();
    run_tool(cx, uv, &["pip", "install", "--python", &py, "--reinstall-package", "local-spark-mcp", &req], &env)?;
    let v = detect::installed_package_version(&env_dir, "local-spark-mcp");
    cx.log(format!("local-spark-mcp {} installed in {}", v.clone().unwrap_or_default(), env_dir.display()));
    // the wheel cache is only useful for the next install; it is larger than the environment
    let _ = std::fs::remove_dir_all(cx.dirs.cache_dir());
    rec.env = Some(env_dir.clone());
    rec.package_version = v;
    rec.profile = Some(profile_name.to_string());
    Ok(env_dir)
}

#[derive(serde::Deserialize)]
struct AdoptiumAsset {
    release_name: String,
    binary: AdoptiumBinary,
}
#[derive(serde::Deserialize)]
struct AdoptiumBinary {
    package: AdoptiumPackage,
}
#[derive(serde::Deserialize)]
struct AdoptiumPackage {
    link: String,
    checksum: String,
}

fn step_jdk(cx: &Context, plan: &Plan, profile: &Profile, rec: &mut Installed) -> Result<PathBuf> {
    (cx.progress)(Progress::Step { step: Step::Jdk, label: "Checking Java".into() });
    if let Some(adopt) = &plan.adopt_jdk {
        let c = detect::check_jdk(adopt, "settings").map_err(RuntimeError::Manifest)?;
        if let Some(m) = c.major {
            if !profile.accepts_java(m) {
                return Err(RuntimeError::Manifest(format!("{} is Java {m}; {} needs Java {}", c.home.display(), profile.extra, profile.java_majors.iter().map(|m| m.to_string()).collect::<Vec<_>>().join("/"))));
            }
        }
        cx.log(format!("using {}", c.label()));
        rec.jdk_home = Some(c.home.clone());
        rec.jdk_major = c.major;
        rec.jdk_adopted = true;
        rec.jdk_vendor = None;
        return Ok(c.home);
    }
    // a managed JDK that fits is kept
    if let Some(c) = detect::find_jdks(Some(profile), Some(&cx.dirs.jdk_dir())).into_iter().find(|c| c.source == "managed") {
        cx.log(format!("keeping {}", c.label()));
        rec.jdk_home = Some(c.home.clone());
        rec.jdk_major = c.major;
        rec.jdk_adopted = false;
        return Ok(c.home);
    }
    let major = profile.java_preferred.first().copied().unwrap_or(21);
    let platform = Platform::current();
    let (url, sha, name) = match plan.jdk_vendor {
        JdkVendor::Microsoft => {
            let (rel, asset) = cx.manifest.microsoft_jdk(major, platform)?;
            (asset.url.clone(), Some(asset.sha256.clone()), format!("microsoft-{}", rel.version))
        }
        JdkVendor::Temurin => {
            let api = cx.manifest.temurin_url(major, platform);
            cx.log(format!("resolving Temurin {major} via {api}"));
            let resp = client()?.get(&api).send().map_err(|e| RuntimeError::Http(e.to_string()))?;
            let assets: Vec<AdoptiumAsset> = resp.json().map_err(|e| RuntimeError::Http(format!("adoptium: {e}")))?;
            let a = assets.into_iter().next().ok_or_else(|| RuntimeError::Http("adoptium returned no builds".into()))?;
            (a.binary.package.link, Some(a.binary.package.checksum), format!("temurin-{}", a.release_name.trim_start_matches("jdk-").replace('+', "_")))
        }
    };
    (cx.progress)(Progress::Step { step: Step::Jdk, label: format!("Downloading {} {major}", plan.jdk_vendor.label()) });
    let file = cx.dirs.downloads_dir().join(url.rsplit('/').next().unwrap_or("jdk-archive"));
    download(cx, &url, &file, sha.as_deref())?;
    let target = cx.dirs.jdk_dir().join(&name);
    let top = extract(cx, &file, &target)?;
    let c = detect::check_jdk(&top, "managed").map_err(RuntimeError::Manifest)?;
    let _ = std::fs::remove_file(&file);
    cx.log(format!("installed {}", c.label()));
    rec.jdk_home = Some(c.home.clone());
    rec.jdk_major = c.major;
    rec.jdk_adopted = false;
    rec.jdk_vendor = Some(plan.jdk_vendor);
    Ok(c.home)
}

/// The worker configuration for a provisioned runtime.
pub fn worker_config(dirs: &RuntimeDirs, profile_name: &str, jdk_home: Option<&Path>, driver_memory: &str, extra: serde_json::Map<String, serde_json::Value>) -> WorkerConfig {
    let mut init = serde_json::Map::new();
    init.insert("driver_memory".into(), serde_json::Value::String(driver_memory.to_string()));
    init.insert("app_name".into(), serde_json::Value::String("cobalt-sqlworks".into()));
    init.insert("state_root".into(), serde_json::Value::String(dirs.state_dir().to_string_lossy().to_string()));
    if let Some(j) = jdk_home {
        init.insert("java_home".into(), serde_json::Value::String(j.to_string_lossy().to_string()));
    }
    let mut confs = serde_json::Map::new();
    confs.insert("spark.jars.ivy".into(), serde_json::Value::String(dirs.ivy_dir().to_string_lossy().to_string()));
    confs.insert("spark.ui.enabled".into(), serde_json::Value::String("false".into()));
    // local[*] only: keep every listener on loopback so Windows Firewall never asks about java.exe
    confs.insert("spark.driver.bindAddress".into(), serde_json::Value::String("127.0.0.1".into()));
    confs.insert("spark.driver.host".into(), serde_json::Value::String("127.0.0.1".into()));
    init.insert("extra_configs".into(), serde_json::Value::Object(confs));
    for (k, v) in extra {
        init.insert(k, v);
    }
    let mut env = vec![("LOCAL_SPARK_PROFILE".to_string(), profile_name.to_string()), ("PYTHONUNBUFFERED".to_string(), "1".to_string()), ("PYTHONIOENCODING".to_string(), "utf-8".to_string())];
    if let Some(j) = jdk_home {
        env.push(("JAVA_HOME".into(), j.to_string_lossy().to_string()));
    }
    WorkerConfig { python: dirs.env_python(profile_name), env, init: serde_json::Value::Object(init), startup_timeout: Duration::from_secs(60 * 20), control: false, module: "local_spark_mcp.worker".into() }
}

/// The worker configuration for the LakeSail engine: Cobalt's own worker module from the Sail
/// environment, no JVM, the control socket always on.
pub fn sail_worker_config(dirs: &RuntimeDirs, extra: serde_json::Map<String, serde_json::Value>) -> WorkerConfig {
    let mut init = serde_json::Map::new();
    init.insert("app_name".into(), serde_json::Value::String("cobalt-sqlworks".into()));
    init.insert("state_root".into(), serde_json::Value::String(dirs.state_dir().to_string_lossy().to_string()));
    for (k, v) in extra {
        init.insert(k, v);
    }
    let env = vec![
        ("PYTHONPATH".to_string(), dirs.sail_env_dir().to_string_lossy().to_string()),
        ("PYTHONUNBUFFERED".to_string(), "1".to_string()),
        ("PYTHONIOENCODING".to_string(), "utf-8".to_string()),
        ("RUST_LOG".to_string(), "warn".to_string()),
    ];
    WorkerConfig { python: dirs.sail_env_python(), env, init: serde_json::Value::Object(init), startup_timeout: Duration::from_secs(60 * 5), control: true, module: crate::SAIL_WORKER_MODULE.into() }
}

/// The LakeSail environment: a venv with pysail, the PySpark Connect client and IPython, plus
/// the worker module. `uv pip install` is idempotent, so an update is the same step.
fn step_sail_env(cx: &Context, uv: &Path, python: &str, rec: &mut Installed) -> Result<PathBuf> {
    let pins = &cx.manifest.sail;
    (cx.progress)(Progress::Step { step: Step::Env, label: format!("Checking the LakeSail environment (Sail {}, created only if missing)", pins.version) });
    let env = uv_env(cx.dirs);
    let env_dir = cx.dirs.sail_env_dir();
    let env_dir_s = env_dir.to_string_lossy().to_string();
    if !cx.dirs.sail_env_python().is_file() {
        run_tool(cx, uv, &["venv", &env_dir_s, "--python", python, "--seed"], &env)?;
    }
    (cx.progress)(Progress::Step { step: Step::Env, label: format!("Installing pysail {} and pyspark-client {} (about 250 MB the first time)", pins.version, pins.pyspark_client) });
    let py = cx.dirs.sail_env_python().to_string_lossy().to_string();
    let mut args: Vec<String> = vec!["pip".into(), "install".into(), "--python".into(), py];
    args.extend(pins.requirements());
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    run_tool(cx, uv, &argv, &env)?;
    std::fs::write(cx.dirs.sail_worker_file(), crate::SAIL_WORKER)?;
    let v = detect::installed_package_version(&env_dir, "pysail");
    let pv = detect::installed_package_version(&env_dir, "pyspark-client").or_else(|| detect::installed_package_version(&env_dir, "pyspark"));
    cx.log(format!("pysail {} + pyspark-client {} installed in {}", v.clone().unwrap_or_default(), pv.clone().unwrap_or_default(), env_dir.display()));
    let _ = std::fs::remove_dir_all(cx.dirs.cache_dir());
    rec.sail_env = Some(env_dir.clone());
    rec.sail_version = v;
    rec.sail_pyspark = pv;
    Ok(env_dir)
}

/// A Fabric package roster into an engine's environment: all at once when the resolver agrees
/// (one resolution, consistent versions), otherwise one package at a time so the ones with no
/// wheel for this platform (or a conflict) are skipped and named, never fatal. Returns the
/// requirements that did not install. The same procedure as local-spark-mcp's own
/// `fabric_packages install`, run by Cobalt so both engines' environments get it with the live log.
fn step_roster(cx: &Context, uv: &Path, env_dir: &Path, env_python: &Path, roster: &crate::manifest::Roster) -> Result<Vec<String>> {
    (cx.progress)(Progress::Step { step: Step::Libraries, label: format!("Installing the {} package roster ({} packages, Fabric Runtime {}'s versions)", roster.profile, roster.packages.len(), roster.fabric_runtime) });
    let env = uv_env(cx.dirs);
    let py = env_python.to_string_lossy().to_string();
    // the manifest's per-platform fallbacks apply to the environment's Python
    let python = env_python_version(env_dir);
    let specs = roster.requirements_for(python);
    for (spec, pin) in specs.iter().zip(&roster.packages) {
        if spec != pin {
            let name = crate::libraries::python_dist_name(pin);
            cx.log(format!("{name}: {spec} instead of {pin} on Python {} ({})", python.map(|(a, b)| format!("{a}.{b}")).unwrap_or_default(), roster.fallbacks.get(&name).map(|f| f.reason.as_str()).unwrap_or("platform fallback")));
        }
    }
    let mut args: Vec<String> = vec!["pip".into(), "install".into(), "--python".into(), py.clone()];
    args.extend(specs.iter().cloned());
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut failed: Vec<String> = Vec::new();
    if let Err(e) = run_tool(cx, uv, &argv, &env) {
        cx.log(format!("roster as a whole did not resolve ({e}); installing package by package"));
        for spec in &specs {
            if cx.cancel.load(Ordering::Relaxed) {
                return Err(RuntimeError::Cancelled);
            }
            let one = ["pip", "install", "--python", py.as_str(), spec.as_str()];
            if let Err(e) = run_tool(cx, uv, &one, &env) {
                cx.log(format!("  skipped {spec}: {}", e.to_string().lines().last().unwrap_or("").chars().take(200).collect::<String>()));
                failed.push(spec.clone());
            }
        }
    }
    let _ = std::fs::remove_dir_all(cx.dirs.cache_dir());
    cx.log(format!("roster {}: {} of {} packages installed{}", roster.profile, roster.packages.len() - failed.len(), roster.packages.len(), if failed.is_empty() { String::new() } else { format!(" · not installable here: {}", failed.join(", ")) }));
    Ok(failed)
}

/// The (major, minor) of an environment's Python, from its `pyvenv.cfg`.
pub(crate) fn env_python_version(env_dir: &Path) -> Option<(u32, u32)> {
    let v = crate::manifest::version_tuple(&detect::venv_python_version(env_dir)?);
    (v.len() >= 2).then(|| (v[0], v[1]))
}

/// The roster chosen for the LakeSail environment (`none` clears the record).
fn step_sail_packages(cx: &Context, uv: &Path, roster_name: &str, rec: &mut Installed) -> Result<()> {
    let Some(roster) = cx.manifest.engine_roster(Engine::Sail, roster_name) else {
        rec.sail_roster = None;
        rec.sail_roster_failed.clear();
        return Ok(());
    };
    let failed = step_roster(cx, uv, &cx.dirs.sail_env_dir(), &cx.dirs.sail_env_python(), &roster)?;
    reassert_sail_pins(cx, uv)?;
    rec.sail_roster = Some(roster_name.to_string());
    rec.sail_roster_failed = failed;
    Ok(())
}

/// After a roster install, put LakeSail's own requirements back where the roster's pins moved
/// them (a resolver does not re-check what it was not asked about).
fn reassert_sail_pins(cx: &Context, uv: &Path) -> Result<()> {
    cx.log("re-asserting LakeSail's own pins over the roster");
    let py = cx.dirs.sail_env_python().to_string_lossy().to_string();
    let mut args: Vec<String> = vec!["pip".into(), "install".into(), "--python".into(), py];
    args.extend(cx.manifest.sail.requirements());
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    run_tool(cx, uv, &argv, &uv_env(cx.dirs))?;
    let _ = std::fs::remove_dir_all(cx.dirs.cache_dir());
    Ok(())
}

/// The roster into an installed environment outside a provisioning run (the "Install Python
/// packages" button): `env` is a profile name or [`crate::SAIL_ENV`], `roster_name` the profile
/// whose packages go in. Records the outcome like the install steps do; returns a summary line.
pub fn install_roster(cx: &Context, env: &str, roster_name: &str) -> Result<String> {
    let sail = env == crate::SAIL_ENV;
    let Some(roster) = cx.manifest.engine_roster(if sail { Engine::Sail } else { Engine::PySpark }, roster_name) else {
        return Ok("no Fabric package roster".into());
    };
    let uv = detect::find_uv(&cx.dirs.uv_exe(), &cx.manifest.uv.min_adopt).map(|c| c.exe).ok_or_else(|| RuntimeError::Manifest("uv is not installed — install the runtime first".into()))?;
    let py = if sail { cx.dirs.sail_env_python() } else { cx.dirs.env_python(env) };
    if !py.is_file() {
        return Err(RuntimeError::Manifest(format!("the {env} environment is not installed — install the runtime first")));
    }
    let env_dir = if sail { cx.dirs.sail_env_dir() } else { cx.dirs.env_dir(env) };
    let failed = step_roster(cx, &uv, &env_dir, &py, &roster)?;
    if sail {
        reassert_sail_pins(cx, &uv)?;
    }
    let mut rec = Installed::load(cx.dirs);
    if sail {
        rec.sail_roster = Some(roster_name.to_string());
        rec.sail_roster_failed = failed.clone();
    } else {
        rec.profile_packages = Some(roster_name.to_string());
        rec.profile_packages_failed = failed.clone();
    }
    rec.save(cx.dirs)?;
    Ok(format!("Fabric packages ({roster_name}): {} of {} installed{}", roster.packages.len() - failed.len(), roster.packages.len(), if failed.is_empty() { String::new() } else { format!(", not installable here: {}", failed.join(", ")) }))
}

/// The profile's own roster into the Local Spark environment, when the plan asks for it.
fn step_profile_packages(cx: &Context, uv: &Path, plan: &Plan, rec: &mut Installed) -> Result<()> {
    let roster = if plan.profile_packages { cx.manifest.engine_roster(Engine::PySpark, &plan.profile) } else { None };
    let Some(roster) = roster else {
        rec.profile_packages = None;
        rec.profile_packages_failed.clear();
        return Ok(());
    };
    let failed = step_roster(cx, uv, &cx.dirs.env_dir(&plan.profile), &cx.dirs.env_python(&plan.profile), &roster)?;
    rec.profile_packages = Some(plan.profile.clone());
    rec.profile_packages_failed = failed;
    Ok(())
}

/// Start the Sail worker once and run `SELECT 1`: proves the wheel loads on this machine.
fn step_sail_warm(cx: &Context, rec: &mut Installed) -> Result<String> {
    (cx.progress)(Progress::Step { step: Step::Warm, label: "Starting a LakeSail session".into() });
    std::fs::create_dir_all(cx.dirs.state_dir())?;
    let cfg = sail_worker_config(cx.dirs, Default::default());
    let (ltx, lrx) = std::sync::mpsc::channel::<String>();
    let log: crate::worker::LogFn = std::sync::Arc::new(move |s: String| {
        let _ = ltx.send(s);
    });
    let mut w = Worker::start(&cfg, log, cx.cancel)?;
    for s in lrx.try_iter() {
        cx.log(format!("  {s}"));
    }
    let info = w.info.clone();
    let sail_version = info.get("engine_version").and_then(|v| v.as_str()).unwrap_or("?").to_string();
    let spark_version = info.get("spark_version").and_then(|v| v.as_str()).unwrap_or("?").to_string();
    cx.log(format!("Sail {sail_version} up (PySpark Connect client {spark_version})"));
    let r = w.run_sql("SELECT 1 AS one, 'ok' AS status", Some(10))?;
    let rows = r.get("rows").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
    cx.log(format!("SELECT 1 returned {rows} row(s): {}", r.get("rows").map(|v| v.to_string()).unwrap_or_default()));
    w.pump();
    for s in lrx.try_iter() {
        cx.log(format!("  {s}"));
    }
    w.shutdown();
    rec.sail_warmed = true;
    rec.sail_spark_version = Some(spark_version);
    Ok(sail_version)
}

/// Provision the LakeSail engine: uv, Python, the environment, one session.
fn provision_sail(cx: &Context, plan: &Plan) -> Result<Installed> {
    std::fs::create_dir_all(cx.dirs.downloads_dir())?;
    let mut rec = Installed::load(cx.dirs);
    rec.last_error = None;
    cx.log(format!("provisioning LakeSail ({}) under {}", cx.manifest.sail.describe(), cx.dirs.root.display()));
    let result = (|| -> Result<()> {
        let uv = step_uv(cx, &mut rec)?;
        rec.save(cx.dirs)?;
        let want = cx.manifest.sail.python_for(Platform::current()).to_string();
        (cx.progress)(Progress::Step { step: Step::Python, label: format!("Checking Python {want} (installed only if missing)") });
        let env = uv_env(cx.dirs);
        std::fs::create_dir_all(cx.dirs.python_dir())?;
        run_tool(cx, &uv, &["python", "install", &want], &env)?;
        rec.save(cx.dirs)?;
        if plan.steps.contains(&Step::Env) || !cx.dirs.sail_env_python().is_file() {
            step_sail_env(cx, &uv, &want, &mut rec)?;
            rec.save(cx.dirs)?;
        } else {
            // the worker module follows the Cobalt build, not the environment
            std::fs::write(cx.dirs.sail_worker_file(), crate::SAIL_WORKER)?;
        }
        if plan.steps.contains(&Step::Env) || plan.steps.contains(&Step::Libraries) {
            step_sail_packages(cx, &uv, &plan.sail_profile, &mut rec)?;
            rec.save(cx.dirs)?;
        }
        if plan.steps.contains(&Step::Warm) {
            step_sail_warm(cx, &mut rec)?;
            rec.save(cx.dirs)?;
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            cx.log("done");
            rec.save(cx.dirs)?;
            Ok(rec)
        }
        Err(e) => {
            rec.last_error = Some(e.to_string());
            let _ = rec.save(cx.dirs);
            cx.log(format!("failed: {e}"));
            Err(e)
        }
    }
}

/// Delete the LakeSail environment only (the JVM runtime stays).
pub fn remove_sail(dirs: &RuntimeDirs) -> std::io::Result<()> {
    let env = dirs.sail_env_dir();
    if env.exists() {
        std::fs::remove_dir_all(&env)?;
    }
    let mut rec = Installed::load(dirs);
    rec.sail_env = None;
    rec.sail_version = None;
    rec.sail_pyspark = None;
    rec.sail_warmed = false;
    rec.sail_spark_version = None;
    rec.sail_roster = None;
    rec.sail_roster_failed.clear();
    let _ = rec.save(dirs);
    Ok(())
}

/// Start a worker, run `SELECT 1`, report versions. The first run pulls Delta and hadoop-azure
/// jars into the Ivy cache, which is the point.
pub fn step_warm(cx: &Context, plan: &Plan, jdk_home: &Path, rec: &mut Installed) -> Result<String> {
    (cx.progress)(Progress::Step { step: Step::Warm, label: "Starting a Spark session (first run downloads Delta and Hadoop jars)".into() });
    std::fs::create_dir_all(cx.dirs.ivy_dir())?;
    std::fs::create_dir_all(cx.dirs.state_dir())?;
    let cfg = worker_config(cx.dirs, &plan.profile, Some(jdk_home), &plan.driver_memory, Default::default());
    let (ltx, lrx) = std::sync::mpsc::channel::<String>();
    let log: crate::worker::LogFn = std::sync::Arc::new(move |s: String| {
        let _ = ltx.send(s);
    });
    let mut w = Worker::start(&cfg, log, cx.cancel)?;
    for s in lrx.try_iter() {
        cx.log(format!("  {s}"));
    }
    let info = w.info.clone();
    let spark_version = info.get("spark_version").and_then(|v| v.as_str()).unwrap_or("?").to_string();
    cx.log(format!("Spark {spark_version} up ({})", info.get("profile").and_then(|v| v.as_str()).unwrap_or("")));
    let r = w.run_sql("SELECT 1 AS one, 'ok' AS status", Some(10))?;
    let rows = r.get("rows").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
    cx.log(format!("SELECT 1 returned {rows} row(s): {}", r.get("rows").map(|v| v.to_string()).unwrap_or_default()));
    w.pump();
    for s in lrx.try_iter() {
        cx.log(format!("  {s}"));
    }
    w.shutdown();
    rec.warmed = true;
    rec.spark_version = Some(spark_version.clone());
    Ok(spark_version)
}

/// Run the plan's steps in order, recording progress in `runtime.json` after each one.
pub fn provision(cx: &Context, plan: &Plan) -> Result<Installed> {
    if plan.engine.is_sail() {
        return provision_sail(cx, plan);
    }
    let profile = cx.manifest.profile(&plan.profile)?.clone();
    std::fs::create_dir_all(cx.dirs.downloads_dir())?;
    let mut rec = Installed::load(cx.dirs);
    if rec.profile.as_deref() != Some(&plan.profile) {
        rec.warmed = false;
        rec.spark_version = None;
    }
    rec.last_error = None;
    cx.log(format!("provisioning {} ({}) under {}", plan.profile, profile.describe(), cx.dirs.root.display()));
    let result = (|| -> Result<()> {
        let uv = step_uv(cx, &mut rec)?;
        rec.save(cx.dirs)?;
        let want = plan.steps.contains(&Step::Python) || plan.steps.contains(&Step::Env);
        let python = if want { step_python(cx, &uv, &profile, &mut rec)? } else { profile.python_for(Platform::current()).to_string() };
        rec.save(cx.dirs)?;
        if plan.steps.contains(&Step::Env) {
            step_env(cx, &uv, &plan.profile, &profile, &python, &mut rec)?;
            rec.save(cx.dirs)?;
        }
        if plan.steps.contains(&Step::Env) || plan.steps.contains(&Step::Libraries) {
            step_profile_packages(cx, &uv, plan, &mut rec)?;
            rec.save(cx.dirs)?;
        }
        let jdk = if plan.steps.contains(&Step::Jdk) || rec.jdk_home.is_none() {
            step_jdk(cx, plan, &profile, &mut rec)?
        } else {
            rec.jdk_home.clone().unwrap()
        };
        rec.save(cx.dirs)?;
        if plan.steps.contains(&Step::Warm) {
            step_warm(cx, plan, &jdk, &mut rec)?;
            rec.save(cx.dirs)?;
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            cx.log("done");
            rec.save(cx.dirs)?;
            Ok(rec)
        }
        Err(e) => {
            rec.last_error = Some(e.to_string());
            let _ = rec.save(cx.dirs);
            cx.log(format!("failed: {e}"));
            Err(e)
        }
    }
}

/// Delete everything Cobalt installed (adopted tools are untouched).
pub fn remove_all(dirs: &RuntimeDirs) -> std::io::Result<()> {
    if dirs.root.exists() {
        std::fs::remove_dir_all(&dirs.root)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha_and_extract_zip() {
        let tmp = std::env::temp_dir().join(format!("cobalt-inst-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let f = tmp.join("a.txt");
        std::fs::write(&f, b"hello").unwrap();
        assert_eq!(sha256_file(&f).unwrap(), "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824");
        // a zip with one top-level folder
        let zpath = tmp.join("t.zip");
        {
            let zf = std::fs::File::create(&zpath).unwrap();
            let mut z = zip::ZipWriter::new(zf);
            let opts = zip::write::SimpleFileOptions::default();
            z.add_directory("top/", opts).unwrap();
            z.start_file("top/bin/x", opts).unwrap();
            z.write_all(b"bin").unwrap();
            z.finish().unwrap();
        }
        let cancel = AtomicBool::new(false);
        let m = Manifest::embedded();
        let dirs = RuntimeDirs::new(tmp.join("rt"));
        let cx = Context { dirs: &dirs, manifest: &m, progress: &|_| {}, cancel: &cancel };
        let top = extract(&cx, &zpath, &tmp.join("out")).unwrap();
        assert!(top.ends_with("top"));
        assert_eq!(std::fs::read(top.join("bin").join("x")).unwrap(), b"bin");
        assert!(find_file(&tmp.join("out"), &["x"], 3).is_some());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn worker_config_shape() {
        let dirs = RuntimeDirs::new("/tmp/rt");
        let cfg = worker_config(&dirs, "fabric-2.0", Some(Path::new("/jdk")), "2g", Default::default());
        assert_eq!(cfg.module, "local_spark_mcp.worker");
        let s = sail_worker_config(&dirs, Default::default());
        assert_eq!(s.module, crate::SAIL_WORKER_MODULE);
        assert!(s.control);
        assert!(s.env.iter().any(|(k, v)| k == "PYTHONPATH" && v.ends_with("sail")));
        assert!(crate::SAIL_WORKER.contains("def run_worker"));
        assert_eq!(cfg.init["driver_memory"], "2g");
        assert_eq!(cfg.init["java_home"], "/jdk");
        assert!(cfg.init["extra_configs"]["spark.jars.ivy"].as_str().unwrap().ends_with("ivy"));
        assert!(cfg.env.iter().any(|(k, v)| k == "LOCAL_SPARK_PROFILE" && v == "fabric-2.0"));
    }
}
