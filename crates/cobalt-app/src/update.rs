//! Update check against the GitHub releases API. One anonymous GET, off the UI thread; the
//! result comes back over a channel and the app decides whether to show anything.

use crossbeam_channel::Sender;
use serde::Deserialize;

pub const REPO: &str = "methodify/cobalt-sqlworks";
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Debug)]
pub struct UpdateInfo {
    pub version: String,
    pub url: String,
    pub notes: String,
    /// The installer for this machine, when the release has one.
    pub asset: Option<Asset>,
    /// The release's `SHA256SUMS`, to verify the download.
    pub sums_url: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Asset {
    pub name: String,
    #[serde(rename = "browser_download_url")]
    pub url: String,
    #[serde(default)]
    pub size: u64,
}

/// Pick the installer this platform should download: the NSIS setup on Windows, the Debian
/// package or AppImage on Linux (by what the machine is), the disk image on macOS.
pub fn installer_for_platform(assets: &[Asset]) -> Option<Asset> {
    let pick = |pred: &dyn Fn(&str) -> bool| assets.iter().find(|a| pred(&a.name.to_ascii_lowercase())).cloned();
    if cfg!(target_os = "windows") {
        pick(&|n| n.ends_with("-setup.exe") || n.ends_with("_x64-setup.exe"))
    } else if cfg!(target_os = "macos") {
        pick(&|n| n.ends_with(".dmg"))
    } else if std::path::Path::new("/etc/debian_version").exists() {
        pick(&|n| n.ends_with(".deb")).or_else(|| pick(&|n| n.ends_with(".appimage")))
    } else {
        pick(&|n| n.ends_with(".appimage")).or_else(|| pick(&|n| n.ends_with(".deb")))
    }
}

/// When the downloaded installer runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallWhen {
    /// Close Cobalt as soon as the download is verified and start the installer.
    Now,
    /// Keep working; the installer starts when Cobalt is closed.
    OnExit,
}

/// Progress of an installer download, shared with the UI thread.
#[derive(Debug, Default)]
pub struct DownloadProgress {
    pub done: u64,
    pub total: u64,
    pub stage: String,
    /// `Some(Ok(path))` once the file is on disk and its checksum matched.
    pub finished: Option<Result<std::path::PathBuf, String>>,
}

/// A download in flight (or finished): what, where, and what to do with it.
#[derive(Clone, Debug)]
pub struct UpdateDownload {
    pub version: String,
    pub name: String,
    pub when: InstallWhen,
    pub progress: std::sync::Arc<parking_lot::Mutex<DownloadProgress>>,
}

/// Download the installer into `dir` (replacing older downloads), verify it against the
/// release's SHA256SUMS when present, and report through `progress`.
pub fn spawn_download(session: &crate::session::SessionManager, egui: egui::Context, info: &UpdateInfo, dir: std::path::PathBuf, progress: std::sync::Arc<parking_lot::Mutex<DownloadProgress>>) {
    let Some(asset) = info.asset.clone() else { return };
    let sums_url = info.sums_url.clone();
    session.spawn(async move {
        let result = download(&asset, sums_url.as_deref(), &dir, &progress, &egui).await;
        progress.lock().finished = Some(result);
        egui.request_repaint();
    });
}

async fn download(asset: &Asset, sums_url: Option<&str>, dir: &std::path::Path, progress: &std::sync::Arc<parking_lot::Mutex<DownloadProgress>>, egui: &egui::Context) -> Result<std::path::PathBuf, String> {
    use sha2::Digest;
    use std::io::Write;

    std::fs::create_dir_all(dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    // older downloads go; only the one being fetched stays
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            if e.file_name() != std::ffi::OsStr::new(&asset.name) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    let client = reqwest::Client::builder().user_agent(format!("cobalt-sqlworks/{CURRENT_VERSION}")).timeout(std::time::Duration::from_secs(60 * 30)).build().map_err(|e| e.to_string())?;
    let expected = match sums_url {
        Some(u) => {
            progress.lock().stage = "Fetching checksums".into();
            let text = client.get(u).send().await.map_err(|e| e.to_string())?.text().await.map_err(|e| e.to_string())?;
            text.lines().find_map(|l| {
                let mut it = l.split_whitespace();
                let hash = it.next()?;
                let name = it.next()?.trim_start_matches('*');
                (name == asset.name || name.ends_with(&format!("/{}", asset.name))).then(|| hash.to_ascii_lowercase())
            })
        }
        None => None,
    };
    let final_path = dir.join(&asset.name);
    let part = dir.join(format!("{}.part", asset.name));
    {
        let mut p = progress.lock();
        p.stage = format!("Downloading {}", asset.name);
        p.total = asset.size;
        p.done = 0;
    }
    let mut resp = client.get(&asset.url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("GitHub answered {}", resp.status()));
    }
    if let Some(len) = resp.content_length() {
        progress.lock().total = len;
    }
    let mut file = std::fs::File::create(&part).map_err(|e| format!("could not write {}: {e}", part.display()))?;
    let mut hasher = sha2::Sha256::new();
    let mut done: u64 = 0;
    let mut last_paint = std::time::Instant::now();
    while let Some(chunk) = resp.chunk().await.map_err(|e| e.to_string())? {
        file.write_all(&chunk).map_err(|e| e.to_string())?;
        hasher.update(&chunk);
        done += chunk.len() as u64;
        progress.lock().done = done;
        if last_paint.elapsed() > std::time::Duration::from_millis(150) {
            egui.request_repaint();
            last_paint = std::time::Instant::now();
        }
    }
    file.flush().map_err(|e| e.to_string())?;
    drop(file);
    let got = format!("{:x}", hasher.finalize());
    match expected {
        Some(want) if want != got => {
            let _ = std::fs::remove_file(&part);
            return Err(format!("the download's checksum does not match the release's SHA256SUMS (expected {}…, got {}…); nothing was installed", &want[..12], &got[..12]));
        }
        Some(_) => progress.lock().stage = "Checksum verified".into(),
        None => progress.lock().stage = "Downloaded (no checksum published for this release)".into(),
    }
    std::fs::rename(&part, &final_path).map_err(|e| e.to_string())?;
    Ok(final_path)
}

/// Start the installer. On Windows the NSIS setup is started after a short delay from a
/// detached shell, so Cobalt has exited and its files are free when the installer copies.
/// Elsewhere the file opens with the system handler (a package installer, a disk image).
pub fn launch_installer(path: &std::path::Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let p = path.to_string_lossy().to_string();
        std::process::Command::new("powershell")
            .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &format!("Start-Sleep -Seconds 2; Start-Process -FilePath '{}'", p.replace('\'', "''"))])
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    #[cfg(not(windows))]
    {
        open::that(path).map_err(|e| e.to_string())
    }
}

#[derive(Clone, Debug)]
pub enum UpdateOutcome {
    UpToDate { manual: bool },
    Available { info: UpdateInfo, manual: bool },
    Failed { error: String, manual: bool },
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    assets: Vec<Asset>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
}

/// The version to compare releases against: the build's, or `COBALT_UPDATE_FAKE_VERSION` in a
/// debug build (to exercise the dialog and the download against the current release).
pub fn current_version() -> String {
    if cfg!(debug_assertions) {
        if let Ok(v) = std::env::var("COBALT_UPDATE_FAKE_VERSION") {
            if !v.trim().is_empty() {
                return v;
            }
        }
    }
    CURRENT_VERSION.to_string()
}

/// `v1.2.3` / `1.2.3` / `1.2.3-beta` → (1, 2, 3). Anything else → `None`.
pub fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let s = s.trim().trim_start_matches(['v', 'V']);
    let core = s.split(['-', '+']).next()?;
    let mut it = core.split('.').map(|p| p.parse::<u64>().ok());
    let major = it.next()??;
    let minor = it.next().unwrap_or(Some(0))?;
    let patch = it.next().unwrap_or(Some(0))?;
    Some((major, minor, patch))
}

pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(c), Some(n)) => c > n,
        _ => false,
    }
}

/// The changelog as shipped with this build (Help → What's new shows it at once).
pub const BUNDLED_CHANGELOG: &str = include_str!("../../../CHANGELOG.md");
pub const CHANGELOG_URL: &str = "https://raw.githubusercontent.com/methodify/cobalt-sqlworks/main/CHANGELOG.md";
pub const CHANGELOG_PAGE: &str = "https://github.com/methodify/cobalt-sqlworks/blob/main/CHANGELOG.md";

/// Fetch the latest changelog from GitHub into `state.changelog` (no-op while one is in flight).
pub fn fetch_changelog(state: &mut crate::state::AppState, cx: &crate::ops::Ctx) {
    if state.changelog.pending.is_some() {
        return;
    }
    let slot = std::sync::Arc::new(parking_lot::Mutex::new(None));
    state.changelog.pending = Some(slot.clone());
    let egui = cx.egui.clone();
    cx.session.spawn(async move {
        let result = async {
            let client = reqwest::Client::builder().user_agent(format!("cobalt-sqlworks/{CURRENT_VERSION}")).timeout(std::time::Duration::from_secs(15)).build().map_err(|e| e.to_string())?;
            let resp = client.get(CHANGELOG_URL).send().await.map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                return Err(format!("GitHub answered {}", resp.status()));
            }
            resp.text().await.map_err(|e| e.to_string())
        }
        .await;
        *slot.lock() = Some(result);
        egui.request_repaint();
    });
}

/// Fire the check on the session runtime; the outcome arrives on `tx`.
pub fn spawn_check(session: &crate::session::SessionManager, tx: Sender<UpdateOutcome>, manual: bool) {
    session.spawn(async move {
        let outcome = match fetch_latest().await {
            Ok(rel) => {
                if is_newer(&rel.tag_name, &current_version()) {
                    let asset = installer_for_platform(&rel.assets);
                    let sums_url = rel.assets.iter().find(|a| a.name == "SHA256SUMS").map(|a| a.url.clone());
                    UpdateOutcome::Available {
                        info: UpdateInfo { version: rel.tag_name.trim_start_matches('v').to_string(), url: rel.html_url, notes: rel.body.unwrap_or_default(), asset, sums_url },
                        manual,
                    }
                } else {
                    UpdateOutcome::UpToDate { manual }
                }
            }
            Err(e) => UpdateOutcome::Failed { error: e, manual },
        };
        let _ = tx.send(outcome);
    });
}

async fn fetch_latest() -> Result<Release, String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let client = reqwest::Client::builder()
        .user_agent(format!("cobalt-sqlworks/{CURRENT_VERSION}"))
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client.get(&url).header("Accept", "application/vnd.github+json").send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("GitHub answered {}", resp.status()));
    }
    let rel: Release = resp.json().await.map_err(|e| e.to_string())?;
    if rel.draft || rel.prerelease {
        return Err("latest release is a draft or pre-release".into());
    }
    Ok(rel)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_installer_for_this_platform() {
        let assets: Vec<Asset> = ["cobalt_0.9.0_x64-setup.exe", "cobalt-sqlworks-0.9.0-windows-x86_64.zip", "cobalt_0.9.0_amd64.deb", "cobalt_0.9.0_x86_64.AppImage", "cobalt-sqlworks-0.9.0-macos-aarch64.dmg", "SHA256SUMS"]
            .iter()
            .map(|n| Asset { name: n.to_string(), url: format!("https://example.invalid/{n}"), size: 1 })
            .collect();
        let got = installer_for_platform(&assets).expect("an installer");
        if cfg!(target_os = "windows") {
            assert_eq!(got.name, "cobalt_0.9.0_x64-setup.exe");
        } else if cfg!(target_os = "macos") {
            assert_eq!(got.name, "cobalt-sqlworks-0.9.0-macos-aarch64.dmg");
        } else {
            assert!(got.name.ends_with(".deb") || got.name.ends_with(".AppImage"));
        }
        assert!(installer_for_platform(&assets[5..]).is_none());
    }

    #[test]
    fn parses_tags_and_plain_versions() {
        assert_eq!(parse_version("v0.1.0"), Some((0, 1, 0)));
        assert_eq!(parse_version("1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("v1.2.3-rc.1"), Some((1, 2, 3)));
        assert_eq!(parse_version("1.4"), Some((1, 4, 0)));
        assert_eq!(parse_version("nightly"), None);
    }

    #[test]
    fn newer_comparison() {
        assert!(is_newer("v0.2.0", "0.1.0"));
        assert!(is_newer("v1.0.0", "0.9.9"));
        assert!(!is_newer("v0.1.0", "0.1.0"));
        assert!(!is_newer("v0.0.9", "0.1.0"));
        assert!(!is_newer("garbage", "0.1.0"));
    }
}
