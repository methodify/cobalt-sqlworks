//! Service principal: OAuth 2.0 client-credentials grant with a client secret.
//!
//! Done as a direct POST rather than `azure_identity::ClientSecretCredential` so the endpoint
//! can be redirected at a mock in tests and errors map through the same AADSTS table.

use super::token::post_token;
use super::{AccessToken, EntraConfig};
use crate::{AuthError, Result};
use chrono::{Duration, Utc};
use cobalt_core::Secret;

/// Acquire a SQL-resource token for an app registration using its client secret.
/// `tenant` must be a real tenant (GUID or domain): `common`/`organizations` are not valid here.
pub async fn client_secret_token(
    cfg: &EntraConfig,
    tenant: &str,
    client_id: &str,
    secret: &Secret,
) -> Result<AccessToken> {
    let tenant = tenant.trim();
    if tenant.is_empty() || matches!(tenant, "common" | "organizations" | "consumers") {
        return Err(AuthError::Other(
            "a service principal needs a specific tenant ID or domain".into(),
        ));
    }
    let client_id = client_id.trim();
    if client_id.is_empty() {
        return Err(AuthError::Other(
            "service principal client ID is empty".into(),
        ));
    }
    if secret.is_empty() {
        return Err(AuthError::Other("service principal secret is empty".into()));
    }
    let cfg = EntraConfig {
        client_id: client_id.to_owned(),
        tenant: Some(tenant.to_owned()),
        ..cfg.clone()
    };
    let scope = cfg.sql_scope();
    let form = [
        ("client_id", client_id),
        ("client_secret", secret.expose()),
        ("grant_type", "client_credentials"),
        ("scope", scope.as_str()),
    ];
    let t = post_token(&cfg, &form).await?;
    Ok(AccessToken {
        token: Secret::new(t.access_token),
        expires_at: Utc::now() + Duration::seconds(t.expires_in.unwrap_or(3600).clamp(0, 86_400)),
        scope: t.scope.unwrap_or(scope),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_multi_tenant_aliases_and_blanks() {
        let cfg = EntraConfig::default();
        for t in ["", "common", "organizations", "consumers"] {
            let e = client_secret_token(&cfg, t, "cid", &Secret::new("s"))
                .await
                .unwrap_err();
            assert!(e.to_string().contains("tenant"), "{t}: {e}");
        }
        assert!(client_secret_token(&cfg, "tid", "", &Secret::new("s"))
            .await
            .is_err());
        assert!(client_secret_token(&cfg, "tid", "cid", &Secret::new(""))
            .await
            .is_err());
    }
}
