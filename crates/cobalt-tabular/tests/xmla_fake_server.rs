//! The XMLA client against a scripted HTTP server: capability flags and `BeginSession` on the
//! first request, `Session` on the next, one retry with a new session when the server lost
//! it, `<Cancel/>` with the session id, and a refusal of non-XML answers.

use cobalt_core::tabular::TabularTarget;
use cobalt_tabular::{Credential, DaxOptions, TabularError, Transport, XmlaClient};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

type Reply = (u16, &'static str, String);

#[derive(Clone, Debug)]
struct Recorded {
    headers: HashMap<String, String>,
    body: String,
}

struct FakeServer {
    url: String,
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
                q2.lock().unwrap().push(Recorded { headers, body });
                let (code, ct, text) = r2.lock().unwrap().pop_front().unwrap_or((500, "text/plain", "no scripted reply".into()));
                let _ = req.respond(tiny_http::Response::from_string(text).with_status_code(code).with_header(tiny_http::Header::from_bytes("Content-Type", ct).unwrap()));
            }
        });
        FakeServer { url: format!("http://127.0.0.1:{port}/olap/msmdpump.dll"), replies, requests, stop, thread: Some(thread) }
    }

    fn reply(&self, code: u16, ct: &'static str, body: impl Into<String>) {
        self.replies.lock().unwrap().push_back((code, ct, body.into()));
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
