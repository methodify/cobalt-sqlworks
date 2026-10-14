//! SOAP 1.1 envelopes for XMLA (MS-SSAS §2.1.2 / §2.1.3, XMLA 1.1): `Discover`, `Execute`
//! with a `Statement` or `Cancel`, and the `BeginSession` / `Session` / `EndSession` headers.
//! Text only; the HTTP side lives in `mod.rs`.

use std::fmt::Write;

/// The XMLA namespace.
pub const NS_XMLA: &str = "urn:schemas-microsoft-com:xml-analysis";
/// The rowset namespace a `root` element carries when it holds rows.
pub const NS_ROWSET: &str = "urn:schemas-microsoft-com:xml-analysis:rowset";
/// The `root` namespace of an answer with no rows (a command, `Cancel`, `EndSession`).
pub const NS_EMPTY: &str = "urn:schemas-microsoft-com:xml-analysis:empty";
/// The `root` namespace of an engine error inside a 200 answer.
pub const NS_EXCEPTION: &str = "urn:schemas-microsoft-com:xml-analysis:exception";
/// The SOAP 1.1 envelope namespace.
pub const NS_SOAP: &str = "http://schemas.xmlsoap.org/soap/envelope/";

/// The `SOAPAction` header values (quotes included, as the spec writes them).
pub const ACTION_DISCOVER: &str = "\"urn:schemas-microsoft-com:xml-analysis:Discover\"";
pub const ACTION_EXECUTE: &str = "\"urn:schemas-microsoft-com:xml-analysis:Execute\"";

/// The value of `SspropInitAppName`.
pub const APP_NAME: &str = "Cobalt SQL Works";

/// What goes in the SOAP header: start a session, continue one, or none at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionHeader<'a> {
    None,
    Begin,
    Existing(&'a str),
    End(&'a str),
}

/// Escape text for element content or an attribute value.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // XML 1.0 cannot carry these at all; drop them rather than send a malformed body
            c if (c as u32) < 0x20 && c != '\t' && c != '\n' && c != '\r' => {}
            c => out.push(c),
        }
    }
    out
}

fn header_xml(session: SessionHeader<'_>) -> String {
    match session {
        SessionHeader::None => String::new(),
        SessionHeader::Begin => format!("<soap:Header><BeginSession xmlns=\"{NS_XMLA}\" soap:mustUnderstand=\"1\"/></soap:Header>"),
        SessionHeader::Existing(id) => format!("<soap:Header><Session xmlns=\"{NS_XMLA}\" soap:mustUnderstand=\"1\" SessionId=\"{}\"/></soap:Header>", escape(id)),
        SessionHeader::End(id) => format!("<soap:Header><EndSession xmlns=\"{NS_XMLA}\" soap:mustUnderstand=\"1\" SessionId=\"{}\"/></soap:Header>", escape(id)),
    }
}

fn envelope(session: SessionHeader<'_>, body: &str) -> String {
    format!("<?xml version=\"1.0\" encoding=\"utf-8\"?><soap:Envelope xmlns:soap=\"{NS_SOAP}\">{}<soap:Body>{body}</soap:Body></soap:Envelope>", header_xml(session))
}

fn property_list(props: &[(&str, String)]) -> String {
    let mut out = String::new();
    for (k, v) in props {
        let _ = write!(out, "<{k}>{}</{k}>", escape(v));
    }
    out
}

/// A `Discover` request: `request_type` (`DISCOVER_PROPERTIES`, `DBSCHEMA_CATALOGS`, …),
/// restrictions and properties as name / value pairs. `Format=Tabular` and `Content=SchemaData`
/// are appended unless the caller set them.
pub fn discover(request_type: &str, restrictions: &[(&str, String)], properties: &[(&str, String)], session: SessionHeader<'_>) -> String {
    let mut props: Vec<(&str, String)> = properties.to_vec();
    if !props.iter().any(|(k, _)| *k == "Format") {
        props.push(("Format", "Tabular".into()));
    }
    if !props.iter().any(|(k, _)| *k == "Content") {
        props.push(("Content", "SchemaData".into()));
    }
    let body = format!(
        "<Discover xmlns=\"{NS_XMLA}\"><RequestType>{}</RequestType><Restrictions><RestrictionList>{}</RestrictionList></Restrictions><Properties><PropertyList>{}</PropertyList></Properties></Discover>",
        escape(request_type),
        property_list(restrictions),
        property_list(&props)
    );
    envelope(session, &body)
}

/// The `PropertyList` of an `Execute`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExecuteProps {
    pub catalog: String,
    /// Seconds; 0 sends no `Timeout` (the server's default applies).
    pub timeout_secs: u32,
    pub activity_id: String,
    pub request_id: String,
    pub locale: u32,
    pub effective_user_name: Option<String>,
    pub roles: Vec<String>,
    pub memory_limit_kb: Option<u64>,
}

impl ExecuteProps {
    fn pairs(&self) -> Vec<(&'static str, String)> {
        let mut props: Vec<(&'static str, String)> = vec![("Catalog", self.catalog.clone()), ("Format", "Tabular".into()), ("Content", "SchemaData".into())];
        if self.timeout_secs > 0 {
            props.push(("Timeout", self.timeout_secs.to_string()));
        }
        props.push(("SspropInitAppName", APP_NAME.into()));
        props.push(("DbpropMsmdActivityID", self.activity_id.clone()));
        props.push(("DbpropMsmdRequestID", self.request_id.clone()));
        props.push(("LocaleIdentifier", if self.locale == 0 { 1033 } else { self.locale }.to_string()));
        if let Some(u) = self.effective_user_name.as_deref().map(str::trim).filter(|u| !u.is_empty()) {
            props.push(("EffectiveUserName", u.to_string()));
        }
        let roles: Vec<&str> = self.roles.iter().map(|r| r.trim()).filter(|r| !r.is_empty()).collect();
        if !roles.is_empty() {
            props.push(("Roles", roles.join(",")));
        }
        if let Some(kb) = self.memory_limit_kb {
            props.push(("DbpropMsmdRequestMemoryLimit", kb.to_string()));
        }
        props
    }
}

/// An `Execute` with a `Statement` (DAX, MDX or a DMV `SELECT`; the server tells them apart).
pub fn execute(statement: &str, props: &ExecuteProps, session: SessionHeader<'_>) -> String {
    let body = format!("<Execute xmlns=\"{NS_XMLA}\"><Command><Statement>{}</Statement></Command><Properties><PropertyList>{}</PropertyList></Properties></Execute>", escape(statement), property_list(&props.pairs()));
    envelope(session, &body)
}

/// An `Execute` with `<Cancel/>`: stops what the session is running.
pub fn cancel(session_id: &str) -> String {
    let body = format!("<Execute xmlns=\"{NS_XMLA}\"><Command><Cancel/></Command><Properties><PropertyList/></Properties></Execute>");
    envelope(SessionHeader::Existing(session_id), &body)
}

/// An `EndSession` header on a `Discover` nobody reads (the cheapest body there is).
pub fn end_session(session_id: &str) -> String {
    let body = format!("<Discover xmlns=\"{NS_XMLA}\"><RequestType>DISCOVER_PROPERTIES</RequestType><Restrictions><RestrictionList><PropertyName>Catalog</PropertyName></RestrictionList></Restrictions><Properties><PropertyList><Format>Tabular</Format><Content>SchemaData</Content></PropertyList></Properties></Discover>");
    envelope(SessionHeader::End(session_id), &body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discover_text() {
        let xml = discover("DBSCHEMA_CATALOGS", &[], &[("Catalog", "Sales & Co".into())], SessionHeader::Begin);
        assert_eq!(
            xml,
            "<?xml version=\"1.0\" encoding=\"utf-8\"?><soap:Envelope xmlns:soap=\"http://schemas.xmlsoap.org/soap/envelope/\"><soap:Header><BeginSession xmlns=\"urn:schemas-microsoft-com:xml-analysis\" soap:mustUnderstand=\"1\"/></soap:Header><soap:Body><Discover xmlns=\"urn:schemas-microsoft-com:xml-analysis\"><RequestType>DBSCHEMA_CATALOGS</RequestType><Restrictions><RestrictionList></RestrictionList></Restrictions><Properties><PropertyList><Catalog>Sales &amp; Co</Catalog><Format>Tabular</Format><Content>SchemaData</Content></PropertyList></Properties></Discover></soap:Body></soap:Envelope>"
        );
        let xml = discover("MDSCHEMA_FUNCTIONS", &[("ORIGIN", "1".into())], &[], SessionHeader::None);
        assert!(xml.contains("<RestrictionList><ORIGIN>1</ORIGIN></RestrictionList>"));
        assert!(!xml.contains("soap:Header"));
    }

    #[test]
    fn execute_text() {
        let props = ExecuteProps { catalog: "Sales".into(), timeout_secs: 30, activity_id: "a-1".into(), request_id: "r-1".into(), locale: 0, effective_user_name: Some("u@x.com".into()), roles: vec!["Reader".into(), " ".into(), "EU".into()], memory_limit_kb: None };
        let xml = execute("EVALUATE FILTER(T, T[x] < 3 && T[y] > \"a\")", &props, SessionHeader::Existing("S-123"));
        assert_eq!(
            xml,
            "<?xml version=\"1.0\" encoding=\"utf-8\"?><soap:Envelope xmlns:soap=\"http://schemas.xmlsoap.org/soap/envelope/\"><soap:Header><Session xmlns=\"urn:schemas-microsoft-com:xml-analysis\" soap:mustUnderstand=\"1\" SessionId=\"S-123\"/></soap:Header><soap:Body><Execute xmlns=\"urn:schemas-microsoft-com:xml-analysis\"><Command><Statement>EVALUATE FILTER(T, T[x] &lt; 3 &amp;&amp; T[y] &gt; &quot;a&quot;)</Statement></Command><Properties><PropertyList><Catalog>Sales</Catalog><Format>Tabular</Format><Content>SchemaData</Content><Timeout>30</Timeout><SspropInitAppName>Cobalt SQL Works</SspropInitAppName><DbpropMsmdActivityID>a-1</DbpropMsmdActivityID><DbpropMsmdRequestID>r-1</DbpropMsmdRequestID><LocaleIdentifier>1033</LocaleIdentifier><EffectiveUserName>u@x.com</EffectiveUserName><Roles>Reader,EU</Roles></PropertyList></Properties></Execute></soap:Body></soap:Envelope>"
        );
        let plain = execute("EVALUATE {1}", &ExecuteProps { catalog: "m".into(), memory_limit_kb: Some(4096), ..ExecuteProps::default() }, SessionHeader::None);
        assert!(!plain.contains("<Timeout>"));
        assert!(!plain.contains("EffectiveUserName"));
        assert!(plain.contains("<DbpropMsmdRequestMemoryLimit>4096</DbpropMsmdRequestMemoryLimit>"));
    }

    #[test]
    fn cancel_and_end() {
        let xml = cancel("S-1");
        assert!(xml.contains("<Session xmlns=\"urn:schemas-microsoft-com:xml-analysis\" soap:mustUnderstand=\"1\" SessionId=\"S-1\"/>"));
        assert!(xml.contains("<Command><Cancel/></Command><Properties><PropertyList/></Properties>"));
        let xml = end_session("S-1");
        assert!(xml.contains("<EndSession xmlns=\"urn:schemas-microsoft-com:xml-analysis\" soap:mustUnderstand=\"1\" SessionId=\"S-1\"/>"));
    }

    #[test]
    fn escaping() {
        assert_eq!(escape("a<b>&\"c'\u{1}"), "a&lt;b&gt;&amp;&quot;c&apos;");
    }
}
