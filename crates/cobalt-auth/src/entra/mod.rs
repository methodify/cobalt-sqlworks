//! Microsoft Entra ID token acquisition against the v2.0 endpoints.
//!
//! Flows (each returns a [`TokenSet`] or [`AccessToken`]):
//! - [`interactive_login`]: authorization-code + PKCE in the system browser, loopback redirect
//! - [`refresh`]: silent renewal from a stored refresh token
//! - [`device_code_login`]: for remote/RDP sessions without a local browser
//! - [`azure_cli_token`]: reuse `az login` via `azure_identity`
//! - [`client_secret_token`]: service principal (client credentials)
//!
//! All HTTP goes through one shared `reqwest::Client`. The authority is overridable so tests
//! can point the flows at a local mock identity provider.

mod azure_cli;
mod client_secret;
mod device_code;
mod interactive;
pub(crate) mod loopback;
pub mod pkce;
mod token;

pub use azure_cli::azure_cli_token;
pub use client_secret::client_secret_token;
pub use device_code::{device_code_login, DeviceCodePrompt};
pub use interactive::{interactive_login, INTERACTIVE_TIMEOUT};
pub use token::{decode_jwt_claims, refresh};

use crate::{AuthError, Result};
use chrono::{DateTime, Utc};
use cobalt_core::Secret;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use url::Url;

/// Default authority host (public cloud). Sovereign clouds: `login.microsoftonline.us`,
/// `login.partner.microsoftonline.cn`.
pub const DEFAULT_AUTHORITY_HOST: &str = "login.microsoftonline.com";
/// Default tenant: any work or school account (no personal Microsoft accounts).
pub const DEFAULT_TENANT: &str = "organizations";
/// The Azure SQL / Fabric SQL resource. Its `.default` scope is what the TDS login wants.
pub const SQL_RESOURCE: &str = "https://database.windows.net/";
/// OIDC scopes we always ask for: a refresh token and enough of an id_token to show who's signed in.
pub const OIDC_SCOPES: &str = "offline_access openid profile";
pub const USER_AGENT: &str = "CobaltSQLWorks/0.1";

/// How a token request is addressed. Built from user settings plus the profile's tenant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntraConfig {
    /// Public-client application (client) ID. Empty means "not configured".
    pub client_id: String,
    /// `organizations` (default), `common`, `consumers`, a tenant GUID, or a verified domain.
    pub tenant: Option<String>,
    /// Host name (`login.microsoftonline.com`) or, for tests and unusual setups, a full base URL
    /// with scheme (`http://127.0.0.1:4321`).
    pub authority_host: String,
    /// Fixed loopback port for the interactive redirect; `None` = ephemeral.
    pub redirect_port: Option<u16>,
    /// Resource whose `.default` scope is requested, e.g. `https://database.windows.net/`.
    pub sql_resource: String,
    /// Send token requests here instead of `{authority}/oauth2/v2.0/token` (tests).
    pub token_endpoint_override: Option<Url>,
}

impl Default for EntraConfig {
    fn default() -> Self {
        Self {
            client_id: String::new(),
            tenant: None,
            authority_host: DEFAULT_AUTHORITY_HOST.into(),
            redirect_port: None,
            sql_resource: SQL_RESOURCE.into(),
            token_endpoint_override: None,
        }
    }
}

impl EntraConfig {
    pub fn new(client_id: impl Into<String>) -> Self {
        Self {
            client_id: client_id.into(),
            ..Default::default()
        }
    }

    /// Build from the persisted user settings.
    pub fn from_settings(s: &cobalt_core::ConnectionSettings) -> Self {
        Self {
            client_id: s.effective_entra_client_id().to_owned(),
            tenant: s
                .entra_default_tenant
                .as_deref()
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(str::to_owned),
            redirect_port: s.entra_redirect_port,
            ..Default::default()
        }
    }

    /// A copy with the tenant replaced when the profile specifies one.
    /// The same settings aimed at another resource (`https://api.fabric.microsoft.com`, …).
    pub fn with_resource(&self, resource: &str) -> Self {
        let mut c = self.clone();
        c.sql_resource = resource.to_string();
        c
    }

    pub fn with_tenant(&self, tenant: Option<&str>) -> Self {
        let mut c = self.clone();
        if let Some(t) = tenant.map(str::trim).filter(|t| !t.is_empty()) {
            c.tenant = Some(t.to_owned());
        }
        c
    }

    pub fn tenant(&self) -> &str {
        self.tenant
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .unwrap_or(DEFAULT_TENANT)
    }

    /// `https://login.microsoftonline.com/{tenant}` (no trailing slash).
    pub fn authority(&self) -> String {
        let host = self.authority_host.trim().trim_end_matches('/');
        let base = if host.contains("://") {
            host.to_owned()
        } else {
            format!("https://{host}")
        };
        format!("{base}/{}", self.tenant())
    }

    pub fn authorize_endpoint(&self) -> String {
        format!("{}/oauth2/v2.0/authorize", self.authority())
    }

    pub fn token_endpoint(&self) -> String {
        match &self.token_endpoint_override {
            Some(u) => u.to_string(),
            None => format!("{}/oauth2/v2.0/token", self.authority()),
        }
    }

    pub fn device_code_endpoint(&self) -> String {
        format!("{}/oauth2/v2.0/devicecode", self.authority())
    }

    /// `https://database.windows.net/.default`
    pub fn sql_scope(&self) -> String {
        let r = self.sql_resource.trim();
        if r.ends_with("/.default") {
            r.to_owned()
        } else {
            format!("{}/.default", r.trim_end_matches('/'))
        }
    }

    /// The scope string sent on interactive / device-code / refresh requests.
    pub fn user_scopes(&self) -> String {
        format!("{} {OIDC_SCOPES}", self.sql_scope())
    }

    pub(crate) fn require_client_id(&self) -> Result<&str> {
        let id = self.client_id.trim();
        if id.is_empty() {
            Err(AuthError::MissingClientId)
        } else {
            Ok(id)
        }
    }
}

/// Who signed in, from the id_token (or the access token's claims when no id_token came back).
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EntraAccount {
    /// `preferred_username` (usually the UPN / e-mail).
    pub username: String,
    /// Display name.
    pub name: Option<String>,
    /// Tenant the account authenticated in (`tid`).
    pub tenant_id: Option<String>,
    /// MSAL-style `{oid}.{tid}`; stable across sign-ins.
    pub home_account_id: Option<String>,
}

#[derive(Clone, Debug)]
pub struct AccessToken {
    pub token: Secret,
    pub expires_at: DateTime<Utc>,
    /// Space-separated scopes actually granted.
    pub scope: String,
}

impl AccessToken {
    /// True when the token is still good for at least `margin` more.
    pub fn is_valid_for(&self, margin: Duration) -> bool {
        let margin = chrono::Duration::from_std(margin).unwrap_or(chrono::Duration::zero());
        self.expires_at - margin > Utc::now()
    }
}

#[derive(Clone, Debug)]
pub struct TokenSet {
    pub access: AccessToken,
    pub refresh: Option<Secret>,
    pub account: EntraAccount,
}

/// A cheap, clonable cancellation flag shared between the UI and a running flow.
#[derive(Clone, Debug, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// The process-wide HTTP client used for every identity-provider call.
pub fn http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .use_native_tls()
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(60))
            .build()
            .expect("reqwest client")
    })
}

/// Open a URL in the system browser without waiting for it. The default `on_url` for the
/// interactive flow when the UI has nothing fancier.
pub fn open_in_browser(url: &str) {
    if let Err(e) = open::that_detached(url) {
        tracing::warn!(error = %e, "could not open the system browser; the sign-in URL was {url}");
    }
}
