//! Fabric's OneLake table API: the lakehouse's own table listing with column types, served at
//! `https://onelake.table.fabric.microsoft.com/delta/<workspace>/<lakehouse>/api/2.1/unity-catalog`
//! (Unity-Catalog-compatible, the storage-scope bearer token). One call lists the schemas, one
//! per schema lists its tables, one per table gives its columns — against the DFS crawl's one
//! listing per top-level folder plus a Delta-log read per table. Shared by the Lakehouse pane,
//! the Spark SQL completion catalog and the LakeSail catalog endpoint.
//!
//! What the API says and what it means here: a plain (non-schema) lakehouse is listed under a
//! single schema `dbo` although its tables live at `Tables/<name>`; `storage_location` on a
//! table carries the real path, so [`TableApiClient::list_tables`] reads one table per schema to
//! tell the two layouts apart and reports `schema: None` for a plain lakehouse (the DFS reader's
//! convention, which every caller already follows). Listings carry `columns: null`; the single
//! GET has the columns with lowercase Spark type names (`integer`, `decimal(18,2)`, and just
//! `struct` / `array` / `map` for nested ones).

use crate::client::{FabricError, Result};
use crate::onelake::OneLakeTable;
use serde_json::Value;
use std::time::Duration;

pub const TABLE_API: &str = "https://onelake.table.fabric.microsoft.com/delta";
const PREFIX: &str = "/api/2.1/unity-catalog";

#[derive(Clone)]
pub struct TableApiClient {
    http: reqwest::Client,
    base: String,
    token: String,
}

/// One column of a table as the API lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableColumn {
    pub name: String,
    /// Spark's spelling, lowercase (`string`, `bigint`, `decimal(18,2)`, `struct`).
    pub type_name: String,
    pub nullable: bool,
}

/// A table with its columns (the single-table GET).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableDetail {
    pub schema: Option<String>,
    pub name: String,
    /// Path below `Tables/` (`schema/name` or `name`), from `storage_location`.
    pub rel_path: String,
    pub columns: Vec<TableColumn>,
}

impl TableApiClient {
    pub fn new(token: impl Into<String>) -> Self {
        Self::with_base(TABLE_API, token)
    }

    pub fn with_base(base: impl Into<String>, token: impl Into<String>) -> Self {
        let http = reqwest::Client::builder().user_agent("cobalt-sqlworks").timeout(Duration::from_secs(30)).build().expect("reqwest client");
        Self { http, base: base.into().trim_end_matches('/').to_string(), token: token.into() }
    }

    /// GET a path under the lakehouse's Unity prefix; `Ok(None)` is a 404.
    pub async fn get_raw(&self, workspace_id: &str, lakehouse_id: &str, path: &str, query: &[(&str, &str)]) -> Result<Option<Value>> {
        let url = format!("{}/{workspace_id}/{lakehouse_id}{PREFIX}{path}", self.base);
        let resp = self.http.get(&url).query(query).bearer_auth(&self.token).send().await?;
        let status = resp.status();
        let body = resp.text().await?;
        if status.as_u16() == 404 {
            return Ok(None);
        }
        if status.as_u16() == 401 {
            return Err(FabricError::Unauthorized(401));
        }
        if !status.is_success() {
            let v: Value = serde_json::from_str(&body).unwrap_or_default();
            let code = v.get("error_code").and_then(Value::as_str).or_else(|| v.pointer("/error/code").and_then(Value::as_str)).unwrap_or("").to_string();
            let message = v.get("message").and_then(Value::as_str).or_else(|| v.pointer("/error/message").and_then(Value::as_str)).unwrap_or(&body).lines().next().unwrap_or("").chars().take(300).collect();
            return Err(FabricError::Api { status: status.as_u16(), code, message });
        }
        serde_json::from_str::<Value>(&body).map(Some).map_err(|e| FabricError::Decode(format!("table API: {e}")))
    }

    /// The schema names of a lakehouse (`dbo` alone for a plain lakehouse).
    pub async fn list_schemas(&self, workspace_id: &str, lakehouse_id: &str) -> Result<Vec<String>> {
        let v = self.get_raw(workspace_id, lakehouse_id, "/schemas", &[("catalog_name", lakehouse_id)]).await?.unwrap_or(Value::Null);
        Ok(schema_names(&v))
    }

    /// The table names of one schema (names only: the listing carries no columns).
    pub async fn list_table_names(&self, workspace_id: &str, lakehouse_id: &str, schema: &str) -> Result<Vec<String>> {
        let v = self.get_raw(workspace_id, lakehouse_id, "/tables", &[("catalog_name", lakehouse_id), ("schema_name", schema)]).await?.unwrap_or(Value::Null);
        Ok(table_names(&v))
    }

    /// One table with its columns; `Ok(None)` when the API does not know it.
    pub async fn get_table(&self, workspace_id: &str, lakehouse_id: &str, schema: &str, table: &str) -> Result<Option<TableDetail>> {
        let Some(v) = self.get_raw(workspace_id, lakehouse_id, &format!("/tables/{lakehouse_id}.{schema}.{table}"), &[]).await? else { return Ok(None) };
        Ok(Some(table_detail(&v, schema, table)))
    }

    /// Every table of the lakehouse as the DFS reader reports them: `schema: None` for a plain
    /// lakehouse (one GET on its first table tells), `Some(schema)` otherwise; sorted by path.
    pub async fn list_tables(&self, workspace_id: &str, lakehouse_id: &str) -> Result<Vec<OneLakeTable>> {
        let mut out = Vec::new();
        let mut plain: Option<bool> = None;
        for schema in self.list_schemas(workspace_id, lakehouse_id).await? {
            let names = self.list_table_names(workspace_id, lakehouse_id, &schema).await?;
            if names.is_empty() {
                continue;
            }
            if plain.is_none() {
                plain = Some(match self.get_table(workspace_id, lakehouse_id, &schema, &names[0]).await? {
                    Some(d) => d.schema.is_none(),
                    None => false,
                });
            }
            for name in names {
                out.push(OneLakeTable { schema: if plain == Some(true) { None } else { Some(schema.clone()) }, name });
            }
        }
        out.sort_by_key(|a| a.rel_path().to_lowercase());
        Ok(out)
    }
}

fn schema_names(v: &Value) -> Vec<String> {
    v.get("schemas").and_then(Value::as_array).map(|a| a.iter().filter_map(|s| s.get("name").and_then(Value::as_str).map(str::to_string)).collect()).unwrap_or_default()
}

fn table_names(v: &Value) -> Vec<String> {
    v.get("tables").and_then(Value::as_array).map(|a| a.iter().filter_map(|t| t.get("name").and_then(Value::as_str).map(str::to_string)).collect()).unwrap_or_default()
}

/// The table GET as a [`TableDetail`]: the path below `Tables/` from `storage_location` (the
/// listed schema when the location says nothing), columns with Spark's type names.
pub fn table_detail(v: &Value, schema: &str, table: &str) -> TableDetail {
    let name = v.get("name").and_then(Value::as_str).unwrap_or(table).to_string();
    let loc = v.get("storage_location").and_then(Value::as_str).unwrap_or("");
    let rel = loc.find("/Tables/").map(|i| loc[i + "/Tables/".len()..].trim_matches('/').to_string()).filter(|r| !r.is_empty()).unwrap_or_else(|| format!("{schema}/{name}"));
    let schema_out = rel.rsplit_once('/').map(|(s, _)| s.to_string());
    let columns = v
        .get("columns")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|c| {
                    let name = c.get("name").and_then(Value::as_str)?.to_string();
                    let type_name = c.get("type_name").and_then(Value::as_str).or_else(|| c.get("type_text").and_then(Value::as_str)).unwrap_or("").trim().to_ascii_lowercase();
                    Some(TableColumn { name, type_name, nullable: c.get("nullable").and_then(Value::as_bool).unwrap_or(true) })
                })
                .collect()
        })
        .unwrap_or_default();
    TableDetail { schema: schema_out, name, rel_path: rel, columns }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_listings() {
        assert_eq!(schema_names(&json!({"schemas": [{"name": "dbo"}, {"name": "sales"}]})), vec!["dbo", "sales"]);
        assert_eq!(table_names(&json!({"tables": [{"name": "a", "columns": null}, {"name": "b"}]})), vec!["a", "b"]);
        assert!(schema_names(&Value::Null).is_empty());
    }

    #[test]
    fn table_detail_tells_plain_from_schema_layout() {
        let schema_enabled = json!({"name": "t", "storage_location": "abfss://ws@onelake.dfs.fabric.microsoft.com/lh/Tables/dbo/t",
            "columns": [{"name": "id", "type_name": "integer", "nullable": false}, {"name": "amount", "type_name": "decimal(18,2)"}, {"name": "o", "type_name": "struct"}]});
        let d = table_detail(&schema_enabled, "dbo", "t");
        assert_eq!(d.schema.as_deref(), Some("dbo"));
        assert_eq!(d.rel_path, "dbo/t");
        assert_eq!(d.columns.len(), 3);
        assert_eq!(d.columns[0], TableColumn { name: "id".into(), type_name: "integer".into(), nullable: false });
        assert_eq!(d.columns[1].type_name, "decimal(18,2)");
        let plain = json!({"name": "t", "storage_location": "https://onelake.dfs.fabric.microsoft.com/ws/lh/Tables/t", "columns": null});
        let d = table_detail(&plain, "dbo", "t");
        assert_eq!(d.schema, None);
        assert_eq!(d.rel_path, "t");
        assert!(d.columns.is_empty());
        // no location: the listed schema stands
        let d = table_detail(&json!({"name": "t"}), "s", "t");
        assert_eq!(d.rel_path, "s/t");
        assert_eq!(d.schema.as_deref(), Some("s"));
    }
}
