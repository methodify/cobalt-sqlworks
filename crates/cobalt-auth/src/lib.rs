//! Authentication for Cobalt SQL Works: Microsoft Entra ID (authorization-code + PKCE with a
//! loopback redirect, device code, Azure CLI, service principal), SQL logins, Windows integrated,
//! and secret storage in the OS credential store.
//!
//! See `docs/decisions/D004_entra_auth.md`. The OAuth flows are hand-rolled over `reqwest`
//! against the Entra v2.0 endpoints; `azure_identity` is used only for the `az login` path.
//!
//! Layout:
//! - [`secrets`]  — [`SecretStore`] trait + keyring / memory / file-fallback stores
//! - [`entra`]    — Entra config, token types, and the four token-acquisition flows
//! - [`provider`] — [`CredentialResolver`]: the per-profile policy (cache → refresh → prompt)

pub mod entra;
pub mod error;
pub mod provider;
pub mod secrets;

pub use entra::{
    azure_cli_token, client_secret_token, device_code_login, interactive_login, open_in_browser, refresh, AccessToken,
    CancelToken, DeviceCodePrompt, EntraAccount, EntraConfig, TokenSet,
};
pub use error::{AuthError, Result};
pub use provider::{CredentialResolver, HeadlessPrompter, Prompter};
pub use secrets::{default_secret_store, FileFallbackStore, KeyringStore, MemoryStore, SecretStore};
