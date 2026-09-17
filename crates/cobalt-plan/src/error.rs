//! Error type for plan parsing.

use thiserror::Error;

/// Errors produced while turning showplan XML into a [`crate::Plan`].
#[derive(Debug, Error)]
pub enum PlanError {
    /// The input was empty (or only whitespace / BOM).
    #[error("empty showplan input")]
    Empty,
    /// The XML was malformed. The position is a byte offset into the input.
    #[error("malformed showplan XML at byte {position}: {message}")]
    Xml { position: u64, message: String },
    /// The document parsed as XML but is not a `<ShowPlanXML>` document.
    #[error("not a showplan document: {0}")]
    NotShowplan(String),
}
