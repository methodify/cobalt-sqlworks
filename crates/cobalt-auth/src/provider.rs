//! The per-profile credential policy: what to do when a connection needs credentials.
//!
//! - SQL login: stored password → prompt.
//! - Entra interactive / device code: unexpired cached token → silent refresh from the stored
//!   refresh token → interactive flow (browser or device code).
//! - Azure CLI, Windows integrated, service principal: straight through.

use crate::entra::{
    azure_cli_token, client_secret_token, device_code_login, interactive_login, refresh, CancelToken, DeviceCodePrompt,
    EntraAccount, EntraConfig, TokenSet,
};
use crate::secrets::SecretStore;
use crate::{AuthError, Result};
use cobalt_core::{AuthMethod, ConnectionProfile, ProfileId, ResolvedCredentials, Secret, SecretRef};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

/// A cached access token is reused only if it has at least this long left.
pub const TOKEN_MARGIN: Duration = Duration::from_secs(60);
/// `SecretRef` kind under which a profile's Entra refresh token is stored.
pub const REFRESH_TOKEN_KIND: &str = "refresh_token";

/// How the resolver talks to the user. Implemented by the UI; [`HeadlessPrompter`] for tests.
pub trait Prompter: Send + Sync {
    /// Ask for a SQL login (or service-principal) password. `Ok(None)` = the user cancelled.
    fn password(&self, profile: &ConnectionProfile) -> Result<Option<Secret>>;
    /// Open the interactive sign-in URL (normally [`crate::entra::open_in_browser`]).
    fn open_browser(&self, url: &str);
    /// Show the device code and where to enter it.
    fn device_code(&self, prompt: &DeviceCodePrompt);
    /// Polled while a browser / device-code flow is in progress.
    fn cancelled(&self) -> bool;
}

pub struct CredentialResolver {
    secrets: Arc<dyn SecretStore>,
    cfg: RwLock<EntraConfig>,
    cache: Mutex<HashMap<ProfileId, TokenSet>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UserFlow {
    Interactive,
    DeviceCode,
}

impl CredentialResolver {
    pub fn new(secrets: Arc<dyn SecretStore>, cfg: EntraConfig) -> Self {
        Self { secrets, cfg: RwLock::new(cfg), cache: Mutex::new(HashMap::new()) }
    }

    pub fn config(&self) -> EntraConfig {
        self.cfg.read().unwrap().clone()
    }

    /// Replace the Entra settings (after the user edits Settings → Connections).
    pub fn set_config(&self, cfg: EntraConfig) {
        *self.cfg.write().unwrap() = cfg;
    }

    pub fn secrets(&self) -> &Arc<dyn SecretStore> {
        &self.secrets
    }

    pub fn refresh_token_ref(profile_id: &ProfileId) -> SecretRef {
        SecretRef::for_profile(profile_id, REFRESH_TOKEN_KIND)
    }

    /// Resolve credentials for `profile`, prompting through `prompt` when nothing silent works.
    pub async fn resolve(&self, profile: &ConnectionProfile, prompt: &dyn Prompter) -> Result<ResolvedCredentials> {
        match &profile.auth {
            AuthMethod::SqlLogin { user, password } => {
                let stored = match password {
                    Some(r) => self.secrets.get(r)?,
                    None => None,
                };
                let password = match stored {
                    Some(p) => p,
                    None => prompt.password(profile)?.ok_or(AuthError::Cancelled)?,
                };
                Ok(ResolvedCredentials::SqlLogin { user: user.clone(), password })
            }
            AuthMethod::EntraInteractive { tenant, account_hint } => {
                self.user_token(profile, tenant.as_deref(), account_hint.as_deref(), UserFlow::Interactive, prompt).await
            }
            AuthMethod::EntraDeviceCode { tenant } => {
                self.user_token(profile, tenant.as_deref(), None, UserFlow::DeviceCode, prompt).await
            }
            AuthMethod::AzureCli { tenant } => {
                let tenant = tenant.as_deref().or(self.config().tenant.as_deref()).map(str::to_owned);
                let t = azure_cli_token(tenant.as_deref()).await?;
                Ok(ResolvedCredentials::EntraToken { token: t.token, expires_at: Some(t.expires_at) })
            }
            AuthMethod::WindowsIntegrated => Ok(ResolvedCredentials::WindowsIntegrated),
            AuthMethod::EntraServicePrincipal { tenant, client_id, secret } => {
                let stored = match secret {
                    Some(r) => self.secrets.get(r)?,
                    None => None,
                };
                let secret = match stored {
                    Some(s) => s,
                    None => prompt.password(profile)?.ok_or(AuthError::Cancelled)?,
                };
                let cfg = self.config();
                let t = client_secret_token(&cfg, tenant, client_id, &secret).await?;
                Ok(ResolvedCredentials::EntraToken { token: t.token, expires_at: Some(t.expires_at) })
            }
        }
    }

    /// Drop the stored refresh token and any cached token for a profile ("Sign out").
    pub fn forget(&self, profile_id: &ProfileId) -> Result<()> {
        self.cache.lock().unwrap().remove(profile_id);
        self.secrets.delete(&Self::refresh_token_ref(profile_id))
    }

    /// The account behind the cached token, if this process has acquired one for the profile.
    pub fn signed_in_account(&self, profile_id: &ProfileId) -> Option<EntraAccount> {
        self.cache.lock().unwrap().get(profile_id).map(|t| t.account.clone())
    }

    /// Whether a refresh token is stored for the profile (i.e. a silent sign-in is likely).
    pub fn is_remembered(&self, profile_id: &ProfileId) -> bool {
        matches!(self.secrets.get(&Self::refresh_token_ref(profile_id)), Ok(Some(_)))
    }

    /// Put a token set in the in-memory cache (and persist its refresh token). Public so the
    /// app can pre-warm from a flow it ran itself, and for tests.
    pub fn insert_cached(&self, profile_id: ProfileId, set: TokenSet) {
        self.remember(profile_id, &set);
    }

    async fn user_token(
        &self,
        profile: &ConnectionProfile,
        tenant: Option<&str>,
        login_hint: Option<&str>,
        flow: UserFlow,
        prompt: &dyn Prompter,
    ) -> Result<ResolvedCredentials> {
        let cfg = self.config().with_tenant(tenant);
        cfg.require_client_id()?;
        let id = profile.id;

        // 1. Cached, still valid.
        let cached = self.cache.lock().unwrap().get(&id).cloned();
        if let Some(ts) = &cached {
            if ts.access.is_valid_for(TOKEN_MARGIN) {
                tracing::debug!(%id, "using cached Entra access token");
                return Ok(token_creds(ts));
            }
        }

        // 2. Silent refresh.
        let rt_ref = Self::refresh_token_ref(&id);
        let stored_rt = match self.secrets.get(&rt_ref) {
            Ok(rt) => rt,
            Err(e) => {
                tracing::warn!(%id, error = %e, "could not read the stored refresh token; continuing without it");
                None
            }
        };
        let rt = stored_rt.or_else(|| cached.as_ref().and_then(|t| t.refresh.clone()));
        if let Some(rt) = rt {
            match refresh(&cfg, &rt).await {
                Ok(mut ts) => {
                    if ts.account == EntraAccount::default() {
                        if let Some(prev) = cached.as_ref() {
                            ts.account = prev.account.clone();
                        }
                    }
                    tracing::info!(%id, user = %ts.account.username, "Entra token refreshed silently");
                    self.remember(id, &ts);
                    return Ok(token_creds(&ts));
                }
                Err(e @ (AuthError::InteractionRequired(_) | AuthError::Provider { .. })) => {
                    tracing::warn!(%id, error = %e, "refresh token rejected; falling back to interactive sign-in");
                    let _ = self.secrets.delete(&rt_ref);
                    self.cache.lock().unwrap().remove(&id);
                }
                Err(e) => return Err(e),
            }
        }

        // 3. Interactive.
        let cancel = CancelToken::new();
        let run = async {
            match flow {
                UserFlow::Interactive => {
                    interactive_login(&cfg, login_hint, |url| prompt.open_browser(url), cancel.clone()).await
                }
                UserFlow::DeviceCode => device_code_login(&cfg, |p| prompt.device_code(p), cancel.clone()).await,
            }
        };
        let watch_cancel = async {
            loop {
                if prompt.cancelled() {
                    cancel.cancel();
                    return;
                }
                tokio::time::sleep(Duration::from_millis(150)).await;
            }
        };
        let ts = tokio::select! {
            r = run => r?,
            _ = watch_cancel => return Err(AuthError::Cancelled),
        };
        self.remember(id, &ts);
        Ok(token_creds(&ts))
    }

    fn remember(&self, id: ProfileId, ts: &TokenSet) {
        if let Some(rt) = &ts.refresh {
            if let Err(e) = self.secrets.set(&Self::refresh_token_ref(&id), rt) {
                tracing::warn!(%id, error = %e, "could not store the refresh token; you will be asked to sign in again next time");
            }
        }
        self.cache.lock().unwrap().insert(id, ts.clone());
    }
}

fn token_creds(ts: &TokenSet) -> ResolvedCredentials {
    ResolvedCredentials::EntraToken { token: ts.access.token.clone(), expires_at: Some(ts.access.expires_at) }
}

// ---------------------------------------------------------------------------------------------

/// A [`Prompter`] with canned answers, for tests and headless use. Records everything it is
/// asked so tests can assert on the prompt order.
#[derive(Default)]
pub struct HeadlessPrompter {
    password: Option<Secret>,
    on_open: Option<Box<dyn Fn(&str) + Send + Sync>>,
    opened: Mutex<Vec<String>>,
    device_prompts: Mutex<Vec<DeviceCodePrompt>>,
    password_prompts: AtomicUsize,
    cancelled: AtomicBool,
}

impl HeadlessPrompter {
    pub fn new() -> Self {
        Self::default()
    }
    /// Answer every password prompt with this value.
    pub fn with_password(mut self, p: impl Into<Secret>) -> Self {
        self.password = Some(p.into());
        self
    }
    /// Run this when the interactive flow hands over a URL (a test's fake browser).
    pub fn with_on_open(mut self, f: impl Fn(&str) + Send + Sync + 'static) -> Self {
        self.on_open = Some(Box::new(f));
        self
    }
    pub fn set_cancelled(&self, v: bool) {
        self.cancelled.store(v, Ordering::SeqCst);
    }
    pub fn opened_urls(&self) -> Vec<String> {
        self.opened.lock().unwrap().clone()
    }
    pub fn device_prompts(&self) -> Vec<DeviceCodePrompt> {
        self.device_prompts.lock().unwrap().clone()
    }
    pub fn password_prompt_count(&self) -> usize {
        self.password_prompts.load(Ordering::SeqCst)
    }
}

impl Prompter for HeadlessPrompter {
    fn password(&self, _profile: &ConnectionProfile) -> Result<Option<Secret>> {
        self.password_prompts.fetch_add(1, Ordering::SeqCst);
        Ok(self.password.clone())
    }
    fn open_browser(&self, url: &str) {
        self.opened.lock().unwrap().push(url.to_owned());
        if let Some(f) = &self.on_open {
            f(url);
        }
    }
    fn device_code(&self, prompt: &DeviceCodePrompt) {
        self.device_prompts.lock().unwrap().push(prompt.clone());
    }
    fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entra::AccessToken;
    use crate::secrets::MemoryStore;
    use chrono::Utc;

    fn resolver() -> (Arc<MemoryStore>, CredentialResolver) {
        let store = Arc::new(MemoryStore::new());
        (store.clone(), CredentialResolver::new(store, EntraConfig::new("test-client")))
    }

    fn token_set(secs: i64, refresh: Option<&str>) -> TokenSet {
        TokenSet {
            access: AccessToken { token: Secret::new("at"), expires_at: Utc::now() + chrono::Duration::seconds(secs), scope: "s".into() },
            refresh: refresh.map(Secret::new),
            account: EntraAccount { username: "u@x.y".into(), ..Default::default() },
        }
    }

    #[tokio::test]
    async fn sql_login_uses_stored_password_without_prompting() {
        let (store, r) = resolver();
        let mut p = ConnectionProfile::new("srv", AuthMethod::SqlLogin { user: "sa".into(), password: None });
        let sref = SecretRef::for_profile(&p.id, "password");
        store.set(&sref, &Secret::new("stored")).unwrap();
        p.auth = AuthMethod::SqlLogin { user: "sa".into(), password: Some(sref) };
        let prompt = HeadlessPrompter::new().with_password("typed");
        match r.resolve(&p, &prompt).await.unwrap() {
            ResolvedCredentials::SqlLogin { user, password } => {
                assert_eq!(user, "sa");
                assert_eq!(password.expose(), "stored");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(prompt.password_prompt_count(), 0);
    }

    #[tokio::test]
    async fn sql_login_prompts_when_nothing_stored() {
        let (_store, r) = resolver();
        let p = ConnectionProfile::new("srv", AuthMethod::SqlLogin { user: "sa".into(), password: None });
        let prompt = HeadlessPrompter::new().with_password("typed");
        match r.resolve(&p, &prompt).await.unwrap() {
            ResolvedCredentials::SqlLogin { password, .. } => assert_eq!(password.expose(), "typed"),
            other => panic!("{other:?}"),
        }
        assert_eq!(prompt.password_prompt_count(), 1);
        // Stored ref that points nowhere also prompts.
        let p2 = ConnectionProfile::new(
            "srv",
            AuthMethod::SqlLogin { user: "sa".into(), password: Some(SecretRef { key: "missing".into() }) },
        );
        assert!(r.resolve(&p2, &prompt).await.is_ok());
        assert_eq!(prompt.password_prompt_count(), 2);
    }

    #[tokio::test]
    async fn sql_login_cancelled_prompt_is_cancelled() {
        let (_store, r) = resolver();
        let p = ConnectionProfile::new("srv", AuthMethod::SqlLogin { user: "sa".into(), password: None });
        let e = r.resolve(&p, &HeadlessPrompter::new()).await.unwrap_err();
        assert!(matches!(e, AuthError::Cancelled));
    }

    #[tokio::test]
    async fn windows_integrated_passes_through() {
        let (_store, r) = resolver();
        let p = ConnectionProfile::new("srv", AuthMethod::WindowsIntegrated);
        assert!(matches!(r.resolve(&p, &HeadlessPrompter::new()).await.unwrap(), ResolvedCredentials::WindowsIntegrated));
    }

    #[tokio::test]
    async fn entra_without_client_id_is_a_clear_error() {
        let store = Arc::new(MemoryStore::new());
        let r = CredentialResolver::new(store, EntraConfig::default());
        let p = ConnectionProfile::new("srv", AuthMethod::EntraInteractive { tenant: None, account_hint: None });
        let e = r.resolve(&p, &HeadlessPrompter::new()).await.unwrap_err();
        assert!(matches!(e, AuthError::MissingClientId));
        assert!(e.to_string().contains("Settings"));
    }

    #[tokio::test]
    async fn cached_valid_token_is_used_without_any_io() {
        let (store, r) = resolver();
        let p = ConnectionProfile::new("srv", AuthMethod::EntraInteractive { tenant: None, account_hint: None });
        r.insert_cached(p.id, token_set(600, Some("rt")));
        assert!(store.contains(&CredentialResolver::refresh_token_ref(&p.id)));
        let prompt = HeadlessPrompter::new().with_on_open(|_| panic!("no browser expected"));
        match r.resolve(&p, &prompt).await.unwrap() {
            ResolvedCredentials::EntraToken { token, expires_at } => {
                assert_eq!(token.expose(), "at");
                assert!(expires_at.is_some());
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(r.signed_in_account(&p.id).unwrap().username, "u@x.y");
        assert!(r.is_remembered(&p.id));
    }

    #[tokio::test]
    async fn forget_clears_cache_and_store() {
        let (store, r) = resolver();
        let id = ProfileId::new();
        r.insert_cached(id, token_set(600, Some("rt")));
        r.forget(&id).unwrap();
        assert!(r.signed_in_account(&id).is_none());
        assert!(!store.contains(&CredentialResolver::refresh_token_ref(&id)));
        assert!(!r.is_remembered(&id));
    }

    #[tokio::test]
    async fn set_config_replaces_settings() {
        let (_store, r) = resolver();
        let mut cfg = r.config();
        cfg.tenant = Some("contoso.com".into());
        r.set_config(cfg.clone());
        assert_eq!(r.config(), cfg);
    }
}
