//! SQL Server / Azure SQL / Fabric implementation over `tiberius-ng`.
//!
//! STATUS: stub — replaced by the driver implementation milestone (M1).

use crate::*;
use async_trait::async_trait;

pub struct MssqlDriver;

impl MssqlDriver {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MssqlDriver {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Driver for MssqlDriver {
    fn name(&self) -> &'static str {
        "SQL Server"
    }

    async fn connect(
        &self,
        _profile: &ConnectionProfile,
        _creds: &ResolvedCredentials,
        _role: ConnectionRole,
    ) -> Result<Box<dyn Connection>> {
        Err(DriverError::Unsupported("mssql driver not implemented yet"))
    }
}
