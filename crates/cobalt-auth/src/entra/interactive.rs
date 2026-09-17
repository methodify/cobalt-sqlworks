//! Authorization-code + PKCE in the system browser with a loopback redirect (RFC 8252).

use super::loopback::Listener;
use super::pkce::{random_state, Pkce};
use super::token::{into_token_set, post_token};
use super::{CancelToken, EntraConfig, TokenSet};
use crate::{AuthError, Result};
use std::time::Duration;
use url::Url;

/// How long we wait for the user to finish in the browser.
pub const INTERACTIVE_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// Build the `/authorize` URL for one request.
pub(crate) fn authorize_url(cfg: &EntraConfig, redirect_uri: &str, state: &str, pkce: &Pkce, login_hint: Option<&str>) -> Result<Url> {
    let client_id = cfg.require_client_id()?;
    let mut url = Url::parse(&cfg.authorize_endpoint()).map_err(|e| AuthError::Other(format!("bad authority: {e}")))?;
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("client_id", client_id)
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("response_mode", "query")
            .append_pair("scope", &cfg.user_scopes())
            .append_pair("state", state)
            .append_pair("code_challenge", &pkce.challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("prompt", "select_account");
        if let Some(hint) = login_hint.map(str::trim).filter(|h| !h.is_empty()) {
            q.append_pair("login_hint", hint);
        }
    }
    Ok(url)
}

/// Sign in interactively.
///
/// 1. Binds a loopback listener on `127.0.0.1:{cfg.redirect_port or ephemeral}`.
/// 2. Calls `on_url` with the authorize URL; the caller opens it (see [`super::open_in_browser`]).
/// 3. Waits (in a blocking task) for the redirect, honouring `cancel` and [`INTERACTIVE_TIMEOUT`].
/// 4. Exchanges the code with the PKCE verifier at the token endpoint.
pub async fn interactive_login(
    cfg: &EntraConfig,
    login_hint: Option<&str>,
    on_url: impl Fn(&str),
    cancel: CancelToken,
) -> Result<TokenSet> {
    cfg.require_client_id()?;
    let listener = Listener::bind(cfg.redirect_port)?;
    let redirect_uri = listener.redirect_uri();
    let state = random_state();
    let pkce = Pkce::generate();
    let url = authorize_url(cfg, &redirect_uri, &state, &pkce, login_hint)?;

    tracing::info!(port = listener.port(), tenant = cfg.tenant(), "starting interactive Entra sign-in");
    on_url(url.as_str());

    let expected_state = state.clone();
    let cancel_for_task = cancel.clone();
    let wait = tokio::task::spawn_blocking(move || listener.wait_for_code(&expected_state, INTERACTIVE_TIMEOUT, &cancel_for_task));
    // A belt-and-braces outer timeout, slightly longer than the listener's own deadline.
    let redirect = match tokio::time::timeout(INTERACTIVE_TIMEOUT + Duration::from_secs(5), wait).await {
        Ok(Ok(r)) => r?,
        Ok(Err(join)) => return Err(AuthError::Other(format!("listener task failed: {join}"))),
        Err(_) => {
            cancel.cancel();
            return Err(AuthError::Timeout);
        }
    };
    if cancel.is_cancelled() {
        return Err(AuthError::Cancelled);
    }

    let scope = cfg.user_scopes();
    let form = [
        ("client_id", cfg.client_id.trim()),
        ("grant_type", "authorization_code"),
        ("code", redirect.code.as_str()),
        ("redirect_uri", redirect_uri.as_str()),
        ("code_verifier", pkce.verifier.as_str()),
        ("scope", scope.as_str()),
    ];
    let t = post_token(cfg, &form).await?;
    let set = into_token_set(cfg, t, None, None);
    tracing::info!(user = %set.account.username, "interactive Entra sign-in complete");
    Ok(set)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn authorize_url_has_every_parameter() {
        let mut cfg = EntraConfig::new("client-123");
        cfg.tenant = Some("contoso.onmicrosoft.com".into());
        let pkce = Pkce::from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk");
        let url = authorize_url(&cfg, "http://localhost:4242", "st4te", &pkce, Some("ada@contoso.com")).unwrap();
        assert_eq!(url.scheme(), "https");
        assert_eq!(url.host_str(), Some("login.microsoftonline.com"));
        assert_eq!(url.path(), "/contoso.onmicrosoft.com/oauth2/v2.0/authorize");
        let q: HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(q["client_id"], "client-123");
        assert_eq!(q["response_type"], "code");
        assert_eq!(q["response_mode"], "query");
        assert_eq!(q["redirect_uri"], "http://localhost:4242");
        assert_eq!(q["scope"], "https://database.windows.net/.default offline_access openid profile");
        assert_eq!(q["state"], "st4te");
        assert_eq!(q["code_challenge"], "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["prompt"], "select_account");
        assert_eq!(q["login_hint"], "ada@contoso.com");
    }

    #[test]
    fn authorize_url_omits_blank_hint_and_uses_default_tenant() {
        let cfg = EntraConfig::new("c");
        let url = authorize_url(&cfg, "http://localhost:1", "s", &Pkce::generate(), Some("  ")).unwrap();
        assert!(url.path().starts_with("/organizations/"));
        assert!(!url.query().unwrap().contains("login_hint"));
    }

    #[test]
    fn missing_client_id_is_rejected_early() {
        let cfg = EntraConfig::default();
        let e = authorize_url(&cfg, "http://localhost:1", "s", &Pkce::generate(), None).unwrap_err();
        assert!(matches!(e, AuthError::MissingClientId));
    }

    #[tokio::test]
    async fn interactive_login_without_client_id_fails_before_listening() {
        let cfg = EntraConfig::default();
        let e = interactive_login(&cfg, None, |_| panic!("must not open a browser"), CancelToken::new()).await.unwrap_err();
        assert!(matches!(e, AuthError::MissingClientId));
    }
}
