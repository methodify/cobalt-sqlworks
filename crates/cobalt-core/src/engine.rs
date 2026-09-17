use serde::{Deserialize, Serialize};

/// Which flavor of SQL Server we're talking to. Drives object-explorer folders, plan support, auth UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineKind {
    SqlServer,
    AzureSqlDb,
    AzureSqlMi,
    SynapseDedicated,
    SynapseServerless,
    FabricWarehouse,
    /// Lakehouse SQL analytics endpoint: read-only.
    FabricSqlEndpoint,
    FabricSqlDb,
    SqlEdge,
    Unknown,
}

impl EngineKind {
    /// Map `SERVERPROPERTY('EngineEdition')` + host heuristics.
    /// 1 Personal, 2 Standard, 3 Enterprise, 4 Express, 5 Azure SQL DB, 6 Synapse dedicated,
    /// 8 Azure SQL MI, 9 SQL Edge, 11 Synapse serverless (also Fabric SQL endpoint / Warehouse report 11 or 5).
    pub fn from_edition(engine_edition: i32, host_is_fabric: bool, edition_name: &str) -> Self {
        let e = edition_name.to_ascii_lowercase();
        if host_is_fabric {
            if e.contains("datawarehouse") || e.contains("warehouse") {
                return EngineKind::FabricWarehouse;
            }
            if e.contains("lakehouse") || e.contains("endpoint") {
                return EngineKind::FabricSqlEndpoint;
            }
            // SQL database in Fabric reports EngineEdition 12 and edition "SQL Azure".
            return match engine_edition {
                5 | 12 => EngineKind::FabricSqlDb,
                _ => EngineKind::FabricWarehouse,
            };
        }
        match engine_edition {
            1..=4 => EngineKind::SqlServer,
            5 | 12 => EngineKind::AzureSqlDb,
            6 => EngineKind::SynapseDedicated,
            8 => EngineKind::AzureSqlMi,
            9 => EngineKind::SqlEdge,
            11 => EngineKind::SynapseServerless,
            _ => EngineKind::Unknown,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            EngineKind::SqlServer => "SQL Server",
            EngineKind::AzureSqlDb => "Azure SQL Database",
            EngineKind::AzureSqlMi => "Azure SQL Managed Instance",
            EngineKind::SynapseDedicated => "Synapse dedicated SQL pool",
            EngineKind::SynapseServerless => "Synapse serverless SQL pool",
            EngineKind::FabricWarehouse => "Fabric Warehouse",
            EngineKind::FabricSqlEndpoint => "Fabric SQL analytics endpoint",
            EngineKind::FabricSqlDb => "Fabric SQL database",
            EngineKind::SqlEdge => "Azure SQL Edge",
            EngineKind::Unknown => "SQL Server (unknown edition)",
        }
    }

    pub fn is_fabric(&self) -> bool {
        matches!(self, EngineKind::FabricWarehouse | EngineKind::FabricSqlEndpoint | EngineKind::FabricSqlDb)
    }

    pub fn capabilities(&self) -> Capabilities {
        use EngineKind::*;
        let full = Capabilities {
            actual_plans: true,
            estimated_plans: true,
            procedures: true,
            functions: true,
            sequences: true,
            synonyms: true,
            table_types: true,
            triggers: true,
            transactions: true,
            read_only: false,
            multiple_databases: true,
            sql_login: true,
            temp_tables: true,
            system_databases: true,
            set_xact_abort: true,
            set_statistics: true,
        };
        match self {
            SqlServer | SqlEdge | AzureSqlMi | Unknown => full,
            AzureSqlDb => Capabilities { multiple_databases: false, system_databases: false, ..full },
            SynapseDedicated => Capabilities { actual_plans: false, sequences: false, synonyms: false, triggers: false, multiple_databases: false, system_databases: false, set_statistics: false, ..full },
            SynapseServerless => Capabilities { actual_plans: false, procedures: true, sequences: false, table_types: false, triggers: false, transactions: false, system_databases: false, set_statistics: false, ..full },
            FabricWarehouse => Capabilities { actual_plans: false, sequences: false, synonyms: false, table_types: false, triggers: false, transactions: true, sql_login: false, system_databases: false, set_xact_abort: false, set_statistics: false, ..full },
            FabricSqlEndpoint => Capabilities { actual_plans: false, procedures: false, sequences: false, synonyms: false, table_types: false, triggers: false, transactions: false, read_only: true, sql_login: false, system_databases: false, set_xact_abort: false, set_statistics: false, ..full },
            FabricSqlDb => Capabilities { sql_login: false, multiple_databases: false, system_databases: false, ..full },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    pub actual_plans: bool,
    pub estimated_plans: bool,
    pub procedures: bool,
    pub functions: bool,
    pub sequences: bool,
    pub synonyms: bool,
    pub table_types: bool,
    pub triggers: bool,
    pub transactions: bool,
    pub read_only: bool,
    pub multiple_databases: bool,
    pub sql_login: bool,
    pub temp_tables: bool,
    pub system_databases: bool,
    /// `SET XACT_ABORT` is accepted (Fabric Warehouse / SQL endpoint reject it).
    pub set_xact_abort: bool,
    /// `SET STATISTICS IO/TIME/XML` are accepted (Synapse / Fabric Warehouse reject them).
    pub set_statistics: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EngineInfo {
    pub kind: EngineKind,
    /// e.g. "16.0.4165.4"
    pub version: String,
    pub product_level: String,
    pub edition: String,
    pub server_name: String,
    /// Full `@@VERSION` text.
    pub version_text: String,
    pub capabilities: Capabilities,
}

impl EngineInfo {
    pub fn major_version(&self) -> u32 {
        self.version.split('.').next().and_then(|s| s.parse().ok()).unwrap_or(0)
    }
    pub fn short_label(&self) -> String {
        match self.kind {
            EngineKind::SqlServer => {
                let yr = match self.major_version() {
                    17 => "2025",
                    16 => "2022",
                    15 => "2019",
                    14 => "2017",
                    13 => "2016",
                    12 => "2014",
                    11 => "2012",
                    _ => "",
                };
                if yr.is_empty() { format!("SQL Server {}", self.version) } else { format!("SQL Server {yr}") }
            }
            k => k.label().to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edition_mapping_for_fabric_hosts() {
        assert_eq!(EngineKind::from_edition(12, true, "SQL Azure"), EngineKind::FabricSqlDb);
        assert_eq!(EngineKind::from_edition(5, true, "SQL Azure"), EngineKind::FabricSqlDb);
        assert_eq!(EngineKind::from_edition(11, true, "DataWarehouse"), EngineKind::FabricWarehouse);
        assert_eq!(EngineKind::from_edition(11, true, "Lakehouse SQL Endpoint"), EngineKind::FabricSqlEndpoint);
        assert_eq!(EngineKind::from_edition(12, false, "SQL Azure"), EngineKind::AzureSqlDb);
        assert_eq!(EngineKind::from_edition(3, false, "Developer Edition (64-bit)"), EngineKind::SqlServer);
    }

    #[test]
    fn fabric_warehouse_rejects_xact_abort_and_statistics() {
        let c = EngineKind::FabricWarehouse.capabilities();
        assert!(!c.set_xact_abort && !c.set_statistics && !c.actual_plans && c.estimated_plans);
        let c = EngineKind::FabricSqlDb.capabilities();
        assert!(c.set_xact_abort && c.set_statistics && c.actual_plans);
    }
}
