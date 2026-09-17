//! Local SQLite store: connection library, query history, tab snapshots, catalog cache.
//!
//! Everything here is synchronous. [`Store`] wraps a single `rusqlite::Connection` in a
//! mutex, so it is `Send + Sync` and can live in an `Arc` shared between the UI thread and
//! worker threads. Secrets never enter the database: profiles carry `SecretRef`s that
//! point at the OS keyring.
//!
//! Modules:
//! - [`paths`]: per-user config/data/cache/log/temp directories.
//! - [`settings`]: `settings.toml` load/save.
//! - [`store`]: the SQLite database (schema v1) and its API.
//! - [`ads_import`]: read Azure Data Studio's `settings.json` into a [`LibraryExport`].

pub mod ads_import;
pub mod error;
pub mod paths;
pub mod settings;
pub mod store;

pub use ads_import::{
    default_ads_settings_path, parse_ads_settings, parse_ads_settings_detailed, strip_jsonc,
    AdsImport,
};
pub use error::{Result, StoreError};
pub use paths::{AppPaths, SPILL_PREFIX};
pub use settings::{load_settings, save_settings};
pub use store::history::{HistoryEntry, HistoryQuery, HistoryStatus, NewHistoryEntry};
pub use store::library::{ImportSummary, LibraryExport};
pub use store::tabs::TabSnapshot;
pub use store::{Store, MIGRATIONS, SCHEMA_VERSION};
