//! A tiny in-process identity provider: serves `/token` and `/devicecode` with canned replies,
//! records every request, and shuts down on drop.

#![allow(dead_code)]

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use std::collections::{HashMap, VecDeque};
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tiny_http::{Header, Response, Server};

#[derive(Clone, Debug)]
pub struct Recorded {
    pub path: String,
    pub form: HashMap<String, String>,
}

#[derive(Default)]
struct State {
    token_replies: Mutex<VecDeque<(u16, serde_json::Value)>>,
    devicecode_reply: Mutex<Option<(u16, serde_json::Value)>>,
    requests: Mutex<Vec<Recorded>>,
}

pub struct MockIdp {
    /// `http://127.0.0.1:{port}` — use as `EntraConfig::authority_host`.
    pub base: String,
    server: Arc<Server>,
    state: Arc<State>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl MockIdp {
    pub fn start() -> Self {
        let server = Arc::new(Server::http("127.0.0.1:0").unwrap());
        let port = server.server_addr().to_ip().unwrap().port();
        let state = Arc::new(State::default());
        let stop = Arc::new(AtomicBool::new(false));
        let (s, st, sp) = (server.clone(), state.clone(), stop.clone());
        let thread = std::thread::spawn(move || {
            while !sp.load(Ordering::SeqCst) {
                let mut req = match s.recv_timeout(Duration::from_millis(50)) {
                    Ok(Some(r)) => r,
                    Ok(None) => continue,
                    Err(_) => break,
                };
                let mut body = String::new();
                let _ = req.as_reader().read_to_string(&mut body);
                let form: HashMap<String, String> = url::form_urlencoded::parse(body.as_bytes()).into_owned().collect();
                let path = req.url().split('?').next().unwrap_or("").to_owned();
                st.requests.lock().unwrap().push(Recorded { path: path.clone(), form });
                let (status, json) = if path.ends_with("/devicecode") {
                    st.devicecode_reply.lock().unwrap().clone().unwrap_or((404, serde_json::json!({"error": "no_devicecode_reply"})))
                } else if path.ends_with("/token") {
                    st.token_replies
                        .lock()
                        .unwrap()
                        .pop_front()
                        .unwrap_or((500, serde_json::json!({"error": "no_reply_queued", "error_description": "mock has no more token replies"})))
                } else {
                    (404, serde_json::json!({"error": "not_found"}))
                };
                let resp = Response::from_string(json.to_string())
                    .with_status_code(status)
                    .with_header(Header::from_bytes("Content-Type", "application/json").unwrap());
                let _ = req.respond(resp);
            }
        });
        Self { base: format!("http://127.0.0.1:{port}"), server, state, stop, thread: Some(thread) }
    }

    pub fn config(&self, client_id: &str) -> cobalt_auth::EntraConfig {
        cobalt_auth::EntraConfig { client_id: client_id.into(), authority_host: self.base.clone(), ..Default::default() }
    }

    pub fn queue_token(&self, status: u16, body: serde_json::Value) {
        self.state.token_replies.lock().unwrap().push_back((status, body));
    }

    pub fn queue_token_ok(&self, access: &str, refresh: Option<&str>, id_token: Option<&str>) {
        self.queue_token(200, token_json(access, 3600, refresh, id_token));
    }

    pub fn queue_token_error(&self, error: &str, description: &str) {
        self.queue_token(400, serde_json::json!({ "error": error, "error_description": description }));
    }

    pub fn set_devicecode(&self, status: u16, body: serde_json::Value) {
        *self.state.devicecode_reply.lock().unwrap() = Some((status, body));
    }

    pub fn requests(&self) -> Vec<Recorded> {
        self.state.requests.lock().unwrap().clone()
    }

    pub fn token_requests(&self) -> Vec<Recorded> {
        self.requests().into_iter().filter(|r| r.path.ends_with("/token")).collect()
    }
}

impl Drop for MockIdp {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.server.unblock();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

pub fn fake_jwt(claims: serde_json::Value) -> String {
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"none","typ":"JWT"}"#);
    let payload = URL_SAFE_NO_PAD.encode(claims.to_string());
    format!("{header}.{payload}.fakesig")
}

pub fn fake_id_token(username: &str, name: &str) -> String {
    fake_jwt(serde_json::json!({
        "preferred_username": username, "name": name,
        "tid": "tenant-1234", "oid": "oid-5678", "aud": "test-client"
    }))
}

pub fn token_json(access: &str, expires_in: i64, refresh: Option<&str>, id_token: Option<&str>) -> serde_json::Value {
    let mut v = serde_json::json!({
        "token_type": "Bearer",
        "scope": "https://database.windows.net/.default openid profile",
        "expires_in": expires_in,
        "ext_expires_in": expires_in,
        "access_token": access,
    });
    if let Some(r) = refresh {
        v["refresh_token"] = serde_json::Value::String(r.into());
    }
    if let Some(i) = id_token {
        v["id_token"] = serde_json::Value::String(i.into());
    }
    v
}

pub fn devicecode_json(interval: u64, expires_in: u64) -> serde_json::Value {
    serde_json::json!({
        "user_code": "ABCD1234",
        "device_code": "device-code-xyz",
        "verification_uri": "https://microsoft.com/devicelogin",
        "expires_in": expires_in,
        "interval": interval,
        "message": "To sign in, use a web browser to open the page https://microsoft.com/devicelogin and enter the code ABCD1234 to authenticate."
    })
}

/// Pretend to be the browser: fetch the authorize URL's `state`/`redirect_uri` and hit the
/// loopback redirect with `query`. Returns the page's status and body.
pub async fn fake_browser(authorize_url: &str, query: impl FnOnce(&str) -> String) -> (u16, String) {
    let url = url::Url::parse(authorize_url).unwrap();
    let q: HashMap<_, _> = url.query_pairs().into_owned().collect();
    let redirect = q["redirect_uri"].replace("localhost", "127.0.0.1");
    let target = format!("{redirect}/?{}", query(&q["state"]));
    let resp = reqwest::Client::new().get(&target).send().await.expect("loopback GET");
    let status = resp.status().as_u16();
    (status, resp.text().await.unwrap_or_default())
}
