//! Shared, dependency-light types for Cobalt SQL Works.
//!
//! Everything here is plain data: no egui, no tokio, no driver types. Every other crate
//! depends on this one; this one depends on nothing of ours.

pub mod catalog;
pub mod engine;
pub mod ids;
pub mod message;
pub mod profile;
pub mod secret;
pub mod settings;
pub mod sqltype;

pub use catalog::*;
pub use engine::*;
pub use ids::*;
pub use message::*;
pub use profile::*;
pub use secret::*;
pub use settings::*;
pub use sqltype::*;

/// Application identity used for config dirs, keyring service names, pipe names.
pub const APP_QUALIFIER: &str = "dev";
pub const APP_ORG: &str = "Cobalt";
pub const APP_NAME: &str = "Cobalt SQL Works";
pub const KEYRING_SERVICE: &str = "cobalt-sqlworks";
pub const AGENT_PIPE_NAME: &str = "cobalt.agent";
