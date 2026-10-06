//! HTTP client: bearer token in, typed models out. Handles paging and `Retry-After`.

use crate::model::*;
use std::time::Duration;

pub const DEFAULT_BASE: &str = "https://api.fabric.microsoft.com/v1";

#[derive(Debug, thiserror::Error)]
pub enum FabricError {
    #[error("not signed in to Fabric (HTTP {0})")]
    Unauthorized(u16),
    #[error("Fabric asked us to slow down; retry in {retry_after}s")]
    Throttled { retry_after: u64 },
    #[error("Fabric returned {status}: {code} {message}")]
    Api { status: u16, code: String, message: String },
    #[error("network error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("unexpected response: {0}")]
    Decode(String),
}

pub type Result<T> = std::result::Result<T, FabricError>;

#[derive(Clone)]
pub struct FabricClient {
    http: reqwest::Client,
    base: String,
    token: String,
}

impl FabricClient {
    /// `token` is a bearer access token for `https://api.fabric.microsoft.com`.
    pub fn new(token: impl Into<String>) -> Self {
        Self::with_base(DEFAULT_BASE, token)
    }

    pub fn with_base(base: impl Into<String>, token: impl Into<String>) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(concat!("cobalt-sqlworks/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(30))
            .build()
            .expect("reqwest client");
        Self { http, base: base.into().trim_end_matches('/').to_string(), token: token.into() }
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T> {
        // one automatic retry on 429 when the wait is short
        for attempt in 0..2 {
            let resp = self.http.get(url).bearer_auth(&self.token).header("Accept", "application/json").send().await?;
            let status = resp.status();
            if status.as_u16() == 429 {
                let retry_after = resp.headers().get("Retry-After").and_then(|v| v.to_str().ok()).and_then(|s| s.parse::<u64>().ok()).unwrap_or(10);
                if attempt == 0 && retry_after <= 5 {
                    tokio::time::sleep(Duration::from_secs(retry_after)).await;
                    continue;
                }
                return Err(FabricError::Throttled { retry_after });
            }
            if status.as_u16() == 401 {
                return Err(FabricError::Unauthorized(401));
            }
            if status.as_u16() == 403 {
                let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
                let code = body.get("errorCode").and_then(|v| v.as_str()).unwrap_or("").to_string();
                if code.is_empty() {
                    return Err(FabricError::Unauthorized(403));
                }
                return Err(FabricError::Api { status: 403, code, message: body.get("message").and_then(|v| v.as_str()).unwrap_or("").to_string() });
            }
            if !status.is_success() {
                let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
                return Err(FabricError::Api {
                    status: status.as_u16(),
                    code: body.get("errorCode").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    message: body.get("message").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                });
            }
            return resp.json::<T>().await.map_err(|e| FabricError::Decode(e.to_string()));
        }
        unreachable!()
    }

    async fn get_all_pages<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<Vec<T>> {
        let mut out = Vec::new();
        let mut next: Option<String> = None;
        loop {
            let u = match &next {
                Some(tok) => format!("{url}{}continuationToken={}", if url.contains('?') { "&" } else { "?" }, tok),
                None => url.to_string(),
            };
            let page: Page<T> = self.get_json(&u).await?;
            out.extend(page.value);
            match page.continuation_token {
                Some(t) if !t.is_empty() => next = Some(t),
                _ => break,
            }
        }
        Ok(out)
    }

    /// Every workspace the signed-in principal can access.
    pub async fn list_workspaces(&self) -> Result<Vec<Workspace>> {
        let mut ws: Vec<Workspace> = self.get_all_pages(&format!("{}/workspaces", self.base)).await?;
        ws.sort_by(|a, b| (a.kind != WorkspaceKind::Personal).cmp(&(b.kind != WorkspaceKind::Personal)).then(a.display_name.to_lowercase().cmp(&b.display_name.to_lowercase())));
        Ok(ws)
    }

    /// Capacities the principal can see (admin or contributor). Requires `Capacity.Read.All`.
    pub async fn list_capacities(&self) -> Result<Vec<Capacity>> {
        self.get_all_pages(&format!("{}/capacities", self.base)).await
    }

    /// SQL-capable items in a workspace (one unfiltered listing, filtered locally).
    pub async fn list_sql_items(&self, workspace_id: &str) -> Result<Vec<SqlItem>> {
        let items: Vec<WireItem> = self.get_all_pages(&format!("{}/workspaces/{workspace_id}/items", self.base)).await?;
        let mut out: Vec<SqlItem> = items.into_iter().filter_map(WireItem::into_sql_item).collect();
        out.sort_by(|a, b| a.display_name.to_lowercase().cmp(&b.display_name.to_lowercase()));
        Ok(out)
    }

    /// Every item in a workspace, optionally one `type` (`Notebook`, `Lakehouse`…).
    pub async fn list_items(&self, workspace_id: &str, item_type: Option<&str>) -> Result<Vec<FabricItem>> {
        let url = match item_type {
            Some(t) => format!("{}/workspaces/{workspace_id}/items?type={t}", self.base),
            None => format!("{}/workspaces/{workspace_id}/items", self.base),
        };
        let items: Vec<WireItem> = self.get_all_pages(&url).await?;
        let mut out: Vec<FabricItem> = items.into_iter().map(WireItem::into_item).collect();
        out.sort_by(|a, b| a.display_name.to_lowercase().cmp(&b.display_name.to_lowercase()));
        Ok(out)
    }

    /// POST with a JSON body. Handles 429 once, maps 401/403, and returns the response for the
    /// caller to read (200 body or 202 long-running operation).
    async fn post(&self, url: &str, body: &serde_json::Value) -> Result<reqwest::Response> {
        for attempt in 0..2 {
            let resp = self.http.post(url).bearer_auth(&self.token).header("Accept", "application/json").json(body).send().await?;
            let status = resp.status();
            if status.as_u16() == 429 {
                let retry_after = resp.headers().get("Retry-After").and_then(|v| v.to_str().ok()).and_then(|s| s.parse::<u64>().ok()).unwrap_or(10);
                if attempt == 0 && retry_after <= 5 {
                    tokio::time::sleep(Duration::from_secs(retry_after)).await;
                    continue;
                }
                return Err(FabricError::Throttled { retry_after });
            }
            if status.as_u16() == 401 {
                return Err(FabricError::Unauthorized(401));
            }
            if status.as_u16() == 403 {
                let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
                let code = body.get("errorCode").and_then(|v| v.as_str()).unwrap_or("").to_string();
                if code.is_empty() {
                    return Err(FabricError::Unauthorized(403));
                }
                return Err(FabricError::Api { status: 403, code, message: body.get("message").and_then(|v| v.as_str()).unwrap_or("").to_string() });
            }
            if !status.is_success() {
                let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
                return Err(FabricError::Api {
                    status: status.as_u16(),
                    code: body.get("errorCode").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    message: body.get("message").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                });
            }
            return Ok(resp);
        }
        unreachable!()
    }

    /// Finish a long-running operation (202 + `Location`): poll until it succeeds, then fetch
    /// `{Location}/result` when the caller wants a body. Returns `None` for 200 responses that
    /// carried their body directly (returned as `Some(body)` instead).
    async fn complete_lro(&self, resp: reqwest::Response, want_result: bool) -> Result<Option<serde_json::Value>> {
        if resp.status().as_u16() != 202 {
            if !want_result {
                return Ok(None);
            }
            let body: serde_json::Value = resp.json().await.map_err(|e| FabricError::Decode(e.to_string()))?;
            return Ok(Some(body));
        }
        let location = resp.headers().get("Location").and_then(|v| v.to_str().ok()).map(str::to_string);
        let op_id = resp.headers().get("x-ms-operation-id").and_then(|v| v.to_str().ok()).map(str::to_string);
        let mut wait = resp.headers().get("Retry-After").and_then(|v| v.to_str().ok()).and_then(|s| s.parse::<u64>().ok()).unwrap_or(2).clamp(1, 20);
        let location = match (location, op_id) {
            (Some(l), _) => l,
            (None, Some(id)) => format!("{}/operations/{id}", self.base),
            (None, None) => return Err(FabricError::Decode("202 without Location or x-ms-operation-id".into())),
        };
        let deadline = std::time::Instant::now() + Duration::from_secs(300);
        loop {
            tokio::time::sleep(Duration::from_secs(wait)).await;
            let st: serde_json::Value = self.get_json(&location).await?;
            match st.get("status").and_then(|v| v.as_str()).unwrap_or("") {
                "Succeeded" => break,
                "Failed" | "Cancelled" => {
                    let err = st.get("error").cloned().unwrap_or(serde_json::Value::Null);
                    return Err(FabricError::Api { status: 200, code: err.get("errorCode").and_then(|v| v.as_str()).unwrap_or("OperationFailed").to_string(), message: err.get("message").and_then(|v| v.as_str()).unwrap_or("the operation failed").to_string() });
                }
                _ => {
                    if std::time::Instant::now() > deadline {
                        return Err(FabricError::Decode("the operation did not finish within 5 minutes".into()));
                    }
                    wait = (wait + 1).min(10);
                }
            }
        }
        if !want_result {
            return Ok(None);
        }
        let result: serde_json::Value = self.get_json(&format!("{}/result", location.trim_end_matches('/'))).await?;
        Ok(Some(result))
    }

    /// The item's definition (files as base64 parts). `format` is e.g. `ipynb` for notebooks;
    /// None returns the item's default form (notebooks: the Git `.py` source).
    pub async fn get_item_definition(&self, workspace_id: &str, item_id: &str, format: Option<&str>) -> Result<ItemDefinition> {
        let url = match format {
            Some(f) => format!("{}/workspaces/{workspace_id}/items/{item_id}/getDefinition?format={f}", self.base),
            None => format!("{}/workspaces/{workspace_id}/items/{item_id}/getDefinition", self.base),
        };
        let resp = self.post(&url, &serde_json::json!({})).await?;
        let body = self.complete_lro(resp, true).await?.unwrap_or(serde_json::Value::Null);
        let def = body.get("definition").cloned().ok_or_else(|| FabricError::Decode("getDefinition returned no definition".into()))?;
        serde_json::from_value(def).map_err(|e| FabricError::Decode(e.to_string()))
    }

    /// Replace the item's definition.
    pub async fn update_item_definition(&self, workspace_id: &str, item_id: &str, definition: &ItemDefinition) -> Result<()> {
        let url = format!("{}/workspaces/{workspace_id}/items/{item_id}/updateDefinition?updateMetadata=false", self.base);
        let resp = self.post(&url, &serde_json::json!({"definition": definition})).await?;
        self.complete_lro(resp, false).await?;
        Ok(())
    }

    /// Create a notebook item from a definition; returns the new item.
    pub async fn create_notebook(&self, workspace_id: &str, display_name: &str, definition: &ItemDefinition) -> Result<FabricItem> {
        let url = format!("{}/workspaces/{workspace_id}/notebooks", self.base);
        let resp = self.post(&url, &serde_json::json!({"displayName": display_name, "definition": definition})).await?;
        let body = self.complete_lro(resp, true).await?.unwrap_or(serde_json::Value::Null);
        let w: WireItem = serde_json::from_value(body).map_err(|e| FabricError::Decode(format!("create notebook: {e}")))?;
        Ok(w.into_item())
    }

    /// Connection details for one item.
    pub async fn item_detail(&self, item: &SqlItem) -> Result<SqlItemDetail> {
        let url = format!("{}/workspaces/{}/{}/{}", self.base, item.workspace_id, item.kind.detail_segment(), item.id);
        let d: WireDetail = self.get_json(&url).await?;
        let target = d.into_target(item).ok_or_else(|| FabricError::Decode(format!("{} {} has no SQL connection details", item.kind.label(), item.display_name)))?;
        Ok(SqlItemDetail { item: item.clone(), target })
    }
}
