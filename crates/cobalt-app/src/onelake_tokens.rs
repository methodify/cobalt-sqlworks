//! A loopback HTTP endpoint that hands OneLake (Azure Storage) bearer tokens to the local Spark
//! JVM. local-spark-mcp's `ch.fs.HttpTokenProvider` GETs `/token` with an `X-Token-Secret`
//! header whenever ABFS needs a token; the token comes from the signed-in Fabric account's
//! refresh token through the app's credential resolver, so the notebook runs as that user and no
//! `az login` is involved. Bound to 127.0.0.1 only; the secret is new for every start.

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

async fn fetch(resolver: &CredentialResolver, slot: ProfileId, tenant: Option<&str>) -> Result<String, String> {
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
                    let path = req.url().split('?').next().unwrap_or("");
                    if path != "/token" {
                        let _ = req.respond(tiny_http::Response::from_string("not found").with_status_code(404));
                        continue;
                    }
                    let ok = req.headers().iter().any(|h| h.field.equiv(SECRET_HEADER) && h.value.as_str() == secret2);
                    if !ok {
                        let _ = req.respond(tiny_http::Response::from_string("forbidden").with_status_code(403));
                        continue;
                    }
                    match handle.block_on(fetch(&resolver, slot, tenant.as_deref())) {
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
