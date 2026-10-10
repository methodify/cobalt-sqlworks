//! A loopback Unity-Catalog-compatible endpoint for the LakeSail engine, backed by Fabric's own
//! OneLake table API. Sail is configured with one `unity` catalog per attached lakehouse pointing
//! here; it asks for schemas, tables and a table's columns the first time a statement names
//! them, and Cobalt answers from Fabric (`onelake.table.fabric.microsoft.com`, the signed-in
//! account's storage token) with two fixes Sail needs: Spark type names normalised to Unity's
//! spelling (`integer` → `INT`, `decimal(18,2)` → `DECIMAL` + precision/scale) and the storage
//! location rewritten to the `abfss://` form by ids. Everything is cached per lakehouse for the
//! session (one listing call per schema, one call per table), so no session ever mounts a table,
//! however many tables the lakehouse has. Creating a table through the catalog (Unity's staging
//! flow) is allowed in write-through mode only and lands under `Tables/<schema>/<name>`;
//! dropping through the catalog is refused (Fabric would delete the folder).

use cobalt_auth::CredentialResolver;
use cobalt_core::ProfileId;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

const PREFIX: &str = "/api/2.1/unity-catalog";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LakehouseRef {
    pub name: String,
    pub id: String,
    pub workspace_id: String,
}

#[derive(Default)]
struct LhCache {
    schemas: Option<Vec<String>>,
    /// schema → table names
    tables: HashMap<String, Vec<String>>,
    /// "schema.table" → full table info (Unity shape, normalised)
    infos: HashMap<String, Value>,
}

struct Registry {
    lakehouses: Vec<LakehouseRef>,
    write_mode: String,
    cache: HashMap<String, LhCache>,
}

pub struct CatalogServer {
    pub url: String,
    registry: Arc<Mutex<Registry>>,
    stop: Arc<AtomicBool>,
    pub requests: Arc<AtomicU64>,
    pub upstream_calls: Arc<AtomicU64>,
    pub last_error: Arc<Mutex<Option<String>>>,
}

impl CatalogServer {
    pub fn start(resolver: Arc<CredentialResolver>, slot: ProfileId, tenant: Option<String>, handle: tokio::runtime::Handle, lakehouses: Vec<LakehouseRef>, write_mode: &str) -> std::io::Result<CatalogServer> {
        let server = tiny_http::Server::http("127.0.0.1:0").map_err(|e| std::io::Error::other(e.to_string()))?;
        let port = match server.server_addr() {
            tiny_http::ListenAddr::IP(a) => a.port(),
            #[allow(unreachable_patterns)]
            _ => return Err(std::io::Error::other("no tcp address")),
        };
        let registry = Arc::new(Mutex::new(Registry { lakehouses, write_mode: write_mode.to_string(), cache: HashMap::new() }));
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(AtomicU64::new(0));
        let upstream_calls = Arc::new(AtomicU64::new(0));
        let last_error = Arc::new(Mutex::new(None));
        let (reg2, stop2, req2, up2, err2) = (registry.clone(), stop.clone(), requests.clone(), upstream_calls.clone(), last_error.clone());
        std::thread::Builder::new()
            .name("sail-catalog".into())
            .spawn(move || {
                let up = Upstream { resolver, slot, tenant, handle, calls: up2, last_error: err2 };
                while !stop2.load(Ordering::Relaxed) {
                    let mut req = match server.recv_timeout(Duration::from_millis(300)) {
                        Ok(Some(r)) => r,
                        Ok(None) => continue,
                        Err(_) => break,
                    };
                    req2.fetch_add(1, Ordering::Relaxed);
                    let method = req.method().as_str().to_string();
                    let url = req.url().to_string();
                    let mut body = String::new();
                    let _ = std::io::Read::read_to_string(req.as_reader(), &mut body);
                    let (code, json) = handle_request(&reg2, &up, &method, &url, &body);
                    let _ = req.respond(tiny_http::Response::from_string(json.to_string()).with_status_code(code).with_header(tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap()));
                }
            })
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        Ok(CatalogServer { url: format!("http://127.0.0.1:{port}{PREFIX}"), registry, stop, requests, upstream_calls, last_error })
    }

    /// Attach a lakehouse (Sail's catalog list is told separately; the registry answers for it).
    pub fn register(&self, lh: LakehouseRef) {
        let mut r = self.registry.lock();
        if !r.lakehouses.iter().any(|x| x.id == lh.id) {
            r.lakehouses.push(lh);
        }
    }

    /// Forget what was fetched for one lakehouse (or all), so the next question goes to Fabric.
    pub fn invalidate(&self, lakehouse_id: Option<&str>) {
        let mut r = self.registry.lock();
        match lakehouse_id {
            Some(id) => {
                r.cache.remove(id);
            }
            None => r.cache.clear(),
        }
    }

    pub fn lakehouses(&self) -> Vec<LakehouseRef> {
        self.registry.lock().lakehouses.clone()
    }
}

impl Drop for CatalogServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

struct Upstream {
    resolver: Arc<CredentialResolver>,
    slot: ProfileId,
    tenant: Option<String>,
    handle: tokio::runtime::Handle,
    calls: Arc<AtomicU64>,
    last_error: Arc<Mutex<Option<String>>>,
}

impl Upstream {
    /// The table's Spark schema from its Delta log (the first commits carry `metaData`, a later
    /// one replaces it after an ALTER), as the `fields` of the schema JSON. None when the log
    /// cannot be read; the API's coarse types stay then.
    fn delta_schema(&self, lh: &LakehouseRef, rel: &str) -> Option<Vec<Value>> {
        let token = self.handle.block_on(crate::onelake_tokens::fetch(&self.resolver, self.slot, self.tenant.as_deref())).ok()?;
        let client = cobalt_fabric::OneLakeClient::new(token);
        self.calls.fetch_add(1, Ordering::Relaxed);
        let dir = format!("{}/Tables/{}", lh.id, rel.trim_matches('/'));
        self.handle.block_on(crate::delta_schema::table_fields(&client, &lh.workspace_id, &dir))
    }

    /// GET on Fabric's table API for a lakehouse (the shared client); `Ok(None)` = 404 there.
    fn get(&self, lh: &LakehouseRef, path: &str, query: &[(&str, &str)]) -> Result<Option<Value>, String> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let token = self.handle.block_on(crate::onelake_tokens::fetch(&self.resolver, self.slot, self.tenant.as_deref()))?;
        let api = cobalt_fabric::TableApiClient::new(token);
        match self.handle.block_on(api.get_raw(&lh.workspace_id, &lh.id, path, query)) {
            Ok(v) => Ok(v),
            Err(e) => {
                let msg = format!("Fabric's table API ({path}): {}", crate::fabric::fabric_error_text(&e));
                *self.last_error.lock() = Some(msg.clone());
                Err(msg)
            }
        }
    }
}

fn err(code: u16, error_code: &str, message: impl Into<String>) -> (u16, Value) {
    (code, json!({"error_code": error_code, "message": message.into()}))
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn query_map(q: Option<&str>) -> HashMap<String, String> {
    q.unwrap_or("").split('&').filter_map(|kv| kv.split_once('=')).map(|(k, v)| (percent_decode(k), percent_decode(v))).collect()
}

/// `cat.schema.table` → (catalog, schema, table); the catalog may itself contain dots.
fn split_full_name(full: &str) -> Option<(String, String, String)> {
    let (rest, table) = full.rsplit_once('.')?;
    let (catalog, schema) = rest.rsplit_once('.')?;
    Some((catalog.to_string(), schema.to_string(), table.to_string()))
}

fn find_lakehouse(reg: &Registry, catalog: &str) -> Option<LakehouseRef> {
    reg.lakehouses.iter().find(|l| l.name.eq_ignore_ascii_case(catalog) || l.id == catalog).cloned()
}

/// Unity's spelling of a Spark type name, with decimal precision and scale split out.
pub fn normalize_column(c: &Value, keep_json: bool) -> Value {
    let mut out = c.clone();
    let tn = c.get("type_name").and_then(Value::as_str).unwrap_or("").trim().to_ascii_lowercase();
    let (name, precision, scale): (String, Option<u64>, Option<u64>) = if let Some(rest) = tn.strip_prefix("decimal") {
        let inner = rest.trim().trim_start_matches('(').trim_end_matches(')');
        let mut it = inner.split(',').map(|x| x.trim().parse::<u64>().ok());
        ("DECIMAL".into(), it.next().flatten(), it.next().flatten())
    } else {
        let n = match tn.as_str() {
            "integer" | "int" => "INT",
            "long" | "bigint" => "LONG",
            "short" | "smallint" => "SHORT",
            "byte" | "tinyint" => "BYTE",
            "string" | "varchar" | "char" => "STRING",
            "boolean" => "BOOLEAN",
            "double" => "DOUBLE",
            "float" => "FLOAT",
            "date" => "DATE",
            "timestamp" => "TIMESTAMP",
            "timestamp_ntz" => "TIMESTAMP_NTZ",
            "binary" => "BINARY",
            "null" | "void" => "NULL",
            "variant" => "VARIANT",
            s if s.starts_with("array") => "ARRAY",
            s if s.starts_with("struct") => "STRUCT",
            s if s.starts_with("map") => "MAP",
            s if s.starts_with("interval") => "INTERVAL",
            _ => "",
        };
        (if n.is_empty() { tn.to_ascii_uppercase() } else { n.to_string() }, None, None)
    };
    out["type_name"] = Value::String(name);
    if let Some(p) = precision {
        out["type_precision"] = json!(p);
    }
    if let Some(s) = scale {
        out["type_scale"] = json!(s);
    }
    if out.get("type_text").and_then(Value::as_str).map(str::is_empty).unwrap_or(true) {
        out["type_text"] = Value::String(tn.clone());
    }
    if keep_json && out.get("type_json").and_then(Value::as_str).map(str::is_empty).unwrap_or(true) {
        let field = json!({"name": c.get("name").and_then(Value::as_str).unwrap_or(""), "type": tn, "nullable": c.get("nullable").and_then(Value::as_bool).unwrap_or(true), "metadata": {}});
        out["type_json"] = Value::String(field.to_string());
    }
    out
}

/// A table as Sail wants it: our catalog name, abfss location by ids, normalised columns.
fn normalize_table(t: &Value, lh: &LakehouseRef, schema: &str) -> Value {
    let mut out = t.clone();
    let name = t.get("name").and_then(Value::as_str).unwrap_or("").to_string();
    out["catalog_name"] = Value::String(lh.name.clone());
    out["schema_name"] = Value::String(schema.to_string());
    out["full_name"] = Value::String(format!("{}.{schema}.{name}", lh.name));
    if out.get("table_type").and_then(Value::as_str).is_none() {
        out["table_type"] = Value::String("EXTERNAL".into());
    }
    if out.get("data_source_format").and_then(Value::as_str).is_none() {
        out["data_source_format"] = Value::String("DELTA".into());
    }
    let cols: Vec<Value> = t.get("columns").and_then(Value::as_array).map(|a| a.iter().map(|c| normalize_column(c, true)).collect()).unwrap_or_default();
    out["columns"] = Value::Array(cols);
    let loc = t.get("storage_location").and_then(Value::as_str).unwrap_or("");
    let rel = loc.find("/Tables/").map(|i| &loc[i + "/Tables/".len()..]).unwrap_or("");
    let rel = if rel.is_empty() { format!("{schema}/{name}") } else { rel.to_string() };
    out["storage_location"] = Value::String(table_location(lh, &rel));
    if !out.get("properties").map(Value::is_object).unwrap_or(false) {
        out["properties"] = json!({});
    }
    out
}

fn table_location(lh: &LakehouseRef, rel: &str) -> String {
    format!("abfss://{}@onelake.dfs.fabric.microsoft.com/{}/Tables/{}", lh.workspace_id, lh.id, rel.trim_matches('/'))
}

fn handle_request(registry: &Arc<Mutex<Registry>>, up: &Upstream, method: &str, url: &str, body: &str) -> (u16, Value) {
    let (path, query) = match url.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (url, None),
    };
    // Cobalt's own: the worker asks to list a schema again after creating a table in it
    if method == "POST" && path == "/cobalt/invalidate" {
        let b: Value = serde_json::from_str(body).unwrap_or(Value::Null);
        let cat = b.get("lakehouse").and_then(Value::as_str).unwrap_or("");
        let schema = b.get("schema").and_then(Value::as_str).map(str::to_string);
        let created = b.get("created").and_then(Value::as_str).map(str::to_string);
        let mut r = registry.lock();
        let Some(lh) = find_lakehouse(&r, cat) else { return err(404, "CATALOG_DOES_NOT_EXIST", cat) };
        match (r.cache.get_mut(&lh.id), schema) {
            // a table the worker just created: add it to the listing rather than asking Fabric
            // again (its discovery can lag by tens of seconds); its columns are fetched on first use
            (Some(c), Some(s)) if created.is_some() => {
                let name = created.unwrap();
                c.infos.remove(&format!("{s}.{name}"));
                if let Some(list) = c.tables.get_mut(&s) {
                    if !list.contains(&name) {
                        list.push(name);
                    }
                }
            }
            (Some(c), Some(s)) => {
                c.tables.remove(&s);
                c.infos.retain(|k, _| !k.starts_with(&format!("{s}.")));
            }
            (Some(_), None) => {
                r.cache.remove(&lh.id);
            }
            (None, _) => {}
        }
        return (200, json!({"ok": true}));
    }
    let Some(rel) = path.strip_prefix(PREFIX) else { return err(404, "NOT_FOUND", format!("unknown path {path}")) };
    let q = query_map(query);
    let parts: Vec<String> = rel.trim_matches('/').split('/').map(percent_decode).collect();
    match (method, parts.first().map(String::as_str), parts.get(1)) {
        ("GET", Some("catalogs"), None) => {
            let names: Vec<Value> = registry.lock().lakehouses.iter().map(|l| json!({"name": l.name, "comment": "Fabric lakehouse"})).collect();
            (200, json!({"catalogs": names}))
        }
        ("GET", Some("catalogs"), Some(name)) => match find_lakehouse(&registry.lock(), name) {
            Some(l) => (200, json!({"name": l.name, "comment": "Fabric lakehouse"})),
            None => err(404, "CATALOG_DOES_NOT_EXIST", name.clone()),
        },
        ("GET", Some("schemas"), None) => {
            let cat = q.get("catalog_name").cloned().unwrap_or_default();
            let Some(lh) = find_lakehouse(&registry.lock(), &cat) else { return err(404, "CATALOG_DOES_NOT_EXIST", cat) };
            match schemas(registry, up, &lh) {
                Ok(list) => (200, json!({"schemas": list.iter().map(|s| json!({"name": s, "catalog_name": lh.name, "full_name": format!("{}.{s}", lh.name)})).collect::<Vec<_>>()})),
                Err(e) => err(502, "UPSTREAM", e),
            }
        }
        ("GET", Some("schemas"), Some(full)) => {
            let (cat, schema) = match full.rsplit_once('.') {
                Some((c, s)) => (c.to_string(), s.to_string()),
                None => return err(404, "SCHEMA_DOES_NOT_EXIST", full.clone()),
            };
            let Some(lh) = find_lakehouse(&registry.lock(), &cat) else { return err(404, "CATALOG_DOES_NOT_EXIST", cat) };
            match schemas(registry, up, &lh) {
                Ok(list) if list.iter().any(|s| s.eq_ignore_ascii_case(&schema)) => (200, json!({"name": schema, "catalog_name": lh.name, "full_name": format!("{}.{schema}", lh.name)})),
                Ok(_) => err(404, "SCHEMA_DOES_NOT_EXIST", full.clone()),
                Err(e) => err(502, "UPSTREAM", e),
            }
        }
        ("GET", Some("tables"), None) => {
            let cat = q.get("catalog_name").cloned().unwrap_or_default();
            let schema = q.get("schema_name").cloned().unwrap_or_else(|| "dbo".into());
            let Some(lh) = find_lakehouse(&registry.lock(), &cat) else { return err(404, "CATALOG_DOES_NOT_EXIST", cat) };
            match table_names(registry, up, &lh, &schema) {
                Ok(names) => {
                    // names only: Fabric's listing carries no columns either, and Sail asks per table
                    let tables: Vec<Value> = names.iter().map(|n| json!({"name": n, "catalog_name": lh.name, "schema_name": schema, "full_name": format!("{}.{schema}.{n}", lh.name), "table_type": "EXTERNAL", "data_source_format": "DELTA", "columns": [], "storage_location": table_location(&lh, &format!("{schema}/{n}")), "properties": {}})).collect();
                    (200, json!({"tables": tables}))
                }
                Err(e) => err(502, "UPSTREAM", e),
            }
        }
        ("GET", Some("tables"), Some(full)) => {
            let Some((cat, schema, table)) = split_full_name(full) else { return err(404, "TABLE_DOES_NOT_EXIST", full.clone()) };
            let Some(lh) = find_lakehouse(&registry.lock(), &cat) else { return err(404, "CATALOG_DOES_NOT_EXIST", cat) };
            match table_info(registry, up, &lh, &schema, &table) {
                Ok(Some(info)) => (200, info),
                Ok(None) => err(404, "TABLE_DOES_NOT_EXIST", full.clone()),
                Err(e) => err(502, "UPSTREAM", e),
            }
        }
        ("POST", Some("staging-tables"), None) => {
            let b: Value = serde_json::from_str(body).unwrap_or(Value::Null);
            let cat = b.get("catalog_name").and_then(Value::as_str).unwrap_or("").to_string();
            let schema = b.get("schema_name").and_then(Value::as_str).unwrap_or("dbo").to_string();
            let name = b.get("name").and_then(Value::as_str).unwrap_or("").to_string();
            let (lh, mode) = {
                let r = registry.lock();
                (find_lakehouse(&r, &cat), r.write_mode.clone())
            };
            let Some(lh) = lh else { return err(404, "CATALOG_DOES_NOT_EXIST", cat) };
            if mode != "writethrough" {
                return err(403, "PERMISSION_DENIED", format!("write_mode is '{mode}': creating {}.{schema}.{name} in the lakehouse needs write-through (lakehouse button); LakeSail has no sandbox clones", lh.name));
            }
            if name.is_empty() || name.contains('/') || name.contains('\\') {
                return err(400, "INVALID_PARAMETER_VALUE", "a table name is required");
            }
            (200, json!({"id": uuid::Uuid::new_v4().to_string(), "staging_location": table_location(&lh, &format!("{schema}/{name}"))}))
        }
        ("POST", Some("tables"), None) => {
            let b: Value = serde_json::from_str(body).unwrap_or(Value::Null);
            let cat = b.get("catalog_name").and_then(Value::as_str).unwrap_or("").to_string();
            let schema = b.get("schema_name").and_then(Value::as_str).unwrap_or("dbo").to_string();
            let name = b.get("name").and_then(Value::as_str).unwrap_or("").to_string();
            let (lh, mode) = {
                let r = registry.lock();
                (find_lakehouse(&r, &cat), r.write_mode.clone())
            };
            let Some(lh) = lh else { return err(404, "CATALOG_DOES_NOT_EXIST", cat) };
            if mode != "writethrough" {
                return err(403, "PERMISSION_DENIED", format!("write_mode is '{mode}': creating {}.{schema}.{name} in the lakehouse needs write-through (lakehouse button)", lh.name));
            }
            // the files are already under Tables/<schema>/<name>: Fabric discovers the table; forget our listing
            {
                let mut r = registry.lock();
                if let Some(c) = r.cache.get_mut(&lh.id) {
                    c.tables.remove(&schema);
                    c.infos.remove(&format!("{schema}.{name}"));
                }
            }
            let mut out = normalize_table(&b, &lh, &schema);
            out["name"] = Value::String(name);
            (200, out)
        }
        ("DELETE", Some("tables"), Some(full)) => err(405, "NOT_SUPPORTED", format!("DROP TABLE {full} is not done through the LakeSail catalog (Fabric would delete the lakehouse folder); delete the table in Fabric, or drop it on the Local Spark engine")),
        ("PATCH", Some("tables"), Some(full)) => err(405, "NOT_SUPPORTED", format!("ALTER TABLE {full} is not supported through the LakeSail catalog")),
        _ => err(404, "NOT_FOUND", format!("{method} {path} is not served by Cobalt's lakehouse catalog")),
    }
}

fn schemas(registry: &Arc<Mutex<Registry>>, up: &Upstream, lh: &LakehouseRef) -> Result<Vec<String>, String> {
    if let Some(s) = registry.lock().cache.get(&lh.id).and_then(|c| c.schemas.clone()) {
        return Ok(s);
    }
    let v = up.get(lh, "/schemas", &[("catalog_name", lh.id.as_str())])?.unwrap_or(json!({"schemas": []}));
    let list: Vec<String> = v.get("schemas").and_then(Value::as_array).map(|a| a.iter().filter_map(|s| s.get("name").and_then(Value::as_str).map(str::to_string)).collect()).unwrap_or_default();
    registry.lock().cache.entry(lh.id.clone()).or_default().schemas = Some(list.clone());
    Ok(list)
}

fn table_names(registry: &Arc<Mutex<Registry>>, up: &Upstream, lh: &LakehouseRef, schema: &str) -> Result<Vec<String>, String> {
    if let Some(t) = registry.lock().cache.get(&lh.id).and_then(|c| c.tables.get(schema).cloned()) {
        return Ok(t);
    }
    let v = up.get(lh, "/tables", &[("catalog_name", lh.id.as_str()), ("schema_name", schema)])?.unwrap_or(json!({"tables": []}));
    let list: Vec<String> = v.get("tables").and_then(Value::as_array).map(|a| a.iter().filter_map(|t| t.get("name").and_then(Value::as_str).map(str::to_string)).collect()).unwrap_or_default();
    registry.lock().cache.entry(lh.id.clone()).or_default().tables.insert(schema.to_string(), list.clone());
    Ok(list)
}

fn table_info(registry: &Arc<Mutex<Registry>>, up: &Upstream, lh: &LakehouseRef, schema: &str, table: &str) -> Result<Option<Value>, String> {
    let key = format!("{schema}.{table}");
    if let Some(i) = registry.lock().cache.get(&lh.id).and_then(|c| c.infos.get(&key).cloned()) {
        return Ok(Some(i));
    }
    let Some(v) = up.get(lh, &format!("/tables/{}.{schema}.{table}", lh.id), &[])? else { return Ok(None) };
    let mut info = normalize_table(&v, lh, schema);
    // Fabric's table API says only "struct" / "array" / "map" for nested columns; the Delta log
    // carries the exact Spark schema, which Sail needs as `type_json`
    let rel = info.get("storage_location").and_then(Value::as_str).and_then(|l| l.find("/Tables/").map(|i| l[i + "/Tables/".len()..].to_string())).unwrap_or_else(|| format!("{schema}/{table}"));
    if let Some(fields) = up.delta_schema(lh, &rel) {
        apply_delta_schema(&mut info, &fields);
    }
    registry.lock().cache.entry(lh.id.clone()).or_default().infos.insert(key, info.clone());
    Ok(Some(info))
}

/// Replace the API's column types with the Delta log's Spark fields (matched by name): the
/// Unity type name, `type_text` and a `type_json` with the whole nested type.
fn apply_delta_schema(info: &mut Value, fields: &[Value]) {
    let Some(cols) = info.get_mut("columns").and_then(Value::as_array_mut) else { return };
    for c in cols.iter_mut() {
        let Some(name) = c.get("name").and_then(Value::as_str) else { continue };
        if let Some(f) = fields.iter().find(|f| f.get("name").and_then(Value::as_str) == Some(name)) {
            let ty = f.get("type").cloned().unwrap_or(Value::Null);
            let (unity, text) = unity_type_of(&ty);
            c["type_name"] = Value::String(unity);
            c["type_text"] = Value::String(text);
            c["type_json"] = Value::String(f.to_string());
            if let Some((p, s)) = decimal_parts(&ty) {
                c["type_precision"] = json!(p);
                c["type_scale"] = json!(s);
            }
            if let Some(n) = f.get("nullable").and_then(Value::as_bool) {
                c["nullable"] = Value::Bool(n);
            }
        }
    }
}

fn decimal_parts(ty: &Value) -> Option<(u64, u64)> {
    let s = ty.as_str()?.strip_prefix("decimal(")?.trim_end_matches(')');
    let mut it = s.split(',').map(|x| x.trim().parse::<u64>().ok());
    Some((it.next()??, it.next()??))
}

/// (Unity type name, Spark DDL text) of a Spark JSON type: a string (`"string"`,
/// `"decimal(18,2)"`) or an object (`{"type": "struct" | "array" | "map", …}`).
pub fn unity_type_of(ty: &Value) -> (String, String) {
    match ty {
        Value::String(s) => {
            let c = normalize_column(&json!({"name": "", "type_name": s}), false);
            (c["type_name"].as_str().unwrap_or("").to_string(), s.clone())
        }
        Value::Object(_) => {
            let kind = ty.get("type").and_then(Value::as_str).unwrap_or("struct");
            let text = match kind {
                "array" => format!("array<{}>", unity_type_of(ty.get("elementType").unwrap_or(&Value::Null)).1),
                "map" => format!("map<{},{}>", unity_type_of(ty.get("keyType").unwrap_or(&Value::Null)).1, unity_type_of(ty.get("valueType").unwrap_or(&Value::Null)).1),
                _ => {
                    let inner: Vec<String> = ty.get("fields").and_then(Value::as_array).map(|a| a.iter().map(|f| format!("{}:{}", f.get("name").and_then(Value::as_str).unwrap_or(""), unity_type_of(f.get("type").unwrap_or(&Value::Null)).1)).collect()).unwrap_or_default();
                    format!("struct<{}>", inner.join(","))
                }
            };
            (kind.to_ascii_uppercase(), text)
        }
        _ => ("STRING".into(), "string".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn types_are_normalised() {
        let c = normalize_column(&json!({"name": "amount", "type_name": "decimal(18,2)", "nullable": true}), true);
        assert_eq!(c["type_name"], "DECIMAL");
        assert_eq!(c["type_precision"], 18);
        assert_eq!(c["type_scale"], 2);
        assert_eq!(c["type_text"], "decimal(18,2)");
        assert!(c["type_json"].as_str().unwrap().contains("\"type\":\"decimal(18,2)\""));
        for (spark, unity) in [("integer", "INT"), ("long", "LONG"), ("timestamp_ntz", "TIMESTAMP_NTZ"), ("string", "STRING"), ("array<string>", "ARRAY"), ("struct<a:int>", "STRUCT")] {
            assert_eq!(normalize_column(&json!({"name": "x", "type_name": spark}), false)["type_name"], unity, "{spark}");
        }
    }

    #[test]
    fn table_shape_and_location() {
        let lh = LakehouseRef { name: "test".into(), id: "lh-id".into(), workspace_id: "ws-id".into() };
        let t = normalize_table(&json!({"name": "t", "columns": null, "storage_location": "https://onelake.dfs.fabric.microsoft.com/Fabric test/test.Lakehouse/Tables/dbo/t", "properties": null}), &lh, "dbo");
        assert_eq!(t["full_name"], "test.dbo.t");
        assert_eq!(t["storage_location"], "abfss://ws-id@onelake.dfs.fabric.microsoft.com/lh-id/Tables/dbo/t");
        assert_eq!(t["table_type"], "EXTERNAL");
        assert!(t["columns"].as_array().unwrap().is_empty());
        assert!(t["properties"].is_object());
        assert_eq!(split_full_name("my.lake.dbo.t"), Some(("my.lake".into(), "dbo".into(), "t".into())));
        assert_eq!(percent_decode("test%2Edbo%2Ecobalt%5Fexport"), "test.dbo.cobalt_export");
    }
}
