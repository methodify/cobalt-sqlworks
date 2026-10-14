//! XMLA over HTTPS: plain SOAP 1.1 `Discover` / `Execute` against Power BI's dedicated
//! clusters, Azure Analysis Services and `msmdpump.dll` (or any XMLA URL). One session per
//! client, a real `<Cancel/>`, text XML only (the capability flags never advertise binary XML or
//! XPRESS), DAX / MDX / DMV text alike. Results come back as the same `DaxItem` sequence the
//! Arrow query API yields.
//!
//! Two flavours of endpoint (`resolve::Endpoint::paas`):
//! - **classic** (`msmdpump.dll`): `SOAPAction`, `X-Transport-Caps-Negotiation-Flags`, `X-AS-*`
//!   ids, the session opened by `BeginSession` on the first `Discover`;
//! - **PaaS** (Power BI, Azure AS): the official client's header set (`x-ms-*`, no `SOAPAction`),
//!   `Authorization: MwcToken` for Power BI, the session opened by an `Execute` with an empty
//!   statement under `BeginSession`, `x-ms-routing-hint` / `x-ms-workload-resource-moniker`
//!   echoed back, a `3xx` taken as "the instance moved" (cluster cache cleared).
//!
//! `soap` builds envelopes, `rowset` parses answers, `resolve` finds the URL and the credential
//! exchange per target, `tmschema` joins the DMVs into a `ModelCatalog`.

pub mod resolve;
pub mod rowset;
pub mod soap;
pub mod tmschema;

use crate::catalog::{attach_hierarchies, catalog_from_results, catalog_query, functions_from_table, DaxFunction, FUNCTIONS_QUERY, HIERARCHY_QUERY};
use crate::decode::{collect_items, DaxItem, DaxResult, DaxTable};
use crate::rest::{DaxOptions, DaxStream};
use crate::TabularError;
use cobalt_core::tabular::{ModelCatalog, TabularTarget};
pub use resolve::Hosts;
use resolve::{Auth, Endpoint};
use rowset::Parsed;
use soap::{ExecuteProps, Headers, SessionHeader, ACTION_DISCOVER, ACTION_EXECUTE};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

/// The Entra resource a bearer token for Azure Analysis Services is minted for.
pub const AZURE_AS_RESOURCE: &str = "https://*.asazure.windows.net";

/// The `User-Agent` on classic endpoints.
pub const USER_AGENT: &str = concat!("cobalt-sqlworks/", env!("CARGO_PKG_VERSION"));

/// `x-ms-xmlaapp-general-info` on PaaS endpoints.
pub const APP_GENERAL_INFO: &str = "AppName=cobalt,AadClient=MSAL,CacheUsed=0,ConStrId=1";

/// Response headers a PaaS cluster wants echoed on the next request.
const ECHO_HEADERS: [&str; 3] = ["x-ms-routing-hint", "x-ms-workload-resource-moniker", "x-ms-root-activity-id"];

/// How the client signs in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Credential {
    Bearer(String),
    Basic { user: String, password: String },
    Anonymous,
}

impl std::fmt::Display for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Credential::Bearer(_) => f.write_str("bearer token"),
            Credential::Basic { user, .. } => write!(f, "basic ({user})"),
            Credential::Anonymous => f.write_str("anonymous"),
        }
    }
}

/// True for text the Arrow query API cannot run: MDX (`SELECT … ON COLUMNS … FROM [Cube]`) and
/// DMV queries (`SELECT … FROM $SYSTEM.…`). DAX (`EVALUATE`, `DEFINE`) is false.
pub fn is_mdx_or_dmv(text: &str) -> bool {
    let body = strip_leading_comments(text);
    let upper = body.to_ascii_uppercase();
    let first = upper.split_whitespace().next().unwrap_or("");
    if first == "EVALUATE" || first == "DEFINE" {
        return false;
    }
    if first != "SELECT" && first != "WITH" {
        return false;
    }
    [" ON COLUMNS", " ON 0", "$SYSTEM.", "FROM ["].iter().any(|m| upper.contains(m))
}

fn strip_leading_comments(text: &str) -> &str {
    let mut s = text.trim_start_matches('\u{feff}').trim_start();
    loop {
        if let Some(rest) = s.strip_prefix("//").or_else(|| s.strip_prefix("--")) {
            s = rest.find('\n').map(|i| &rest[i + 1..]).unwrap_or("").trim_start();
        } else if let Some(rest) = s.strip_prefix("/*") {
            s = rest.find("*/").map(|i| &rest[i + 2..]).unwrap_or("").trim_start();
        } else {
            return s;
        }
    }
}

/// The Windows LCID for a culture name; 1033 (en-US) when unknown.
pub fn locale_id(culture: Option<&str>) -> u32 {
    match culture.map(|c| c.trim().to_ascii_lowercase()).as_deref() {
        Some("en-gb") => 2057,
        Some("en-au") => 3081,
        Some("en-ca") => 4105,
        Some("de-de") => 1031,
        Some("fr-fr") => 1036,
        Some("es-es") => 3082,
        Some("it-it") => 1040,
        Some("nl-nl") => 1043,
        Some("pt-br") => 1046,
        Some("pt-pt") => 2070,
        Some("ja-jp") => 1041,
        Some("zh-cn") => 2052,
        Some("sv-se") => 1053,
        Some("da-dk") => 1030,
        Some("nb-no") => 1044,
        Some("fi-fi") => 1035,
        Some("pl-pl") => 1045,
        Some("ru-ru") => 1049,
        _ => 1033,
    }
}

/// The HTTP timeout for a run: the query timeout plus a minute, or a day when there is none.
fn request_timeout(timeout_secs: Option<u32>) -> Duration {
    match timeout_secs {
        Some(s) if s > 0 => Duration::from_secs(u64::from(s) + 60),
        _ => Duration::from_secs(24 * 3600),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Discover,
    Execute,
}

/// One endpoint, one session, many runs. Not `Clone`: the session belongs to this value;
/// share it behind an `Arc`.
pub struct XmlaClient {
    http: reqwest::Client,
    hosts: Hosts,
    endpoint: Endpoint,
    session: Mutex<Option<String>>,
    /// Response headers to echo on the next request (PaaS).
    echo: Mutex<Vec<(String, String)>>,
    negotiated: AtomicBool,
    server_version: Option<String>,
    server_name: Option<String>,
}

impl std::fmt::Debug for XmlaClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("XmlaClient").field("url", &self.endpoint.url).field("catalog", &self.endpoint.catalog).field("paas", &self.endpoint.paas).field("session", &self.session_id()).finish()
    }
}

/// No automatic redirects: api.powerbi.com's `307`s are followed by hand with the bearer kept,
/// and a `3xx` from a cluster means the instance moved.
fn http_client() -> Result<reqwest::Client, TabularError> {
    Ok(reqwest::Client::builder().user_agent(USER_AGENT).redirect(reqwest::redirect::Policy::none()).connect_timeout(Duration::from_secs(30)).build()?)
}

fn first_line(text: &str) -> String {
    text.trim().lines().next().unwrap_or("").chars().take(300).collect()
}

impl XmlaClient {
    /// Resolve the endpoint and open a session; on a classic endpoint with a
    /// `DISCOVER_PROPERTIES` under `BeginSession`, on a PaaS endpoint with the official
    /// client's `Execute` (then a `DISCOVER_PROPERTIES` for the server version, best effort).
    pub async fn connect(target: &TabularTarget, cred: Credential) -> Result<XmlaClient, TabularError> {
        Self::connect_with(target, cred, &Hosts::default()).await
    }

    /// `connect` against other hosts (tests).
    pub async fn connect_with(target: &TabularTarget, cred: Credential, hosts: &Hosts) -> Result<XmlaClient, TabularError> {
        let http = http_client()?;
        let endpoint = resolve::resolve(&http, target, &cred, hosts).await?;
        tracing::debug!(url = %endpoint.url, catalog = %endpoint.catalog, paas = endpoint.paas, cred = %cred, "xmla connect");
        let mut client = XmlaClient { http, hosts: hosts.clone(), endpoint, session: Mutex::new(None), echo: Mutex::new(Vec::new()), negotiated: AtomicBool::new(false), server_version: None, server_name: None };
        if client.endpoint.paas {
            if let Err(e) = client.create_session().await {
                resolve::forget(target);
                return Err(e);
            }
        }
        let props = match client.discover("DISCOVER_PROPERTIES", &[]).await {
            Ok(t) => t,
            Err(e) if client.endpoint.paas => {
                tracing::debug!(error = %e, "DISCOVER_PROPERTIES failed after the session opened; no server version");
                DaxTable { columns: Vec::new(), batches: Vec::new(), rows: 0 }
            }
            Err(e) => {
                resolve::forget(target);
                return Err(e);
            }
        };
        let value_of = |name: &str| (0..props.num_rows()).find(|&r| props.text("PropertyName", r).is_some_and(|n| n.eq_ignore_ascii_case(name))).and_then(|r| props.text("Value", r));
        client.server_version = value_of("DBMSVersion");
        client.server_name = value_of("ServerName");
        Ok(client)
    }

    /// `DBMSVersion` from `DISCOVER_PROPERTIES`.
    pub fn server_version(&self) -> Option<String> {
        self.server_version.clone()
    }

    /// `ServerName` from `DISCOVER_PROPERTIES`.
    pub fn server_name(&self) -> Option<String> {
        self.server_name.clone()
    }

    /// The URL the requests go to.
    pub fn url(&self) -> &str {
        &self.endpoint.url
    }

    /// The `Catalog` runs use.
    pub fn catalog(&self) -> &str {
        &self.endpoint.catalog
    }

    /// True on Power BI / Azure AS (the official client's header set and session flow).
    pub fn is_paas(&self) -> bool {
        self.endpoint.paas
    }

    /// The session id, once the server issued one.
    pub fn session_id(&self) -> Option<String> {
        self.session.lock().ok().and_then(|s| s.clone())
    }

    fn set_session(&self, id: Option<String>) {
        if let Ok(mut s) = self.session.lock() {
            *s = id;
        }
    }

    fn headers<'a>(&self, session: SessionHeader<'a>) -> Headers<'a> {
        Headers { session, paas: self.endpoint.paas }
    }

    /// PaaS: the session-creating `Execute`; the server must answer with a `Session` header.
    async fn create_session(&self) -> Result<(), TabularError> {
        let body = soap::create_session(&self.endpoint.catalog, 1033, &uuid::Uuid::new_v4().to_string(), self.headers(SessionHeader::Begin));
        let parsed = self.post(Action::Execute, body, None, Duration::from_secs(120), None).await?;
        if let Some(f) = parsed.fault {
            return Err(TabularError::Query { code: f.code, message: f.message });
        }
        match parsed.session_id {
            Some(id) => {
                self.set_session(Some(id));
                Ok(())
            }
            None => Err(TabularError::Decode("the server opened no session (no Session header in the BeginSession answer)".into())),
        }
    }

    /// One POST and its parsed answer; a lost session is retried once with a new session.
    async fn call(&self, action: Action, build: &(dyn Fn(Headers<'_>) -> String + Sync), timeout: Duration, row_cap: Option<u64>) -> Result<Parsed, TabularError> {
        for attempt in 0..2 {
            if self.endpoint.paas && self.session_id().is_none() {
                self.create_session().await?;
            }
            let session = self.session_id();
            let header = match session.as_deref() {
                Some(id) => SessionHeader::Existing(id),
                None => SessionHeader::Begin,
            };
            let body = build(self.headers(header));
            let parsed = self.post(action, body, session.as_deref(), timeout, row_cap).await?;
            if let Some(id) = parsed.session_id.as_deref() {
                self.set_session(Some(id.to_string()));
            }
            if let Some(f) = parsed.fault.as_ref() {
                if attempt == 0 && session.is_some() && f.is_session_lost() {
                    tracing::debug!(message = %f.message, "xmla session lost; starting a new one");
                    self.set_session(None);
                    continue;
                }
            }
            return Ok(parsed);
        }
        unreachable!("the retry loop returns")
    }

    async fn authorization(&self) -> Result<Option<String>, TabularError> {
        Ok(match &self.endpoint.auth {
            Auth::Credential(Credential::Bearer(t)) => Some(format!("Bearer {t}")),
            Auth::Credential(Credential::Basic { user, password }) => {
                use base64::Engine;
                Some(format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"))))
            }
            Auth::Credential(Credential::Anonymous) => None,
            Auth::Mwc { entra, workspace_id, capacity_id, model } => Some(format!("MwcToken {}", resolve::mwc_token(&self.http, &self.hosts, entra, workspace_id, capacity_id, model).await?)),
        })
    }

    async fn post(&self, action: Action, body: String, session: Option<&str>, timeout: Duration, row_cap: Option<u64>) -> Result<Parsed, TabularError> {
        let paas = self.endpoint.paas;
        let mut req = self.http.post(&self.endpoint.url).timeout(timeout);
        if paas {
            req = req
                .header("Content-Type", "text/xml")
                .header("User-Agent", resolve::AS_CLIENT_AGENT)
                .header("X-AS-AcquireTokenStats", "AppName=")
                .header("x-ms-xmlaapp-general-info", APP_GENERAL_INFO)
                .header("x-ms-xmlaclienttraits", "1")
                .header("x-ms-xmladedicatedconnection", "0")
                .header("x-ms-xmlacaps-negotiation-flags", "0,0,0,0,0")
                .header("x-ms-request-registration-id", uuid::Uuid::new_v4().to_string())
                .header("x-ms-accepts-continuations", "1")
                .header("x-ms-round-trip-id", "0")
                .header("x-ms-parent-activity-id", uuid::Uuid::new_v4().to_string());
            if let Some(id) = session {
                req = req.header("x-ms-xmlasession-id", id);
            }
            if let Ok(echo) = self.echo.lock() {
                for (k, v) in echo.iter() {
                    req = req.header(k.as_str(), v.as_str());
                }
            }
        } else {
            let flags = if self.negotiated.load(Ordering::Relaxed) { "1,0,0,0,0" } else { "0,0,0,0,0" };
            req = req
                .header("Content-Type", "text/xml; charset=utf-8")
                .header("SOAPAction", if action == Action::Discover { ACTION_DISCOVER } else { ACTION_EXECUTE })
                .header("X-Transport-Caps-Negotiation-Flags", flags)
                .header("x-ms-xmlacaps-negotiation-flags", flags)
                .header("X-AS-ActivityID", uuid::Uuid::new_v4().to_string())
                .header("X-AS-RequestID", uuid::Uuid::new_v4().to_string());
            if let Some(id) = session {
                req = req.header("X-AS-SessionID", id).header("x-ms-xmlasession-id", id);
            }
        }
        for (k, v) in &self.endpoint.headers {
            req = req.header(*k, v);
        }
        if let Some(a) = self.authorization().await? {
            req = req.header("Authorization", a);
        }
        let resp = req.body(body).send().await?;
        let status = resp.status().as_u16();
        let header = |name: &str| resp.headers().get(name).and_then(|v| v.to_str().ok()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        let content_type = header("content-type").unwrap_or_default().to_ascii_lowercase();
        let extended_error = header("x-ms-xmlaerror-extended");
        let wrong_node = status == 400 && header("x-ms-wrong-node").as_deref() == Some("1");
        if paas {
            let echoes: Vec<(String, String)> = ECHO_HEADERS.iter().filter_map(|n| header(n).map(|v| (n.to_string(), v))).collect();
            if let Ok(mut e) = self.echo.lock() {
                if status == 200 {
                    for (k, v) in echoes {
                        e.retain(|(x, _)| x != &k);
                        e.push((k, v));
                    }
                } else if !wrong_node {
                    e.clear();
                }
            }
        }
        if (300..400).contains(&status) {
            resolve::forget_clusters();
            return Err(TabularError::Api { status, code: "ServerMoved".into(), message: "the Analysis Services instance was moved to another node; reconnect".into() });
        }
        let bytes = resp.bytes().await?;
        let text = String::from_utf8_lossy(&bytes);
        let looks_xml = content_type.starts_with("text/xml") || content_type.starts_with("application/soap+xml") || content_type.starts_with("application/xml") || (content_type.is_empty() && text.trim_start().starts_with('<'));
        tracing::debug!(status, content_type = %content_type, bytes = bytes.len(), "xmla response");
        if !looks_xml {
            let message = extended_error.unwrap_or_else(|| first_line(&text));
            return Err(match status {
                401 | 403 => TabularError::Unauthorized(status, message),
                200..=299 => TabularError::Unsupported(format!("the server answered with {}; binary XML is not supported", if content_type.is_empty() { "no content type" } else { content_type.as_str() })),
                _ => match crate::rest::parse_http_error(status, &text) {
                    TabularError::Api { status, code, message: m } => TabularError::Api { status, code, message: if m.is_empty() { message } else { m } },
                    other => other,
                },
            });
        }
        self.negotiated.store(true, Ordering::Relaxed);
        let parsed = rowset::parse(&text, row_cap)?;
        if status == 401 || status == 403 {
            let detail = extended_error.or_else(|| parsed.fault.as_ref().map(|f| f.message.clone())).unwrap_or_else(|| first_line(&text));
            return Err(TabularError::Unauthorized(status, detail));
        }
        if !(200..300).contains(&status) && !wrong_node && parsed.fault.is_none() {
            return Err(TabularError::Api { status, code: String::new(), message: extended_error.unwrap_or_else(|| first_line(&text)) });
        }
        Ok(parsed)
    }

    /// A `Discover` as one table (its first rowset); a fault is `TabularError::Query`.
    pub async fn discover(&self, request_type: &str, restrictions: &[(&str, String)]) -> Result<DaxTable, TabularError> {
        let props: Vec<(&str, String)> = if self.endpoint.catalog.trim().is_empty() { Vec::new() } else { vec![("Catalog", self.endpoint.catalog.clone())] };
        let build = |h: Headers<'_>| soap::discover(request_type, restrictions, &props, h);
        let parsed = self.call(Action::Discover, &build, Duration::from_secs(120), None).await?;
        if let Some(f) = parsed.fault {
            return Err(TabularError::Query { code: f.code, message: f.message });
        }
        let results = collect_items(parsed.items);
        match results.into_iter().next() {
            Some(DaxResult::Table(t)) => Ok(t),
            Some(DaxResult::Error { message, code }) => Err(TabularError::Query { code, message }),
            None => Ok(DaxTable { columns: Vec::new(), batches: Vec::new(), rows: 0 }),
        }
    }

    /// Run DAX, MDX or a DMV query against the client's catalog.
    pub async fn execute(&self, text: &str, opts: &DaxOptions) -> Result<DaxStream, TabularError> {
        self.execute_in(&self.endpoint.catalog, text, opts).await
    }

    /// Run against another catalog on the same server.
    pub async fn execute_in(&self, catalog: &str, text: &str, opts: &DaxOptions) -> Result<DaxStream, TabularError> {
        let props = ExecuteProps {
            catalog: catalog.to_string(),
            timeout_secs: opts.timeout_secs.unwrap_or(0),
            activity_id: uuid::Uuid::new_v4().to_string(),
            request_id: uuid::Uuid::new_v4().to_string(),
            locale: locale_id(opts.culture.as_deref()),
            effective_user_name: opts.effective_username.clone(),
            roles: opts.roles.clone(),
            memory_limit_kb: None,
        };
        let row_cap = if opts.schema_only { Some(0) } else { opts.row_limit };
        let build = |h: Headers<'_>| soap::execute(text, &props, h);
        let parsed = self.call(Action::Execute, &build, request_timeout(opts.timeout_secs), row_cap).await?;
        let items = match parsed.fault {
            Some(f) => vec![DaxItem::Error { message: f.message, code: f.code }],
            None => parsed.items,
        };
        Ok(DaxStream::from_items(items))
    }

    /// Stop what the session is running (a separate request, so it can go while a query is in
    /// flight). Nothing to cancel without a session.
    pub async fn cancel(&self) -> Result<(), TabularError> {
        let Some(id) = self.session_id() else { return Ok(()) };
        let body = soap::cancel(&id, self.endpoint.paas);
        let parsed = self.post(Action::Execute, body, Some(&id), Duration::from_secs(60), None).await?;
        if let Some(f) = parsed.fault {
            tracing::debug!(message = %f.message, "xmla cancel answered with a fault");
        }
        Ok(())
    }

    /// `DBSCHEMA_CATALOGS`: the models on the server (on Power BI, the workspace's models by
    /// their friendly names).
    pub async fn list_catalogs(&self) -> Result<Vec<String>, TabularError> {
        let t = self.discover("DBSCHEMA_CATALOGS", &[]).await?;
        Ok((0..t.num_rows()).filter_map(|r| t.text("CATALOG_NAME", r)).collect())
    }

    async fn collect_in(&self, catalog: &str, text: &str) -> Result<Vec<DaxResult>, TabularError> {
        let opts = DaxOptions { timeout_secs: Some(120), ..DaxOptions::default() };
        self.execute_in(catalog, text, &opts).await?.collect().await
    }

    /// The model tree: `INFO.VIEW` through `Execute` when the server has it, else the TMSCHEMA
    /// DMVs joined on ids.
    pub async fn load_catalog(&self, model: &str) -> Result<ModelCatalog, TabularError> {
        let clean = |r: Result<Vec<DaxResult>, TabularError>| match r {
            Ok(r) if !r.iter().any(|x| x.error().is_some()) => Ok(r),
            Ok(r) => Err(r.iter().find_map(|x| x.error()).unwrap_or_else(|| TabularError::Decode("no result".into()))),
            Err(e) => Err(e),
        };
        let info = match clean(self.collect_in(model, &catalog_query(false)).await) {
            Ok(r) => Some(r),
            Err(_) => match clean(self.collect_in(model, &catalog_query(true)).await) {
                Ok(r) => Some(r),
                Err(e) => {
                    tracing::debug!(error = %e, "INFO.VIEW refused; using the TMSCHEMA DMVs");
                    None
                }
            },
        };
        if let Some(results) = info {
            let mut catalog = catalog_from_results(&results, model)?;
            if let Ok(h) = self.collect_in(model, HIERARCHY_QUERY).await {
                attach_hierarchies(&mut catalog, &h);
            }
            return Ok(catalog);
        }
        let mut sections: Vec<Option<DaxTable>> = Vec::with_capacity(tmschema::DMV_QUERIES.len());
        for (i, q) in tmschema::DMV_QUERIES.iter().enumerate() {
            let table = match self.collect_in(model, q).await {
                Ok(r) => match r.into_iter().next() {
                    Some(DaxResult::Table(t)) => Some(t),
                    Some(DaxResult::Error { message, code }) if i == 0 => return Err(TabularError::Query { code, message }),
                    _ => None,
                },
                Err(e) if i == 0 => return Err(e),
                Err(_) => None,
            };
            sections.push(table);
        }
        let tables = sections[0].take().ok_or_else(|| TabularError::Decode("TMSCHEMA_TABLES returned nothing".into()))?;
        Ok(tmschema::catalog_from_dmvs(&tables, sections[1].as_ref(), sections[2].as_ref(), sections[3].as_ref(), sections[4].as_ref(), sections[5].as_ref(), model))
    }

    /// The functions the engine knows: `MDSCHEMA_FUNCTIONS`, else `INFO.FUNCTIONS()`.
    pub async fn load_functions(&self) -> Result<Vec<DaxFunction>, TabularError> {
        if let Ok(t) = self.discover("MDSCHEMA_FUNCTIONS", &[]).await {
            let f = functions_from_table(&t);
            if !f.is_empty() {
                return Ok(f);
            }
        }
        let opts = DaxOptions { timeout_secs: Some(120), ..DaxOptions::default() };
        let results = match self.execute(FUNCTIONS_QUERY, &opts).await?.collect().await {
            Ok(r) if r.first().and_then(DaxResult::table).is_some() => r,
            _ => self.execute("EVALUATE INFO.FUNCTIONS()", &opts).await?.collect().await?,
        };
        let first = results.into_iter().next().ok_or_else(|| TabularError::Decode("INFO.FUNCTIONS returned nothing".into()))?;
        match first {
            DaxResult::Table(t) => Ok(functions_from_table(&t)),
            DaxResult::Error { message, code } => Err(TabularError::Query { code, message }),
        }
    }

    /// `EndSession`; errors are ignored (the server drops idle sessions anyway).
    pub async fn end_session(&self) {
        let Some(id) = self.session_id() else { return };
        self.set_session(None);
        let body = soap::end_session(&id, self.endpoint.paas);
        if let Err(e) = self.post(Action::Discover, body, Some(&id), Duration::from_secs(30), None).await {
            tracing::debug!(error = %e, "xmla EndSession failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mdx_detection() {
        assert!(is_mdx_or_dmv("SELECT [Measures].[Sales] ON COLUMNS FROM [Model]"));
        assert!(is_mdx_or_dmv("select {[Measures].[Sales]} on 0, [Date].[Year].Members on 1 from [Model]"));
        assert!(is_mdx_or_dmv("WITH MEMBER [Measures].[x] AS 1 SELECT [Measures].[x] ON COLUMNS FROM [Model]"));
        assert!(is_mdx_or_dmv("SELECT * FROM $SYSTEM.DISCOVER_SESSIONS"));
        assert!(is_mdx_or_dmv("-- sessions\nselect SESSION_ID from $system.discover_sessions"));
        assert!(is_mdx_or_dmv("/* dmv */ SELECT * FROM $SYSTEM.TMSCHEMA_TABLES"));
        assert!(!is_mdx_or_dmv("EVALUATE Sales"));
        assert!(!is_mdx_or_dmv("// comment\nDEFINE VAR x = 1\nEVALUATE {x}"));
        assert!(!is_mdx_or_dmv("SELECT 1"), "a bare SELECT is neither");
        assert!(!is_mdx_or_dmv(""));
    }

    #[test]
    fn client_and_its_futures_are_send() {
        fn assert_send_sync<T: Send + Sync>() {}
        fn assert_send<F: std::future::Future + Send>(_: F) {}
        assert_send_sync::<XmlaClient>();
        assert_send_sync::<crate::Transport>();
        let t = TabularTarget::Http { url: "http://127.0.0.1:9/x".into(), model: "m".into() };
        assert_send(XmlaClient::connect(&t, Credential::Anonymous));
        assert_send(crate::Transport::connect(&t, Credential::Anonymous, false));
    }

    #[test]
    fn locales_and_timeouts() {
        assert_eq!(locale_id(None), 1033);
        assert_eq!(locale_id(Some("en-US")), 1033);
        assert_eq!(locale_id(Some("de-DE")), 1031);
        assert_eq!(locale_id(Some("xx")), 1033);
        assert_eq!(request_timeout(None), Duration::from_secs(86_400));
        assert_eq!(request_timeout(Some(0)), Duration::from_secs(86_400));
        assert_eq!(request_timeout(Some(30)), Duration::from_secs(90));
    }
}
