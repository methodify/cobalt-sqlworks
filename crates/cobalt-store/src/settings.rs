//! Load/save `Settings` as TOML.

use crate::Result;
use cobalt_core::Settings;
use std::fs;
use std::path::{Path, PathBuf};

const HEADER: &str = "\
# Cobalt SQL Works settings.
# Every key is optional; missing keys take their defaults. The app rewrites this file
# when settings change, so edit it while the app is closed or use the Settings dialog.

";

/// Read settings from `path`. A missing file yields defaults. A file that does not parse
/// yields defaults too, logs a warning, and is copied next to itself as `settings.toml.bad`
/// so nothing the user typed is lost.
pub fn load_settings(path: &Path) -> Settings {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Settings::default(),
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "could not read settings; using defaults");
            return Settings::default();
        }
    };
    match toml::from_str::<Settings>(&text) {
        Ok(s) => s,
        Err(e) => {
            let bad = bad_path(path);
            tracing::warn!(
                path = %path.display(),
                backup = %bad.display(),
                error = %e,
                "settings file did not parse; using defaults and keeping a backup copy"
            );
            if let Err(copy_err) = fs::copy(path, &bad) {
                tracing::warn!(error = %copy_err, "could not write settings backup");
            }
            Settings::default()
        }
    }
}

/// Write settings atomically (temp file in the same directory, then rename over the target).
pub fn save_settings(path: &Path, settings: &Settings) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let body = toml::to_string_pretty(settings)?;
    let tmp = tmp_path(path);
    fs::write(&tmp, format!("{HEADER}{body}"))?;
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

/// Where `load_settings` parks an unparseable file.
pub fn bad_path(path: &Path) -> PathBuf {
    sibling(path, ".bad")
}

fn tmp_path(path: &Path) -> PathBuf {
    sibling(path, &format!(".{}.tmp", std::process::id()))
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_else(|| "settings.toml".into());
    name.push(suffix);
    path.with_file_name(name)
}
