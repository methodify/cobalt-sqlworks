//! Import the connection library from Azure Data Studio's `settings.json`.
//!
//! ADS keeps groups under `datasource.connectionGroups` and connections under
//! `datasource.connections`. The file is JSON-with-comments and may have trailing commas,
//! so it goes through [`strip_jsonc`] first. Passwords are not in the file (they live in
//! ADS's credential store) and are never imported; SQL logins come across as "prompt".

use crate::{LibraryExport, Result, StoreError};
use cobalt_core::{
    ApplicationIntent, AuthMethod, Color, ConnectionOptions, ConnectionProfile, Encrypt, GroupId,
    ProfileId, ServerGroup,
};
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// Result of parsing an ADS settings file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AdsImport {
    pub library: LibraryExport,
    /// Connections skipped because their provider is not MSSQL: `(display name, provider)`.
    pub skipped: Vec<(String, String)>,
    /// Non-fatal oddities worth showing the user.
    pub warnings: Vec<String>,
}

impl AdsImport {
    pub fn skipped_count(&self) -> usize {
        self.skipped.len()
    }
}

/// Parse ADS `settings.json` text into a [`LibraryExport`] (groups + MSSQL profiles).
pub fn parse_ads_settings(json: &str) -> Result<LibraryExport> {
    Ok(parse_ads_settings_detailed(json)?.library)
}

/// Like [`parse_ads_settings`] but also reports skipped connections and warnings.
pub fn parse_ads_settings_detailed(json: &str) -> Result<AdsImport> {
    let clean = strip_jsonc(json);
    let root: Value = serde_json::from_str(&clean)
        .map_err(|e| StoreError::Ads(format!("settings.json is not valid JSON: {e}")))?;
    let root = root
        .as_object()
        .ok_or_else(|| StoreError::Ads("settings.json root is not an object".into()))?;

    let mut out = AdsImport::default();
    let empty = Vec::new();
    let raw_groups = root
        .get("datasource.connectionGroups")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    let raw_conns = root
        .get("datasource.connections")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    if raw_groups.is_empty() && raw_conns.is_empty() {
        out.warnings
            .push("no datasource.connections or datasource.connectionGroups keys found".into());
    }

    // ---- groups ------------------------------------------------------------------------
    struct RawGroup {
        ads_id: String,
        name: String,
        color: Option<String>,
        description: Option<String>,
        parent: Option<String>,
    }
    let mut groups: Vec<RawGroup> = Vec::new();
    for g in raw_groups {
        let Some(obj) = g.as_object() else { continue };
        let Some(ads_id) = str_field(obj, "id") else {
            out.warnings
                .push("skipped a connection group without an id".into());
            continue;
        };
        groups.push(RawGroup {
            ads_id,
            name: str_field(obj, "name").unwrap_or_default(),
            color: str_field(obj, "color"),
            description: str_field(obj, "description").filter(|s| !s.is_empty()),
            parent: str_field(obj, "parentId").filter(|s| !s.is_empty()),
        });
    }
    let all_ids: HashSet<&str> = groups.iter().map(|g| g.ads_id.as_str()).collect();
    // ADS's implicit root: the group with no parent (usually named/idd "ROOT"). Anything whose
    // parent does not exist is treated as top-level too.
    let root_ids: HashSet<String> = groups
        .iter()
        .filter(|g| {
            g.parent.is_none()
                || g.ads_id.eq_ignore_ascii_case("ROOT")
                || g.name.eq_ignore_ascii_case("ROOT")
        })
        .map(|g| g.ads_id.clone())
        .collect();

    let mut group_ids: HashMap<String, GroupId> = HashMap::new();
    for g in &groups {
        if root_ids.contains(&g.ads_id) {
            continue;
        }
        group_ids.insert(
            g.ads_id.clone(),
            GroupId::parse(&g.ads_id).unwrap_or_default(),
        );
    }
    let mut palette_i = 0usize;
    for g in &groups {
        if root_ids.contains(&g.ads_id) {
            continue;
        }
        let parent = match &g.parent {
            Some(p) if root_ids.contains(p) => None,
            Some(p) if all_ids.contains(p.as_str()) => group_ids.get(p).copied(),
            Some(p) => {
                out.warnings.push(format!(
                    "group {:?} refers to unknown parent {p}; placed at top level",
                    g.name
                ));
                None
            }
            None => None,
        };
        let color = g
            .color
            .as_deref()
            .and_then(parse_ads_color)
            .unwrap_or_else(|| {
                let c = Color::PALETTE[palette_i % Color::PALETTE.len()];
                palette_i += 1;
                c
            });
        out.library.groups.push(ServerGroup {
            id: group_ids[&g.ads_id],
            name: if g.name.is_empty() {
                "Imported group".into()
            } else {
                g.name.clone()
            },
            color,
            parent,
            description: g.description.clone(),
            sort_order: out.library.groups.len() as i32,
        });
    }

    // ---- connections -------------------------------------------------------------------
    let empty_obj = Map::new();
    for c in raw_conns {
        let Some(obj) = c.as_object() else { continue };
        let options = obj
            .get("options")
            .and_then(Value::as_object)
            .unwrap_or(&empty_obj);
        let provider = str_field(obj, "providerName").unwrap_or_else(|| "MSSQL".into());
        let display = str_field(options, "connectionName")
            .filter(|s| !s.is_empty())
            .or_else(|| str_field(options, "server"))
            .unwrap_or_else(|| "(unnamed)".into());
        if !provider.eq_ignore_ascii_case("MSSQL") {
            out.skipped.push((display, provider));
            continue;
        }
        let Some(server) = str_field(options, "server")
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
        else {
            out.warnings
                .push(format!("skipped connection {display:?}: no server"));
            continue;
        };

        let group = str_field(obj, "groupId")
            .or_else(|| str_field(options, "groupId"))
            .filter(|g| !g.is_empty() && !root_ids.contains(g))
            .and_then(|g| {
                let mapped = group_ids.get(&g).copied();
                if mapped.is_none() {
                    out.warnings.push(format!(
                        "connection {display:?} refers to unknown group {g}; left ungrouped"
                    ));
                }
                mapped
            });

        let user = str_field(options, "user").filter(|s| !s.is_empty());
        let tenant = str_field(options, "azureTenantId").filter(|s| !s.is_empty());
        let account = str_field(options, "azureAccount").filter(|s| !s.is_empty());
        let auth_type = str_field(options, "authenticationType").unwrap_or_default();
        let auth = match auth_type.as_str() {
            "AzureMFA" | "AzureMFAAndUser" | "azureMFA" | "azureMFAAndUser" => {
                AuthMethod::EntraInteractive {
                    tenant,
                    account_hint: account.or(user),
                }
            }
            "Integrated" | "integrated" => AuthMethod::WindowsIntegrated,
            "SqlLogin" | "sqlLogin" | "" => AuthMethod::SqlLogin {
                user: user.unwrap_or_default(),
                password: None,
            },
            other => {
                out.warnings.push(format!("connection {display:?}: unknown authenticationType {other:?}; treated as SQL login"));
                AuthMethod::SqlLogin {
                    user: user.unwrap_or_default(),
                    password: None,
                }
            }
        };

        let mut opts = ConnectionOptions::default();
        if let Some(e) = str_field(options, "encrypt") {
            opts.encrypt = match e.trim().to_ascii_lowercase().as_str() {
                "strict" => Encrypt::Strict,
                "false" | "optional" | "no" => Encrypt::Optional,
                _ => Encrypt::Mandatory,
            };
        }
        if let Some(b) = bool_field(options, "trustServerCertificate") {
            opts.trust_server_certificate = b;
        }
        opts.host_name_in_certificate =
            str_field(options, "hostNameInCertificate").filter(|s| !s.is_empty());
        if let Some(a) = str_field(options, "applicationName").filter(|s| !s.is_empty()) {
            // ADS stamps its own name; don't carry that over.
            if !a.eq_ignore_ascii_case("azdata")
                && !a.to_ascii_lowercase().contains("azuredatastudio")
            {
                opts.application_name = a;
            }
        }
        if let Some(t) = num_field(options, "connectTimeout") {
            opts.connect_timeout_secs = t as u32;
        }
        if let Some(t) = num_field(options, "commandTimeout") {
            opts.command_timeout_secs = t as u32;
        }
        if let Some(i) = str_field(options, "applicationIntent") {
            if i.eq_ignore_ascii_case("ReadOnly") {
                opts.application_intent = ApplicationIntent::ReadOnly;
            }
        }
        if let Some(b) = bool_field(options, "multiSubnetFailover") {
            opts.multi_subnet_failover = b;
        }
        if let Some(b) = bool_field(options, "multipleActiveResultSets") {
            opts.mars = b;
        }
        opts.packet_size = num_field(options, "packetSize")
            .filter(|v| *v > 0)
            .map(|v| v as u32);

        let id = str_field(obj, "id")
            .and_then(|s| ProfileId::parse(&s))
            .unwrap_or_default();
        out.library.profiles.push(ConnectionProfile {
            id,
            name: str_field(options, "connectionName")
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty()),
            server,
            port: num_field(options, "port")
                .filter(|v| *v > 0 && *v <= 65535)
                .map(|v| v as u16),
            database: str_field(options, "database")
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty()),
            auth,
            group,
            color: None,
            options: opts,
            read_only_guard: false,
            last_used: None,
        });
    }

    out.library.version = LibraryExport::VERSION;
    out.library.exported_at = Some(chrono::Utc::now());
    Ok(out)
}

/// Where ADS keeps its user settings on this platform, if the file exists.
pub fn default_ads_settings_path() -> Option<PathBuf> {
    ads_settings_candidates().into_iter().find(|p| p.is_file())
}

/// Every location ADS might use on this platform, whether or not the file exists.
pub fn ads_settings_candidates() -> Vec<PathBuf> {
    let tail = ["azuredatastudio", "User", "settings.json"];
    let join = |base: PathBuf| tail.iter().fold(base, |p, s| p.join(s));
    let mut out = Vec::new();
    if let Some(appdata) = std::env::var_os("APPDATA").filter(|s| !s.is_empty()) {
        out.push(join(PathBuf::from(appdata)));
    }
    if let Some(home) = std::env::var_os("HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
    {
        out.push(join(home.join("Library").join("Application Support")));
        if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|s| !s.is_empty()) {
            out.push(join(PathBuf::from(xdg)));
        }
        out.push(join(home.join(".config")));
    }
    out
}

/// Map an ADS group color: `#RGB`, `#RRGGBB`, `#RRGGBBAA`, or a CSS-ish color name.
pub fn parse_ads_color(s: &str) -> Option<Color> {
    let t = s.trim();
    if let Some(hex) = t.strip_prefix('#') {
        return match hex.len() {
            3 => {
                let mut full = String::with_capacity(6);
                for ch in hex.chars() {
                    full.push(ch);
                    full.push(ch);
                }
                Color::parse(&full)
            }
            6 => Color::parse(hex),
            8 => Color::parse(&hex[..6]),
            _ => None,
        };
    }
    let named: Option<Color> = match t.to_ascii_lowercase().as_str() {
        "red" => Some(Color(0xD1, 0x3B, 0x3B)),
        "green" => Some(Color(0x2E, 0xA0, 0x43)),
        "blue" => Some(Color(0x1F, 0x6F, 0xEB)),
        "yellow" | "gold" => Some(Color(0xD2, 0x9A, 0x22)),
        "orange" => Some(Color(0xE0, 0x6C, 0x2A)),
        "purple" | "violet" | "indigo" => Some(Color(0x8A, 0x4F, 0xD3)),
        "teal" | "cyan" | "aqua" => Some(Color(0x16, 0x9C, 0xA8)),
        "gray" | "grey" | "slate" | "silver" => Some(Color(0x6B, 0x72, 0x80)),
        "pink" | "magenta" => Some(Color(0xC8, 0x3E, 0x8E)),
        "brown" | "maroon" => Some(Color(0x8B, 0x4A, 0x2B)),
        "navy" => Some(Color(0x1E, 0x3A, 0x8A)),
        "olive" | "lime" => Some(Color(0x6B, 0x8E, 0x23)),
        "black" => Some(Color(0x1F, 0x1F, 0x1F)),
        "white" => Some(Color(0xE5, 0xE7, 0xEB)),
        _ => None,
    };
    if named.is_some() {
        return named;
    }
    Color::parse(t)
}

fn str_field(obj: &Map<String, Value>, key: &str) -> Option<String> {
    match obj.get(key)? {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn bool_field(obj: &Map<String, Value>, key: &str) -> Option<bool> {
    match obj.get(key)? {
        Value::Bool(b) => Some(*b),
        Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
            "true" | "yes" | "1" | "mandatory" => Some(true),
            "false" | "no" | "0" | "" => Some(false),
            _ => None,
        },
        Value::Number(n) => n.as_i64().map(|v| v != 0),
        _ => None,
    }
}

fn num_field(obj: &Map<String, Value>, key: &str) -> Option<i64> {
    match obj.get(key)? {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Value::String(s) => s
            .trim()
            .parse::<i64>()
            .ok()
            .or_else(|| s.trim().parse::<f64>().ok().map(|f| f as i64)),
        _ => None,
    }
}

/// Remove `//` and `/* */` comments and trailing commas (before `]` / `}`) so the text can be
/// fed to a strict JSON parser. String literals are left untouched.
pub fn strip_jsonc(src: &str) -> String {
    // Pass 1: comments.
    let mut out = String::with_capacity(src.len());
    let mut chars = src.chars().peekable();
    let mut in_str = false;
    while let Some(c) = chars.next() {
        if in_str {
            out.push(c);
            if c == '\\' {
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_str = true;
                out.push(c);
            }
            '/' => match chars.peek() {
                Some('/') => {
                    for n in chars.by_ref() {
                        if n == '\n' {
                            out.push('\n');
                            break;
                        }
                    }
                }
                Some('*') => {
                    chars.next();
                    let mut prev = '\0';
                    for n in chars.by_ref() {
                        if prev == '*' && n == '/' {
                            break;
                        }
                        prev = n;
                    }
                    out.push(' ');
                }
                _ => out.push(c),
            },
            _ => out.push(c),
        }
    }
    // Pass 2: trailing commas.
    let bytes: Vec<char> = out.chars().collect();
    let mut res = String::with_capacity(out.len());
    let mut in_str = false;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if in_str {
            res.push(c);
            if c == '\\' && i + 1 < bytes.len() {
                res.push(bytes[i + 1]);
                i += 1;
            } else if c == '"' {
                in_str = false;
            }
        } else if c == '"' {
            in_str = true;
            res.push(c);
        } else if c == ',' {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j].is_whitespace() {
                j += 1;
            }
            if j < bytes.len() && (bytes[j] == ']' || bytes[j] == '}') {
                // drop the comma; keep the whitespace
            } else {
                res.push(c);
            }
        } else {
            res.push(c);
        }
        i += 1;
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonc_strips_comments_and_trailing_commas() {
        let src = r#"{
            // line comment
            "a": 1, /* block
            comment */ "b": "keep // this /* too */",
            "c": [1, 2, 3,],
            "d": { "x": "y", },
        }"#;
        let v: Value = serde_json::from_str(&strip_jsonc(src)).unwrap();
        assert_eq!(v["a"], 1);
        assert_eq!(v["b"], "keep // this /* too */");
        assert_eq!(v["c"].as_array().unwrap().len(), 3);
        assert_eq!(v["d"]["x"], "y");
    }

    #[test]
    fn jsonc_keeps_escaped_quotes() {
        let src = r#"{"a": "say \"hi\", // not a comment", "b": 2,}"#;
        let v: Value = serde_json::from_str(&strip_jsonc(src)).unwrap();
        assert_eq!(v["a"], "say \"hi\", // not a comment");
        assert_eq!(v["b"], 2);
    }

    #[test]
    fn color_forms() {
        assert_eq!(parse_ads_color("#a1634d"), Some(Color(0xA1, 0x63, 0x4D)));
        assert_eq!(parse_ads_color("#abc"), Some(Color(0xAA, 0xBB, 0xCC)));
        assert_eq!(parse_ads_color("#11223344"), Some(Color(0x11, 0x22, 0x33)));
        assert!(parse_ads_color("Teal").is_some());
        assert_eq!(parse_ads_color("nonsense"), None);
    }
}
