//! The loopback HTTP listener that receives the authorization-code redirect, plus the two
//! self-contained HTML pages it serves.

use super::CancelToken;
use crate::{AuthError, Result};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tiny_http::{Header, Response, Server};

/// What the browser delivered to the redirect URI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Redirect {
    pub code: String,
}

pub(crate) struct Listener {
    server: Server,
    port: u16,
}

impl Listener {
    /// Bind `127.0.0.1:{port}` (`0` = ephemeral).
    pub fn bind(port: Option<u16>) -> Result<Self> {
        let addr = format!("127.0.0.1:{}", port.unwrap_or(0));
        let server = Server::http(&addr).map_err(|e| AuthError::Other(format!("cannot listen on {addr}: {e}")))?;
        let port = server
            .server_addr()
            .to_ip()
            .map(|a| a.port())
            .ok_or_else(|| AuthError::Other("loopback listener has no IP address".into()))?;
        Ok(Self { server, port })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// The URI Entra redirects to. `localhost` rather than `127.0.0.1` because that is what a
    /// public-client app registration allows on any port.
    pub fn redirect_uri(&self) -> String {
        format!("http://localhost:{}", self.port)
    }

    /// Block until the redirect arrives, the deadline passes, or `cancel` flips. Meant to run
    /// inside `spawn_blocking`; polls every 200 ms so a cancel is honoured promptly.
    pub fn wait_for_code(self, expected_state: &str, timeout: Duration, cancel: &CancelToken) -> Result<Redirect> {
        let deadline = Instant::now() + timeout;
        loop {
            if cancel.is_cancelled() {
                return Err(AuthError::Cancelled);
            }
            if Instant::now() >= deadline {
                return Err(AuthError::Timeout);
            }
            let req = match self.server.recv_timeout(Duration::from_millis(200)) {
                Ok(Some(r)) => r,
                Ok(None) => continue,
                Err(e) => return Err(AuthError::Other(format!("loopback listener failed: {e}"))),
            };
            let url = req.url().to_owned();
            let (path, query) = match url.split_once('?') {
                Some((p, q)) => (p, q),
                None => (url.as_str(), ""),
            };
            // Browsers also ask for /favicon.ico etc. — ignore anything that is not the redirect.
            if path != "/" || query.is_empty() {
                let _ = req.respond(Response::from_string("").with_status_code(404));
                continue;
            }
            let params: HashMap<String, String> = url::form_urlencoded::parse(query.as_bytes()).into_owned().collect();
            let outcome = interpret(&params, expected_state);
            let (status, page) = match &outcome {
                Ok(_) => (200, success_page()),
                Err(e) => (400, error_page(&e.to_string())),
            };
            let _ = req.respond(html(status, page));
            return outcome;
        }
    }
}

/// Decide what a redirect's query string means.
pub(crate) fn interpret(params: &HashMap<String, String>, expected_state: &str) -> Result<Redirect> {
    if let Some(err) = params.get("error") {
        let desc = params.get("error_description").cloned().unwrap_or_default();
        return Err(AuthError::Provider { error: err.clone(), description: desc });
    }
    match params.get("state") {
        Some(s) if s == expected_state => {}
        _ => return Err(AuthError::Other("sign-in response did not match this request (state mismatch); please try again".into())),
    }
    match params.get("code") {
        Some(c) if !c.is_empty() => Ok(Redirect { code: c.clone() }),
        _ => Err(AuthError::Other("sign-in response carried no authorization code".into())),
    }
}

fn html(status: u16, body: String) -> Response<std::io::Cursor<Vec<u8>>> {
    Response::from_string(body)
        .with_status_code(status)
        .with_header(Header::from_bytes("Content-Type", "text/html; charset=utf-8").unwrap())
        .with_header(Header::from_bytes("Cache-Control", "no-store").unwrap())
        .with_header(Header::from_bytes("Connection", "close").unwrap())
}

const STYLE: &str = r#"
:root{color-scheme:light dark}
body{margin:0;font:15px/1.5 system-ui,-apple-system,"Segoe UI",Roboto,sans-serif;background:#F6F8FA;color:#1F2328;display:flex;min-height:100vh;align-items:center;justify-content:center}
@media(prefers-color-scheme:dark){body{background:#0D1117;color:#E6EDF3}}
.card{max-width:26rem;padding:2.5rem 2.75rem;border-radius:12px;background:#fff;box-shadow:0 8px 32px rgba(31,111,235,.12);border-top:6px solid #1F6FEB;text-align:center}
@media(prefers-color-scheme:dark){.card{background:#161B22;box-shadow:0 8px 32px rgba(0,0,0,.5)}}
.card.err{border-top-color:#D13B3B}
h1{font-size:1.25rem;margin:.25rem 0 .5rem}
p{margin:.25rem 0;opacity:.85}
.mark{width:44px;height:44px;border-radius:50%;background:#1F6FEB;margin:0 auto 1rem;display:flex;align-items:center;justify-content:center}
.err .mark{background:#D13B3B}
code{font-size:.85em;opacity:.8;word-break:break-word}
"#;

pub(crate) fn success_page() -> String {
    format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><title>Signed in - Cobalt SQL Works</title><meta name="viewport" content="width=device-width,initial-scale=1"><style>{STYLE}</style></head>
<body><div class="card"><div class="mark"><svg width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="#fff" stroke-width="3" stroke-linecap="round" stroke-linejoin="round"><path d="M5 12.5l4.5 4.5L19 7.5"/></svg></div>
<h1>You're signed in to Cobalt SQL Works</h1><p>You can close this tab and return to the app.</p></div>
<script>setTimeout(function(){{try{{window.close()}}catch(e){{}}}},1500)</script></body></html>"#
    )
}

pub(crate) fn error_page(message: &str) -> String {
    let msg = escape(message);
    format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><title>Sign-in failed - Cobalt SQL Works</title><meta name="viewport" content="width=device-width,initial-scale=1"><style>{STYLE}</style></head>
<body><div class="card err"><div class="mark"><svg width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="#fff" stroke-width="3" stroke-linecap="round"><path d="M6 6l12 12M18 6L6 18"/></svg></div>
<h1>Sign-in didn't complete</h1><p>Cobalt SQL Works could not finish signing you in.</p><p><code>{msg}</code></p><p>Close this tab and try again from the app.</p></div></body></html>"#
    )
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn interpret_success() {
        let r = interpret(&params(&[("code", "abc"), ("state", "s1")]), "s1").unwrap();
        assert_eq!(r.code, "abc");
    }

    #[test]
    fn interpret_state_mismatch() {
        let e = interpret(&params(&[("code", "abc"), ("state", "other")]), "s1").unwrap_err();
        assert!(e.to_string().contains("state mismatch"), "{e}");
        let e = interpret(&params(&[("code", "abc")]), "s1").unwrap_err();
        assert!(e.to_string().contains("state mismatch"), "{e}");
    }

    #[test]
    fn interpret_provider_error_wins_over_state() {
        let e = interpret(&params(&[("error", "access_denied"), ("error_description", "AADSTS65004: user declined"), ("state", "s1")]), "s1")
            .unwrap_err();
        match e {
            AuthError::Provider { error, description } => {
                assert_eq!(error, "access_denied");
                assert!(description.contains("AADSTS65004"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn interpret_missing_code() {
        let e = interpret(&params(&[("state", "s1")]), "s1").unwrap_err();
        assert!(e.to_string().contains("no authorization code"));
    }

    #[test]
    fn pages_are_self_contained_and_escaped() {
        let ok = success_page();
        assert!(ok.contains("#1F6FEB"));
        assert!(ok.contains("You're signed in to Cobalt SQL Works"));
        assert!(!ok.contains("http://") && !ok.contains("https://"), "no external resources");
        let err = error_page("<script>alert(1)</script> & \"q\"");
        assert!(err.contains("&lt;script&gt;"));
        assert!(!err.contains("<script>alert"));
        assert!(err.contains("&amp; &quot;q&quot;"));
    }

    #[test]
    fn listener_binds_ephemeral_port() {
        let l = Listener::bind(None).unwrap();
        assert!(l.port() > 0);
        assert_eq!(l.redirect_uri(), format!("http://localhost:{}", l.port()));
    }

    #[test]
    fn listener_cancel_unblocks() {
        let l = Listener::bind(None).unwrap();
        let cancel = CancelToken::new();
        let c2 = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            c2.cancel();
        });
        let started = Instant::now();
        let e = l.wait_for_code("s", Duration::from_secs(30), &cancel).unwrap_err();
        assert!(matches!(e, AuthError::Cancelled));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn listener_times_out() {
        let l = Listener::bind(None).unwrap();
        let e = l.wait_for_code("s", Duration::from_millis(300), &CancelToken::new()).unwrap_err();
        assert!(matches!(e, AuthError::Timeout));
    }
}
