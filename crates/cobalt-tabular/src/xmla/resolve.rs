//! Where the XMLA POSTs go for each target, and how they sign in. `Http` is the URL as given
//! with the caller's credential. The two PaaS flavours follow the official client
//! (`docs/research/adomd_powerbi_xmla.md`, confirmed live for Power BI on 2026-10-14):
//!
//! - **Power BI**: the workspace record from `api.powerbi.com` (`id`, `capacityUri`), the Entra
//!   bearer exchanged for an **MWC token** at `metadata/v201606/generateastoken` (cached five
//!   minutes per workspace / capacity / model), the capacity resolved to its cluster with an
//!   unauthenticated `POST https://<rollout>/webapi/clusterResolve` (`clusterFQDN`,
//!   `coreServerName`; cached an hour), then `https://<clusterFQDN>/webapi/xmla` with
//!   `Authorization: MwcToken …` and `x-ms-xmlaserver: <coreServerName>`; `<Catalog>` is the
//!   friendly model name.
//! - **Azure Analysis Services**: the same `clusterResolve` on `<region>.asazure.windows.net`
//!   with `premiumPublicXmlaEndpoint:false` and the instance name, then the bearer token.
//!
//! `api.powerbi.com` answers `307` to another host with `PreferClientRouting`, and reqwest drops
//! `Authorization` across hosts, so those redirects are followed here by hand.

use super::Credential;
use crate::TabularError;
use cobalt_core::tabular::TabularTarget;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The hosts the Power BI path talks to; tests point them at a local server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hosts {
    /// `https://api.powerbi.com`
    pub api_base: String,
    /// The scheme for `clusterResolve` and the cluster (`https`).
    pub cluster_scheme: String,
}

impl Default for Hosts {
    fn default() -> Self {
        Self { api_base: "https://api.powerbi.com".into(), cluster_scheme: "https".into() }
    }
}

/// How requests to an endpoint are signed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Auth {
    /// `Bearer` / `Basic` / nothing, as the caller gave it.
    Credential(Credential),
    /// `MwcToken <token>`, minted from the Entra bearer for this workspace / capacity / model
    /// (and re-minted from the cache when it ages out).
    Mwc { entra: String, workspace_id: String, capacity_id: String, model: String },
}

/// The XMLA URL and the headers every request to it carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub url: String,
    pub headers: Vec<(&'static str, String)>,
    /// The `Catalog` property (the model), when the target names one.
    pub catalog: String,
    pub auth: Auth,
    /// A PaaS endpoint (Power BI, Azure AS): the official client's header set, no `SOAPAction`,
    /// an `Execute` with `BeginSession` to open the session.
    pub paas: bool,
}

/// The Power BI resolution failed; the text the status bar shows.
pub const POWERBI_UNRESOLVED: &str = "the Power BI XMLA endpoint could not be resolved; this path needs a protocol capture (see docs/design/semantic_models.md §7)";

/// `User-Agent` on the api.powerbi.com and cluster calls (what the service sees from the
/// official client).
pub const AS_CLIENT_AGENT: &str = "ASClient/.NET-Core";

const CLUSTER_TTL: Duration = Duration::from_secs(60 * 60);
const TOKEN_TTL: Duration = Duration::from_secs(5 * 60);

struct Caches {
    clusters: HashMap<String, (Instant, Cluster)>,
    tokens: HashMap<String, (Instant, String)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Cluster {
    fqdn: String,
    server: String,
}

fn caches() -> &'static Mutex<Caches> {
    static CACHE: OnceLock<Mutex<Caches>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Caches { clusters: HashMap::new(), tokens: HashMap::new() }))
}

fn cached_cluster(key: &str) -> Option<Cluster> {
    let c = caches().lock().ok()?;
    c.clusters.get(key).filter(|(t, _)| t.elapsed() < CLUSTER_TTL).map(|(_, v)| v.clone())
}

fn remember_cluster(key: &str, cluster: &Cluster) {
    if let Ok(mut c) = caches().lock() {
        c.clusters.insert(key.to_string(), (Instant::now(), cluster.clone()));
    }
}

/// Forget a cached cluster (after a 3xx from the endpoint, so the next connect resolves again).
pub fn forget(target: &TabularTarget) {
    if let Ok(mut c) = caches().lock() {
        c.clusters.remove(&cache_key(target));
    }
}

/// Forget every cached cluster (a `3xx` from a cluster: the instance moved).
pub fn forget_clusters() {
    if let Ok(mut c) = caches().lock() {
        c.clusters.clear();
    }
}

/// Drop every cached cluster and token (tests; a sign-out).
pub fn clear_caches() {
    if let Ok(mut c) = caches().lock() {
        c.clusters.clear();
        c.tokens.clear();
    }
}

fn cache_key(target: &TabularTarget) -> String {
    match target {
        TabularTarget::PowerBi { workspace_id, workspace_name, .. } => format!("pbi:{}", if workspace_id.is_empty() { workspace_name } else { workspace_id }),
        TabularTarget::AzureAs { region, server, .. } => format!("asazure:{region}/{server}"),
        TabularTarget::Http { url, .. } => format!("http:{url}"),
    }
}

/// Resolve the target. `http` is the client the XMLA calls will use too (no automatic redirects).
pub async fn resolve(http: &reqwest::Client, target: &TabularTarget, cred: &Credential, hosts: &Hosts) -> Result<Endpoint, TabularError> {
    match target {
        TabularTarget::Http { url, model } => Ok(Endpoint { url: url.trim().to_string(), headers: Vec::new(), catalog: model.clone(), auth: Auth::Credential(cred.clone()), paas: false }),
        TabularTarget::AzureAs { region, server, model } => {
            let key = cache_key(target);
            let cluster = match cached_cluster(&key) {
                Some(c) => c,
                None => {
                    let front = format!("{region}.asazure.windows.net");
                    let c = cluster_resolve(http, hosts, &front, server, false).await.unwrap_or_else(|| Cluster { fqdn: front, server: server.clone() });
                    remember_cluster(&key, &c);
                    c
                }
            };
            Ok(Endpoint { url: format!("{}://{}/webapi/xmla", hosts.cluster_scheme, cluster.fqdn), headers: vec![("x-ms-xmlaserver", cluster.server)], catalog: model.clone(), auth: Auth::Credential(cred.clone()), paas: true })
        }
        TabularTarget::PowerBi { workspace_id, workspace_name, model_name, .. } => {
            let Credential::Bearer(entra) = cred else {
                return Err(TabularError::Unsupported(format!("the Power BI XMLA endpoint needs an Entra bearer token, not {cred}")));
            };
            let ws = workspace_record(http, hosts, entra, workspace_id, workspace_name).await?;
            let key = cache_key(target);
            let cluster = match cached_cluster(&key) {
                Some(c) => c,
                None => {
                    let c = cluster_resolve(http, hosts, &ws.rollout_host, &ws.capacity_id, true).await.unwrap_or_else(|| Cluster { fqdn: ws.rollout_host.clone(), server: ws.capacity_id.clone() });
                    remember_cluster(&key, &c);
                    c
                }
            };
            // mint the token now so a refusal (no Build, tenant setting off) surfaces at connect
            mwc_token(http, hosts, entra, &ws.id, &ws.capacity_id, model_name).await?;
            Ok(Endpoint {
                url: format!("{}://{}/webapi/xmla", hosts.cluster_scheme, cluster.fqdn),
                headers: vec![("x-ms-xmlaserver", cluster.server)],
                catalog: model_name.clone(),
                auth: Auth::Mwc { entra: entra.clone(), workspace_id: ws.id, capacity_id: ws.capacity_id, model: model_name.clone() },
                paas: true,
            })
        }
    }
}

fn head(s: &str) -> String {
    s.chars().take(300).collect::<String>().replace(['\n', '\r'], " ")
}

/// One JSON call to api.powerbi.com with the Entra bearer: `?PreferClientRouting=true`, the
/// official agent, a `RequestId`, and up to three redirects followed with the bearer kept.
async fn api_call(http: &reqwest::Client, method: reqwest::Method, url: &str, bearer: &str, body: Option<&serde_json::Value>) -> Result<(u16, String), TabularError> {
    let mut url = if url.contains('?') { format!("{url}&PreferClientRouting=true") } else { format!("{url}?PreferClientRouting=true") };
    for hop in 0..4 {
        let mut req = http.request(method.clone(), &url).bearer_auth(bearer).header("User-Agent", AS_CLIENT_AGENT).header("RequestId", uuid::Uuid::new_v4().to_string()).header("Accept", "application/json").timeout(Duration::from_secs(30));
        if let Some(b) = body {
            req = req.json(b);
        }
        let resp = req.send().await?;
        let status = resp.status().as_u16();
        let location = resp.headers().get(reqwest::header::LOCATION).and_then(|v| v.to_str().ok()).map(str::to_string);
        let text = resp.text().await.unwrap_or_default();
        tracing::debug!(%url, status, body = %head(&text), "powerbi api");
        match (status, location) {
            (301 | 302 | 303 | 307 | 308, Some(loc)) if hop < 3 => {
                url = match reqwest::Url::parse(&url).ok().and_then(|base| base.join(&loc).ok()) {
                    Some(u) => u.to_string(),
                    None => loc,
                };
                continue;
            }
            _ => return Ok((status, text)),
        }
    }
    Err(TabularError::Decode("api.powerbi.com redirected more than three times".into()))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Workspace {
    id: String,
    name: String,
    capacity_id: String,
    /// `westus.pbidedicated.windows.net`, from `capacityUri`.
    rollout_host: String,
}

fn workspace_from_json(ws: &serde_json::Value) -> Option<Workspace> {
    let s = |k: &str| ws.get(k).and_then(|v| v.as_str()).map(str::trim).filter(|v| !v.is_empty()).map(str::to_string);
    let id = s("id").or_else(|| s("objectId"))?;
    let name = s("name").or_else(|| s("displayName")).unwrap_or_default();
    let capacity_uri = s("capacityUri").unwrap_or_default();
    let (rollout_host, uri_id) = match capacity_uri.split_once("://") {
        Some((_, rest)) => {
            let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
            (host.to_string(), path.trim_matches('/').split('/').next().unwrap_or("").to_string())
        }
        None => (String::new(), String::new()),
    };
    let capacity_id = s("capacityObjectId").or(if uri_id.is_empty() { None } else { Some(uri_id) })?;
    if rollout_host.is_empty() {
        return None;
    }
    Some(Workspace { id, name, capacity_id, rollout_host })
}

/// The workspace record: by name first (confirmed live; the guid form answered 404), then by id,
/// then the list matched on id or name (`type == "User"` is My workspace when the name is blank).
async fn workspace_record(http: &reqwest::Client, hosts: &Hosts, entra: &str, workspace_id: &str, workspace_name: &str) -> Result<Workspace, TabularError> {
    let base = format!("{}/powerbi/databases/v201606/workspaces", hosts.api_base.trim_end_matches('/'));
    let mut urls: Vec<String> = Vec::new();
    if !workspace_name.trim().is_empty() {
        urls.push(format!("{base}/{}", url_encode(workspace_name.trim())));
    }
    if !workspace_id.trim().is_empty() {
        urls.push(format!("{base}/{}", workspace_id.trim()));
    }
    urls.push(format!("{base}?includeMyWorkspace=true"));
    let mut last_auth: Option<TabularError> = None;
    let mut off_capacity: Option<String> = None;
    for url in urls {
        let (status, text) = match api_call(http, reqwest::Method::GET, &url, entra, None).await {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!(%url, error = %e, "powerbi workspace lookup failed");
                continue;
            }
        };
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
        let single = candidates.len() == 1 && !url.contains('?');
        let wanted = |ws: &serde_json::Value| -> bool {
            if single {
                return true;
            }
            let field = |k: &str| ws.get(k).and_then(|x| x.as_str()).unwrap_or("");
            let id_match = !workspace_id.is_empty() && (field("id").eq_ignore_ascii_case(workspace_id) || field("objectId").eq_ignore_ascii_case(workspace_id));
            let name_match = !workspace_name.is_empty() && field("name").trim().eq_ignore_ascii_case(workspace_name.trim());
            let my_workspace = workspace_name.is_empty() && workspace_id.is_empty() && field("type").eq_ignore_ascii_case("User");
            id_match || name_match || my_workspace
        };
        for ws in candidates.iter().filter(|ws| wanted(ws)) {
            if let Some(w) = workspace_from_json(ws) {
                return Ok(w);
            }
            let sku = ws.get("capacitySku").and_then(|x| x.as_str()).unwrap_or("none");
            off_capacity = Some(format!("workspace '{}' is not on a Premium or Fabric capacity (capacitySku {sku}); the XMLA endpoint needs one", ws.get("name").and_then(|x| x.as_str()).unwrap_or(workspace_name)));
        }
    }
    if let Some(e) = last_auth {
        return Err(e);
    }
    if let Some(m) = off_capacity {
        return Err(TabularError::Unsupported(m));
    }
    Err(TabularError::Unsupported(POWERBI_UNRESOLVED.into()))
}

/// The MWC token for the cluster: `POST metadata/v201606/generateastoken` with the Entra bearer,
/// cached five minutes per workspace / capacity / model. A refusal names `generateastoken` so
/// the hint can say what it usually means (no Build on the model, or the XMLA tenant setting off).
pub async fn mwc_token(http: &reqwest::Client, hosts: &Hosts, entra: &str, workspace_id: &str, capacity_id: &str, model: &str) -> Result<String, TabularError> {
    let key = format!("{workspace_id}|{capacity_id}|{model}|{}", entra.len());
    if let Ok(c) = caches().lock() {
        if let Some((t, tok)) = c.tokens.get(&key) {
            if t.elapsed() < TOKEN_TTL {
                return Ok(tok.clone());
            }
        }
    }
    let url = format!("{}/metadata/v201606/generateastoken", hosts.api_base.trim_end_matches('/'));
    let body = serde_json::json!({
        "applyAuxiliaryPermission": false,
        "auxiliaryPermissionOwner": null,
        "bypassBuildPermission": false,
        "capacityObjectId": capacity_id,
        "datasetName": model,
        "intendedUsage": 0,
        "sourceCapacityObjectId": null,
        "workspaceObjectId": workspace_id,
    });
    let (status, text) = api_call(http, reqwest::Method::POST, &url, entra, Some(&body)).await?;
    if !(200..300).contains(&status) {
        return Err(match crate::rest::parse_http_error(status, &text) {
            TabularError::Unauthorized(s, d) => TabularError::Unauthorized(s, format!("generateastoken refused: {d}")),
            TabularError::Api { status, code, message } => TabularError::Api { status, code, message: format!("generateastoken refused: {message}") },
            other => other,
        });
    }
    let v: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
    let token = ["Token", "token", "accessToken"].iter().find_map(|k| v.get(k).and_then(|t| t.as_str())).map(str::trim).filter(|t| !t.is_empty()).map(str::to_string);
    let Some(token) = token else {
        return Err(TabularError::Decode(format!("generateastoken answered without a Token: {}", head(&text))));
    };
    if let Ok(mut c) = caches().lock() {
        c.tokens.insert(key, (Instant::now(), token.clone()));
    }
    Ok(token)
}

/// `POST <scheme>://<front>/webapi/clusterResolve` (no auth) → the cluster host and the server
/// name for `x-ms-xmlaserver`. `None` on any failure (the caller falls back to the front door).
async fn cluster_resolve(http: &reqwest::Client, hosts: &Hosts, front: &str, server: &str, premium_public_xmla: bool) -> Option<Cluster> {
    let url = format!("{}://{front}/webapi/clusterResolve", hosts.cluster_scheme);
    let body = serde_json::json!({ "databaseName": null, "premiumPublicXmlaEndpoint": premium_public_xmla, "serverName": server });
    let req = http.post(&url).json(&body).header("User-Agent", AS_CLIENT_AGENT).header("x-ms-parent-activity-id", uuid::Uuid::new_v4().to_string()).timeout(Duration::from_secs(30));
    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!(%url, error = %e, "clusterResolve failed; using the front door");
            return None;
        }
    };
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    tracing::debug!(%url, status, body = %head(&text), "clusterResolve");
    if !(200..300).contains(&status) {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(str::trim).filter(|x| !x.is_empty()).map(str::to_string);
    let marker = if premium_public_xmla { ".pbidedicated." } else { ".asazure." };
    let fqdn = s("clusterFQDN").and_then(|f| host_in(&f, marker).or(Some(f))).or_else(|| find_host(&text, marker))?;
    let server = s("coreServerName").unwrap_or_else(|| server.to_string());
    Some(Cluster { fqdn, server })
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
    let end = idx + after.find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == ':')).unwrap_or(after.len());
    let host = s[start..end].trim_matches('.');
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

/// The first host containing `marker` anywhere in a JSON (or plain) text.
fn find_host(text: &str, marker: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(text).unwrap_or(serde_json::Value::Null);
    let from_json = json_strings(&v).find_map(|s| host_in(s, marker));
    from_json.or_else(|| host_in(text, marker))
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
        assert_eq!(find_host(r#"{"clusterFQDN":"aspaaseastus2.asazure.windows.net"}"#, ".asazure.").as_deref(), Some("aspaaseastus2.asazure.windows.net"));
        assert_eq!(find_host(r#"{"x":{"url":"https://aspaaseastus2.asazure.windows.net/webapi/xmla"}}"#, ".asazure.").as_deref(), Some("aspaaseastus2.asazure.windows.net"));
        assert_eq!(find_host("<html>nope</html>", ".asazure."), None);
        assert_eq!(host_in("abc.pbidedicated.windows.net", ".pbidedicated.").as_deref(), Some("abc.pbidedicated.windows.net"));
        assert_eq!(host_in("https://pbipwus16-westus.pbidedicated.windows.net/webapi/xmla", ".pbidedicated.").as_deref(), Some("pbipwus16-westus.pbidedicated.windows.net"));
        assert_eq!(url_encode("Fabric test"), "Fabric%20test");
    }

    #[test]
    fn workspace_records() {
        let ws = serde_json::json!({"id": "eb24c8db-6189-4c38-94d3-390fabe47782", "name": "Fabric test", "capacitySku": "Premium", "capacityObjectId": "ba8d4c98-e720-4344-91d4-fb18bdc13a17", "capacityUri": "pbidedicated://westus.pbidedicated.windows.net/ba8d4c98-e720-4344-91d4-fb18bdc13a17", "isSharedOnPremium": false});
        let w = workspace_from_json(&ws).unwrap();
        assert_eq!(w.id, "eb24c8db-6189-4c38-94d3-390fabe47782");
        assert_eq!(w.capacity_id, "ba8d4c98-e720-4344-91d4-fb18bdc13a17");
        assert_eq!(w.rollout_host, "westus.pbidedicated.windows.net");
        // the capacity id from the uri when capacityObjectId is absent
        let ws = serde_json::json!({"id": "w", "name": "n", "capacitySku": "Premium", "capacityUri": "pbidedicated://westus.pbidedicated.windows.net/cap-1/"});
        assert_eq!(workspace_from_json(&ws).unwrap().capacity_id, "cap-1");
        // a Pro workspace has no capacity
        let ws = serde_json::json!({"id": "w", "name": "n", "capacitySku": "Shared"});
        assert!(workspace_from_json(&ws).is_none());
    }

    #[tokio::test]
    async fn http_target_is_as_given() {
        let http = reqwest::Client::new();
        let t = TabularTarget::Http { url: " http://localhost/olap/msmdpump.dll ".into(), model: "AdventureWorks".into() };
        let e = resolve(&http, &t, &Credential::Anonymous, &Hosts::default()).await.unwrap();
        assert_eq!(e, Endpoint { url: "http://localhost/olap/msmdpump.dll".into(), headers: Vec::new(), catalog: "AdventureWorks".into(), auth: Auth::Credential(Credential::Anonymous), paas: false });
        // Power BI needs a bearer
        let t = TabularTarget::PowerBi { workspace_id: "w".into(), workspace_name: "W".into(), dataset_id: "d".into(), model_name: "M".into() };
        assert!(matches!(resolve(&http, &t, &Credential::Anonymous, &Hosts::default()).await, Err(TabularError::Unsupported(_))));
    }

    #[test]
    fn keys_and_caches() {
        let t = TabularTarget::AzureAs { region: "westeurope".into(), server: "srv".into(), model: "m".into() };
        assert_eq!(cache_key(&t), "asazure:westeurope/srv");
        let c = Cluster { fqdn: "cluster.asazure.windows.net".into(), server: "srv".into() };
        remember_cluster(&cache_key(&t), &c);
        assert_eq!(cached_cluster(&cache_key(&t)), Some(c));
        forget(&t);
        assert_eq!(cached_cluster(&cache_key(&t)), None);
    }
}
