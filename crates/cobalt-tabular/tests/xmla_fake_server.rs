//! The XMLA client against a scripted HTTP server: capability flags and `BeginSession` on the
//! first request, `Session` on the next, one retry with a new session when the server lost
//! it, `<Cancel/>` with the session id, a refusal of non-XML answers, and the Power BI path
//! (workspace record, `generateastoken`, `clusterResolve`, `MwcToken`, the `Execute` that opens
//! the session, routing-hint echo, a `3xx` as "moved").

use cobalt_core::tabular::TabularTarget;
use cobalt_tabular::xmla::Hosts;
use cobalt_tabular::{Credential, DaxOptions, TabularError, Transport, XmlaClient};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

type Reply = (u16, &'static str, String, Vec<(&'static str, String)>);

#[derive(Clone, Debug)]
struct Recorded {
    method: String,
    url: String,
    headers: HashMap<String, String>,
    body: String,
}

struct FakeServer {
    url: String,
    base: String,
    replies: Arc<Mutex<VecDeque<Reply>>>,
    requests: Arc<Mutex<Vec<Recorded>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl FakeServer {
    fn start() -> Self {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let port = match server.server_addr() {
            tiny_http::ListenAddr::IP(a) => a.port(),
            #[allow(unreachable_patterns)]
            _ => panic!("no tcp address"),
        };
        let replies: Arc<Mutex<VecDeque<Reply>>> = Arc::new(Mutex::new(VecDeque::new()));
        let requests: Arc<Mutex<Vec<Recorded>>> = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (r2, q2, s2) = (replies.clone(), requests.clone(), stop.clone());
        let thread = std::thread::spawn(move || {
            while !s2.load(Ordering::Relaxed) {
                let mut req = match server.recv_timeout(Duration::from_millis(50)) {
                    Ok(Some(r)) => r,
                    Ok(None) => continue,
                    Err(_) => break,
                };
                let headers: HashMap<String, String> = req.headers().iter().map(|h| (h.field.as_str().to_string().to_ascii_lowercase(), h.value.as_str().to_string())).collect();
                let mut body = String::new();
                let _ = std::io::Read::read_to_string(req.as_reader(), &mut body);
                q2.lock().unwrap().push(Recorded { method: req.method().as_str().to_string(), url: req.url().to_string(), headers, body });
                let (code, ct, text, extra) = r2.lock().unwrap().pop_front().unwrap_or((500, "text/plain", "no scripted reply".into(), Vec::new()));
                let mut resp = tiny_http::Response::from_string(text).with_status_code(code).with_header(tiny_http::Header::from_bytes("Content-Type", ct).unwrap());
                for (k, v) in extra {
                    resp = resp.with_header(tiny_http::Header::from_bytes(k, v.as_str()).unwrap());
                }
                let _ = req.respond(resp);
            }
        });
        FakeServer { url: format!("http://127.0.0.1:{port}/olap/msmdpump.dll"), base: format!("http://127.0.0.1:{port}"), replies, requests, stop, thread: Some(thread) }
    }

    fn reply(&self, code: u16, ct: &'static str, body: impl Into<String>) {
        self.replies.lock().unwrap().push_back((code, ct, body.into(), Vec::new()));
    }

    fn reply_with(&self, code: u16, ct: &'static str, body: impl Into<String>, headers: Vec<(&'static str, String)>) {
        self.replies.lock().unwrap().push_back((code, ct, body.into(), headers));
    }

    /// The resolver's cluster and token caches are process-wide and keyed by workspace /
    /// capacity / model, so every test uses ids of its own.
    fn hosts(&self) -> Hosts {
        Hosts { api_base: self.base.clone(), cluster_scheme: "http".into() }
    }

    fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

const ENV: &str = "<?xml version=\"1.0\"?><soap:Envelope xmlns:soap=\"http://schemas.xmlsoap.org/soap/envelope/\">";

fn rowset(elements: &str, rows: &str) -> String {
    format!(
        "<root xmlns=\"urn:schemas-microsoft-com:xml-analysis:rowset\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xmlns:xsd=\"http://www.w3.org/2001/XMLSchema\"><xsd:schema targetNamespace=\"urn:schemas-microsoft-com:xml-analysis:rowset\" xmlns:sql=\"urn:schemas-microsoft-com:xml-sql\" elementFormDefault=\"qualified\"><xsd:element name=\"root\"><xsd:complexType><xsd:sequence minOccurs=\"0\" maxOccurs=\"unbounded\"><xsd:element name=\"row\" type=\"row\"/></xsd:sequence></xsd:complexType></xsd:element><xsd:complexType name=\"row\"><xsd:sequence>{elements}</xsd:sequence></xsd:complexType></xsd:schema>{rows}</root>"
    )
}

fn session_header(id: &str) -> String {
    format!("<soap:Header><Session xmlns=\"urn:schemas-microsoft-com:xml-analysis\" SessionId=\"{id}\"/></soap:Header>")
}

fn discover_properties(session: &str) -> String {
    let root = rowset(
        "<xsd:element sql:field=\"PropertyName\" name=\"PropertyName\" type=\"xsd:string\"/><xsd:element sql:field=\"Value\" name=\"Value\" type=\"xsd:string\" minOccurs=\"0\"/>",
        "<row><PropertyName>DBMSVersion</PropertyName><Value>16.0.123.4</Value></row><row><PropertyName>ServerName</PropertyName><Value>FAKE\\AS</Value></row>",
    );
    format!("{ENV}{}<soap:Body><DiscoverResponse xmlns=\"urn:schemas-microsoft-com:xml-analysis\"><return>{root}</return></DiscoverResponse></soap:Body></soap:Envelope>", session_header(session))
}

fn execute_rows(session: Option<&str>, rows: &[(&str, i64)]) -> String {
    let body: String = rows.iter().map(|(n, v)| format!("<row><Name>{n}</Name><_x005B_Value_x005D_>{v}</_x005B_Value_x005D_></row>")).collect();
    let root = rowset("<xsd:element name=\"Name\" type=\"xsd:string\" minOccurs=\"0\"/><xsd:element sql:field=\"[Value]\" name=\"_x005B_Value_x005D_\" type=\"xsd:long\" minOccurs=\"0\"/>", &body);
    format!("{ENV}{}<soap:Body><ExecuteResponse xmlns=\"urn:schemas-microsoft-com:xml-analysis\"><return>{root}</return></ExecuteResponse></soap:Body></soap:Envelope>", session.map(session_header).unwrap_or_default())
}

fn fault(text: &str) -> String {
    format!("{ENV}<soap:Body><soap:Fault><faultcode>XMLAnalysisError.0xc10a0001</faultcode><faultstring>{text}</faultstring><detail><Error ErrorCode=\"3238789121\" Description=\"{text}\" Source=\"Microsoft Analysis Services\" HelpFile=\"\"/></detail></soap:Fault></soap:Body></soap:Envelope>")
}

const EMPTY: &str = "<root xmlns=\"urn:schemas-microsoft-com:xml-analysis:empty\"/>";

fn empty_response() -> String {
    format!("{ENV}<soap:Body><ExecuteResponse xmlns=\"urn:schemas-microsoft-com:xml-analysis\"><return>{EMPTY}</return></ExecuteResponse></soap:Body></soap:Envelope>")
}

#[tokio::test]
async fn session_flow() {
    let server = FakeServer::start();
    let target = TabularTarget::Http { url: server.url.clone(), model: "Adventure Works".into() };

    // 1. connect: DISCOVER_PROPERTIES with BeginSession; the server issues S1
    server.reply(200, "text/xml; charset=utf-8", discover_properties("S1"));
    let client = XmlaClient::connect(&target, Credential::Basic { user: "bob".into(), password: "pw".into() }).await.unwrap();
    assert_eq!(client.server_version().as_deref(), Some("16.0.123.4"));
    assert_eq!(client.server_name().as_deref(), Some("FAKE\\AS"));
    assert_eq!(client.session_id().as_deref(), Some("S1"));
    let reqs = server.requests();
    assert_eq!(reqs.len(), 1);
    let first = &reqs[0];
    assert_eq!(first.headers.get("x-transport-caps-negotiation-flags").map(String::as_str), Some("0,0,0,0,0"));
    assert_eq!(first.headers.get("x-ms-xmlacaps-negotiation-flags").map(String::as_str), Some("0,0,0,0,0"));
    assert_eq!(first.headers.get("soapaction").map(String::as_str), Some("\"urn:schemas-microsoft-com:xml-analysis:Discover\""));
    assert!(first.headers.get("content-type").unwrap().starts_with("text/xml"));
    assert!(first.headers.get("user-agent").unwrap().starts_with("cobalt-sqlworks/"));
    assert!(first.headers.get("authorization").unwrap().starts_with("Basic "));
    assert!(first.headers.contains_key("x-as-activityid") && first.headers.contains_key("x-as-requestid"));
    assert!(!first.headers.contains_key("x-as-sessionid"));
    assert!(first.body.contains("<BeginSession xmlns=\"urn:schemas-microsoft-com:xml-analysis\" soap:mustUnderstand=\"1\"/>"));
    assert!(first.body.contains("<RequestType>DISCOVER_PROPERTIES</RequestType>"));
    assert!(first.body.contains("<Catalog>Adventure Works</Catalog>"));

    // 2. execute: Session header, flags 1,0,0,0,0, the session id headers
    server.reply(200, "text/xml", execute_rows(None, &[("a", 1), ("b", 2)]));
    let opts = DaxOptions { timeout_secs: Some(30), effective_username: Some("alice@x.com".into()), ..DaxOptions::default() };
    let results = client.execute("EVALUATE T", &opts).await.unwrap().collect().await.unwrap();
    assert_eq!(results.len(), 1);
    let t = results[0].table().unwrap();
    assert_eq!(t.rows, 2);
    assert_eq!(t.text("Name", 1).as_deref(), Some("b"));
    assert_eq!(t.int("[Value]", 1), Some(2));
    let reqs = server.requests();
    let second = &reqs[1];
    assert_eq!(second.headers.get("x-transport-caps-negotiation-flags").map(String::as_str), Some("1,0,0,0,0"));
    assert_eq!(second.headers.get("soapaction").map(String::as_str), Some("\"urn:schemas-microsoft-com:xml-analysis:Execute\""));
    assert_eq!(second.headers.get("x-as-sessionid").map(String::as_str), Some("S1"));
    assert_eq!(second.headers.get("x-ms-xmlasession-id").map(String::as_str), Some("S1"));
    assert!(second.body.contains("<Session xmlns=\"urn:schemas-microsoft-com:xml-analysis\" soap:mustUnderstand=\"1\" SessionId=\"S1\"/>"));
    assert!(second.body.contains("<Statement>EVALUATE T</Statement>"));
    assert!(second.body.contains("<Timeout>30</Timeout>"));
    assert!(second.body.contains("<EffectiveUserName>alice@x.com</EffectiveUserName>"));
    assert!(second.body.contains("<Catalog>Adventure Works</Catalog>"));

    // 3. the server lost the session: one retry with BeginSession, new id S2
    server.reply(500, "text/xml", fault("The session with ID 'S1' does not exist or has expired."));
    server.reply(200, "text/xml", execute_rows(Some("S2"), &[("c", 3)]));
    let results = client.execute("EVALUATE U", &DaxOptions::default()).await.unwrap().collect().await.unwrap();
    assert_eq!(results[0].table().unwrap().rows, 1);
    let reqs = server.requests();
    assert_eq!(reqs.len(), 4);
    assert!(reqs[2].body.contains("SessionId=\"S1\""));
    assert!(reqs[3].body.contains("<BeginSession "));
    assert!(!reqs[3].headers.contains_key("x-as-sessionid"));
    assert_eq!(client.session_id().as_deref(), Some("S2"));

    // 4. a query error is an error item, not an Err
    server.reply(500, "text/xml", fault("Query (1, 1) The syntax for 'EVALUAT' is incorrect."));
    let results = client.execute("EVALUAT", &DaxOptions::default()).await.unwrap().collect().await.unwrap();
    let e = results[0].error().unwrap();
    assert_eq!(e.to_string(), "Query (1, 1) The syntax for 'EVALUAT' is incorrect. (3238789121)");
    assert_eq!(client.session_id().as_deref(), Some("S2"), "a query fault keeps the session");

    // 5. cancel posts the Cancel command under the session
    server.reply(200, "text/xml", empty_response());
    client.cancel().await.unwrap();
    let reqs = server.requests();
    let cancel = reqs.last().unwrap();
    assert!(cancel.body.contains("<Command><Cancel/></Command>"));
    assert!(cancel.body.contains("SessionId=\"S2\""));
    assert_eq!(cancel.headers.get("x-as-sessionid").map(String::as_str), Some("S2"));
    assert_eq!(cancel.headers.get("soapaction").map(String::as_str), Some("\"urn:schemas-microsoft-com:xml-analysis:Execute\""));

    // 6. list catalogs and the functions through Discover
    let catalogs = rowset("<xsd:element sql:field=\"CATALOG_NAME\" name=\"CATALOG_NAME\" type=\"xsd:string\"/>", "<row><CATALOG_NAME>Adventure Works</CATALOG_NAME></row><row><CATALOG_NAME>HR</CATALOG_NAME></row>");
    server.reply(200, "text/xml", format!("{ENV}<soap:Body><DiscoverResponse xmlns=\"urn:schemas-microsoft-com:xml-analysis\"><return>{catalogs}</return></DiscoverResponse></soap:Body></soap:Envelope>"));
    assert_eq!(client.list_catalogs().await.unwrap(), vec!["Adventure Works".to_string(), "HR".to_string()]);
    let funcs = rowset(
        "<xsd:element sql:field=\"FUNCTION_NAME\" name=\"FUNCTION_NAME\" type=\"xsd:string\"/><xsd:element sql:field=\"DESCRIPTION\" name=\"DESCRIPTION\" type=\"xsd:string\" minOccurs=\"0\"/><xsd:element sql:field=\"PARAMETER_LIST\" name=\"PARAMETER_LIST\" type=\"xsd:string\" minOccurs=\"0\"/><xsd:element sql:field=\"INTERFACE_NAME\" name=\"INTERFACE_NAME\" type=\"xsd:string\" minOccurs=\"0\"/>",
        "<row><FUNCTION_NAME>SUMX</FUNCTION_NAME><DESCRIPTION>Sums</DESCRIPTION><PARAMETER_LIST>(Table, Expression)</PARAMETER_LIST><INTERFACE_NAME>MATH</INTERFACE_NAME></row>",
    );
    server.reply(200, "text/xml", format!("{ENV}<soap:Body><DiscoverResponse xmlns=\"urn:schemas-microsoft-com:xml-analysis\"><return>{funcs}</return></DiscoverResponse></soap:Body></soap:Envelope>"));
    let f = client.load_functions().await.unwrap();
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].name, "SUMX");
    assert_eq!(f[0].category.as_deref(), Some("MATH"));
    let last = server.requests().pop().unwrap();
    assert!(last.body.contains("<RequestType>MDSCHEMA_FUNCTIONS</RequestType>"));

    // 7. end the session
    server.reply(200, "text/xml", empty_response());
    client.end_session().await;
    let last = server.requests().pop().unwrap();
    assert!(last.body.contains("<EndSession xmlns=\"urn:schemas-microsoft-com:xml-analysis\" soap:mustUnderstand=\"1\" SessionId=\"S2\"/>"));
    assert_eq!(client.session_id(), None);
}

#[tokio::test]
async fn refusals() {
    let server = FakeServer::start();
    let target = TabularTarget::Http { url: server.url.clone(), model: "m".into() };
    // binary XML (or anything that is not text XML) is refused
    server.reply(200, "application/octet-stream", "\u{1}\u{2}binary");
    let e = XmlaClient::connect(&target, Credential::Anonymous).await.unwrap_err();
    assert!(matches!(&e, TabularError::Unsupported(m) if m == "the server answered with application/octet-stream; binary XML is not supported"), "{e}");
    // 401 without XML
    server.reply(401, "text/html", "<html>Unauthorized</html>");
    let e = XmlaClient::connect(&target, Credential::Anonymous).await.unwrap_err();
    assert!(matches!(e, TabularError::Unauthorized(401, _)));
    assert!(e.is_auth());
    // 403 with a fault: the fault text is the detail
    server.reply(403, "text/xml", fault("Either the user does not have access to the database, or the database does not exist."));
    let e = XmlaClient::connect(&target, Credential::Bearer("t".into())).await.unwrap_err();
    assert!(matches!(&e, TabularError::Unauthorized(403, m) if m.starts_with("Either the user")), "{e}");
    let last = server.requests().pop().unwrap();
    assert_eq!(last.headers.get("authorization").map(String::as_str), Some("Bearer t"));
    // a 500 with no fault body
    server.reply(502, "text/plain", "Bad gateway\nmore");
    let e = XmlaClient::connect(&target, Credential::Anonymous).await.unwrap_err();
    assert!(matches!(&e, TabularError::Api { status: 502, message, .. } if message == "Bad gateway"), "{e}");
}

#[tokio::test]
async fn transport_facade_over_xmla() {
    let server = FakeServer::start();
    let target = TabularTarget::Http { url: server.url.clone(), model: "m".into() };
    server.reply(200, "text/xml", discover_properties("S9"));
    let t = Transport::connect(&target, Credential::Anonymous, false).await.unwrap();
    assert_eq!(t.name(), "XMLA");
    assert_eq!(t.server_version().as_deref(), Some("16.0.123.4"));
    server.reply(200, "text/xml", execute_rows(None, &[("x", 1)]));
    let r = t.execute(&target, "SELECT * FROM $SYSTEM.DISCOVER_SESSIONS", &DaxOptions::default()).await.unwrap().collect().await.unwrap();
    assert_eq!(r[0].table().unwrap().rows, 1);
    let last = server.requests().pop().unwrap();
    assert!(last.body.contains("<Statement>SELECT * FROM $SYSTEM.DISCOVER_SESSIONS</Statement>"));
    // the row cap keeps the first n rows
    server.reply(200, "text/xml", execute_rows(None, &[("a", 1), ("b", 2), ("c", 3)]));
    let r = t.execute(&target, "EVALUATE T", &DaxOptions { row_limit: Some(2), ..DaxOptions::default() }).await.unwrap().collect().await.unwrap();
    assert_eq!(r[0].table().unwrap().rows, 2);
    // the Power BI resolution is refused without a reachable api.powerbi.com answer; here a
    // target with no id or name is refused before any call
    server.reply(200, "text/xml", empty_response());
    t.close().await;
}

#[tokio::test]
async fn catalog_falls_back_to_the_dmvs() {
    let server = FakeServer::start();
    let target = TabularTarget::Http { url: server.url.clone(), model: "m".into() };
    server.reply(200, "text/xml", discover_properties("S1"));
    let client = XmlaClient::connect(&target, Credential::Anonymous).await.unwrap();
    // INFO.VIEW (full and minimal) both fault, so the six DMVs are asked
    server.reply(500, "text/xml", fault("The INFO.VIEW.TABLES function does not exist."));
    server.reply(500, "text/xml", fault("The INFO.VIEW.TABLES function does not exist."));
    let dmv = |elements: &str, rows: &str| format!("{ENV}<soap:Body><ExecuteResponse xmlns=\"urn:schemas-microsoft-com:xml-analysis\"><return>{}</return></ExecuteResponse></soap:Body></soap:Envelope>", rowset(elements, rows));
    server.reply(200, "text/xml", dmv("<xsd:element name=\"ID\" type=\"xsd:long\"/><xsd:element name=\"Name\" type=\"xsd:string\"/><xsd:element name=\"IsHidden\" type=\"xsd:boolean\"/>", "<row><ID>1</ID><Name>Sales</Name><IsHidden>false</IsHidden></row><row><ID>2</ID><Name>Date</Name><IsHidden>false</IsHidden></row>"));
    server.reply(200, "text/xml", dmv("<xsd:element name=\"ID\" type=\"xsd:long\"/><xsd:element name=\"TableID\" type=\"xsd:long\"/><xsd:element name=\"ExplicitName\" type=\"xsd:string\" minOccurs=\"0\"/><xsd:element name=\"ExplicitDataType\" type=\"xsd:long\"/><xsd:element name=\"Type\" type=\"xsd:long\"/><xsd:element name=\"IsHidden\" type=\"xsd:boolean\"/>", "<row><ID>10</ID><TableID>1</TableID><ExplicitName>Amount</ExplicitName><ExplicitDataType>10</ExplicitDataType><Type>1</Type><IsHidden>false</IsHidden></row><row><ID>11</ID><TableID>1</TableID><ExplicitName>RowNumber-1</ExplicitName><ExplicitDataType>6</ExplicitDataType><Type>3</Type><IsHidden>true</IsHidden></row><row><ID>20</ID><TableID>2</TableID><ExplicitName>Date</ExplicitName><ExplicitDataType>9</ExplicitDataType><Type>1</Type><IsHidden>false</IsHidden></row>"));
    server.reply(200, "text/xml", dmv("<xsd:element name=\"ID\" type=\"xsd:long\"/><xsd:element name=\"TableID\" type=\"xsd:long\"/><xsd:element name=\"Name\" type=\"xsd:string\"/><xsd:element name=\"Expression\" type=\"xsd:string\" minOccurs=\"0\"/><xsd:element name=\"DataType\" type=\"xsd:long\"/>", "<row><ID>100</ID><TableID>1</TableID><Name>Total</Name><Expression>SUM(Sales[Amount])</Expression><DataType>10</DataType></row>"));
    server.reply(500, "text/xml", fault("no relationships DMV here"));
    server.reply(200, "text/xml", dmv("<xsd:element name=\"ID\" type=\"xsd:long\"/><xsd:element name=\"TableID\" type=\"xsd:long\"/><xsd:element name=\"Name\" type=\"xsd:string\"/>", "<row><ID>50</ID><TableID>2</TableID><Name>Calendar</Name></row>"));
    server.reply(200, "text/xml", dmv("<xsd:element name=\"ID\" type=\"xsd:long\"/><xsd:element name=\"HierarchyID\" type=\"xsd:long\"/><xsd:element name=\"Ordinal\" type=\"xsd:long\"/><xsd:element name=\"Name\" type=\"xsd:string\"/><xsd:element name=\"ColumnID\" type=\"xsd:long\"/>", "<row><ID>500</ID><HierarchyID>50</HierarchyID><Ordinal>0</Ordinal><Name>Date</Name><ColumnID>20</ColumnID></row>"));
    let c = client.load_catalog("m").await.unwrap();
    assert_eq!(c.tables.len(), 2);
    assert_eq!(c.table("Sales").unwrap().columns.len(), 1);
    assert_eq!(c.table("Sales").unwrap().measures[0].expression.as_deref(), Some("SUM(Sales[Amount])"));
    assert!(c.relationships.is_empty());
    assert_eq!(c.table("Date").unwrap().hierarchies[0].levels[0].column, "Date");
    let reqs = server.requests();
    assert_eq!(reqs.len(), 9);
    assert!(reqs[1].body.contains("INFO.VIEW.TABLES"));
    assert!(reqs[3].body.contains("<Statement>SELECT * FROM $SYSTEM.TMSCHEMA_TABLES</Statement>"));
    assert!(reqs[8].body.contains("TMSCHEMA_LEVELS"));
}

const WS_ID: &str = "eb24c8db-6189-4c38-94d3-390fabe47782";
const CAP_ID: &str = "ba8d4c98-e720-4344-91d4-fb18bdc13a17";

#[tokio::test]
async fn powerbi_path() {
    let server = FakeServer::start();
    let host = server.base.trim_start_matches("http://").to_string();
    let target = TabularTarget::PowerBi { workspace_id: WS_ID.into(), workspace_name: "Fabric test".into(), dataset_id: "d".into(), model_name: "RetailDemo".into() };

    // 1. workspace record by name: a 307 to another path first (PreferClientRouting), then the record
    server.reply_with(307, "text/plain", "", vec![("Location", format!("{}/redirected/workspaces/Fabric%20test?PreferClientRouting=true", server.base))]);
    server.reply(200, "application/json", format!(r#"{{"id":"{WS_ID}","name":"Fabric test","capacitySku":"Premium","capacityObjectId":"{CAP_ID}","capacityUri":"pbidedicated://{host}/{CAP_ID}","isSharedOnPremium":false}}"#));
    // 2. clusterResolve (no auth)
    server.reply(200, "application/json", format!(r#"{{"clusterFQDN":"{host}","coreServerName":"trialcd233ca20151481199729d0cf6eee1d8","tenantId":"t"}}"#));
    // 3. generateastoken
    server.reply(200, "application/json", r#"{"Token":"MWC.eyJhbGciOi.abc"}"#);
    // 4. the session-creating Execute: Session header, empty root, trailing NUL, routing headers
    let session_answer = format!("{ENV}{}<soap:Body><ExecuteResponse xmlns=\"urn:schemas-microsoft-com:xml-analysis\"><return>{EMPTY}</return></ExecuteResponse></soap:Body></soap:Envelope>", session_header("D4F84F15-1111-2222-3333-444444444444")) + "\u{0}";
    server.reply_with(200, "text/xml", session_answer, vec![("x-ms-routing-hint", "rh-1".into()), ("x-ms-workload-resource-moniker", "mon-1".into()), ("x-ms-xmlacaps-negotiation-flags", "0,1,1,0,0".into())]);
    // 5. DISCOVER_PROPERTIES
    server.reply(200, "text/xml", discover_properties("D4F84F15-1111-2222-3333-444444444444"));

    let client = XmlaClient::connect_with(&target, Credential::Bearer("entra-token".into()), &server.hosts()).await.unwrap();
    assert!(client.is_paas());
    assert_eq!(client.url(), format!("http://{host}/webapi/xmla"));
    assert_eq!(client.catalog(), "RetailDemo");
    assert_eq!(client.session_id().as_deref(), Some("D4F84F15-1111-2222-3333-444444444444"));
    assert_eq!(client.server_version().as_deref(), Some("16.0.123.4"));

    let reqs = server.requests();
    assert_eq!(reqs.len(), 6, "{:?}", reqs.iter().map(|r| format!("{} {}", r.method, r.url)).collect::<Vec<_>>());
    assert_eq!((reqs[0].method.as_str(), reqs[0].url.as_str()), ("GET", "/powerbi/databases/v201606/workspaces/Fabric%20test?PreferClientRouting=true"));
    assert_eq!(reqs[0].headers.get("authorization").map(String::as_str), Some("Bearer entra-token"));
    assert_eq!(reqs[0].headers.get("user-agent").map(String::as_str), Some("ASClient/.NET-Core"));
    assert_eq!(reqs[1].url, "/redirected/workspaces/Fabric%20test?PreferClientRouting=true");
    assert_eq!(reqs[1].headers.get("authorization").map(String::as_str), Some("Bearer entra-token"), "the bearer survives the 307");
    assert_eq!((reqs[2].method.as_str(), reqs[2].url.as_str()), ("POST", "/webapi/clusterResolve"));
    assert!(!reqs[2].headers.contains_key("authorization"), "clusterResolve is unauthenticated");
    let cr: serde_json::Value = serde_json::from_str(&reqs[2].body).unwrap();
    assert_eq!(cr, serde_json::json!({"databaseName": null, "premiumPublicXmlaEndpoint": true, "serverName": CAP_ID}));
    assert_eq!((reqs[3].method.as_str(), reqs[3].url.as_str()), ("POST", "/metadata/v201606/generateastoken?PreferClientRouting=true"));
    assert_eq!(reqs[3].headers.get("authorization").map(String::as_str), Some("Bearer entra-token"));
    let gen: serde_json::Value = serde_json::from_str(&reqs[3].body).unwrap();
    assert_eq!(gen, serde_json::json!({"applyAuxiliaryPermission": false, "auxiliaryPermissionOwner": null, "bypassBuildPermission": false, "capacityObjectId": CAP_ID, "datasetName": "RetailDemo", "intendedUsage": 0, "sourceCapacityObjectId": null, "workspaceObjectId": WS_ID}));

    let first = &reqs[4];
    assert_eq!((first.method.as_str(), first.url.as_str()), ("POST", "/webapi/xmla"));
    assert_eq!(first.headers.get("authorization").map(String::as_str), Some("MwcToken MWC.eyJhbGciOi.abc"));
    assert_eq!(first.headers.get("user-agent").map(String::as_str), Some("ASClient/.NET-Core"));
    assert_eq!(first.headers.get("x-ms-xmlaserver").map(String::as_str), Some("trialcd233ca20151481199729d0cf6eee1d8"));
    assert_eq!(first.headers.get("x-as-acquiretokenstats").map(String::as_str), Some("AppName="));
    assert_eq!(first.headers.get("x-ms-xmlaapp-general-info").map(String::as_str), Some("AppName=cobalt,AadClient=MSAL,CacheUsed=0,ConStrId=1"));
    assert_eq!(first.headers.get("x-ms-xmlaclienttraits").map(String::as_str), Some("1"));
    assert_eq!(first.headers.get("x-ms-xmladedicatedconnection").map(String::as_str), Some("0"));
    assert_eq!(first.headers.get("x-ms-xmlacaps-negotiation-flags").map(String::as_str), Some("0,0,0,0,0"));
    assert_eq!(first.headers.get("x-ms-accepts-continuations").map(String::as_str), Some("1"));
    assert_eq!(first.headers.get("x-ms-round-trip-id").map(String::as_str), Some("0"));
    assert!(first.headers.contains_key("x-ms-request-registration-id") && first.headers.contains_key("x-ms-parent-activity-id"));
    assert!(first.headers.get("content-type").unwrap().starts_with("text/xml"));
    for absent in ["soapaction", "x-transport-caps-negotiation-flags", "x-as-activityid", "x-as-requestid", "x-as-sessionid", "x-ms-xmlasession-id", "x-ms-xmlaworkspaceobjectid", "x-ms-xmladatabase", "x-as-routing", "x-ms-routing-hint"] {
        assert!(!first.headers.contains_key(absent), "{absent} must not be sent: {:?}", first.headers);
    }
    assert!(first.body.contains("<soap:Header><BeginSession xmlns=\"urn:schemas-microsoft-com:xml-analysis\" soap:mustUnderstand=\"1\"/><Version Sequence=\"922\" xmlns=\"http://schemas.microsoft.com/analysisservices/2003/engine/2\"/><NamespaceCompatibility xmlns=\"http://schemas.microsoft.com/analysisservices/2003/xmla\" mustUnderstand=\"0\"/></soap:Header>"));
    assert!(first.body.contains("<Execute xmlns=\"urn:schemas-microsoft-com:xml-analysis\"><Command><Statement></Statement></Command><Properties><PropertyList><Catalog>RetailDemo</Catalog><LocaleIdentifier>1033</LocaleIdentifier><DbpropMsmdActivityID>"));
    assert!(first.body.contains("<DbpropMsmdCurrentActivityID>"));
    assert!(!first.body.contains("<Discover"));

    let second = &reqs[5];
    assert_eq!(second.headers.get("x-ms-xmlasession-id").map(String::as_str), Some("D4F84F15-1111-2222-3333-444444444444"));
    assert_eq!(second.headers.get("x-ms-routing-hint").map(String::as_str), Some("rh-1"), "echoed from the previous 200");
    assert_eq!(second.headers.get("x-ms-workload-resource-moniker").map(String::as_str), Some("mon-1"));
    assert!(second.body.contains("<Session xmlns=\"urn:schemas-microsoft-com:xml-analysis\" soap:mustUnderstand=\"1\" SessionId=\"D4F84F15-1111-2222-3333-444444444444\"/><Version Sequence=\"922\""));
    assert!(second.body.contains("<RequestType>DISCOVER_PROPERTIES</RequestType>"));
    assert!(second.body.contains("<Catalog>RetailDemo</Catalog>"));

    // 6. a query: the cached MWC token is reused (no generateastoken call), rows come back
    server.reply(200, "text/xml", execute_rows(None, &[("a", 1)]));
    let r = client.execute("EVALUATE T", &DaxOptions::default()).await.unwrap().collect().await.unwrap();
    assert_eq!(r[0].table().unwrap().rows, 1);
    let reqs = server.requests();
    assert_eq!(reqs.len(), 7);
    assert_eq!(reqs[6].url, "/webapi/xmla");
    assert!(reqs[6].body.contains("<Statement>EVALUATE T</Statement>") && reqs[6].body.contains("<Catalog>RetailDemo</Catalog>"));

    // 7. an engine fault with the AS Error Description is surfaced as the error item
    server.reply(500, "text/xml", fault("Query (1, 1) The syntax for 'EVALUAT' is incorrect."));
    let r = client.execute("EVALUAT", &DaxOptions::default()).await.unwrap().collect().await.unwrap();
    assert_eq!(r[0].error().unwrap().to_string(), "Query (1, 1) The syntax for 'EVALUAT' is incorrect. (3238789121)");

    // 8. a 3xx from the cluster: the instance moved
    server.reply_with(307, "text/plain", "", vec![("Location", format!("{}/elsewhere", server.base))]);
    let e = match client.execute("EVALUATE T", &DaxOptions::default()).await {
        Err(e) => e,
        Ok(_) => panic!("a 3xx must fail"),
    };
    assert!(matches!(&e, TabularError::Api { status: 307, code, .. } if code == "ServerMoved"), "{e}");

    // 9. the Transport facade takes the same path when forced to XMLA (default hosts: unreachable here, so only the Rest default is checked)
    let t = Transport::connect(&target, Credential::Bearer("entra-token".into()), false).await.unwrap();
    assert_eq!(t.name(), "Arrow query API");
}

#[tokio::test]
async fn powerbi_refusals() {
    let server = FakeServer::start();
    let target = TabularTarget::PowerBi { workspace_id: String::new(), workspace_name: "Pro ws".into(), dataset_id: "d".into(), model_name: "M".into() };
    // a workspace without a capacity: by name answers, the list is not needed
    server.reply(200, "application/json", r#"{"id":"w1","name":"Pro ws","type":"Workspace","capacitySku":"Shared"}"#);
    let e = match XmlaClient::connect_with(&target, Credential::Bearer("t".into()), &server.hosts()).await {
        Err(e) => e,
        Ok(_) => panic!("must fail"),
    };
    assert!(matches!(&e, TabularError::Unsupported(m) if m.contains("not on a Premium or Fabric capacity")), "{e}");

    // generateastoken refused: Build / tenant setting hint
    let host = server.base.trim_start_matches("http://").to_string();
    let (ws, cap) = ("22222222-0000-0000-0000-000000000002", "33333333-0000-0000-0000-000000000003");
    let target = TabularTarget::PowerBi { workspace_id: ws.into(), workspace_name: "Locked ws".into(), dataset_id: "d".into(), model_name: "Locked".into() };
    server.reply(200, "application/json", format!(r#"{{"id":"{ws}","name":"Locked ws","capacitySku":"Premium","capacityUri":"pbidedicated://{host}/{cap}"}}"#));
    server.reply(200, "application/json", format!(r#"{{"clusterFQDN":"{host}","coreServerName":"core"}}"#));
    server.reply(403, "application/json", r#"{"error":{"code":"PowerBINotAuthorizedException","message":""}}"#);
    let e = match XmlaClient::connect_with(&target, Credential::Bearer("t".into()), &server.hosts()).await {
        Err(e) => e,
        Ok(_) => panic!("must fail"),
    };
    assert!(matches!(&e, TabularError::Unauthorized(403, m) if m.starts_with("generateastoken refused: PowerBINotAuthorizedException")), "{e}");
    assert_eq!(e.hint(), Some(cobalt_tabular::HINT_XMLA_TOKEN));

    // the workspace lookup falls through name, id and the list; nothing found
    let target = TabularTarget::PowerBi { workspace_id: "w9".into(), workspace_name: "Nope".into(), dataset_id: "d".into(), model_name: "M".into() };
    server.reply(404, "application/json", "{}");
    server.reply(404, "application/json", "{}");
    server.reply(200, "application/json", r#"[{"id":"other","name":"Other","capacitySku":"Premium","capacityUri":"pbidedicated://x.pbidedicated.windows.net/c"}]"#);
    let e = match XmlaClient::connect_with(&target, Credential::Bearer("t".into()), &server.hosts()).await {
        Err(e) => e,
        Ok(_) => panic!("must fail"),
    };
    assert!(matches!(&e, TabularError::Unsupported(m) if m.starts_with("the Power BI XMLA endpoint could not be resolved")), "{e}");
    let reqs = server.requests();
    let urls: Vec<&str> = reqs[reqs.len() - 3..].iter().map(|r| r.url.as_str()).collect();
    assert_eq!(urls, vec!["/powerbi/databases/v201606/workspaces/Nope?PreferClientRouting=true", "/powerbi/databases/v201606/workspaces/w9?PreferClientRouting=true", "/powerbi/databases/v201606/workspaces?includeMyWorkspace=true&PreferClientRouting=true"]);
}
