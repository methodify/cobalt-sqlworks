//! Crate-wide error type.

/// Every fallible operation in `cobalt-store` returns this.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("settings parse error: {0}")]
    TomlDe(#[from] toml::de::Error),
    #[error("settings serialize error: {0}")]
    TomlSer(#[from] toml::ser::Error),
    #[error("{0} not found")]
    NotFound(String),
    #[error("invalid operation: {0}")]
    Invalid(String),
    #[error("store lock poisoned by a panic on another thread")]
    Poisoned,
    #[error("bad timestamp in store: {0:?}")]
    Timestamp(String),
    #[error("Azure Data Studio import: {0}")]
    Ads(String),
    #[error("no per-user project directories are available on this platform")]
    NoProjectDirs,
}

pub type Result<T, E = StoreError> = std::result::Result<T, E>;
