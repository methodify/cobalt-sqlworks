//! OAuth 2.0 device authorization grant (RFC 8628) — for RDP/SSH sessions with no local browser.

use super::token::{into_token_set, map_oauth_error, post_form, TokenReply};
use super::{CancelToken, EntraConfig, TokenSet};
use crate::{AuthError, Result};
use serde::Deserialize;
use std::time::{Duration, Instant};

/// What the UI shows the user: "go to `verification_uri` and enter `user_code`".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceCodePrompt {
    pub user_code: String,
    pub verification_uri: String,
    /// Entra's ready-made sentence ("To sign in, use a web browser to open ...").
    pub message: String,
    pub expires_in: Duration,
}

#[derive(Debug, Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    interval: Option<u64>,
    #[serde(default)]
    message: Option<String>,
}

/// Start the device-code flow, call `on_prompt` once with the code to show, then poll the token
/// endpoint until the user completes, declines, the code expires, or `cancel` flips.
pub async fn device_code_login(
    cfg: &EntraConfig,
    on_prompt: impl Fn(&DeviceCodePrompt),
    cancel: CancelToken,
) -> Result<TokenSet> {
    let client_id = cfg.require_client_id()?;
    let scope = cfg.user_scopes();

    let resp = super::http_client()
        .post(cfg.device_code_endpoint())
        .form(&[("client_id", client_id), ("scope", scope.as_str())])
        .send()
        .await?;
    let status = resp.status();
    let body = resp.bytes().await?;
    if !status.is_success() {
        let err: super::token::OAuthErrorBody = serde_json::from_slice(&body).unwrap_or_default();
        return Err(map_oauth_error(err));
    }
    let dc: DeviceCodeResponse = serde_json::from_slice(&body)
        .map_err(|e| AuthError::Other(format!("malformed devicecode response: {e}")))?;

    let expires_in = Duration::from_secs(dc.expires_in.unwrap_or(900));
    let mut interval = Duration::from_secs(dc.interval.unwrap_or(5));
    let prompt = DeviceCodePrompt {
        user_code: dc.user_code.clone(),
        verification_uri: dc.verification_uri.clone(),
        message: dc.message.clone().unwrap_or_else(|| {
            format!(
                "To sign in, open {} in a web browser and enter the code {} to authenticate.",
                dc.verification_uri, dc.user_code
            )
        }),
        expires_in,
    };
    tracing::info!(code = %prompt.user_code, "device-code sign-in started");
    on_prompt(&prompt);

    let deadline = Instant::now() + expires_in;
    let form = [
        ("client_id", client_id),
        ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
        ("device_code", dc.device_code.as_str()),
    ];
    loop {
        sleep_cancellable(interval, &cancel).await?;
        if Instant::now() >= deadline {
            return Err(AuthError::Timeout);
        }
        match post_form(&cfg.token_endpoint(), &form).await? {
            TokenReply::Ok(t) => {
                let set = into_token_set(cfg, t, None, None);
                tracing::info!(user = %set.account.username, "device-code sign-in complete");
                return Ok(set);
            }
            TokenReply::Err(e) => match e.error.as_str() {
                "authorization_pending" => continue,
                "slow_down" => {
                    interval += Duration::from_secs(5);
                    continue;
                }
                "expired_token" => return Err(AuthError::Timeout),
                "authorization_declined" => {
                    return Err(AuthError::Provider {
                        error: e.error,
                        description: if e.error_description.is_empty() {
                            "the sign-in was declined".into()
                        } else {
                            e.error_description
                        },
                    })
                }
                _ => return Err(map_oauth_error(e)),
            },
        }
    }
}

/// Sleep `d`, waking early (with `Cancelled`) if the token flips.
async fn sleep_cancellable(d: Duration, cancel: &CancelToken) -> Result<()> {
    let end = Instant::now() + d;
    loop {
        if cancel.is_cancelled() {
            return Err(AuthError::Cancelled);
        }
        let now = Instant::now();
        if now >= end {
            return Ok(());
        }
        tokio::time::sleep((end - now).min(Duration::from_millis(100))).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancel_interrupts_sleep() {
        let c = CancelToken::new();
        let c2 = c.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            c2.cancel();
        });
        let started = Instant::now();
        let e = sleep_cancellable(Duration::from_secs(10), &c)
            .await
            .unwrap_err();
        assert!(matches!(e, AuthError::Cancelled));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[tokio::test]
    async fn device_code_requires_client_id() {
        let e = device_code_login(&EntraConfig::default(), |_| {}, CancelToken::new())
            .await
            .unwrap_err();
        assert!(matches!(e, AuthError::MissingClientId));
    }
}
