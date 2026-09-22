use crate::{GroupId, ProfileId, Secret, SecretRef};
use serde::{Deserialize, Serialize};

/// A saved connection. Lives in the connection library (SQLite); secrets live in the keychain.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConnectionProfile {
    pub id: ProfileId,
    /// Friendly name; when `None`, display `server` (and database).
    pub name: Option<String>,
    /// Host name, `host,port`, `host\instance`, or a Fabric endpoint host.
    pub server: String,
    pub port: Option<u16>,
    pub database: Option<String>,
    pub auth: AuthMethod,
    pub group: Option<GroupId>,
    /// Overrides the group color when set.
    pub color: Option<Color>,
    pub options: ConnectionOptions,
    /// Ask before running non-read statements on this connection.
    pub read_only_guard: bool,
    #[serde(default)]
    pub last_used: Option<chrono::DateTime<chrono::Utc>>,
}

impl ConnectionProfile {
    pub fn new(server: impl Into<String>, auth: AuthMethod) -> Self {
        Self {
            id: ProfileId::new(),
            name: None,
            server: server.into(),
            port: None,
            database: None,
            auth,
            group: None,
            color: None,
            options: ConnectionOptions::default(),
            read_only_guard: false,
            last_used: None,
        }
    }

    /// What to show in trees and tabs.
    pub fn display_name(&self) -> String {
        match &self.name {
            Some(n) if !n.trim().is_empty() => n.clone(),
            _ => self.server.clone(),
        }
    }

    /// Fabric hosts look like `xxx.datawarehouse.fabric.microsoft.com` or `*.pbidedicated.windows.net`.
    pub fn looks_like_fabric(&self) -> bool {
        let s = self.server.to_ascii_lowercase();
        s.contains(".fabric.microsoft.com") || s.contains(".pbidedicated.windows.net")
    }

    pub fn looks_like_azure(&self) -> bool {
        let s = self.server.to_ascii_lowercase();
        s.contains(".database.windows.net") || s.contains(".sql.azuresynapse.net") || self.looks_like_fabric()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuthMethod {
    SqlLogin {
        user: String,
        /// `None` = prompt every time.
        password: Option<SecretRef>,
    },
    /// Browser-based OAuth2 auth-code + PKCE with loopback redirect.
    EntraInteractive {
        tenant: Option<String>,
        /// Sign-in hint (UPN) to preselect the account.
        account_hint: Option<String>,
    },
    EntraDeviceCode {
        tenant: Option<String>,
    },
    /// Reuse `az login`.
    AzureCli {
        tenant: Option<String>,
    },
    /// SSPI / Kerberos.
    WindowsIntegrated,
    EntraServicePrincipal {
        tenant: String,
        client_id: String,
        secret: Option<SecretRef>,
    },
}

/// Secrets found in a pasted connection string (never stored on the profile).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ConnStringSecrets {
    pub password: Option<String>,
    pub client_secret: Option<String>,
    /// Keys we did not understand, for a note to the user.
    pub ignored: Vec<String>,
}

impl ConnectionProfile {
    /// Fill this profile from an ADO.NET / SqlClient connection string
    /// (`Server=…;Database=…;User ID=…;Password=…;Encrypt=…`). Unknown keys are reported, not
    /// fatal. Returns the secrets found so the caller can put them in the keychain.
    pub fn apply_connection_string(&mut self, s: &str) -> Result<ConnStringSecrets, String> {
        let mut out = ConnStringSecrets::default();
        let mut user: Option<String> = None;
        let mut auth: Option<String> = None;
        let mut integrated = false;
        let mut seen = 0;
        for part in split_conn_string(s) {
            let Some((k, v)) = part.split_once('=') else {
                if !part.trim().is_empty() {
                    out.ignored.push(part.trim().to_string());
                }
                continue;
            };
            seen += 1;
            let key = k.trim().to_ascii_lowercase();
            let val = unquote(v.trim());
            match key.as_str() {
                "server" | "data source" | "address" | "addr" | "network address" => {
                    let mut host = val.trim_start_matches("tcp:").trim().to_string();
                    if let Some((h, p)) = host.rsplit_once(',') {
                        if let Ok(port) = p.trim().parse::<u16>() {
                            self.port = Some(port);
                            host = h.trim().to_string();
                        }
                    }
                    self.server = host;
                }
                "initial catalog" | "database" => self.database = if val.is_empty() { None } else { Some(val) },
                "user id" | "uid" | "user" | "user name" => user = Some(val),
                "password" | "pwd" => out.password = Some(val),
                "integrated security" | "trusted_connection" => integrated = matches!(val.to_ascii_lowercase().as_str(), "true" | "yes" | "sspi"),
                "authentication" => auth = Some(val.to_ascii_lowercase()),
                "encrypt" => {
                    self.options.encrypt = match val.to_ascii_lowercase().as_str() {
                        "strict" => Encrypt::Strict,
                        "false" | "no" | "optional" => Encrypt::Optional,
                        _ => Encrypt::Mandatory,
                    }
                }
                "trustservercertificate" | "trust server certificate" => self.options.trust_server_certificate = matches!(val.to_ascii_lowercase().as_str(), "true" | "yes"),
                "hostnameincertificate" | "host name in certificate" => self.options.host_name_in_certificate = if val.is_empty() { None } else { Some(val) },
                "application name" | "app" => self.options.application_name = val,
                "connect timeout" | "connection timeout" | "timeout" => {
                    if let Ok(n) = val.parse() {
                        self.options.connect_timeout_secs = n;
                    }
                }
                "command timeout" => {
                    if let Ok(n) = val.parse() {
                        self.options.command_timeout_secs = n;
                    }
                }
                "applicationintent" | "application intent" => self.options.application_intent = if val.eq_ignore_ascii_case("readonly") { ApplicationIntent::ReadOnly } else { ApplicationIntent::ReadWrite },
                "multipleactiveresultsets" | "multiple active result sets" => self.options.mars = matches!(val.to_ascii_lowercase().as_str(), "true" | "yes"),
                "packet size" => self.options.packet_size = val.parse().ok(),
                "persist security info" | "pooling" | "min pool size" | "max pool size" | "connection lifetime" | "load balance timeout" | "workstation id" | "failover partner" | "attachdbfilename" | "type system version" | "multisubnetfailover" | "column encryption setting" | "enlist" | "current language" | "language" | "replication" | "transaction binding" | "user instance" | "context connection" | "network library" | "net" => {}
                other => out.ignored.push(other.to_string()),
            }
        }
        if seen == 0 {
            return Err("That does not look like a connection string (expected key=value pairs separated by semicolons).".into());
        }
        if self.server.trim().is_empty() {
            return Err("The connection string has no Server / Data Source.".into());
        }
        // authentication
        self.auth = match auth.as_deref() {
            Some(a) if a.contains("service principal") => {
                out.client_secret = out.password.take();
                AuthMethod::EntraServicePrincipal { tenant: String::new(), client_id: user.clone().unwrap_or_default(), secret: None }
            }
            Some(a) if a.contains("device code") => AuthMethod::EntraDeviceCode { tenant: None },
            Some(a) if a.contains("active directory") || a.contains("activedirectory") => {
                out.password = None; // password flows are not supported; use the browser with the account hint
                AuthMethod::EntraInteractive { tenant: None, account_hint: user.clone().filter(|u| !u.is_empty()) }
            }
            _ if integrated => AuthMethod::WindowsIntegrated,
            _ => AuthMethod::SqlLogin { user: user.unwrap_or_default(), password: None },
        };
        Ok(out)
    }
}

/// Split on `;` outside quotes and `{}` braces.
fn split_conn_string(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut brace = 0usize;
    for c in s.chars() {
        match c {
            '"' | '\'' if brace == 0 => {
                match quote {
                    Some(q) if q == c => quote = None,
                    None => quote = Some(c),
                    _ => {}
                }
                cur.push(c);
            }
            '{' if quote.is_none() => {
                brace += 1;
                cur.push(c);
            }
            '}' if quote.is_none() && brace > 0 => {
                brace -= 1;
                cur.push(c);
            }
            ';' if quote.is_none() && brace == 0 => {
                parts.push(std::mem::take(&mut cur));
            }
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        parts.push(cur);
    }
    parts
}

fn unquote(v: &str) -> String {
    let v = v.trim();
    if (v.starts_with('"') && v.ends_with('"') && v.len() >= 2) || (v.starts_with('\'') && v.ends_with('\'') && v.len() >= 2) || (v.starts_with('{') && v.ends_with('}') && v.len() >= 2) {
        v[1..v.len() - 1].to_string()
    } else {
        v.to_string()
    }
}

impl AuthMethod {
    pub fn label(&self) -> &'static str {
        match self {
            AuthMethod::SqlLogin { .. } => "SQL Login",
            AuthMethod::EntraInteractive { .. } => "Microsoft Entra ID (interactive)",
            AuthMethod::EntraDeviceCode { .. } => "Microsoft Entra ID (device code)",
            AuthMethod::AzureCli { .. } => "Azure CLI (az login)",
            AuthMethod::WindowsIntegrated => "Windows Authentication",
            AuthMethod::EntraServicePrincipal { .. } => "Entra service principal",
        }
    }
    pub fn is_entra(&self) -> bool {
        matches!(
            self,
            AuthMethod::EntraInteractive { .. }
                | AuthMethod::EntraDeviceCode { .. }
                | AuthMethod::AzureCli { .. }
                | AuthMethod::EntraServicePrincipal { .. }
        )
    }
    pub fn user_name(&self) -> Option<&str> {
        match self {
            AuthMethod::SqlLogin { user, .. } => Some(user),
            AuthMethod::EntraInteractive { account_hint, .. } => account_hint.as_deref(),
            _ => None,
        }
    }
}

/// Credentials resolved just before connecting (secrets pulled from the keychain, tokens acquired).
#[derive(Clone, Debug)]
pub enum ResolvedCredentials {
    SqlLogin { user: String, password: Secret },
    /// A bearer access token for `https://database.windows.net/`.
    EntraToken { token: Secret, expires_at: Option<chrono::DateTime<chrono::Utc>> },
    WindowsIntegrated,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Encrypt {
    /// TDS 8.0 strict TLS.
    Strict,
    /// TLS required (SqlClient "Mandatory"/true). Default since ADS 1.40 / SqlClient 4.
    #[default]
    Mandatory,
    /// Encrypt only if the server insists.
    Optional,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ApplicationIntent {
    #[default]
    ReadWrite,
    ReadOnly,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ConnectionOptions {
    pub encrypt: Encrypt,
    pub trust_server_certificate: bool,
    pub host_name_in_certificate: Option<String>,
    pub application_name: String,
    pub connect_timeout_secs: u32,
    /// 0 = no timeout.
    pub command_timeout_secs: u32,
    pub application_intent: ApplicationIntent,
    pub mars: bool,
    pub packet_size: Option<u32>,
    pub multi_subnet_failover: bool,
}

impl Default for ConnectionOptions {
    fn default() -> Self {
        Self {
            encrypt: Encrypt::Mandatory,
            trust_server_certificate: false,
            host_name_in_certificate: None,
            application_name: "Cobalt SQL Works".into(),
            connect_timeout_secs: 30,
            command_timeout_secs: 0,
            application_intent: ApplicationIntent::ReadWrite,
            mars: false,
            packet_size: None,
            multi_subnet_failover: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ServerGroup {
    pub id: GroupId,
    pub name: String,
    pub color: Color,
    pub parent: Option<GroupId>,
    pub description: Option<String>,
    pub sort_order: i32,
}

impl ServerGroup {
    pub fn new(name: impl Into<String>, color: Color) -> Self {
        Self { id: GroupId::new(), name: name.into(), color, parent: None, description: None, sort_order: 0 }
    }
}

/// sRGB color, no alpha. Serialized as `#RRGGBB`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Color(pub u8, pub u8, pub u8);

impl Color {
    pub fn hex(&self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.0, self.1, self.2)
    }
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().trim_start_matches('#');
        if s.len() != 6 {
            return None;
        }
        let v = u32::from_str_radix(s, 16).ok()?;
        Some(Self(((v >> 16) & 0xff) as u8, ((v >> 8) & 0xff) as u8, (v & 0xff) as u8))
    }
    /// Default group palette (8 entries), chosen to read on both light and dark.
    pub const PALETTE: [Color; 8] = [
        Color(0x1F, 0x6F, 0xEB), // cobalt blue
        Color(0x2E, 0xA0, 0x43), // green
        Color(0xD2, 0x9A, 0x22), // amber
        Color(0xD1, 0x3B, 0x3B), // red
        Color(0x8A, 0x4F, 0xD3), // purple
        Color(0x16, 0x9C, 0xA8), // teal
        Color(0xE0, 0x6C, 0x2A), // orange
        Color(0x6B, 0x72, 0x80), // slate
    ];
}

impl Serialize for Color {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.hex())
    }
}
impl<'de> Deserialize<'de> for Color {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Color::parse(&s).ok_or_else(|| serde::de::Error::custom(format!("bad color {s:?}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn color_roundtrip() {
        let c = Color(0x1F, 0x6F, 0xEB);
        assert_eq!(Color::parse(&c.hex()), Some(c));
        assert_eq!(serde_json::to_string(&c).unwrap(), "\"#1F6FEB\"");
    }
    #[test]
    fn fabric_detection() {
        let p = ConnectionProfile::new("abc-xyz.datawarehouse.fabric.microsoft.com", AuthMethod::EntraInteractive { tenant: None, account_hint: None });
        assert!(p.looks_like_fabric() && p.looks_like_azure());
    }
}

#[cfg(test)]
mod conn_string_tests {
    use super::*;

    #[test]
    fn sql_login_with_port_and_options() {
        let mut p = ConnectionProfile::new("", AuthMethod::WindowsIntegrated);
        let s = p.apply_connection_string("Server=tcp:db.example.com,1533;Initial Catalog=Sales;User ID=app;Password=\"p;w=1\";Encrypt=True;TrustServerCertificate=true;Application Name=x;Connect Timeout=45;ApplicationIntent=ReadOnly;Bogus=1").unwrap();
        assert_eq!(p.server, "db.example.com");
        assert_eq!(p.port, Some(1533));
        assert_eq!(p.database.as_deref(), Some("Sales"));
        assert!(matches!(&p.auth, AuthMethod::SqlLogin { user, .. } if user == "app"));
        assert_eq!(s.password.as_deref(), Some("p;w=1"));
        assert_eq!(p.options.encrypt, Encrypt::Mandatory);
        assert!(p.options.trust_server_certificate);
        assert_eq!(p.options.connect_timeout_secs, 45);
        assert_eq!(p.options.application_intent, ApplicationIntent::ReadOnly);
        assert_eq!(s.ignored, vec!["bogus".to_string()]);
    }

    #[test]
    fn entra_and_integrated() {
        let mut p = ConnectionProfile::new("", AuthMethod::WindowsIntegrated);
        p.apply_connection_string("Data Source=x.datawarehouse.fabric.microsoft.com;Authentication=Active Directory Interactive;User ID=me@corp.com;Encrypt=Strict").unwrap();
        assert!(matches!(&p.auth, AuthMethod::EntraInteractive { account_hint: Some(h), .. } if h == "me@corp.com"));
        assert_eq!(p.options.encrypt, Encrypt::Strict);
        let mut q = ConnectionProfile::new("", AuthMethod::WindowsIntegrated);
        q.apply_connection_string("Server=.\\SQLEXPRESS;Database=master;Integrated Security=SSPI").unwrap();
        assert_eq!(q.server, ".\\SQLEXPRESS");
        assert!(matches!(q.auth, AuthMethod::WindowsIntegrated));
        let mut sp = ConnectionProfile::new("", AuthMethod::WindowsIntegrated);
        let s = sp.apply_connection_string("Server=s;Authentication=Active Directory Service Principal;User Id=client-guid;Password=sec").unwrap();
        assert!(matches!(&sp.auth, AuthMethod::EntraServicePrincipal { client_id, .. } if client_id == "client-guid"));
        assert_eq!(s.client_secret.as_deref(), Some("sec"));
        assert!(s.password.is_none());
    }

    #[test]
    fn rejects_garbage() {
        let mut p = ConnectionProfile::new("", AuthMethod::WindowsIntegrated);
        assert!(p.apply_connection_string("hello world").is_err());
    }
}
