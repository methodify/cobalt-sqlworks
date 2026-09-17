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
