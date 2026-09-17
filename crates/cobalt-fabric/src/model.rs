//! Typed subset of the Fabric REST models.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    pub id: String,
    #[serde(rename = "displayName")]
    pub display_name: String,
    #[serde(default)]
    pub description: String,
    #[serde(rename = "type", default)]
    pub kind: WorkspaceKind,
    #[serde(rename = "capacityId", default)]
    pub capacity_id: Option<String>,
    #[serde(rename = "capacityRegion", default)]
    pub capacity_region: Option<String>,
    #[serde(rename = "apiEndpoint", default)]
    pub api_endpoint: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum WorkspaceKind {
    Personal,
    #[default]
    Workspace,
    AdminWorkspace,
    #[serde(other)]
    Other,
}

/// Fabric item types Cobalt can open a SQL connection to. Every other `ItemType` is skipped.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SqlItemKind {
    Warehouse,
    /// A lakehouse, reached through its SQL analytics endpoint.
    Lakehouse,
    #[serde(rename = "SQLDatabase")]
    SqlDatabase,
    MirroredDatabase,
    MirroredWarehouse,
    /// Mirrored Azure Databricks / generic mirrored catalogs expose a SQL endpoint too.
    MirroredCatalog,
    WarehouseSnapshot,
    /// The endpoint listed as an item of its own (child of a lakehouse / mirror).
    #[serde(rename = "SQLEndpoint")]
    SqlEndpoint,
}

impl SqlItemKind {
    pub fn parse(item_type: &str) -> Option<Self> {
        Some(match item_type {
            "Warehouse" => Self::Warehouse,
            "Lakehouse" => Self::Lakehouse,
            "SQLDatabase" => Self::SqlDatabase,
            "MirroredDatabase" => Self::MirroredDatabase,
            "MirroredWarehouse" => Self::MirroredWarehouse,
            "MirroredCatalog" | "MirroredAzureDatabricksCatalog" => Self::MirroredCatalog,
            "WarehouseSnapshot" => Self::WarehouseSnapshot,
            "SQLEndpoint" => Self::SqlEndpoint,
            _ => return None,
        })
    }

    /// REST path segment for the item-specific "get" call.
    pub fn detail_segment(&self) -> &'static str {
        match self {
            Self::Warehouse => "warehouses",
            Self::Lakehouse => "lakehouses",
            Self::SqlDatabase => "sqlDatabases",
            Self::MirroredDatabase => "mirroredDatabases",
            Self::MirroredWarehouse => "mirroredWarehouses",
            Self::MirroredCatalog => "mirroredAzureDatabricksCatalogs",
            Self::WarehouseSnapshot => "warehouseSnapshots",
            Self::SqlEndpoint => "sqlEndpoints",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Warehouse => "Warehouse",
            Self::Lakehouse => "Lakehouse (SQL endpoint)",
            Self::SqlDatabase => "SQL database",
            Self::MirroredDatabase => "Mirrored database",
            Self::MirroredWarehouse => "Mirrored warehouse",
            Self::MirroredCatalog => "Mirrored catalog",
            Self::WarehouseSnapshot => "Warehouse snapshot",
            Self::SqlEndpoint => "SQL endpoint",
        }
    }

    /// Items that are the SQL endpoint of another item (listed under their parent, not twice).
    pub fn is_child_endpoint(&self) -> bool {
        matches!(self, Self::SqlEndpoint)
    }
}

/// A SQL-capable item as listed in a workspace (no connection details yet).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SqlItem {
    pub id: String,
    pub workspace_id: String,
    pub display_name: String,
    #[serde(default)]
    pub description: String,
    pub kind: SqlItemKind,
    #[serde(default)]
    pub folder_id: Option<String>,
}

/// What a query tab needs to connect to an item.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SqlTarget {
    /// Host, optionally with `,port` (`xxx.datawarehouse.fabric.microsoft.com` or
    /// `yyy.database.fabric.microsoft.com,1433`).
    pub server: String,
    /// Initial catalog: the item's display name for warehouses/lakehouses/mirrors, the
    /// `<name>-<guid>` database name for SQL database in Fabric.
    pub database: String,
    /// SQL endpoint provisioning state for lakehouse-style items (`Success`, `InProgress`, `Failed`).
    #[serde(default)]
    pub provisioning: Option<String>,
    /// OneLake `Tables` path when the item has one (lakehouses, mirrors).
    #[serde(default)]
    pub onelake_tables_path: Option<String>,
    #[serde(default)]
    pub default_schema: Option<String>,
    #[serde(default)]
    pub collation: Option<String>,
}

impl SqlTarget {
    pub fn is_ready(&self) -> bool {
        self.provisioning.as_deref().map(|p| p.eq_ignore_ascii_case("Success")).unwrap_or(true)
    }
}

/// Item detail: the listing plus its target.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SqlItemDetail {
    pub item: SqlItem,
    pub target: SqlTarget,
}

// --- wire shapes -----------------------------------------------------------------------------

#[derive(Deserialize)]
pub(crate) struct Page<T> {
    #[serde(default = "Vec::new")]
    pub value: Vec<T>,
    #[serde(rename = "continuationToken", default)]
    pub continuation_token: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct WireItem {
    pub id: String,
    #[serde(rename = "workspaceId")]
    pub workspace_id: String,
    #[serde(rename = "displayName")]
    pub display_name: String,
    #[serde(default)]
    pub description: String,
    #[serde(rename = "type")]
    pub item_type: String,
    #[serde(rename = "folderId", default)]
    pub folder_id: Option<String>,
}

impl WireItem {
    pub(crate) fn into_sql_item(self) -> Option<SqlItem> {
        let kind = SqlItemKind::parse(&self.item_type)?;
        Some(SqlItem { id: self.id, workspace_id: self.workspace_id, display_name: self.display_name, description: self.description, kind, folder_id: self.folder_id })
    }
}

#[derive(Deserialize, Default)]
pub(crate) struct WireDetail {
    #[serde(default)]
    pub properties: WireProps,
}

#[derive(Deserialize, Default)]
pub(crate) struct WireProps {
    #[serde(rename = "connectionString", default)]
    pub connection_string: Option<String>,
    #[serde(rename = "serverFqdn", default)]
    pub server_fqdn: Option<String>,
    #[serde(rename = "databaseName", default)]
    pub database_name: Option<String>,
    #[serde(rename = "sqlEndpointProperties", default)]
    pub sql_endpoint: Option<WireSqlEndpoint>,
    #[serde(rename = "oneLakeTablesPath", default)]
    pub onelake_tables_path: Option<String>,
    #[serde(rename = "defaultSchema", default)]
    pub default_schema: Option<String>,
    #[serde(rename = "collationType", default)]
    pub collation_type: Option<String>,
    #[serde(default)]
    pub collation: Option<String>,
}

#[derive(Deserialize, Default)]
pub(crate) struct WireSqlEndpoint {
    #[serde(rename = "connectionString", default)]
    pub connection_string: Option<String>,
    #[serde(rename = "provisioningStatus", default)]
    pub provisioning_status: Option<String>,
}

impl WireDetail {
    /// Map the per-type property bag onto a connection target.
    pub(crate) fn into_target(self, item: &SqlItem) -> Option<SqlTarget> {
        let p = self.properties;
        match item.kind {
            SqlItemKind::SqlDatabase => {
                let server = p.server_fqdn.clone().or_else(|| connection_string_part(p.connection_string.as_deref()?, "Data Source"))?;
                let database = p.database_name.clone().or_else(|| connection_string_part(p.connection_string.as_deref()?, "Initial Catalog"))?;
                Some(SqlTarget { server, database, provisioning: None, onelake_tables_path: None, default_schema: None, collation: p.collation })
            }
            SqlItemKind::Warehouse | SqlItemKind::WarehouseSnapshot => {
                let server = p.connection_string.clone()?;
                Some(SqlTarget { server, database: item.display_name.clone(), provisioning: None, onelake_tables_path: p.onelake_tables_path, default_schema: p.default_schema, collation: p.collation_type })
            }
            SqlItemKind::Lakehouse | SqlItemKind::MirroredDatabase | SqlItemKind::MirroredWarehouse | SqlItemKind::MirroredCatalog | SqlItemKind::SqlEndpoint => {
                let ep = p.sql_endpoint.as_ref();
                let server = ep.and_then(|e| e.connection_string.clone()).or(p.connection_string.clone())?;
                Some(SqlTarget {
                    server,
                    database: item.display_name.clone(),
                    provisioning: ep.and_then(|e| e.provisioning_status.clone()),
                    onelake_tables_path: p.onelake_tables_path,
                    default_schema: p.default_schema,
                    collation: p.collation_type,
                })
            }
        }
    }
}

/// `Data Source=host,1433;Initial Catalog=db;…` → value for `key` (case-insensitive).
pub fn connection_string_part(cs: &str, key: &str) -> Option<String> {
    cs.split(';').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        if k.trim().eq_ignore_ascii_case(key) { Some(v.trim().to_string()) } else { None }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(kind: SqlItemKind, name: &str) -> SqlItem {
        SqlItem { id: "i".into(), workspace_id: "w".into(), display_name: name.into(), description: String::new(), kind, folder_id: None }
    }

    #[test]
    fn warehouse_target_uses_connection_string_and_name() {
        let d: WireDetail = serde_json::from_str(r#"{"properties":{"connectionString":"abc.datawarehouse.fabric.microsoft.com","collationType":"Latin1_General_100_BIN2_UTF8"}}"#).unwrap();
        let t = d.into_target(&item(SqlItemKind::Warehouse, "Sales_DW")).unwrap();
        assert_eq!(t.server, "abc.datawarehouse.fabric.microsoft.com");
        assert_eq!(t.database, "Sales_DW");
        assert!(t.is_ready());
    }

    #[test]
    fn lakehouse_target_reads_sql_endpoint_and_status() {
        let d: WireDetail = serde_json::from_str(r#"{"properties":{"oneLakeTablesPath":"https://onelake.dfs.fabric.microsoft.com/w/i/Tables","sqlEndpointProperties":{"connectionString":"abc.datawarehouse.fabric.microsoft.com","id":"e","provisioningStatus":"InProgress"},"defaultSchema":"dbo"}}"#).unwrap();
        let t = d.into_target(&item(SqlItemKind::Lakehouse, "lh_bronze")).unwrap();
        assert_eq!(t.database, "lh_bronze");
        assert!(!t.is_ready());
        assert_eq!(t.default_schema.as_deref(), Some("dbo"));
        assert!(t.onelake_tables_path.unwrap().ends_with("/Tables"));
    }

    #[test]
    fn sql_database_target_uses_fqdn_and_database_name() {
        let d: WireDetail = serde_json::from_str(r#"{"properties":{"connectionString":"Data Source=x.database.fabric.microsoft.com,1433;Initial Catalog=SQLDatabase1-45c6;Encrypt=True","databaseName":"SQLDatabase1-45c6","serverFqdn":"x.database.fabric.microsoft.com,1433","collation":"Albanian_CI_AI_WS"}}"#).unwrap();
        let t = d.into_target(&item(SqlItemKind::SqlDatabase, "SQLDatabase1")).unwrap();
        assert_eq!(t.server, "x.database.fabric.microsoft.com,1433");
        assert_eq!(t.database, "SQLDatabase1-45c6");
    }

    #[test]
    fn sql_database_falls_back_to_connection_string_parts() {
        let d: WireDetail = serde_json::from_str(r#"{"properties":{"connectionString":"Data Source=h,1433;Initial Catalog=db-1;Encrypt=True"}}"#).unwrap();
        let t = d.into_target(&item(SqlItemKind::SqlDatabase, "n")).unwrap();
        assert_eq!((t.server.as_str(), t.database.as_str()), ("h,1433", "db-1"));
    }

    #[test]
    fn non_sql_items_are_skipped() {
        let w: WireItem = serde_json::from_str(r#"{"id":"1","workspaceId":"w","displayName":"nb","type":"Notebook"}"#).unwrap();
        assert!(w.into_sql_item().is_none());
        let w: WireItem = serde_json::from_str(r#"{"id":"1","workspaceId":"w","displayName":"db","type":"SQLDatabase"}"#).unwrap();
        assert_eq!(w.into_sql_item().unwrap().kind, SqlItemKind::SqlDatabase);
    }
}
