//! OneLake directory listing (ADLS Gen2 `filesystem` list API on `onelake.dfs.fabric.microsoft.com`).
//!
//! The Fabric REST `lakehouses/{id}/tables` endpoint refuses schema-enabled lakehouses
//! (`UnsupportedOperationForSchemasEnabledLakehouse`), and what a Spark session can mount is the
//! storage layout anyway: `Tables/<table>/_delta_log`, or `Tables/<schema>/<table>/_delta_log` in
//! a schema-enabled lakehouse (both can coexist). The token is an Azure Storage token
//! (`https://storage.azure.com/.default`) or a Fabric API token carrying `OneLake.ReadWrite.All`.

use crate::client::{FabricError, Result};
use std::time::Duration;

pub const ONELAKE_DFS: &str = "https://onelake.dfs.fabric.microsoft.com";
const API_VERSION: &str = "2023-11-03";

/// A Delta table found under a lakehouse's `Tables/` folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OneLakeTable {
    /// `Some("dbo")` for `Tables/dbo/<name>`; `None` for `Tables/<name>`.
    pub schema: Option<String>,
    pub name: String,
}

impl OneLakeTable {
    /// `schema/name` or `name`: the path below `Tables/`.
    pub fn rel_path(&self) -> String {
        match &self.schema {
            Some(s) => format!("{s}/{}", self.name),
            None => self.name.clone(),
        }
    }
}

#[derive(Clone)]
pub struct OneLakeClient {
    http: reqwest::Client,
    base: String,
    token: String,
}

#[derive(serde::Deserialize)]
struct ListResponse {
    #[serde(default)]
    paths: Vec<PathEntry>,
}

#[derive(serde::Deserialize)]
struct PathEntry {
    name: String,
    #[serde(rename = "isDirectory", default)]
    is_directory: Option<String>,
    #[serde(rename = "contentLength", default)]
    content_length: Option<serde_json::Value>,
}

/// One child of a OneLake directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
}

impl OneLakeClient {
    pub fn new(token: impl Into<String>) -> Self {
        Self::with_base(ONELAKE_DFS, token)
    }

    pub fn with_base(base: impl Into<String>, token: impl Into<String>) -> Self {
        let http = reqwest::Client::builder().user_agent("cobalt-sqlworks").timeout(Duration::from_secs(30)).build().expect("reqwest client");
        Self { http, base: base.into().trim_end_matches('/').to_string(), token: token.into() }
    }

    /// Immediate children of `directory` (`<lakehouse-id>/Tables`), following continuation tokens.
    pub async fn list_dir(&self, workspace_id: &str, directory: &str) -> Result<Vec<DirEntry>> {
        let mut out = Vec::new();
        let mut continuation: Option<String> = None;
        loop {
            let mut url = format!("{}/{workspace_id}?resource=filesystem&recursive=false&directory={}", self.base, urlencode(directory));
            if let Some(c) = &continuation {
                url.push_str("&continuation=");
                url.push_str(&urlencode(c));
            }
            let resp = self.http.get(&url).bearer_auth(&self.token).header("x-ms-version", API_VERSION).send().await?;
            let status = resp.status();
            let next = resp.headers().get("x-ms-continuation").and_then(|v| v.to_str().ok()).filter(|s| !s.is_empty()).map(|s| s.to_string());
            if status.as_u16() == 401 || status.as_u16() == 403 {
                // OneLake says why in the body or the WWW-Authenticate header; keep it
                let www = resp.headers().get("www-authenticate").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
                let body = resp.text().await.unwrap_or_default();
                let detail = format!("{} {}", www, body.lines().next().unwrap_or("")).trim().chars().take(400).collect::<String>();
                return Err(FabricError::Api { status: status.as_u16(), code: "OneLake".into(), message: if detail.is_empty() { "unauthorized".into() } else { detail } });
            }
            let body = resp.text().await?;
            if !status.is_success() {
                let v: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
                let code = v.pointer("/error/code").and_then(|c| c.as_str()).unwrap_or("").to_string();
                let message = v.pointer("/error/message").and_then(|c| c.as_str()).unwrap_or(&body).lines().next().unwrap_or("").to_string();
                return Err(FabricError::Api { status: status.as_u16(), code, message });
            }
            let parsed: ListResponse = serde_json::from_str(&body).map_err(|e| FabricError::Decode(format!("OneLake list: {e}")))?;
            for p in parsed.paths {
                let base = p.name.rsplit('/').next().unwrap_or(&p.name).to_string();
                let is_dir = p.is_directory.as_deref().map(|s| s.eq_ignore_ascii_case("true")).unwrap_or(false);
                let size = match &p.content_length {
                    Some(serde_json::Value::String(s)) => s.parse().unwrap_or(0),
                    Some(serde_json::Value::Number(n)) => n.as_u64().unwrap_or(0),
                    _ => 0,
                };
                out.push(DirEntry { name: base, is_dir, size });
            }
            out.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
            match next {
                Some(c) => continuation = Some(c),
                None => break,
            }
        }
        Ok(out)
    }

    /// Every Delta table under `Tables/`: top-level folders with a `_delta_log`, and, for folders
    /// without one (schemas), their table folders. Sorted by `schema/name`.
    pub async fn list_tables(&self, workspace_id: &str, lakehouse_id: &str) -> Result<Vec<OneLakeTable>> {
        let root = format!("{lakehouse_id}/Tables");
        let mut out = Vec::new();
        for e in self.list_dir(workspace_id, &root).await? {
            let name = e.name;
            if !e.is_dir || name.starts_with('_') || name.starts_with('.') {
                continue;
            }
            let kids = self.list_dir(workspace_id, &format!("{root}/{name}")).await?;
            if kids.iter().any(|k| k.name == "_delta_log") {
                out.push(OneLakeTable { schema: None, name });
            } else {
                for k in kids {
                    if k.is_dir && !k.name.starts_with('_') && !k.name.starts_with('.') {
                        out.push(OneLakeTable { schema: Some(name.clone()), name: k.name });
                    }
                }
            }
        }
        out.sort_by(|a, b| a.rel_path().to_lowercase().cmp(&b.rel_path().to_lowercase()));
        Ok(out)
    }
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rel_paths_and_encoding() {
        assert_eq!(OneLakeTable { schema: Some("dbo".into()), name: "t".into() }.rel_path(), "dbo/t");
        assert_eq!(OneLakeTable { schema: None, name: "t".into() }.rel_path(), "t");
        assert_eq!(urlencode("a b/c=d"), "a%20b/c%3Dd");
    }

    #[test]
    fn parses_list_response() {
        let r: ListResponse = serde_json::from_str(r#"{"paths":[{"name":"lh/Tables/dbo","isDirectory":"true","etag":"x"},{"name":"lh/Tables/f.txt","contentLength":"1234"}]}"#).unwrap();
        assert_eq!(r.paths.len(), 2);
        assert_eq!(r.paths[0].is_directory.as_deref(), Some("true"));
        assert!(r.paths[1].is_directory.is_none());
        assert_eq!(r.paths[1].content_length, Some(serde_json::Value::String("1234".into())));
    }
}
