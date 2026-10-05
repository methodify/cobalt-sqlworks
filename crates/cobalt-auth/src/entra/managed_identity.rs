//! Azure managed identity: a token from the instance metadata service (IMDS) on an Azure VM /
//! VMSS / AKS node, or from the App Service / Functions identity endpoint
//! (`IDENTITY_ENDPOINT` + `IDENTITY_HEADER`). No interaction, no secrets on disk.

use crate::entra::AccessToken;
use crate::error::{AuthError, Result};
use chrono::{DateTime, Duration, Utc};
use cobalt_core::Secret;
use serde::Deserialize;

const IMDS_URL: &str = "http://169.254.169.254/metadata/identity/oauth2/token";

#[derive(Deserialize)]
struct ImdsToken {
    access_token: String,
    /// Seconds since the epoch (IMDS sends it as a string).
    expires_on: Option<String>,
    expires_in: Option<String>,
}

/// `client_id` selects a user-assigned identity; `None` uses the system-assigned one.
pub async fn managed_identity_token(resource: &str, client_id: Option<&str>) -> Result<AccessToken> {
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(5)).build().map_err(|e| AuthError::Other(e.to_string()))?;
    let (endpoint, header, api_version) = match (std::env::var("IDENTITY_ENDPOINT"), std::env::var("IDENTITY_HEADER")) {
        (Ok(ep), Ok(h)) if !ep.is_empty() => (ep, Some(("X-IDENTITY-HEADER", h)), "2019-08-01"),
        _ => (IMDS_URL.to_string(), None, "2018-02-01"),
    };
    let mut req = client.get(&endpoint).query(&[("api-version", api_version), ("resource", resource)]);
    if let Some(id) = client_id.map(str::trim).filter(|s| !s.is_empty()) {
        req = req.query(&[("client_id", id)]);
    }
    req = match header {
        Some((name, value)) => req.header(name, value),
        None => req.header("Metadata", "true"),
    };
    let resp = req.send().await.map_err(|e| {
        if e.is_timeout() || e.is_connect() {
            AuthError::Other("no managed identity endpoint answered: this only works on an Azure VM, App Service, Functions or AKS with an identity assigned".into())
        } else {
            AuthError::Other(format!("managed identity request failed: {e}"))
        }
    })?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(AuthError::Other(format!("managed identity endpoint returned {status}: {}", body.chars().take(300).collect::<String>())));
    }
    let t: ImdsToken = serde_json::from_str(&body).map_err(|e| AuthError::Other(format!("unexpected managed identity response: {e}")))?;
    let expires_at = t
        .expires_on
        .as_deref()
        .and_then(|s| s.parse::<i64>().ok())
        .and_then(|secs| DateTime::<Utc>::from_timestamp(secs, 0))
        .or_else(|| t.expires_in.as_deref().and_then(|s| s.parse::<i64>().ok()).map(|s| Utc::now() + Duration::seconds(s)))
        .unwrap_or_else(|| Utc::now() + Duration::seconds(3600));
    Ok(AccessToken { token: Secret::new(t.access_token), expires_at, scope: resource.to_string() })
}
