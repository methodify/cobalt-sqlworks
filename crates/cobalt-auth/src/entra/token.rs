//! Token endpoint plumbing shared by every flow: POSTing forms, parsing success and error
//! bodies, mapping Entra error codes, and decoding JWT claims for display.

use super::{AccessToken, EntraAccount, EntraConfig, TokenSet};
use crate::{AuthError, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use chrono::{Duration, Utc};
use cobalt_core::Secret;
use serde::Deserialize;

/// Success body from `/oauth2/v2.0/token`.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct TokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub expires_in: Option<i64>,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub id_token: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
}

/// Error body from any Entra OAuth endpoint.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct OAuthErrorBody {
    #[serde(default)]
    pub error: String,
    #[serde(default)]
    pub error_description: String,
    #[serde(default)]
    pub error_codes: Vec<i64>,
}

/// Raw outcome of a token POST, before flow-specific interpretation (device code needs to see
/// `authorization_pending` etc. as data rather than as failures).
pub(crate) enum TokenReply {
    Ok(TokenResponse),
    Err(OAuthErrorBody),
}

pub(crate) async fn post_form(url: &str, form: &[(&str, &str)]) -> Result<TokenReply> {
    let resp = super::http_client().post(url).form(form).send().await?;
    let status = resp.status();
    let body = resp.bytes().await?;
    if status.is_success() {
        let ok: TokenResponse = serde_json::from_slice(&body)
            .map_err(|e| AuthError::Other(format!("malformed token response ({status}): {e}")))?;
        Ok(TokenReply::Ok(ok))
    } else {
        let err: OAuthErrorBody = serde_json::from_slice(&body).unwrap_or_else(|_| OAuthErrorBody {
            error: format!("http_{}", status.as_u16()),
            error_description: String::from_utf8_lossy(&body).chars().take(500).collect(),
            error_codes: vec![],
        });
        Ok(TokenReply::Err(err))
    }
}

/// POST to the token endpoint and treat any OAuth error as a failure.
pub(crate) async fn post_token(cfg: &EntraConfig, form: &[(&str, &str)]) -> Result<TokenResponse> {
    match post_form(&cfg.token_endpoint(), form).await? {
        TokenReply::Ok(t) => Ok(t),
        TokenReply::Err(e) => Err(map_oauth_error(e)),
    }
}

/// AADSTS codes whose meaning is "come back through the interactive flow".
const INTERACTION_CODES: &[i64] = &[
    50076,  // MFA required (conditional access)
    50079,  // MFA enrollment required
    53003,  // blocked by conditional access
    50173,  // fresh auth token needed (password change etc.)
    70008,  // refresh token expired
    700082, // refresh token expired (inactivity)
    50078,  // MFA required
    50074,  // strong auth required
    65001,  // consent required
    50058,  // silent sign-in failed
    50133,  // session revoked
    50089,  // flow token expired
    16000,  // interaction required (generic)
];

pub(crate) fn map_oauth_error(e: OAuthErrorBody) -> AuthError {
    let desc = if e.error_description.is_empty() { e.error.clone() } else { e.error_description.clone() };
    let interactive_by_code = e.error_codes.iter().any(|c| INTERACTION_CODES.contains(c))
        || INTERACTION_CODES.iter().any(|c| desc.contains(&format!("AADSTS{c}")));
    match e.error.as_str() {
        "interaction_required" | "invalid_grant" | "login_required" | "consent_required" => {
            AuthError::InteractionRequired(desc)
        }
        _ if interactive_by_code => AuthError::InteractionRequired(desc),
        _ => AuthError::Provider { error: e.error, description: desc },
    }
}

/// Assemble a [`TokenSet`] from a token response. `previous_refresh` is kept when the response
/// carries no new refresh token (Entra usually rotates, but not always). `fallback_account` is
/// used when no id_token and no usable access-token claims came back.
pub(crate) fn into_token_set(
    cfg: &EntraConfig,
    t: TokenResponse,
    previous_refresh: Option<&Secret>,
    fallback_account: Option<&EntraAccount>,
) -> TokenSet {
    let expires_in = t.expires_in.unwrap_or(3600).clamp(0, 86_400 * 7);
    let access = AccessToken {
        token: Secret::new(t.access_token.clone()),
        expires_at: Utc::now() + Duration::seconds(expires_in),
        scope: t.scope.clone().unwrap_or_else(|| cfg.user_scopes()),
    };
    let refresh = t.refresh_token.map(Secret::new).or_else(|| previous_refresh.cloned());
    let account = t
        .id_token
        .as_deref()
        .and_then(account_from_jwt)
        .or_else(|| account_from_jwt(&t.access_token))
        .or_else(|| fallback_account.cloned())
        .unwrap_or_default();
    TokenSet { access, refresh, account }
}

/// Decode a JWT's payload segment as JSON. **No signature check**: the caller only displays
/// these claims; the token itself is validated by SQL Server / the IdP.
pub fn decode_jwt_claims(jwt: &str) -> Option<serde_json::Value> {
    let payload = jwt.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Pull the display claims out of an id_token (or, as a fallback, an access token).
pub(crate) fn account_from_jwt(jwt: &str) -> Option<EntraAccount> {
    let c = decode_jwt_claims(jwt)?;
    let s = |k: &str| c.get(k).and_then(|v| v.as_str()).map(str::to_owned);
    let username = s("preferred_username").or_else(|| s("upn")).or_else(|| s("unique_name")).or_else(|| s("email"))?;
    let tid = s("tid");
    let oid = s("oid").or_else(|| s("sub"));
    let home_account_id = match (&oid, &tid) {
        (Some(o), Some(t)) => Some(format!("{o}.{t}")),
        (Some(o), None) => Some(o.clone()),
        _ => None,
    };
    Some(EntraAccount { username, name: s("name"), tenant_id: tid, home_account_id })
}

/// Silent renewal. Sends `grant_type=refresh_token`; the returned set keeps the old refresh
/// token if the IdP did not rotate it. Errors that mean "sign in again" surface as
/// [`AuthError::InteractionRequired`].
pub async fn refresh(cfg: &EntraConfig, refresh_token: &Secret) -> Result<TokenSet> {
    let client_id = cfg.require_client_id()?;
    let scope = cfg.user_scopes();
    let form = [
        ("client_id", client_id),
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token.expose()),
        ("scope", scope.as_str()),
    ];
    let t = post_token(cfg, &form).await?;
    Ok(into_token_set(cfg, t, Some(refresh_token), None))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_jwt(claims: serde_json::Value) -> String {
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"none","typ":"JWT"}"#);
        let payload = URL_SAFE_NO_PAD.encode(claims.to_string());
        format!("{header}.{payload}.sig")
    }

    #[test]
    fn id_token_decodes_to_account() {
        let jwt = fake_jwt(serde_json::json!({
            "preferred_username": "ada@contoso.com", "name": "Ada Lovelace",
            "tid": "11111111-2222-3333-4444-555555555555", "oid": "aaaa-bbbb"
        }));
        let a = account_from_jwt(&jwt).unwrap();
        assert_eq!(a.username, "ada@contoso.com");
        assert_eq!(a.name.as_deref(), Some("Ada Lovelace"));
        assert_eq!(a.tenant_id.as_deref(), Some("11111111-2222-3333-4444-555555555555"));
        assert_eq!(a.home_account_id.as_deref(), Some("aaaa-bbbb.11111111-2222-3333-4444-555555555555"));
    }

    #[test]
    fn access_token_upn_is_a_fallback() {
        let jwt = fake_jwt(serde_json::json!({ "upn": "bob@contoso.com", "tid": "t1", "oid": "o1" }));
        let a = account_from_jwt(&jwt).unwrap();
        assert_eq!(a.username, "bob@contoso.com");
        assert_eq!(a.name, None);
    }

    #[test]
    fn garbage_jwt_is_none() {
        assert!(decode_jwt_claims("not.a.jwt").is_none());
        assert!(decode_jwt_claims("").is_none());
        assert!(account_from_jwt(&fake_jwt(serde_json::json!({ "sub": "x" }))).is_none());
    }

    #[test]
    fn padded_jwt_payload_decodes() {
        // Some encoders leave '=' padding on; we must accept it.
        let header = URL_SAFE_NO_PAD.encode(b"{}");
        let payload = base64::engine::general_purpose::URL_SAFE.encode(br#"{"preferred_username":"p@x.y"}"#);
        assert!(payload.ends_with('='));
        let a = account_from_jwt(&format!("{header}.{payload}.s")).unwrap();
        assert_eq!(a.username, "p@x.y");
    }

    #[test]
    fn error_mapping_interaction_required() {
        let body = |error: &str, desc: &str, codes: Vec<i64>| OAuthErrorBody {
            error: error.into(),
            error_description: desc.into(),
            error_codes: codes,
        };
        assert!(matches!(map_oauth_error(body("invalid_grant", "AADSTS70008: expired", vec![70008])), AuthError::InteractionRequired(_)));
        assert!(matches!(map_oauth_error(body("interaction_required", "AADSTS50076: MFA", vec![50076])), AuthError::InteractionRequired(_)));
        // code only in the description
        assert!(matches!(map_oauth_error(body("access_denied", "AADSTS53003: blocked by CA", vec![])), AuthError::InteractionRequired(_)));
        // code only in error_codes
        assert!(matches!(map_oauth_error(body("access_denied", "blocked", vec![50079])), AuthError::InteractionRequired(_)));
        // unrelated → Provider
        match map_oauth_error(body("invalid_client", "AADSTS7000218: public client", vec![7000218])) {
            AuthError::Provider { error, description } => {
                assert_eq!(error, "invalid_client");
                assert!(description.contains("AADSTS7000218"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn provider_hints_for_common_aadsts() {
        let e = AuthError::provider("access_denied", "AADSTS53003: Access has been blocked by Conditional Access policies");
        assert!(e.hint().unwrap().contains("device-code"));
        let e = AuthError::provider("unauthorized_client", "AADSTS700016: Application not found in the directory");
        assert!(e.hint().unwrap().contains("client ID"));
        let e = AuthError::provider("invalid_request", "AADSTS50011: The redirect URI does not match");
        assert!(e.hint().unwrap().contains("localhost"));
        assert!(AuthError::MissingClientId.hint().is_some());
        assert!(AuthError::AzureCli("az not found on PATH".into()).hint().unwrap().contains("Install"));
        assert!(AuthError::AzureCli("Please run 'az login'".into()).hint().unwrap().contains("az login"));
        assert!(AuthError::Cancelled.hint().is_none());
    }

    #[test]
    fn token_set_keeps_old_refresh_when_absent() {
        let cfg = EntraConfig::new("cid");
        let old = Secret::new("old-rt");
        let t = TokenResponse { access_token: "at".into(), expires_in: Some(10), refresh_token: None, id_token: None, scope: None };
        let ts = into_token_set(&cfg, t, Some(&old), None);
        assert_eq!(ts.refresh.as_ref().unwrap().expose(), "old-rt");
        assert_eq!(ts.access.scope, cfg.user_scopes());
        assert!(ts.access.is_valid_for(std::time::Duration::from_secs(5)));
        assert!(!ts.access.is_valid_for(std::time::Duration::from_secs(30)));
        assert_eq!(ts.account, EntraAccount::default());

        let t = TokenResponse { access_token: "at".into(), expires_in: None, refresh_token: Some("new-rt".into()), id_token: None, scope: Some("s".into()) };
        let fallback = EntraAccount { username: "prev@x.y".into(), ..Default::default() };
        let ts = into_token_set(&cfg, t, Some(&old), Some(&fallback));
        assert_eq!(ts.refresh.as_ref().unwrap().expose(), "new-rt");
        assert_eq!(ts.account.username, "prev@x.y");
    }
}
