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
            if status.as_u16() == 401 || status.as_u16() == 403 {
                return Err(FabricError::Unauthorized(status.as_u16()));
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

    /// Connection details for one item.
    pub async fn item_detail(&self, item: &SqlItem) -> Result<SqlItemDetail> {
        let url = format!("{}/workspaces/{}/{}/{}", self.base, item.workspace_id, item.kind.detail_segment(), item.id);
        let d: WireDetail = self.get_json(&url).await?;
        let target = d.into_target(item).ok_or_else(|| FabricError::Decode(format!("{} {} has no SQL connection details", item.kind.label(), item.display_name)))?;
        Ok(SqlItemDetail { item: item.clone(), target })
    }
}
