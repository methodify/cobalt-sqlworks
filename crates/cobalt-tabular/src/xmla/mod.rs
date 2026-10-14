//! XMLA over HTTPS: plain SOAP 1.1 `Discover` / `Execute` against Azure Analysis Services,
//! `msmdpump.dll` (or any XMLA URL) and, once its routing is captured, Power BI. One session
//! per client (`BeginSession` on the first call, `Session` after, `EndSession` at the end), a
//! real `<Cancel/>`, text XML only (the transport capability flags never advertise binary XML
//! or XPRESS), DAX / MDX / DMV text alike. Results come back as the same `DaxItem` sequence the
//! Arrow query API yields. `soap` builds envelopes, `rowset` parses answers, `resolve` finds the
//! URL per target, `tmschema` joins the DMVs into a `ModelCatalog`.

pub mod resolve;
pub mod rowset;
pub mod soap;
pub mod tmschema;

use crate::catalog::{attach_hierarchies, catalog_from_results, catalog_query, functions_from_table, DaxFunction, FUNCTIONS_QUERY, HIERARCHY_QUERY};
use crate::decode::{collect_items, DaxItem, DaxResult, DaxTable};
use crate::rest::{DaxOptions, DaxStream};
use crate::TabularError;
use cobalt_core::tabular::{ModelCatalog, TabularTarget};
use resolve::Endpoint;
use rowset::Parsed;
use soap::{ExecuteProps, SessionHeader, ACTION_DISCOVER, ACTION_EXECUTE};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

/// The Entra resource a bearer token for Azure Analysis Services is minted for.
pub const AZURE_AS_RESOURCE: &str = "https://*.asazure.windows.net";

/// The `User-Agent`.
pub const USER_AGENT: &str = concat!("cobalt-sqlworks/", env!("CARGO_PKG_VERSION"));

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
    endpoint: Endpoint,
    cred: Credential,
    session: Mutex<Option<String>>,
    negotiated: AtomicBool,
    server_version: Option<String>,
    server_name: Option<String>,
}

impl std::fmt::Debug for XmlaClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("XmlaClient").field("url", &self.endpoint.url).field("catalog", &self.endpoint.catalog).field("session", &self.session_id()).finish()
    }
}

fn http_client() -> Result<reqwest::Client, TabularError> {
    Ok(reqwest::Client::builder().user_agent(USER_AGENT).connect_timeout(Duration::from_secs(30)).build()?)
}

fn first_line(text: &str) -> String {
    text.trim().lines().next().unwrap_or("").chars().take(300).collect()
}

impl XmlaClient {
    /// Resolve the endpoint and open a session with a `DISCOVER_PROPERTIES` (which also proves
    /// the credential and reads the server version).
    pub async fn connect(target: &TabularTarget, cred: Credential) -> Result<XmlaClient, TabularError> {
        let http = http_client()?;
        let endpoint = resolve::resolve(&http, target, &cred).await?;
        tracing::debug!(url = %endpoint.url, catalog = %endpoint.catalog, cred = %cred, "xmla connect");
        let mut client = XmlaClient { http, endpoint, cred, session: Mutex::new(None), negotiated: AtomicBool::new(false), server_version: None, server_name: None };
        let props = match client.discover("DISCOVER_PROPERTIES", &[]).await {
            Ok(t) => t,
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

    /// The session id, once the server issued one.
    pub fn session_id(&self) -> Option<String> {
        self.session.lock().ok().and_then(|s| s.clone())
    }

    fn set_session(&self, id: Option<String>) {
        if let Ok(mut s) = self.session.lock() {
            *s = id;
        }
    }

    /// One POST and its parsed answer; a lost session is retried once with a new `BeginSession`.
    async fn call(&self, action: Action, build: &(dyn Fn(SessionHeader<'_>) -> String + Sync), timeout: Duration, row_cap: Option<u64>) -> Result<Parsed, TabularError> {
        for attempt in 0..2 {
            let session = self.session_id();
            let header = match session.as_deref() {
                Some(id) => SessionHeader::Existing(id),
                None => SessionHeader::Begin,
            };
            let body = build(header);
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

    async fn post(&self, action: Action, body: String, session: Option<&str>, timeout: Duration, row_cap: Option<u64>) -> Result<Parsed, TabularError> {
        let flags = if self.negotiated.load(Ordering::Relaxed) { "1,0,0,0,0" } else { "0,0,0,0,0" };
        let mut req = self
            .http
            .post(&self.endpoint.url)
            .header("Content-Type", "text/xml; charset=utf-8")
            .header("SOAPAction", if action == Action::Discover { ACTION_DISCOVER } else { ACTION_EXECUTE })
            .header("X-Transport-Caps-Negotiation-Flags", flags)
            .header("x-ms-xmlacaps-negotiation-flags", flags)
            .header("X-AS-ActivityID", uuid::Uuid::new_v4().to_string())
            .header("X-AS-RequestID", uuid::Uuid::new_v4().to_string())
            .timeout(timeout);
        if let Some(id) = session {
            req = req.header("X-AS-SessionID", id).header("x-ms-xmlasession-id", id);
        }
        for (k, v) in &self.endpoint.headers {
            req = req.header(*k, v);
        }
        req = match &self.cred {
            Credential::Bearer(t) => req.bearer_auth(t),
            Credential::Basic { user, password } => req.basic_auth(user, Some(password)),
            Credential::Anonymous => req,
        };
        let resp = req.body(body).send().await?;
        let status = resp.status().as_u16();
        let content_type = resp.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("").trim().to_ascii_lowercase();
        let bytes = resp.bytes().await?;
        let text = String::from_utf8_lossy(&bytes);
        let looks_xml = content_type.starts_with("text/xml") || content_type.starts_with("application/soap+xml") || content_type.starts_with("application/xml") || (content_type.is_empty() && text.trim_start().starts_with('<'));
        tracing::debug!(status, content_type = %content_type, bytes = bytes.len(), "xmla response");
        if !looks_xml {
            return Err(match status {
                401 | 403 => TabularError::Unauthorized(status, first_line(&text)),
                200..=299 if content_type.starts_with("application/json") => {
                    // the Power BI dedicated cluster's front door answers JSON on routing failures
                    let msg = serde_json::from_str::<serde_json::Value>(&text).ok().and_then(|v| v.get("message").and_then(|m| m.as_str()).map(str::to_string)).unwrap_or_else(|| first_line(&text));
                    TabularError::Unsupported(format!("the XMLA front door answered: {}. The Power BI XMLA routing is not confirmed yet (the capacity, cluster and database are resolved; the remaining header needs a protocol capture, see docs/design/semantic_models.md §7). The Arrow query API path is unaffected.", msg.trim_end_matches('.')))
                }
                200..=299 => TabularError::Unsupported(format!("the server answered with {}; binary XML is not supported", if content_type.is_empty() { "no content type" } else { content_type.as_str() })),
                _ => TabularError::Api { status, code: String::new(), message: first_line(&text) },
            });
        }
        self.negotiated.store(true, Ordering::Relaxed);
        let parsed = rowset::parse(&text, row_cap)?;
        if status == 401 || status == 403 {
            let detail = parsed.fault.as_ref().map(|f| f.message.clone()).unwrap_or_else(|| first_line(&text));
            return Err(TabularError::Unauthorized(status, detail));
        }
        if !(200..300).contains(&status) && parsed.fault.is_none() {
            return Err(TabularError::Api { status, code: String::new(), message: first_line(&text) });
        }
        Ok(parsed)
    }

    /// A `Discover` as one table (its first rowset); a fault is `TabularError::Query`.
    pub async fn discover(&self, request_type: &str, restrictions: &[(&str, String)]) -> Result<DaxTable, TabularError> {
        let props: Vec<(&str, String)> = if self.endpoint.catalog.trim().is_empty() { Vec::new() } else { vec![("Catalog", self.endpoint.catalog.clone())] };
        let build = |h: SessionHeader<'_>| soap::discover(request_type, restrictions, &props, h);
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
        let build = |h: SessionHeader<'_>| soap::execute(text, &props, h);
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
        let body = soap::cancel(&id);
        let parsed = self.post(Action::Execute, body, Some(&id), Duration::from_secs(60), None).await?;
        if let Some(f) = parsed.fault {
            tracing::debug!(message = %f.message, "xmla cancel answered with a fault");
        }
        Ok(())
    }

    /// `DBSCHEMA_CATALOGS`: the models on the server.
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
        let body = soap::end_session(&id);
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
