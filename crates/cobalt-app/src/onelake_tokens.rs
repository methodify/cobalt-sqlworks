//! A loopback HTTP endpoint that hands Azure bearer tokens to the local Spark worker. The JVM's
//! `ch.fs.HttpTokenProvider` GETs `/token` with an `X-Token-Secret` header whenever ABFS needs a
//! OneLake (Azure Storage) token; since local-spark-mcp 0.4.1 the Python side asks the same
//! endpoint with `?scope=<scope>` for every other Azure token it needs (Fabric REST for
//! discovery, Key Vault for `getSecret`), so nothing in the worker constructs its own credential.
//! Tokens come from the signed-in Fabric account's refresh token through the app's credential
//! resolver, so the notebook runs as that user and no `az login` is involved. Bound to 127.0.0.1
//! only; the secret is new for every start.

use cobalt_auth::provider::{FABRIC_API_RESOURCE, ONELAKE_RESOURCE};
use cobalt_auth::CredentialResolver;
use cobalt_core::ProfileId;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub const SECRET_HEADER: &str = "X-Token-Secret";

pub struct TokenServer {
    pub url: String,
    pub secret: String,
    stop: Arc<AtomicBool>,
    /// Requests served (for the status UI).
    pub served: Arc<std::sync::atomic::AtomicU64>,
    pub last_error: Arc<parking_lot::Mutex<Option<String>>>,
}

pub const STORAGE_SCOPE: &str = "https://storage.azure.com/.default";
pub const FABRIC_SCOPE: &str = "https://api.fabric.microsoft.com/.default";

/// Which token a `scope` query asks for. `None` = a scope Cobalt does not serve.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Storage,
    FabricApi,
}

pub fn scope_of(query: Option<&str>) -> Option<Scope> {
    let raw = query.and_then(|q| q.split('&').find_map(|kv| kv.strip_prefix("scope="))).map(percent_decode).unwrap_or_default();
    let s = raw.trim().trim_end_matches('/');
    if s.is_empty() || s.starts_with("https://storage.azure.com") {
        Some(Scope::Storage)
    } else if s.starts_with("https://api.fabric.microsoft.com") || s.starts_with("https://analysis.windows.net/powerbi/api") {
        Some(Scope::FabricApi)
    } else {
        None
    }
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() + 0 && i + 2 <= b.len() - 1 => {
                let hex = &s[i + 1..i + 3];
                match u8::from_str_radix(hex, 16) {
                    Ok(v) => {
                        out.push(v);
                        i += 3;
                        continue;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            b'+' => out.push(b' '),
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

/// A Fabric REST token, silently.
pub(crate) async fn fetch_fabric(resolver: &CredentialResolver, slot: ProfileId, tenant: Option<&str>) -> Result<String, String> {
    match resolver.resource_token_silent(slot, FABRIC_API_RESOURCE, tenant).await {
        Ok(Some(ts)) => Ok(ts.access.token.expose().to_string()),
        Ok(None) => Err("no Fabric token for this account (sign in to Fabric again)".into()),
        Err(e) => Err(e.to_string()),
    }
}

/// A token OneLake accepts, silently.
pub(crate) async fn fetch(resolver: &CredentialResolver, slot: ProfileId, tenant: Option<&str>) -> Result<String, String> {
    match resolver.resource_token_silent(slot, ONELAKE_RESOURCE, tenant).await {
        Ok(Some(ts)) => return Ok(ts.access.token.expose().to_string()),
        Ok(None) => {}
        Err(e) => tracing::debug!("onelake token: {e}"),
    }
    match resolver.resource_token_silent(slot, FABRIC_API_RESOURCE, tenant).await {
        Ok(Some(ts)) if ts.access.scope.split(' ').any(|s| s.contains("OneLake")) => Ok(ts.access.token.expose().to_string()),
        Ok(_) => Err("no OneLake token for this account (sign in to Fabric again)".into()),
        Err(e) => Err(e.to_string()),
    }
}

impl TokenServer {
    pub fn start(resolver: Arc<CredentialResolver>, slot: ProfileId, tenant: Option<String>, handle: tokio::runtime::Handle) -> std::io::Result<TokenServer> {
        let server = tiny_http::Server::http("127.0.0.1:0").map_err(|e| std::io::Error::other(e.to_string()))?;
        let port = match server.server_addr() {
            tiny_http::ListenAddr::IP(a) => a.port(),
            #[allow(unreachable_patterns)]
            _ => return Err(std::io::Error::other("unexpected listener address")),
        };
        let secret = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
        let stop = Arc::new(AtomicBool::new(false));
        let served = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let last_error = Arc::new(parking_lot::Mutex::new(None));
        let (stop2, served2, err2, secret2) = (stop.clone(), served.clone(), last_error.clone(), secret.clone());
        std::thread::Builder::new()
            .name("onelake-tokens".into())
            .spawn(move || {
                while !stop2.load(Ordering::Relaxed) {
                    let req = match server.recv_timeout(Duration::from_millis(300)) {
                        Ok(Some(r)) => r,
                        Ok(None) => continue,
                        Err(_) => break,
                    };
                    let (path, query) = match req.url().split_once('?') {
                        Some((p, q)) => (p, Some(q.to_string())),
                        None => (req.url(), None),
                    };
                    if path != "/token" {
                        let _ = req.respond(tiny_http::Response::from_string("not found").with_status_code(404));
                        continue;
                    }
                    let ok = req.headers().iter().any(|h| h.field.equiv(SECRET_HEADER) && h.value.as_str() == secret2);
                    if !ok {
                        let _ = req.respond(tiny_http::Response::from_string("forbidden").with_status_code(403));
                        continue;
                    }
                    let scope = match scope_of(query.as_deref()) {
                        Some(s) => s,
                        None => {
                            let _ = req.respond(tiny_http::Response::from_string(format!("Cobalt does not serve tokens for scope {:?} (OneLake storage and the Fabric API only)", query.unwrap_or_default())).with_status_code(404));
                            continue;
                        }
                    };
                    let got = match scope {
                        Scope::Storage => handle.block_on(fetch(&resolver, slot, tenant.as_deref())),
                        Scope::FabricApi => handle.block_on(fetch_fabric(&resolver, slot, tenant.as_deref())),
                    };
                    match got {
                        Ok(tok) => {
                            served2.fetch_add(1, Ordering::Relaxed);
                            *err2.lock() = None;
                            let _ = req.respond(tiny_http::Response::from_string(tok).with_header(tiny_http::Header::from_bytes("Content-Type", "text/plain").unwrap()));
                        }
                        Err(e) => {
                            tracing::warn!("OneLake token endpoint: {e}");
                            *err2.lock() = Some(e.clone());
                            let _ = req.respond(tiny_http::Response::from_string(format!("token error: {e}")).with_status_code(500));
                        }
                    }
                }
            })
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        Ok(TokenServer { url: format!("http://127.0.0.1:{port}/token"), secret, stop, served, last_error })
    }
}

impl Drop for TokenServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes() {
        assert_eq!(scope_of(None), Some(Scope::Storage));
        assert_eq!(scope_of(Some("scope=https%3A%2F%2Fstorage.azure.com%2F.default")), Some(Scope::Storage));
        assert_eq!(scope_of(Some("x=1&scope=https%3A%2F%2Fapi.fabric.microsoft.com%2F.default")), Some(Scope::FabricApi));
        assert_eq!(scope_of(Some("scope=https%3A%2F%2Fvault.azure.net%2F.default")), None);
        assert_eq!(percent_decode("a%20b+c%zz"), "a b c%zz");
    }
}
