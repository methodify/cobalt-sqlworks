//! Notebook documents for Cobalt SQL Works.
//!
//! The on-disk form is nbformat 4 (`.ipynb`), read and written with every metadata key preserved
//! so a notebook round-trips through the Fabric portal, Git and Azure Data Studio unchanged. The
//! Fabric Git form (`notebook-content.py`) is a second reader/writer. Outputs follow nbformat
//! (`stream`, `execute_result`, `display_data`, `error`); Cobalt adds a tabular output carrying
//! an Arrow IPC stream (`application/vnd.apache.arrow.stream`, base64) next to HTML, Markdown and
//! plain-text previews, so other tools still render something.
//!
//! No egui, no tokio, no Arrow types: the app layer turns result sets into bytes and back.

pub mod export;
pub mod fabric_py;
pub mod ipynb;
pub mod model;

pub use model::*;

#[derive(Debug, thiserror::Error)]
pub enum NotebookError {
    #[error("not a notebook: {0}")]
    Format(String),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, NotebookError>;
