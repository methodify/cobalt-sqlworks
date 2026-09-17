//! Microsoft Fabric REST client for the Fabric explorer: list the workspaces a user can reach,
//! the SQL-capable items in them, and the connection details needed to open a query tab.
//!
//! Read-only. One bearer token (audience `https://api.fabric.microsoft.com`) is supplied per call
//! batch by the caller; this crate knows nothing about Entra. Pagination and `Retry-After` are
//! handled here. Everything is modelled from the v1 REST reference (2026-09).

pub mod client;
pub mod model;

pub use client::{FabricClient, FabricError};
pub use model::*;
