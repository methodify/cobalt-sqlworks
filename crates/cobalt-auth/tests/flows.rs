//! End-to-end flow tests against an in-process mock identity provider.

mod common;

use cobalt_auth::entra::pkce::challenge_for;
use cobalt_auth::{
    client_secret_token, device_code_login, interactive_login, refresh, AuthError, CancelToken, CredentialResolver,
    EntraConfig, HeadlessPrompter, MemoryStore,
};
use cobalt_core::{AuthMethod, ConnectionProfile, ProfileId, ResolvedCredentials, Secret};
use common::*;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn spawn_browser(url: &str, query: fn(&str) -> String) -> tokio::task::JoinHandle<(u16, String)> {
    let url = url.to_owned();
    tokio::spawn(async move { fake_browser(&url, query).await })
}

fn good_redirect(state: &str) -> String {
    format!("code=fake-auth-code&state={state}")
}

// ---------------------------------------------------------------------------------------------
// Interactive (auth code + PKCE)

#[tokio::test]
async fn interactive_flow_end_to_end() {
    let idp = MockIdp::start();
    idp.queue_token_ok("access-1", Some("refresh-1"), Some(&fake_id_token("ada@contoso.com", "Ada Lovelace")));
    let cfg = idp.config("test-client");

    let seen_url = Arc::new(Mutex::new(None::<String>));
    let page = Arc::new(Mutex::new(None));
    let (u2, p2) = (seen_url.clone(), page.clone());
    let set = interactive_login(
        &cfg,
        Some("ada@contoso.com"),
        move |url| {
            *u2.lock().unwrap() = Some(url.to_owned());
            let h = spawn_browser(url, good_redirect);
            *p2.lock().unwrap() = Some(h);
        },
        CancelToken::new(),
    )
    .await
    .expect("interactive login");

    // The browser saw the success page.
    let (status, body) = page.lock().unwrap().take().unwrap().await.unwrap();
    assert_eq!(status, 200);
    assert!(body.contains("You're signed in to Cobalt SQL Works"));
    assert!(body.contains("#1F6FEB"));

    // Token set came from the mock and the id_token was decoded.
    assert_eq!(set.access.token.expose(), "access-1");
    assert_eq!(set.refresh.as_ref().unwrap().expose(), "refresh-1");
    assert_eq!(set.account.username, "ada@contoso.com");
    assert_eq!(set.account.name.as_deref(), Some("Ada Lovelace"));
    assert_eq!(set.account.tenant_id.as_deref(), Some("tenant-1234"));
    assert_eq!(set.account.home_account_id.as_deref(), Some("oid-5678.tenant-1234"));
    assert!(set.access.is_valid_for(Duration::from_secs(3000)));
    assert!(set.access.scope.contains("database.windows.net"));

    // The authorize URL and the token exchange agree on PKCE and redirect URI.
    let url = url::Url::parse(&seen_url.lock().unwrap().clone().unwrap()).unwrap();
    assert_eq!(url.path(), "/organizations/oauth2/v2.0/authorize");
    let q: HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(q["login_hint"], "ada@contoso.com");
    assert_eq!(q["code_challenge_method"], "S256");
    let reqs = idp.token_requests();
    assert_eq!(reqs.len(), 1);
    let f = &reqs[0].form;
    assert_eq!(reqs[0].path, "/organizations/oauth2/v2.0/token");
    assert_eq!(f["grant_type"], "authorization_code");
    assert_eq!(f["code"], "fake-auth-code");
    assert_eq!(f["client_id"], "test-client");
    assert_eq!(f["redirect_uri"], q["redirect_uri"]);
    assert_eq!(challenge_for(&f["code_verifier"]), q["code_challenge"]);
    assert!(f["scope"].contains("offline_access"));
}

#[tokio::test]
async fn interactive_rejects_state_mismatch() {
    let idp = MockIdp::start();
    let cfg = idp.config("test-client");
    let page = Arc::new(Mutex::new(None));
    let p2 = page.clone();
    let err = interactive_login(
        &cfg,
        None,
        move |url| {
            *p2.lock().unwrap() = Some(spawn_browser(url, |_state| "code=fake&state=WRONG".to_owned()));
        },
        CancelToken::new(),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("state mismatch"), "{err}");
    let (status, body) = page.lock().unwrap().take().unwrap().await.unwrap();
    assert_eq!(status, 400);
    assert!(body.contains("Sign-in didn't complete"));
    assert!(idp.token_requests().is_empty(), "no code exchange on a bad state");
}

#[tokio::test]
async fn interactive_surfaces_provider_error_page() {
    let idp = MockIdp::start();
    let cfg = idp.config("test-client");
    let page = Arc::new(Mutex::new(None));
    let p2 = page.clone();
    let err = interactive_login(
        &cfg,
        None,
        move |url| {
            *p2.lock().unwrap() = Some(spawn_browser(url, |state| {
                format!("error=access_denied&error_description=AADSTS65004%3A+User+declined+to+consent&state={state}")
            }));
        },
        CancelToken::new(),
    )
    .await
    .unwrap_err();
    match &err {
        AuthError::Provider { error, description } => {
            assert_eq!(error, "access_denied");
            assert!(description.contains("AADSTS65004"), "{description}");
        }
        other => panic!("{other:?}"),
    }
    assert!(err.hint().is_some());
    let (status, body) = page.lock().unwrap().take().unwrap().await.unwrap();
    assert_eq!(status, 400);
    assert!(body.contains("AADSTS65004"));
    assert!(idp.token_requests().is_empty());
}

#[tokio::test]
async fn interactive_cancel_unblocks_listener() {
    let idp = MockIdp::start();
    let cfg = idp.config("test-client");
    let cancel = CancelToken::new();
    let c2 = cancel.clone();
    let started = Instant::now();
    let err = interactive_login(
        &cfg,
        None,
        move |_url| {
            let c = c2.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(150)).await;
                c.cancel();
            });
        },
        cancel,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, AuthError::Cancelled), "{err:?}");
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn interactive_code_exchange_failure_is_reported() {
    let idp = MockIdp::start();
    idp.queue_token_error("invalid_client", "AADSTS7000218: The request body must contain client_assertion or client_secret");
    let cfg = idp.config("test-client");
    let err = interactive_login(
        &cfg,
        None,
        |url| {
            spawn_browser(url, good_redirect);
        },
        CancelToken::new(),
    )
    .await
    .unwrap_err();
    match err {
        AuthError::Provider { error, .. } => assert_eq!(error, "invalid_client"),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn interactive_honours_fixed_redirect_port() {
    let idp = MockIdp::start();
    idp.queue_token_ok("a", None, None);
    // Find a free port, release it, and ask the flow to bind it.
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let cfg = EntraConfig { redirect_port: Some(port), ..idp.config("test-client") };
    let seen = Arc::new(Mutex::new(String::new()));
    let s2 = seen.clone();
    interactive_login(
        &cfg,
        None,
        move |url| {
            *s2.lock().unwrap() = url.to_owned();
            spawn_browser(url, good_redirect);
        },
        CancelToken::new(),
    )
    .await
    .unwrap();
    assert!(seen.lock().unwrap().contains(&format!("localhost%3A{port}")), "{}", seen.lock().unwrap());
}

// ---------------------------------------------------------------------------------------------
// Refresh

#[tokio::test]
async fn refresh_rotates_token_and_decodes_account() {
    let idp = MockIdp::start();
    idp.queue_token_ok("access-2", Some("refresh-2"), Some(&fake_id_token("bob@contoso.com", "Bob")));
    let cfg = idp.config("test-client");
    let set = refresh(&cfg, &Secret::new("refresh-1")).await.unwrap();
    assert_eq!(set.access.token.expose(), "access-2");
    assert_eq!(set.refresh.as_ref().unwrap().expose(), "refresh-2");
    assert_eq!(set.account.username, "bob@contoso.com");
    let f = &idp.token_requests()[0].form;
    assert_eq!(f["grant_type"], "refresh_token");
    assert_eq!(f["refresh_token"], "refresh-1");
    assert_eq!(f["client_id"], "test-client");
    assert!(f["scope"].contains("https://database.windows.net/.default"));
}

#[tokio::test]
async fn refresh_keeps_old_token_when_none_returned() {
    let idp = MockIdp::start();
    idp.queue_token_ok("access-3", None, None);
    let cfg = idp.config("test-client");
    let set = refresh(&cfg, &Secret::new("refresh-old")).await.unwrap();
    assert_eq!(set.refresh.as_ref().unwrap().expose(), "refresh-old");
}

#[tokio::test]
async fn refresh_maps_expired_grant_to_interaction_required() {
    let idp = MockIdp::start();
    idp.queue_token_error("invalid_grant", "AADSTS70008: The provided authorization code or refresh token has expired");
    let cfg = idp.config("test-client");
    let err = refresh(&cfg, &Secret::new("stale")).await.unwrap_err();
    assert!(matches!(err, AuthError::InteractionRequired(_)), "{err:?}");
    assert!(err.needs_interaction());
}

#[tokio::test]
async fn refresh_uses_token_endpoint_override() {
    let idp = MockIdp::start();
    idp.queue_token_ok("access-4", None, None);
    let cfg = EntraConfig {
        client_id: "c".into(),
        token_endpoint_override: Some(url::Url::parse(&format!("{}/custom/token", idp.base)).unwrap()),
        ..Default::default()
    };
    assert_eq!(cfg.authority(), "https://login.microsoftonline.com/organizations");
    refresh(&cfg, &Secret::new("r")).await.unwrap();
    assert_eq!(idp.token_requests()[0].path, "/custom/token");
}

#[tokio::test]
async fn network_failure_is_an_http_error() {
    // A port nothing listens on.
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let cfg = EntraConfig { client_id: "c".into(), authority_host: format!("http://127.0.0.1:{port}"), ..Default::default() };
    let err = refresh(&cfg, &Secret::new("r")).await.unwrap_err();
    assert!(matches!(err, AuthError::Http(_)), "{err:?}");
}

// ---------------------------------------------------------------------------------------------
// Device code

#[tokio::test]
async fn device_code_flow_pending_then_success() {
    let idp = MockIdp::start();
    idp.set_devicecode(200, devicecode_json(0, 300));
    idp.queue_token_error("authorization_pending", "AADSTS70016: Pending end-user authorization.");
    idp.queue_token_error("authorization_pending", "AADSTS70016: Pending end-user authorization.");
    idp.queue_token_ok("access-dc", Some("refresh-dc"), Some(&fake_id_token("cy@contoso.com", "Cy")));
    let cfg = idp.config("test-client");
    let prompts = Arc::new(Mutex::new(Vec::new()));
    let p2 = prompts.clone();
    let set = device_code_login(&cfg, move |p| p2.lock().unwrap().push(p.clone()), CancelToken::new()).await.unwrap();
    assert_eq!(set.access.token.expose(), "access-dc");
    assert_eq!(set.account.username, "cy@contoso.com");
    let prompts = prompts.lock().unwrap();
    assert_eq!(prompts.len(), 1);
    assert_eq!(prompts[0].user_code, "ABCD1234");
    assert_eq!(prompts[0].verification_uri, "https://microsoft.com/devicelogin");
    assert!(prompts[0].message.contains("ABCD1234"));
    assert_eq!(prompts[0].expires_in, Duration::from_secs(300));
    let reqs = idp.requests();
    assert!(reqs[0].path.ends_with("/devicecode"));
    assert_eq!(reqs[0].form["client_id"], "test-client");
    let polls = idp.token_requests();
    assert_eq!(polls.len(), 3);
    assert_eq!(polls[0].form["grant_type"], "urn:ietf:params:oauth:grant-type:device_code");
    assert_eq!(polls[0].form["device_code"], "device-code-xyz");
}

#[tokio::test]
async fn device_code_declined_and_expired() {
    let idp = MockIdp::start();
    idp.set_devicecode(200, devicecode_json(0, 300));
    idp.queue_token_error("authorization_declined", "AADSTS70020: The end user has denied the authorization request.");
    let cfg = idp.config("test-client");
    let err = device_code_login(&cfg, |_| {}, CancelToken::new()).await.unwrap_err();
    match &err {
        AuthError::Provider { error, .. } => assert_eq!(error, "authorization_declined"),
        other => panic!("{other:?}"),
    }
    assert!(err.hint().unwrap().contains("declined"));

    idp.queue_token_error("expired_token", "AADSTS70019: The device code has expired.");
    let err = device_code_login(&cfg, |_| {}, CancelToken::new()).await.unwrap_err();
    assert!(matches!(err, AuthError::Timeout), "{err:?}");
}

#[tokio::test]
async fn device_code_request_error_is_mapped() {
    let idp = MockIdp::start();
    idp.set_devicecode(400, serde_json::json!({ "error": "unauthorized_client", "error_description": "AADSTS700016: app not found" }));
    let cfg = idp.config("test-client");
    let err = device_code_login(&cfg, |_| panic!("no prompt on failure"), CancelToken::new()).await.unwrap_err();
    assert!(matches!(err, AuthError::Provider { .. }), "{err:?}");
    assert!(err.hint().unwrap().contains("client ID"));
}

#[tokio::test]
async fn device_code_cancel_stops_polling() {
    let idp = MockIdp::start();
    idp.set_devicecode(200, devicecode_json(1, 300));
    for _ in 0..50 {
        idp.queue_token_error("authorization_pending", "pending");
    }
    let cfg = idp.config("test-client");
    let cancel = CancelToken::new();
    let c2 = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        c2.cancel();
    });
    let started = Instant::now();
    let err = device_code_login(&cfg, |_| {}, cancel).await.unwrap_err();
    assert!(matches!(err, AuthError::Cancelled), "{err:?}");
    assert!(started.elapsed() < Duration::from_secs(3));
}

// ---------------------------------------------------------------------------------------------
// Service principal

#[tokio::test]
async fn client_secret_flow_posts_client_credentials() {
    let idp = MockIdp::start();
    idp.queue_token(200, token_json("sp-token", 1800, None, None));
    let cfg = idp.config("ignored-public-client");
    let t = client_secret_token(&cfg, "11111111-2222-3333-4444-555555555555", "sp-client", &Secret::new("sp-secret")).await.unwrap();
    assert_eq!(t.token.expose(), "sp-token");
    assert!(t.is_valid_for(Duration::from_secs(1700)));
    let r = &idp.token_requests()[0];
    assert_eq!(r.path, "/11111111-2222-3333-4444-555555555555/oauth2/v2.0/token");
    assert_eq!(r.form["grant_type"], "client_credentials");
    assert_eq!(r.form["client_id"], "sp-client");
    assert_eq!(r.form["client_secret"], "sp-secret");
    assert_eq!(r.form["scope"], "https://database.windows.net/.default");
    assert!(!r.form.contains_key("code_verifier"));
}

#[tokio::test]
async fn client_secret_error_is_provider_error() {
    let idp = MockIdp::start();
    idp.queue_token_error("invalid_client", "AADSTS7000215: Invalid client secret provided.");
    let cfg = idp.config("x");
    let err = client_secret_token(&cfg, "contoso.com", "sp", &Secret::new("bad")).await.unwrap_err();
    assert!(matches!(err, AuthError::Provider { .. }), "{err:?}");
}

// ---------------------------------------------------------------------------------------------
// Resolver policy

fn entra_profile() -> ConnectionProfile {
    ConnectionProfile::new("srv.database.windows.net", AuthMethod::EntraInteractive { tenant: None, account_hint: Some("ada@contoso.com".into()) })
}

fn token_of(c: ResolvedCredentials) -> String {
    match c {
        ResolvedCredentials::EntraToken { token, expires_at } => {
            assert!(expires_at.is_some());
            token.expose().to_owned()
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn resolver_policy_cache_then_refresh_then_browser() {
    let idp = MockIdp::start();
    let store = Arc::new(MemoryStore::new());
    let resolver = CredentialResolver::new(store.clone(), idp.config("test-client"));
    let profile = entra_profile();
    let rt_ref = CredentialResolver::refresh_token_ref(&profile.id);

    // A stored refresh token from a previous run: silent refresh, no browser.
    store.set(&rt_ref, &Secret::new("rt-old")).unwrap();
    idp.queue_token_ok("access-r1", Some("rt-new"), Some(&fake_id_token("ada@contoso.com", "Ada")));
    let prompt = HeadlessPrompter::new().with_on_open(|_| panic!("browser must not open on a silent refresh"));
    assert_eq!(token_of(resolver.resolve(&profile, &prompt).await.unwrap()), "access-r1");
    assert_eq!(store.get(&rt_ref).unwrap().unwrap().expose(), "rt-new", "rotated refresh token persisted");
    assert_eq!(resolver.signed_in_account(&profile.id).unwrap().username, "ada@contoso.com");
    assert_eq!(idp.token_requests().len(), 1);

    // Second call: cached token, no HTTP at all.
    assert_eq!(token_of(resolver.resolve(&profile, &prompt).await.unwrap()), "access-r1");
    assert_eq!(idp.token_requests().len(), 1);

    // Simulate a restart with a refresh token the IdP now rejects: refresh → interactive.
    resolver.forget(&profile.id).unwrap();
    store.set(&rt_ref, &Secret::new("rt-revoked")).unwrap();
    idp.queue_token_error("invalid_grant", "AADSTS50173: The provided grant has expired due to it being revoked");
    idp.queue_token_ok("access-i1", Some("rt-fresh"), Some(&fake_id_token("ada@contoso.com", "Ada")));
    let prompt = HeadlessPrompter::new().with_on_open(|url| {
        spawn_browser(url, good_redirect);
    });
    assert_eq!(token_of(resolver.resolve(&profile, &prompt).await.unwrap()), "access-i1");
    assert_eq!(prompt.opened_urls().len(), 1);
    assert!(prompt.opened_urls()[0].contains("login_hint=ada%40contoso.com"));
    assert_eq!(store.get(&rt_ref).unwrap().unwrap().expose(), "rt-fresh");
    let reqs = idp.token_requests();
    assert_eq!(reqs.len(), 3);
    assert_eq!(reqs[1].form["grant_type"], "refresh_token");
    assert_eq!(reqs[2].form["grant_type"], "authorization_code");

    // Forget = signed out: nothing cached, nothing stored.
    resolver.forget(&profile.id).unwrap();
    assert!(resolver.signed_in_account(&profile.id).is_none());
    assert!(!resolver.is_remembered(&profile.id));
}

#[tokio::test]
async fn resolver_goes_straight_to_browser_when_nothing_stored() {
    let idp = MockIdp::start();
    idp.queue_token_ok("access-b", Some("rt-b"), None);
    let store = Arc::new(MemoryStore::new());
    let resolver = CredentialResolver::new(store.clone(), idp.config("test-client"));
    let profile = entra_profile();
    let prompt = HeadlessPrompter::new().with_on_open(|url| {
        spawn_browser(url, good_redirect);
    });
    assert_eq!(token_of(resolver.resolve(&profile, &prompt).await.unwrap()), "access-b");
    assert_eq!(idp.token_requests().len(), 1);
    assert_eq!(idp.token_requests()[0].form["grant_type"], "authorization_code");
    assert!(resolver.is_remembered(&profile.id));
}

#[tokio::test]
async fn resolver_profile_tenant_overrides_settings_tenant() {
    let idp = MockIdp::start();
    idp.queue_token_ok("a", None, None);
    let mut cfg = idp.config("test-client");
    cfg.tenant = Some("common".into());
    let resolver = CredentialResolver::new(Arc::new(MemoryStore::new()), cfg);
    let profile = ConnectionProfile::new("srv", AuthMethod::EntraInteractive { tenant: Some("contoso.onmicrosoft.com".into()), account_hint: None });
    let prompt = HeadlessPrompter::new().with_on_open(|url| {
        spawn_browser(url, good_redirect);
    });
    resolver.resolve(&profile, &prompt).await.unwrap();
    assert!(prompt.opened_urls()[0].contains("/contoso.onmicrosoft.com/oauth2/v2.0/authorize"));
    assert_eq!(idp.token_requests()[0].path, "/contoso.onmicrosoft.com/oauth2/v2.0/token");
}

#[tokio::test]
async fn resolver_device_code_policy() {
    let idp = MockIdp::start();
    idp.set_devicecode(200, devicecode_json(0, 300));
    idp.queue_token_error("authorization_pending", "pending");
    idp.queue_token_ok("access-dc", Some("rt-dc"), Some(&fake_id_token("dee@contoso.com", "Dee")));
    let store = Arc::new(MemoryStore::new());
    let resolver = CredentialResolver::new(store.clone(), idp.config("test-client"));
    let profile = ConnectionProfile::new("srv", AuthMethod::EntraDeviceCode { tenant: None });
    let prompt = HeadlessPrompter::new().with_on_open(|_| panic!("device code must not open a browser"));
    assert_eq!(token_of(resolver.resolve(&profile, &prompt).await.unwrap()), "access-dc");
    assert_eq!(prompt.device_prompts().len(), 1);
    assert_eq!(prompt.device_prompts()[0].user_code, "ABCD1234");
    assert_eq!(resolver.signed_in_account(&profile.id).unwrap().username, "dee@contoso.com");
    assert!(store.contains(&CredentialResolver::refresh_token_ref(&profile.id)));

    // Next time: silent refresh, no prompt.
    resolver.forget(&profile.id).unwrap();
    store.set(&CredentialResolver::refresh_token_ref(&profile.id), &Secret::new("rt-dc")).unwrap();
    idp.queue_token_ok("access-dc2", None, None);
    assert_eq!(token_of(resolver.resolve(&profile, &prompt).await.unwrap()), "access-dc2");
    assert_eq!(prompt.device_prompts().len(), 1);
}

#[tokio::test]
async fn resolver_cancel_via_prompter() {
    let idp = MockIdp::start();
    let resolver = CredentialResolver::new(Arc::new(MemoryStore::new()), idp.config("test-client"));
    let profile = entra_profile();
    let prompt = Arc::new(HeadlessPrompter::new());
    let p2 = prompt.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        p2.set_cancelled(true);
    });
    let started = Instant::now();
    let err = resolver.resolve(&profile, prompt.as_ref()).await.unwrap_err();
    assert!(matches!(err, AuthError::Cancelled), "{err:?}");
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(prompt.opened_urls().len(), 1, "the browser was opened before the cancel");
}

#[tokio::test]
async fn resolver_refresh_network_error_is_not_swallowed() {
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let cfg = EntraConfig { client_id: "c".into(), authority_host: format!("http://127.0.0.1:{port}"), ..Default::default() };
    let store = Arc::new(MemoryStore::new());
    let resolver = CredentialResolver::new(store.clone(), cfg);
    let profile = entra_profile();
    store.set(&CredentialResolver::refresh_token_ref(&profile.id), &Secret::new("rt")).unwrap();
    let prompt = HeadlessPrompter::new().with_on_open(|_| panic!("offline: must not fall through to the browser"));
    let err = resolver.resolve(&profile, &prompt).await.unwrap_err();
    assert!(matches!(err, AuthError::Http(_)), "{err:?}");
    assert!(resolver.is_remembered(&profile.id), "refresh token kept for when the network is back");
}

#[tokio::test]
async fn resolver_service_principal_uses_stored_secret_or_prompt() {
    let idp = MockIdp::start();
    idp.queue_token(200, token_json("sp-1", 600, None, None));
    idp.queue_token(200, token_json("sp-2", 600, None, None));
    let store = Arc::new(MemoryStore::new());
    let resolver = CredentialResolver::new(store.clone(), idp.config("public"));
    let id = ProfileId::new();
    let sref = cobalt_core::SecretRef::for_profile(&id, "client_secret");
    store.set(&sref, &Secret::new("stored-secret")).unwrap();
    let mut profile = ConnectionProfile::new(
        "srv",
        AuthMethod::EntraServicePrincipal { tenant: "contoso.com".into(), client_id: "sp".into(), secret: Some(sref) },
    );
    profile.id = id;
    let prompt = HeadlessPrompter::new().with_password("typed-secret");
    assert_eq!(token_of(resolver.resolve(&profile, &prompt).await.unwrap()), "sp-1");
    assert_eq!(idp.token_requests()[0].form["client_secret"], "stored-secret");
    assert_eq!(prompt.password_prompt_count(), 0);

    profile.auth = AuthMethod::EntraServicePrincipal { tenant: "contoso.com".into(), client_id: "sp".into(), secret: None };
    assert_eq!(token_of(resolver.resolve(&profile, &prompt).await.unwrap()), "sp-2");
    assert_eq!(idp.token_requests()[1].form["client_secret"], "typed-secret");
    assert_eq!(prompt.password_prompt_count(), 1);
}
