//! Reuse an existing `az login` session through `azure_identity::AzureCliCredential`.

use super::{AccessToken, EntraConfig};
use crate::{AuthError, Result};
use azure_core::credentials::TokenCredential;
use azure_identity::{AzureCliCredential, AzureCliCredentialOptions};
use chrono::{DateTime, Utc};
use cobalt_core::Secret;

/// Get a SQL-resource token from the Azure CLI (`az account get-access-token`).
pub async fn azure_cli_token(tenant: Option<&str>) -> Result<AccessToken> {
    azure_cli_token_for_scope(tenant, &EntraConfig::default().sql_scope()).await
}

pub async fn azure_cli_token_for_scope(tenant: Option<&str>, scope: &str) -> Result<AccessToken> {
    let tenant_id = tenant
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_owned);
    let cred = AzureCliCredential::new(Some(AzureCliCredentialOptions {
        tenant_id,
        ..Default::default()
    }))
    .map_err(|e| AuthError::AzureCli(e.to_string()))?;
    let tok = cred
        .get_token(&[scope], None)
        .await
        .map_err(map_cli_error)?;
    let expires_at = DateTime::<Utc>::from_timestamp(tok.expires_on.unix_timestamp(), 0)
        .unwrap_or_else(Utc::now);
    Ok(AccessToken {
        token: Secret::new(tok.token.secret()),
        expires_at,
        scope: scope.to_owned(),
    })
}

/// Turn azure_identity's error text into something a user can act on.
pub(crate) fn map_cli_error(e: azure_core::Error) -> AuthError {
    AuthError::AzureCli(classify_cli_message(&e.to_string()))
}

pub(crate) fn classify_cli_message(msg: &str) -> String {
    let lower = msg.to_ascii_lowercase();
    let not_installed = [
        "not found",
        "no such file",
        "cannot find",
        "not recognized",
        "program not found",
        "os error 2",
    ]
    .iter()
    .any(|s| lower.contains(s));
    if not_installed {
        return "the `az` command was not found on PATH — the Azure CLI is not installed (or not on PATH for this app)".into();
    }
    if lower.contains("az login") || lower.contains("not logged in") || lower.contains("please run")
    {
        return "not signed in — run `az login` (add `--tenant <id>` for a specific directory) and try again".into();
    }
    if lower.contains("aadsts") {
        // Preserve the AADSTS text so the hint machinery can classify it.
        return format!("token request failed: {}", msg.trim());
    }
    msg.trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_messages_are_classified() {
        assert!(classify_cli_message("program not found").contains("not found on PATH"));
        assert!(
            classify_cli_message("'az' is not recognized as an internal or external command")
                .contains("not found on PATH")
        );
        assert!(classify_cli_message(
            "AzureCliCredential authentication failed. Please run 'az login' to setup account."
        )
        .contains("az login"));
        assert!(classify_cli_message("  something else  ") == "something else");
        let e = AuthError::AzureCli(classify_cli_message("ERROR: please run 'az login'"));
        assert!(e.hint().unwrap().contains("az login"));
    }

    #[tokio::test]
    async fn cli_rejects_bad_tenant_cleanly() {
        // azure_identity validates the tenant id before spawning anything.
        let e = azure_cli_token(Some("not a tenant; rm -rf /"))
            .await
            .unwrap_err();
        assert!(matches!(e, AuthError::AzureCli(_)), "{e:?}");
    }
}
