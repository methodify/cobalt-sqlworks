use cobalt_core::ServerMessage;

#[derive(Debug, thiserror::Error)]
pub enum DriverError {
    #[error("connection failed: {0}")]
    Connect(String),
    #[error("login failed: {0}")]
    Login(String),
    #[error("TLS error: {0}")]
    Tls(String),
    #[error("{}", .0.message)]
    Server(ServerMessage),
    #[error("connection lost: {0}")]
    Disconnected(String),
    #[error("operation cancelled")]
    Cancelled,
    #[error("timed out after {0} s")]
    Timeout(u32),
    #[error("unsupported: {0}")]
    Unsupported(&'static str),
    #[error("type conversion error in column {column}: {detail}")]
    Conversion { column: String, detail: String },
    #[error("{0}")]
    Other(String),
}

impl DriverError {
    /// A hint we can show under the raw error (certificate/firewall/login guidance).
    pub fn hint(&self) -> Option<&'static str> {
        let text = self.to_string().to_ascii_lowercase();
        if text.contains("certificate") || text.contains("self signed") || text.contains("self-signed") || text.contains("unknown issuer") {
            return Some("The server's TLS certificate isn't trusted. For dev servers, enable \"Trust server certificate\" in the connection's advanced options.");
        }
        if text.contains("login failed for user") {
            return Some("Check the user name and password, and that the login is allowed on this database.");
        }
        if text.contains("firewall") || text.contains("not allowed to access the server") {
            return Some("Your IP isn't allowed by the server firewall. Add a rule in the Azure portal.");
        }
        if text.contains("no such host") || text.contains("could not resolve") || text.contains("getaddrinfo") {
            return Some("The server name didn't resolve. Check for typos and VPN.");
        }
        if text.contains("timed out") || text.contains("connection refused") {
            return Some("Nothing answered on that host/port. Check the port, that the instance is running, and firewalls.");
        }
        None
    }
}
