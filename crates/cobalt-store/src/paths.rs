//! Per-user directories for config, data, cache, logs, and spill files.

use crate::{Result, StoreError};
use std::fs;
use std::path::{Path, PathBuf};

/// Prefix of temp files written by the results spill layer; stale ones are removed at startup.
pub const SPILL_PREFIX: &str = "cobalt-spill-";

/// Resolved application directories. All exist on disk once constructed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppPaths {
    /// `settings.toml`, keybindings, themes.
    pub config_dir: PathBuf,
    /// `cobalt.db` and anything else that must survive.
    pub data_dir: PathBuf,
    /// Disposable caches.
    pub cache_dir: PathBuf,
    /// Rolling log files.
    pub log_dir: PathBuf,
    /// Large scratch data (result spill files). May live on a different volume.
    pub temp_dir: PathBuf,
}

impl AppPaths {
    /// Platform-standard directories via `directories::ProjectDirs`, created if missing.
    ///
    /// Windows: `%APPDATA%\Cobalt\Cobalt SQL Works\{config,data}`, `%LOCALAPPDATA%\...\cache`.
    /// Linux: `~/.config/cobalt sql works`, `~/.local/share/...`, `~/.cache/...`.
    /// macOS: `~/Library/Application Support/dev.Cobalt.Cobalt-SQL-Works`.
    pub fn new() -> Result<Self> {
        let dirs = directories::ProjectDirs::from(
            cobalt_core::APP_QUALIFIER,
            cobalt_core::APP_ORG,
            cobalt_core::APP_NAME,
        )
        .ok_or(StoreError::NoProjectDirs)?;
        let paths = Self {
            config_dir: dirs.config_dir().to_path_buf(),
            data_dir: dirs.data_dir().to_path_buf(),
            cache_dir: dirs.cache_dir().to_path_buf(),
            log_dir: dirs.data_local_dir().join("logs"),
            temp_dir: std::env::temp_dir().join("cobalt-sqlworks"),
        };
        paths.ensure_dirs()?;
        Ok(paths)
    }

    /// Everything under one root; for tests and portable installs.
    pub fn for_test(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref();
        let paths = Self {
            config_dir: root.join("config"),
            data_dir: root.join("data"),
            cache_dir: root.join("cache"),
            log_dir: root.join("logs"),
            temp_dir: root.join("temp"),
        };
        // Best effort: tests own the tempdir, so creation is expected to succeed.
        let _ = paths.ensure_dirs();
        paths
    }

    /// Redirect `temp_dir` (e.g. from `Settings.advanced.temp_dir`). Creates it.
    pub fn with_temp_dir(mut self, temp_dir: impl Into<PathBuf>) -> Result<Self> {
        self.temp_dir = temp_dir.into();
        fs::create_dir_all(&self.temp_dir)?;
        Ok(self)
    }

    pub fn ensure_dirs(&self) -> Result<()> {
        for d in [
            &self.config_dir,
            &self.data_dir,
            &self.cache_dir,
            &self.log_dir,
            &self.temp_dir,
        ] {
            fs::create_dir_all(d)?;
        }
        Ok(())
    }

    pub fn settings_file(&self) -> PathBuf {
        self.config_dir.join("settings.toml")
    }

    pub fn db_file(&self) -> PathBuf {
        self.data_dir.join("cobalt.db")
    }

    pub fn spill_dir(&self) -> PathBuf {
        self.temp_dir.join("spill")
    }

    /// Create the spill dir and delete stale `cobalt-spill-*` entries left by a previous run.
    /// Returns how many entries were removed. Never fails on a single undeletable file
    /// (another instance may still hold it open).
    pub fn clean_spill_dir(&self) -> Result<usize> {
        let dir = self.spill_dir();
        fs::create_dir_all(&dir)?;
        let mut removed = 0;
        for entry in fs::read_dir(&dir)? {
            let Ok(entry) = entry else { continue };
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if !name.starts_with(SPILL_PREFIX) {
                continue;
            }
            let path = entry.path();
            let res = if path.is_dir() {
                fs::remove_dir_all(&path)
            } else {
                fs::remove_file(&path)
            };
            match res {
                Ok(()) => removed += 1,
                Err(e) => {
                    tracing::debug!(path = %path.display(), error = %e, "could not remove stale spill file")
                }
            }
        }
        Ok(removed)
    }
}
