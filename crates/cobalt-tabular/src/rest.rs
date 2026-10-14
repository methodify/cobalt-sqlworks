//! The Power BI Arrow query API: `POST …/datasets/{id}/executeDaxQueries` with a bearer token
//! for `https://analysis.windows.net/powerbi/api`, `Accept: application/vnd.apache.arrow.stream`,
//! the body streamed through `decode::Decoder` as chunks arrive. HTTP refusals become
//! `TabularError`; engine errors arrive inside the body as `DaxItem::Error`.

use crate::decode::{collect_items, DaxItem, DaxResult, Decoder};
use crate::TabularError;
use bytes::Bytes;
use cobalt_core::tabular::TabularTarget;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// `https://api.powerbi.com/v1.0/myorg`.
pub const DEFAULT_BASE: &str = "https://api.powerbi.com/v1.0/myorg";

/// The Power BI token audience (resource) this client needs.
pub const POWERBI_RESOURCE: &str = "https://analysis.windows.net/powerbi/api";

const APPLICATION_CONTEXT: &str = "{\"app\":\"Cobalt SQL Works\"}";

/// Per-run knobs; every field optional. `roles` / `effective_username` / `custom_data` are the
/// RLS "view as" controls (workspace admins only).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DaxOptions {
    /// `queryTimeout` in seconds; the service's default applies when `None`.
    #[serde(default)]
    pub timeout_secs: Option<u32>,
    /// `resultSetRowCountLimit`; the service defaults to 1,000,000.
    #[serde(default)]
    pub row_limit: Option<u64>,
    /// `schemaOnly`: columns and types, no rows.
    #[serde(default)]
    pub schema_only: bool,
    #[serde(default)]
    pub effective_username: Option<String>,
    #[serde(default)]
    pub roles: Vec<String>,
    #[serde(default)]
    pub custom_data: Option<String>,
    /// `culture`, e.g. `en-US`, for locale-aware formatting functions.
    #[serde(default)]
    pub culture: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RequestBody<'a> {
    query: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    query_timeout: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result_set_row_count_limit: Option<u64>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    schema_only: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    effective_username: Option<&'a str>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    roles: Vec<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    custom_data: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    culture: Option<&'a str>,
    application_context: &'static str,
}

/// The JSON the API wants for one run; null fields are omitted.
pub fn request_json(dax: &str, opts: &DaxOptions) -> serde_json::Value {
    let body = RequestBody {
        query: dax,
        query_timeout: opts.timeout_secs,
        result_set_row_count_limit: opts.row_limit,
        schema_only: opts.schema_only,
        effective_username: opts.effective_username.as_deref().filter(|s| !s.trim().is_empty()),
        roles: opts.roles.iter().map(String::as_str).filter(|s| !s.trim().is_empty()).collect(),
        custom_data: opts.custom_data.as_deref(),
        culture: opts.culture.as_deref().filter(|s| !s.trim().is_empty()),
        application_context: APPLICATION_CONTEXT,
    };
    serde_json::to_value(body).unwrap_or(serde_json::Value::Null)
}

/// One token, many runs. Cheap to clone.
#[derive(Clone)]
pub struct PowerBiClient {
    http: reqwest::Client,
    base: String,
    token: String,
}

impl PowerBiClient {
    /// `token` is a bearer access token for [`POWERBI_RESOURCE`].
    pub fn new(token: String) -> Self {
        Self::with_base(DEFAULT_BASE, token)
    }

    pub fn with_base(base: impl Into<String>, token: String) -> Self {
        // no overall timeout: a long query sends its first batch only when the engine is done
        let http = reqwest::Client::builder().user_agent(concat!("cobalt-sqlworks/", env!("CARGO_PKG_VERSION"))).connect_timeout(Duration::from_secs(30)).build().expect("reqwest client");
        Self { http, base: base.into().trim_end_matches('/').to_string(), token }
    }

    /// The `executeDaxQueries` URL for a target; `AzureAs` / `Http` targets are refused.
    pub fn query_url(&self, target: &TabularTarget) -> Result<String, TabularError> {
        match target {
            TabularTarget::PowerBi { workspace_id, dataset_id, .. } if workspace_id.trim().is_empty() => Ok(format!("{}/datasets/{dataset_id}/executeDaxQueries", self.base)),
            TabularTarget::PowerBi { workspace_id, dataset_id, .. } => Ok(format!("{}/groups/{workspace_id}/datasets/{dataset_id}/executeDaxQueries", self.base)),
            other => Err(TabularError::Unsupported(format!("{} is reached over XMLA, not the Arrow query API", crate::describe_target(other)))),
        }
    }

    /// Run one DAX query text (any number of `EVALUATE`s) and stream its result sets.
    pub async fn execute(&self, target: &TabularTarget, dax: &str, opts: &DaxOptions) -> Result<DaxStream, TabularError> {
        let url = self.query_url(target)?;
        let body = request_json(dax, opts);
        for attempt in 0..2 {
            let mut req = self.http.post(&url).bearer_auth(&self.token).header("Accept", "application/vnd.apache.arrow.stream").json(&body);
            if let Some(secs) = opts.timeout_secs {
                req = req.timeout(Duration::from_secs(u64::from(secs) + 120));
            }
            let resp = req.send().await?;
            let status = resp.status().as_u16();
            if status == 429 {
                let retry_after = retry_after(&resp);
                if attempt == 0 && retry_after <= 5 {
                    tokio::time::sleep(Duration::from_secs(retry_after)).await;
                    continue;
                }
                return Err(TabularError::Throttled { retry_after });
            }
            if !resp.status().is_success() {
                let text = resp.text().await.unwrap_or_default();
                return Err(parse_http_error(status, &text));
            }
            return Ok(DaxStream { source: Source::Http(resp), decoder: Decoder::new(), done: false });
        }
        Err(TabularError::Throttled { retry_after: 10 })
    }
}

fn retry_after(resp: &reqwest::Response) -> u64 {
    resp.headers().get("Retry-After").and_then(|v| v.to_str().ok()).and_then(|s| s.trim().parse::<u64>().ok()).unwrap_or(10)
}

/// A non-2xx answer to `TabularError`. Understands `{"error":{"code","message"}}`, the
/// `pbi.error` shape with `details`, and Fabric's `{"errorCode","message"}`; anything else
/// keeps the body's first line.
pub fn parse_http_error(status: u16, body: &str) -> TabularError {
    let v: serde_json::Value = serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
    let (code, message) = error_parts(&v).unwrap_or_else(|| (String::new(), body.trim().lines().next().unwrap_or("").chars().take(500).collect()));
    match status {
        401 | 403 => {
            let detail = if message.is_empty() {
                code
            } else if code.is_empty() || message.contains(code.as_str()) {
                message
            } else {
                format!("{code}: {message}")
            };
            TabularError::Unauthorized(status, detail)
        }
        _ => TabularError::Api { status, code, message },
    }
}

fn error_parts(v: &serde_json::Value) -> Option<(String, String)> {
    let s = |x: Option<&serde_json::Value>| x.and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    if let Some(err) = v.get("error") {
        let code = s(err.get("code")).unwrap_or_default();
        let mut message = s(err.get("message")).unwrap_or_default();
        if let Some(pbi) = err.get("pbi.error") {
            let pbi_code = s(pbi.get("code")).unwrap_or_default();
            let details: Vec<String> = pbi.get("details").and_then(|d| d.as_array()).map(|a| a.iter().filter_map(|d| s(d.pointer("/detail/value"))).collect()).unwrap_or_default();
            if message.is_empty() {
                message = details.join("; ");
            } else if !details.is_empty() {
                message = format!("{message} ({})", details.join("; "));
            }
            let code = if code.is_empty() { pbi_code } else { code };
            return Some((code, message));
        }
        if code.is_empty() && message.is_empty() {
            return None;
        }
        return Some((code, message));
    }
    let code = s(v.get("errorCode")).or_else(|| s(v.get("code")));
    let message = s(v.get("message"));
    match (code, message) {
        (None, None) => None,
        (c, m) => Some((c.unwrap_or_default(), m.unwrap_or_default())),
    }
}

enum Source {
    Http(reqwest::Response),
    Chunks(std::vec::IntoIter<Bytes>),
    /// Items already decoded (the XMLA transport parses a whole SOAP body at once).
    Items(std::vec::IntoIter<DaxItem>),
}

impl Source {
    async fn next_chunk(&mut self) -> Result<Option<Bytes>, TabularError> {
        match self {
            Source::Http(resp) => Ok(resp.chunk().await?),
            Source::Chunks(it) => Ok(it.next()),
            Source::Items(_) => Ok(None),
        }
    }
}

/// The result sets of one run, pulled item by item as the body streams in (or, for a
/// transport that decodes its whole response first, from a list of items).
pub struct DaxStream {
    source: Source,
    decoder: Decoder,
    done: bool,
}

impl DaxStream {
    /// A stream over bytes already in hand (tests, recorded bodies).
    pub fn from_chunks(chunks: impl IntoIterator<Item = Bytes>) -> Self {
        Self { source: Source::Chunks(chunks.into_iter().collect::<Vec<_>>().into_iter()), decoder: Decoder::new(), done: false }
    }

    /// A stream over items decoded elsewhere (the XMLA rowset parser).
    pub fn from_items(items: Vec<DaxItem>) -> Self {
        Self { source: Source::Items(items.into_iter()), decoder: Decoder::new(), done: true }
    }

    /// The next item, `None` when the body is over.
    pub async fn next(&mut self) -> Result<Option<DaxItem>, TabularError> {
        if let Source::Items(it) = &mut self.source {
            return Ok(it.next());
        }
        loop {
            if let Some(item) = self.decoder.pop() {
                return Ok(Some(item));
            }
            if self.done {
                return match self.decoder.take_error() {
                    Some(e) => Err(e),
                    None => Ok(None),
                };
            }
            match self.source.next_chunk().await? {
                Some(chunk) => self.decoder.push(&chunk)?,
                None => {
                    self.done = true;
                    self.decoder.finish();
                }
            }
        }
    }

    /// Everything, in memory, grouped per `EVALUATE`.
    pub async fn collect(mut self) -> Result<Vec<DaxResult>, TabularError> {
        let mut items = Vec::new();
        while let Some(item) = self.next().await? {
            items.push(item);
        }
        Ok(collect_items(items))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::testing::*;

    #[test]
    fn body_omits_nulls() {
        let v = request_json("EVALUATE {1}", &DaxOptions::default());
        assert_eq!(v, serde_json::json!({"query": "EVALUATE {1}", "applicationContext": APPLICATION_CONTEXT}));
        let opts = DaxOptions { timeout_secs: Some(60), row_limit: Some(5000), schema_only: true, effective_username: Some("u@x.com".into()), roles: vec!["R".into(), String::new()], custom_data: Some("NA".into()), culture: None };
        let v = request_json("EVALUATE T", &opts);
        assert_eq!(v["queryTimeout"], 60);
        assert_eq!(v["resultSetRowCountLimit"], 5000);
        assert_eq!(v["schemaOnly"], true);
        assert_eq!(v["effectiveUsername"], "u@x.com");
        assert_eq!(v["roles"], serde_json::json!(["R"]));
        assert_eq!(v["customData"], "NA");
        assert!(v.get("culture").is_none());
    }

    #[test]
    fn urls() {
        let c = PowerBiClient::new("t".into());
        let t = TabularTarget::PowerBi { workspace_id: "W".into(), workspace_name: String::new(), dataset_id: "D".into(), model_name: String::new() };
        assert_eq!(c.query_url(&t).unwrap(), "https://api.powerbi.com/v1.0/myorg/groups/W/datasets/D/executeDaxQueries");
        let t = TabularTarget::PowerBi { workspace_id: String::new(), workspace_name: String::new(), dataset_id: "D".into(), model_name: String::new() };
        assert_eq!(c.query_url(&t).unwrap(), "https://api.powerbi.com/v1.0/myorg/datasets/D/executeDaxQueries");
        let t = TabularTarget::AzureAs { region: "r".into(), server: "s".into(), model: "m".into() };
        assert!(matches!(c.query_url(&t), Err(TabularError::Unsupported(_))));
    }

    #[test]
    fn http_errors() {
        let e = parse_http_error(400, r#"{"error":{"code":"InvalidRequest","message":"XMLA endpoint feature is disabled. Turn on the tenant setting 'Allow XMLA endpoints and Analyze in Excel with on-premises semantic models' to enable this feature."}}"#);
        let TabularError::Api { status, code, message } = &e else { panic!("{e}") };
        assert_eq!((*status, code.as_str()), (400, "InvalidRequest"));
        assert!(message.starts_with("XMLA endpoint"));
        assert_eq!(e.hint(), Some(crate::HINT_TENANT_SETTINGS));

        let e = parse_http_error(400, r#"{"error":{"code":"DatasetExecuteQueriesError","pbi.error":{"code":"DatasetExecuteQueriesError","parameters":{},"details":[{"code":"DetailsMessage","detail":{"type":1,"value":"Query (1, 1) The syntax is incorrect."}},{"code":"DetailsErrorCode","detail":{"type":1,"value":"0xC1140001"}}],"exceptionCulprit":1}}}"#);
        let TabularError::Api { code, message, .. } = &e else { panic!("{e}") };
        assert_eq!(code, "DatasetExecuteQueriesError");
        assert_eq!(message, "Query (1, 1) The syntax is incorrect.; 0xC1140001");

        let e = parse_http_error(403, r#"{"error":{"code":"PowerBINotAuthorizedException","message":""}}"#);
        assert!(matches!(&e, TabularError::Unauthorized(403, m) if m == "PowerBINotAuthorizedException"), "{e:?}");
        assert_eq!(e.hint(), Some(crate::HINT_BUILD));

        let e = parse_http_error(401, "");
        assert!(matches!(e, TabularError::Unauthorized(401, _)));
        assert_eq!(e.hint(), Some(crate::HINT_SIGN_IN));

        let e = parse_http_error(404, r#"{"errorCode":"ItemNotFound","message":"Dataset not found"}"#);
        assert!(matches!(&e, TabularError::Api { status: 404, code, message } if code == "ItemNotFound" && message == "Dataset not found"));

        let e = parse_http_error(502, "<html>Bad gateway</html>\nmore");
        assert!(matches!(&e, TabularError::Api { status: 502, code, message } if code.is_empty() && message == "<html>Bad gateway</html>"));
    }

    #[tokio::test]
    async fn stream_over_chunks() {
        let schema = sample_schema();
        let mut body = write_stream(&sample_batches(&schema), &schema, true);
        body.extend(error_stream());
        let mut s = DaxStream::from_chunks(chunked(&body, 7));
        let mut kinds = Vec::new();
        while let Some(i) = s.next().await.unwrap() {
            kinds.push(match i {
                DaxItem::ResultSetStart { .. } => "start",
                DaxItem::Batch(_) => "batch",
                DaxItem::ResultSetEnd { .. } => "end",
                DaxItem::Error { .. } => "error",
            });
        }
        assert_eq!(kinds, vec!["start", "batch", "batch", "end", "error"]);
        let r = DaxStream::from_chunks(chunked(&body, 4096)).collect().await.unwrap();
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].table().unwrap().rows, 8);
        assert!(r[1].error().is_some());
    }
}
