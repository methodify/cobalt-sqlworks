use std::fmt;

/// Everything that can go wrong while resolving credentials.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    /// The OS credential store cannot be used on this machine (e.g. Linux without a
    /// Secret Service provider such as gnome-keyring or KWallet).
    #[error("the OS credential store is unavailable: {0}")]
    SecretStoreUnavailable(String),
    /// The credential store failed for a specific entry.
    #[error("credential store error: {0}")]
    Secret(String),
    #[error("HTTP error talking to the identity provider: {0}")]
    Http(#[from] reqwest::Error),
    /// The identity provider returned an OAuth error object.
    #[error("{error}: {description}")]
    Provider { error: String, description: String },
    /// A silent path (cached/refresh token) is exhausted; the user must sign in again.
    #[error("interactive sign-in required: {0}")]
    InteractionRequired(String),
    #[error("sign-in cancelled")]
    Cancelled,
    #[error("timed out waiting for sign-in")]
    Timeout,
    #[error("Azure CLI: {0}")]
    AzureCli(String),
    #[error("no Entra client ID is configured — set one under Settings → Connections → Entra client ID")]
    MissingClientId,
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, AuthError>;

impl AuthError {
    pub fn provider(error: impl Into<String>, description: impl Into<String>) -> Self {
        AuthError::Provider { error: error.into(), description: description.into() }
    }

    pub fn other(msg: impl fmt::Display) -> Self {
        AuthError::Other(msg.to_string())
    }

    /// Whether the UI should re-run the interactive flow rather than show a hard failure.
    pub fn needs_interaction(&self) -> bool {
        matches!(self, AuthError::InteractionRequired(_))
    }

    /// Short, actionable advice for the UI to show under the error message.
    pub fn hint(&self) -> Option<&'static str> {
        match self {
            AuthError::SecretStoreUnavailable(_) => Some(
                "Install a Secret Service provider (gnome-keyring or KWallet) or enable the encrypted-file fallback in Settings.",
            ),
            AuthError::MissingClientId => Some("Set an Entra app (client) ID under Settings → Connections."),
            AuthError::InteractionRequired(_) => Some("Sign in again in the browser."),
            AuthError::Timeout => Some("Try again, or use the device-code method if the browser can't reach this machine."),
            AuthError::AzureCli(msg) => {
                if msg.contains("not found") || msg.contains("not installed") {
                    Some("Install the Azure CLI (https://aka.ms/azcli) and run `az login`.")
                } else {
                    Some("Run `az login` (add `--tenant <id>` for a specific directory) and try again.")
                }
            }
            AuthError::Provider { error, description } => provider_hint(error, description),
            _ => None,
        }
    }
}

/// AADSTS codes that mean "this public client can't satisfy the tenant's policy".
const CONDITIONAL_ACCESS: &[&str] = &["AADSTS53000", "AADSTS53001", "AADSTS53003", "AADSTS530032", "AADSTS50076", "AADSTS50079", "AADSTS50074"];

fn provider_hint(error: &str, description: &str) -> Option<&'static str> {
    if CONDITIONAL_ACCESS.iter().any(|c| description.contains(c)) {
        return Some(
            "This tenant's Conditional Access or MFA policy rejected a plain browser sign-in. Try the Azure CLI (az login) or device-code method.",
        );
    }
    if description.contains("AADSTS700016") || description.contains("AADSTS90002") {
        return Some(
            "The app (client) ID isn't registered in this tenant, or the tenant is wrong. Check Settings → Connections → Entra client ID and the profile's tenant.",
        );
    }
    if description.contains("AADSTS50011") {
        return Some("The app registration needs `http://localhost` as a Mobile and desktop redirect URI.");
    }
    if description.contains("AADSTS65001") || description.contains("AADSTS65004") {
        return Some("Consent was not granted for Azure SQL Database access. An admin may need to grant it for this app.");
    }
    if description.contains("AADSTS7000218") {
        return Some("The app registration must be a public client (\"Allow public client flows\" = Yes).");
    }
    match error {
        "authorization_declined" => Some("The sign-in was declined in the browser."),
        "expired_token" => Some("The device code expired before it was entered. Start again."),
        "access_denied" => Some("Access was denied by the identity provider or the user."),
        _ => None,
    }
}
