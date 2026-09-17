//! Update check against the GitHub releases API. One anonymous GET, off the UI thread; the
//! result comes back over a channel and the app decides whether to show anything.

use crossbeam_channel::Sender;
use serde::Deserialize;

pub const REPO: &str = "methodify/cobalt-sqlworks";
pub const RELEASES_PAGE: &str = "https://github.com/methodify/cobalt-sqlworks/releases/latest";
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Debug)]
pub struct UpdateInfo {
    pub version: String,
    pub url: String,
    pub notes: String,
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
    body: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
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

/// Fire the check on the session runtime; the outcome arrives on `tx`.
pub fn spawn_check(session: &crate::session::SessionManager, tx: Sender<UpdateOutcome>, manual: bool) {
    session.spawn(async move {
        let outcome = match fetch_latest().await {
            Ok(rel) => {
                if is_newer(&rel.tag_name, CURRENT_VERSION) {
                    UpdateOutcome::Available {
                        info: UpdateInfo { version: rel.tag_name.trim_start_matches('v').to_string(), url: rel.html_url, notes: rel.body.unwrap_or_default() },
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
