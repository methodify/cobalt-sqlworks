//! `ConnectionProfile` → `tiberius::Config`, including server-string parsing.
//!
//! Accepted server forms (all case-insensitive, optional `tcp:` prefix):
//! `host`, `host,port`, `host:port`, `host\instance`, `host\instance,port`, `[::1]:1433`.

use crate::{DriverError, Result};
use cobalt_core::{ApplicationIntent, AuthMethod, ConnectionProfile, Encrypt, ResolvedCredentials};
use tiberius::{AuthMethod as TdsAuth, Config, EncryptionLevel};

/// The parsed parts of `ConnectionProfile::server`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerSpec {
    pub host: String,
    /// Port given inline (`host,1433` / `host:1433`).
    pub port: Option<u16>,
    /// Named instance (`host\SQLEXPRESS`), resolved through SQL Browser when no port is given.
    pub instance: Option<String>,
}

/// Parse a server string. Never fails: an empty string yields `localhost`.
pub fn parse_server(server: &str) -> ServerSpec {
    let mut s = server.trim();
    if s.len() >= 4 && s[..4].eq_ignore_ascii_case("tcp:") {
        s = s[4..].trim_start();
    }
    if s.is_empty() {
        return ServerSpec { host: "localhost".into(), port: None, instance: None };
    }

    // host part may be a bracketed IPv6 literal: [::1]:1433 / [::1],1433
    let (host_and_instance, mut port) = match s.rsplit_once(',') {
        Some((h, p)) if p.trim().parse::<u16>().is_ok() => (h.trim(), Some(p.trim().parse::<u16>().unwrap())),
        _ => (s, None),
    };

    let (host_part, instance) = match host_and_instance.split_once('\\') {
        Some((h, inst)) => (h.trim(), Some(inst.trim().to_string()).filter(|i| !i.is_empty())),
        None => (host_and_instance, None),
    };

    let mut host = host_part.to_string();
    if port.is_none() {
        if let Some(rest) = host_part.strip_prefix('[') {
            // [ipv6]:port
            if let Some((inner, tail)) = rest.split_once(']') {
                host = inner.to_string();
                if let Some(p) = tail.strip_prefix(':') {
                    port = p.parse().ok();
                }
            }
        } else if host_part.matches(':').count() == 1 {
            // host:port (a single colon; bare IPv6 has several)
            if let Some((h, p)) = host_part.rsplit_once(':') {
                if let Ok(pp) = p.parse::<u16>() {
                    host = h.to_string();
                    port = Some(pp);
                }
            }
        }
    } else if let Some(rest) = host_part.strip_prefix('[') {
        if let Some((inner, _)) = rest.split_once(']') {
            host = inner.to_string();
        }
    }

    if host.is_empty() || host == "." || host.eq_ignore_ascii_case("(local)") {
        host = "localhost".into();
    }

    ServerSpec { host, port, instance }
}

/// Fabric / Azure SQL endpoints always listen on 1433 and require TLS.
pub fn is_azure_like(profile: &ConnectionProfile) -> bool {
    profile.looks_like_azure()
}

/// The port to dial, if known. `None` means "ask SQL Browser for the instance".
pub fn effective_port(profile: &ConnectionProfile, spec: &ServerSpec) -> Option<u16> {
    if let Some(p) = profile.port.or(spec.port) {
        return Some(p);
    }
    if spec.instance.is_some() && !is_azure_like(profile) {
        None
    } else {
        Some(1433)
    }
}

/// The effective encryption level after applying the Azure/Fabric floor.
pub fn effective_encryption(profile: &ConnectionProfile) -> EncryptionLevel {
    match profile.options.encrypt {
        Encrypt::Strict => EncryptionLevel::Strict,
        Encrypt::Mandatory => EncryptionLevel::Required,
        // "Encrypt only if the server insists": login-only encryption unless the server
        // upgrades to full encryption in the pre-login exchange.
        Encrypt::Optional if is_azure_like(profile) => EncryptionLevel::Required,
        Encrypt::Optional => EncryptionLevel::Off,
    }
}

/// Build the tiberius configuration for a profile and its resolved credentials.
pub fn build_config(profile: &ConnectionProfile, creds: &ResolvedCredentials) -> Result<(Config, ServerSpec)> {
    let spec = parse_server(&profile.server);
    let mut config = Config::new();
    config.host(&spec.host);
    if let Some(port) = effective_port(profile, &spec) {
        config.port(port);
    }
    if let Some(instance) = &spec.instance {
        config.instance_name(instance);
    }
    if let Some(db) = profile.database.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
        config.database(db);
    }
    config.application_name(&profile.options.application_name);
    config.encryption(effective_encryption(profile));
    if profile.options.trust_server_certificate {
        config.trust_cert();
    }
    if let Some(h) = profile.options.host_name_in_certificate.as_deref().map(str::trim).filter(|h| !h.is_empty()) {
        config.hostname_in_certificate(h);
    }
    if let Some(size) = profile.options.packet_size {
        config.packet_size(size);
    }
    config.readonly(profile.options.application_intent == ApplicationIntent::ReadOnly);
    config.multi_subnet_failover(profile.options.multi_subnet_failover);
    if let Ok(host) = std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")) {
        if !host.is_empty() {
            config.client_name(host);
        }
    }

    config.authentication(auth_method(profile, creds)?);
    Ok((config, spec))
}

fn auth_method(profile: &ConnectionProfile, creds: &ResolvedCredentials) -> Result<TdsAuth> {
    match creds {
        ResolvedCredentials::SqlLogin { user, password } => Ok(TdsAuth::sql_server(user, password.expose())),
        ResolvedCredentials::EntraToken { token, .. } => {
            if token.is_empty() {
                return Err(DriverError::Login("empty Entra access token".into()));
            }
            Ok(TdsAuth::aad_token(token.expose()))
        }
        ResolvedCredentials::WindowsIntegrated => {
            #[cfg(windows)]
            {
                let _ = profile;
                Ok(TdsAuth::Integrated)
            }
            #[cfg(not(windows))]
            {
                let _ = profile;
                Err(DriverError::Unsupported("Windows integrated authentication is only available on Windows in this build"))
            }
        }
    }
}

/// True when the profile's auth method and the resolved credentials disagree (a programming
/// error upstream); used to give a clear message instead of a confusing login failure.
pub fn credentials_match(profile: &ConnectionProfile, creds: &ResolvedCredentials) -> bool {
    match (&profile.auth, creds) {
        (AuthMethod::SqlLogin { .. }, ResolvedCredentials::SqlLogin { .. }) => true,
        (AuthMethod::WindowsIntegrated, ResolvedCredentials::WindowsIntegrated) => true,
        (a, ResolvedCredentials::EntraToken { .. }) => a.is_entra(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cobalt_core::Secret;

    fn spec(host: &str, port: Option<u16>, instance: Option<&str>) -> ServerSpec {
        ServerSpec { host: host.into(), port, instance: instance.map(String::from) }
    }

    #[test]
    fn parses_plain_forms() {
        assert_eq!(parse_server("localhost"), spec("localhost", None, None));
        assert_eq!(parse_server("  tcp:myhost  "), spec("myhost", None, None));
        assert_eq!(parse_server("myhost,1444"), spec("myhost", Some(1444), None));
        assert_eq!(parse_server("myhost:1444"), spec("myhost", Some(1444), None));
        assert_eq!(parse_server("myhost\\SQLEXPRESS"), spec("myhost", None, Some("SQLEXPRESS")));
        assert_eq!(parse_server("myhost\\SQLEXPRESS,1500"), spec("myhost", Some(1500), Some("SQLEXPRESS")));
        assert_eq!(parse_server("."), spec("localhost", None, None));
        assert_eq!(parse_server("(local)\\inst"), spec("localhost", None, Some("inst")));
        assert_eq!(parse_server(""), spec("localhost", None, None));
    }

    #[test]
    fn parses_ipv6() {
        assert_eq!(parse_server("[::1]:1433"), spec("::1", Some(1433), None));
        assert_eq!(parse_server("[::1],1433"), spec("::1", Some(1433), None));
        assert_eq!(parse_server("fe80::1"), spec("fe80::1", None, None));
    }

    #[test]
    fn port_and_encryption_defaults() {
        let sql = AuthMethod::SqlLogin { user: "sa".into(), password: None };
        let mut p = ConnectionProfile::new("myhost\\inst", sql.clone());
        assert_eq!(effective_port(&p, &parse_server(&p.server)), None, "named instance → SQL Browser");
        p.port = Some(1500);
        assert_eq!(effective_port(&p, &parse_server(&p.server)), Some(1500));

        let p = ConnectionProfile::new("plain", sql.clone());
        assert_eq!(effective_port(&p, &parse_server(&p.server)), Some(1433));
        assert_eq!(effective_encryption(&p), EncryptionLevel::Required);

        let mut p = ConnectionProfile::new("abc.datawarehouse.fabric.microsoft.com", AuthMethod::EntraInteractive { tenant: None, account_hint: None });
        p.options.encrypt = Encrypt::Optional;
        assert!(is_azure_like(&p));
        assert_eq!(effective_port(&p, &parse_server(&p.server)), Some(1433));
        assert_eq!(effective_encryption(&p), EncryptionLevel::Required, "Fabric floors encryption at Required");
        p.options.encrypt = Encrypt::Strict;
        assert_eq!(effective_encryption(&p), EncryptionLevel::Strict);

        let mut p = ConnectionProfile::new("x.database.windows.net", sql);
        p.options.encrypt = Encrypt::Optional;
        assert_eq!(effective_encryption(&p), EncryptionLevel::Required);
        p.server = "onprem".into();
        assert_eq!(effective_encryption(&p), EncryptionLevel::Off);
    }

    #[test]
    fn builds_config() {
        let mut p = ConnectionProfile::new("myhost,1500", AuthMethod::SqlLogin { user: "sa".into(), password: None });
        p.database = Some("cobalt_test".into());
        p.options.trust_server_certificate = true;
        let creds = ResolvedCredentials::SqlLogin { user: "sa".into(), password: Secret::new("pw") };
        let (cfg, spec) = build_config(&p, &creds).unwrap();
        assert_eq!(cfg.get_addr(), "myhost:1500");
        assert_eq!(spec.port, Some(1500));

        let creds = ResolvedCredentials::EntraToken { token: Secret::new(""), expires_at: None };
        assert!(matches!(build_config(&p, &creds), Err(DriverError::Login(_))));
    }
}
