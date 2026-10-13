//! XMLA over HTTPS (slice C, not started): SOAP 1.1 Discover / Execute envelopes, the
//! `root/row` rowset parser with its inline XSD schema, `BeginSession` / `Session` /
//! `EndSession` headers with `<Cancel/>`, and target resolution for Azure Analysis Services,
//! `msmdpump.dll` and (after the routing capture) Power BI's `*.pbidedicated.windows.net`
//! clusters. Until it lands, `PowerBiClient::execute` refuses `AzureAs` and `Http` targets
//! with `TabularError::Unsupported`.
