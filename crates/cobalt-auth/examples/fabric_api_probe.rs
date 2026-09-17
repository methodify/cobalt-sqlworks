//! Dev helper: exchange a profile's cached refresh token for a Fabric REST token and walk
//! workspaces → items → SQL connection details, printing what a Fabric explorer would need.
//! Usage: cargo run -p cobalt-auth --example fabric_api_probe -- <profile-id> [tenant]
use cobalt_auth::{CredentialResolver, KeyringStore, SecretStore};
use cobalt_core::ProfileId;
use serde_json::Value;

const FABRIC_SCOPE: &str = "https://api.fabric.microsoft.com/.default offline_access openid profile";
const CLIENT_ID: &str = cobalt_core::DEFAULT_ENTRA_CLIENT_ID;

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let id = ProfileId::parse(&args.next().expect("profile id")).expect("uuid");
    let tenant = args.next().unwrap_or_else(|| "organizations".into());
    let http = reqwest::Client::new();
    let login = std::env::var("FABRIC_LOGIN").is_ok();

    // 1. token for the Fabric API audience: interactive (FABRIC_LOGIN=1, prompts consent for the
    //    registration's Fabric permissions) or silently via the profile's cached refresh token.
    let access: String = if login {
        let mut cfg = cobalt_auth::EntraConfig::new(CLIENT_ID).with_tenant(Some(&tenant));
        cfg.sql_resource = "https://api.fabric.microsoft.com".into();
        let ts = cobalt_auth::entra::interactive_login(&cfg, None, |url| println!("open in browser: {url}"), cobalt_auth::entra::CancelToken::new())
            .await
            .expect("interactive login");
        println!("interactive login ok as {} (scope: {})", ts.account.username, ts.access.scope);
        if let Some(rt) = &ts.refresh {
            // keep it so the silent path works next time
            KeyringStore.set(&CredentialResolver::refresh_token_ref(&id), rt).expect("store refresh token");
            println!("refresh token stored on profile {id}");
        }
        ts.access.token.expose().to_string()
    } else {
        let rt = KeyringStore.get(&CredentialResolver::refresh_token_ref(&id)).expect("keyring").expect("no refresh token stored");
        let form = [("client_id", CLIENT_ID), ("grant_type", "refresh_token"), ("refresh_token", rt.expose()), ("scope", FABRIC_SCOPE)];
        let tok: Value = http
            .post(format!("https://login.microsoftonline.com/{tenant}/oauth2/v2.0/token"))
            .form(&form)
            .send()
            .await
            .expect("token request")
            .json()
            .await
            .expect("token json");
        let Some(access) = tok.get("access_token").and_then(|v| v.as_str()) else {
            println!("TOKEN ERROR: {}", serde_json::to_string_pretty(&tok).unwrap());
            return;
        };
        println!("fabric token ok, scope granted: {}", tok.get("scope").and_then(|v| v.as_str()).unwrap_or("?"));
        access.to_string()
    };
    let access = access.as_str();

    let get = |url: String| {
        let http = http.clone();
        let access = access.to_string();
        async move {
            let r = http.get(&url).bearer_auth(&access).send().await.expect("get");
            let status = r.status();
            let body: Value = r.json().await.unwrap_or(Value::Null);
            (status, body)
        }
    };

    // 2. workspaces
    let (st, ws) = get("https://api.fabric.microsoft.com/v1/workspaces".into()).await;
    println!("GET /v1/workspaces -> {st}");
    let Some(list) = ws.get("value").and_then(|v| v.as_array()) else {
        println!("{}", serde_json::to_string_pretty(&ws).unwrap());
        return;
    };
    for w in list {
        let wid = w["id"].as_str().unwrap_or("");
        println!("\nWORKSPACE {} ({}) type={} capacity={}", w["displayName"], wid, w["type"], w["capacityId"].as_str().unwrap_or("-"));
        // 3. items (all types) for this workspace
        let (st, items) = get(format!("https://api.fabric.microsoft.com/v1/workspaces/{wid}/items")).await;
        if !st.is_success() {
            println!("  items -> {st} {}", items);
            continue;
        }
        for it in items.get("value").and_then(|v| v.as_array()).cloned().unwrap_or_default() {
            let ty = it["type"].as_str().unwrap_or("");
            let iid = it["id"].as_str().unwrap_or("");
            println!("  ITEM {:<18} {}  ({})", ty, it["displayName"], iid);
            // 4. SQL connection details per type
            let detail_url = match ty {
                "Warehouse" => Some(format!("https://api.fabric.microsoft.com/v1/workspaces/{wid}/warehouses/{iid}")),
                "Lakehouse" => Some(format!("https://api.fabric.microsoft.com/v1/workspaces/{wid}/lakehouses/{iid}")),
                "SQLDatabase" => Some(format!("https://api.fabric.microsoft.com/v1/workspaces/{wid}/sqlDatabases/{iid}")),
                "MirroredDatabase" => Some(format!("https://api.fabric.microsoft.com/v1/workspaces/{wid}/mirroredDatabases/{iid}")),
                "SQLEndpoint" => Some(format!("https://api.fabric.microsoft.com/v1/workspaces/{wid}/sqlEndpoints/{iid}")),
                _ => None,
            };
            if let Some(u) = detail_url {
                let (st, d) = get(u).await;
                let props = d.get("properties").cloned().unwrap_or(Value::Null);
                println!("      detail -> {st} properties={}", serde_json::to_string(&props).unwrap_or_default());
            }
        }
    }
}
