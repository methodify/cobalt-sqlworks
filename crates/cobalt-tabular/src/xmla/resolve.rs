//! Where the XMLA POSTs go for each target. `Http` is the URL as given. Azure Analysis
//! Services resolves the server to its cluster (`/webapi/clusterResolve`), then posts to
//! `https://{cluster}/webapi/xmla` with `x-ms-xmlaserver`. Power BI resolves the workspace to its
//! `*.pbidedicated.windows.net` cluster through `api.powerbi.com` — a best effort, since the
//! shapes are UNCONFIRMED until a capture exists (see `docs/design/semantic_models.md` §7) —
//! then posts with the routing headers. Resolved clusters are cached per workspace / server
//! for the process.

use super::Credential;
use crate::TabularError;
use cobalt_core::tabular::TabularTarget;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// The XMLA URL and the headers every request to it carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub url: String,
    pub headers: Vec<(&'static str, String)>,
    /// The `Catalog` property (the model), when the target names one.
    pub catalog: String,
}

/// The Power BI resolution failed; the text the status bar shows.
pub const POWERBI_UNRESOLVED: &str = "the Power BI XMLA endpoint could not be resolved; this path needs a protocol capture (see docs/design/semantic_models.md §7)";

fn cache() -> &'static Mutex<HashMap<String, String>> {
    static CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cached(key: &str) -> Option<String> {
    cache().lock().ok()?.get(key).cloned()
}

fn remember(key: &str, cluster: &str) {
    if let Ok(mut c) = cache().lock() {
        c.insert(key.to_string(), cluster.to_string());
    }
}

/// Forget a cached cluster (after a routing failure, so the next connect resolves again).
pub fn forget(target: &TabularTarget) {
    if let Ok(mut c) = cache().lock() {
        c.remove(&cache_key(target));
    }
}

fn cache_key(target: &TabularTarget) -> String {
    match target {
        TabularTarget::PowerBi { workspace_id, workspace_name, .. } => format!("pbi:{}", if workspace_id.is_empty() { workspace_name } else { workspace_id }),
        TabularTarget::AzureAs { region, server, .. } => format!("asazure:{region}/{server}"),
        TabularTarget::Http { url, .. } => format!("http:{url}"),
    }
}

/// Resolve the target. `http` is the client the XMLA calls will use too.
pub async fn resolve(http: &reqwest::Client, target: &TabularTarget, cred: &Credential) -> Result<Endpoint, TabularError> {
    match target {
        TabularTarget::Http { url, model } => Ok(Endpoint { url: url.trim().to_string(), headers: Vec::new(), catalog: model.clone() }),
        TabularTarget::AzureAs { region, server, model } => {
            let key = cache_key(target);
            let cluster = match cached(&key) {
                Some(c) => c,
                None => {
                    let c = resolve_asazure(http, region, server, cred).await;
                    remember(&key, &c);
                    c
                }
            };
            Ok(Endpoint { url: format!("https://{cluster}/webapi/xmla"), headers: vec![("x-ms-xmlaserver", server.clone())], catalog: model.clone() })
        }
        TabularTarget::PowerBi { workspace_id, workspace_name, model_name, dataset_id } => {
            // Confirmed live (2026-10-14): the workspace record's `capacityUri` is
            // `pbidedicated://{cluster}/{capacity object id}`; the dedicated cluster's front door
            // finds a "server" only by that capacity object id (workspace names and ids answer
            // NotFound); `getDatabaseName` maps the model name to the dataset GUID the service
            // calls the database. What the front door still needs beyond that is unconfirmed:
            // with these headers it answers 200 + JSON "An internal error occurred." (see
            // `docs/design/semantic_models.md` §7, the capture).
            let key = cache_key(target);
            let cluster = match cached(&key) {
                Some(c) => c,
                None => {
                    let c = resolve_powerbi(http, workspace_id, workspace_name, cred).await?;
                    remember(&key, &c);
                    c
                }
            };
            let mut parts = cluster.splitn(3, '|');
            let host = parts.next().unwrap_or_default().to_string();
            let capacity = parts.next().unwrap_or_default().to_string();
            let resolved_id = parts.next().unwrap_or_default().to_string();
            let ws_id = if workspace_id.is_empty() { resolved_id.as_str() } else { workspace_id.as_str() };
            let database = match database_name(http, ws_id, model_name, cred).await {
                Some(d) => d,
                None if !dataset_id.is_empty() => dataset_id.clone(),
                None => model_name.clone(),
            };
            let mut headers = vec![("x-ms-xmlaserver", if capacity.is_empty() { format!("powerbi://api.powerbi.com/v1.0/myorg/{workspace_name}") } else { capacity }), ("x-ms-xmladatabase", database.clone())];
            if !ws_id.is_empty() {
                headers.push(("x-ms-xmlaworkspaceobjectid", ws_id.to_string()));
            }
            Ok(Endpoint { url: format!("https://{host}/webapi/xmla"), headers, catalog: database })
        }
    }
}

fn apply_cred(req: reqwest::RequestBuilder, cred: &Credential) -> reqwest::RequestBuilder {
    match cred {
        Credential::Bearer(t) => req.bearer_auth(t),
        Credential::Basic { user, password } => req.basic_auth(user, Some(password)),
        Credential::Anonymous => req,
    }
}

fn head(s: &str) -> String {
    s.chars().take(300).collect::<String>().replace(['\n', '\r'], " ")
}

/// Azure AS: `POST https://{region}.asazure.windows.net/webapi/clusterResolve` with
/// `{"serverName":"asazure://…"}` (UNCONFIRMED shape; parsed leniently). Any failure falls back
/// to the regional front door itself, which routes by `x-ms-xmlaserver`.
async fn resolve_asazure(http: &reqwest::Client, region: &str, server: &str, cred: &Credential) -> String {
    let front = format!("{region}.asazure.windows.net");
    let url = format!("https://{front}/webapi/clusterResolve");
    let body = serde_json::json!({ "serverName": format!("asazure://{front}/{server}") });
    let req = apply_cred(http.post(&url).json(&body).timeout(Duration::from_secs(30)), cred);
    match req.send().await {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let text = resp.text().await.unwrap_or_default();
            tracing::debug!(status, body = %head(&text), "asazure clusterResolve");
            if (200..300).contains(&status) {
                if let Some(host) = find_host(&text, ".asazure.windows.net") {
                    return host;
                }
            }
            front
        }
        Err(e) => {
            tracing::debug!(error = %e, "asazure clusterResolve failed; using the regional front door");
            front
        }
    }
}

/// Power BI: the workspace's cluster through `api.powerbi.com/powerbi/databases/v201606/…`
/// (UNCONFIRMED; three guesses at the path, lenient parsing of the answer).
async fn resolve_powerbi(http: &reqwest::Client, workspace_id: &str, workspace_name: &str, cred: &Credential) -> Result<String, TabularError> {
    let base = "https://api.powerbi.com/powerbi/databases/v201606/workspaces";
    let mut urls: Vec<String> = Vec::new();
    if !workspace_id.trim().is_empty() {
        urls.push(format!("{base}/{}", workspace_id.trim()));
    }
    if !workspace_name.trim().is_empty() {
        urls.push(format!("{base}/{}", url_encode(workspace_name.trim())));
    }
    urls.push(format!("{base}?includeMyWorkspace=true"));
    let mut last_auth: Option<TabularError> = None;
    for url in urls {
        let req = apply_cred(http.get(&url).header("Accept", "application/json").timeout(Duration::from_secs(30)), cred);
        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!(url = %url, error = %e, "powerbi workspace resolve failed");
                continue;
            }
        };
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        tracing::debug!(url = %url, status, body = %head(&text), "powerbi workspace resolve");
        if status == 401 || status == 403 {
            last_auth = Some(crate::rest::parse_http_error(status, &text));
            continue;
        }
        if !(200..300).contains(&status) {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let candidates: Vec<&serde_json::Value> = match v.as_array() {
            Some(a) => a.iter().collect(),
            None => match v.get("value").and_then(|x| x.as_array()) {
                Some(a) => a.iter().collect(),
                None => vec![&v],
            },
        };
        let wanted = |ws: &serde_json::Value| -> bool {
            if candidates.len() == 1 {
                return true;
            }
            let id_match = !workspace_id.is_empty() && json_strings(ws).any(|s| s.eq_ignore_ascii_case(workspace_id));
            let name_match = !workspace_name.is_empty() && ["name", "displayName", "workspaceName"].iter().any(|k| ws.get(k).and_then(|x| x.as_str()).is_some_and(|n| n.eq_ignore_ascii_case(workspace_name)));
            id_match || name_match
        };
        for ws in candidates.iter().filter(|ws| wanted(ws)) {
            // `capacityUri`: pbidedicated://{cluster}/{capacity object id}
            if let Some(uri) = ws.get("capacityUri").and_then(|v| v.as_str()) {
                if let Some(rest) = uri.strip_prefix("pbidedicated://") {
                    let (host, cap) = rest.split_once('/').unwrap_or((rest, ""));
                    let id = ws.get("id").and_then(|v| v.as_str()).unwrap_or("");
                    if !host.is_empty() {
                        return Ok(if cap.is_empty() { host.to_string() } else { format!("{host}|{cap}|{id}") });
                    }
                }
            }
            if let Some(host) = json_strings(ws).find_map(|s| host_in(s, ".pbidedicated.")) {
                return Ok(host);
            }
        }
    }
    if let Some(e) = last_auth {
        return Err(e);
    }
    Err(TabularError::Unsupported(POWERBI_UNRESOLVED.into()))
}

/// Power BI: the dataset GUID the dedicated cluster calls the database, from
/// `POST api.powerbi.com/powerbi/databases/v201606/workspaces/{ws}/getDatabaseName`
/// (`{"datasetName": …}` → `[{"databaseName": "<guid>", …}]`; confirmed live 2026-10-14).
async fn database_name(http: &reqwest::Client, workspace_id: &str, model_name: &str, cred: &Credential) -> Option<String> {
    if workspace_id.is_empty() || model_name.is_empty() {
        return None;
    }
    let url = format!("https://api.powerbi.com/powerbi/databases/v201606/workspaces/{workspace_id}/getDatabaseName");
    let req = apply_cred(http.post(&url), cred).json(&serde_json::json!({"datasetName": model_name}));
    let resp = req.send().await.ok()?;
    let status = resp.status().as_u16();
    let text = resp.text().await.ok()?;
    tracing::debug!(url, status, body = %head(&text), "powerbi getDatabaseName");
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let first = v.as_array().and_then(|a| a.first()).unwrap_or(&v);
    first.get("databaseName").and_then(|d| d.as_str()).map(str::to_string)
}

/// Every string value in a JSON document, any depth.
fn json_strings(v: &serde_json::Value) -> Box<dyn Iterator<Item = &str> + '_> {
    match v {
        serde_json::Value::String(s) => Box::new(std::iter::once(s.as_str())),
        serde_json::Value::Array(a) => Box::new(a.iter().flat_map(json_strings)),
        serde_json::Value::Object(o) => Box::new(o.values().flat_map(json_strings)),
        _ => Box::new(std::iter::empty()),
    }
}

/// A host name containing `marker` inside `s` (a bare host, a URL, or text), without scheme or path.
fn host_in(s: &str, marker: &str) -> Option<String> {
    let idx = s.find(marker)?;
    let before = &s[..idx];
    let start = before.rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '.')).map(|p| p + 1).unwrap_or(0);
    let after = &s[idx..];
    let end = idx + after.find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '.')).unwrap_or(after.len());
    let host = s[start..end].trim_matches('.');
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

/// The first host ending in `suffix` anywhere in a JSON (or plain) text.
fn find_host(text: &str, suffix: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(text).unwrap_or(serde_json::Value::Null);
    let from_json = json_strings(&v).find_map(|s| host_in(s, suffix).filter(|h| h.ends_with(suffix.trim_start_matches('.')) || h.contains(suffix)));
    from_json.or_else(|| host_in(text, suffix))
}

fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_in_json() {
        assert_eq!(find_host(r#"{"clusterFQDN":"aspaaseastus2.asazure.windows.net"}"#, ".asazure.windows.net").as_deref(), Some("aspaaseastus2.asazure.windows.net"));
        assert_eq!(find_host(r#"{"x":{"url":"https://aspaaseastus2.asazure.windows.net/webapi/xmla"}}"#, ".asazure.windows.net").as_deref(), Some("aspaaseastus2.asazure.windows.net"));
        assert_eq!(find_host(r#"{"serverName":"asazure://eastus2.asazure.windows.net/srv"}"#, ".asazure.windows.net").as_deref(), Some("eastus2.asazure.windows.net"));
        assert_eq!(find_host("<html>nope</html>", ".asazure.windows.net"), None);
        let ws = serde_json::json!({"id": 1, "objectId": "abc", "capacity": {"xmlaEndpoint": "https://wabi-west-europe-redirect.pbidedicated.windows.net/xmla"}});
        assert_eq!(json_strings(&ws).find_map(|s| host_in(s, ".pbidedicated.")).as_deref(), Some("wabi-west-europe-redirect.pbidedicated.windows.net"));
        assert_eq!(host_in("abc.pbidedicated.windows.net", ".pbidedicated.").as_deref(), Some("abc.pbidedicated.windows.net"));
        assert_eq!(url_encode("Fabric test"), "Fabric%20test");
    }

    #[tokio::test]
    async fn http_target_is_as_given() {
        let http = reqwest::Client::new();
        let t = TabularTarget::Http { url: " http://localhost/olap/msmdpump.dll ".into(), model: "AdventureWorks".into() };
        let e = resolve(&http, &t, &Credential::Anonymous).await.unwrap();
        assert_eq!(e, Endpoint { url: "http://localhost/olap/msmdpump.dll".into(), headers: Vec::new(), catalog: "AdventureWorks".into() });
    }

    #[test]
    fn keys() {
        let t = TabularTarget::AzureAs { region: "westeurope".into(), server: "srv".into(), model: "m".into() };
        assert_eq!(cache_key(&t), "asazure:westeurope/srv");
        remember(&cache_key(&t), "cluster.asazure.windows.net");
        assert_eq!(cached(&cache_key(&t)).as_deref(), Some("cluster.asazure.windows.net"));
        forget(&t);
        assert_eq!(cached(&cache_key(&t)), None);
    }
}
