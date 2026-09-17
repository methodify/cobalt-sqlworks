//! Cloud targets for the Delta and Parquet exports: OneLake (Microsoft Fabric) today, ADLS/S3
//! later. Delta tables go through delta-rs's object-store layer; Parquet files are uploaded
//! whole after being written locally.

use crate::{DeltaError, DeltaOptions, DeltaTableBuilder, Result};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Once;


/// Where a remote export lands.
#[derive(Clone, Debug)]
pub struct RemoteTarget {
    /// `abfss://<workspace-id>@onelake.dfs.fabric.microsoft.com/<lakehouse-id>/Tables/<table>` etc.
    pub url: url::Url,
    /// object_store options (`bearer_token`, `use_fabric_endpoint`, …).
    pub storage_options: HashMap<String, String>,
}

impl RemoteTarget {
    /// A OneLake location inside a lakehouse (`Tables/<name>` for Delta, `Files/<name>` for files).
    pub fn onelake(workspace_id: &str, lakehouse_id: &str, relative: &str, bearer_token: &str) -> Result<Self> {
        let rel = relative.trim_matches('/');
        let url = url::Url::parse(&format!("abfss://{workspace_id}@onelake.dfs.fabric.microsoft.com/{lakehouse_id}/{rel}")).map_err(|e| DeltaError::InvalidPath(e.to_string()))?;
        let mut storage_options = HashMap::new();
        storage_options.insert("bearer_token".to_string(), bearer_token.to_string());
        storage_options.insert("use_fabric_endpoint".to_string(), "true".to_string());
        Ok(Self { url, storage_options })
    }

    /// `onelake.dfs.fabric.microsoft.com/<ws>/<lh>/Tables/<name>` — the path people recognise.
    pub fn display(&self) -> String {
        format!("{}{}", self.url.host_str().unwrap_or(""), self.url.path())
    }
}

static REGISTER: Once = Once::new();

/// Make delta-rs understand `abfss://` / `az://` URLs. Idempotent.
pub fn register_cloud_handlers() {
    REGISTER.call_once(|| {
        deltalake::azure::register_handlers(None);
    });
}

/// Whether a Delta table already exists at the target.
pub async fn remote_table_exists(target: &RemoteTarget) -> Result<bool> {
    register_cloud_handlers();
    match DeltaTableBuilder::from_url(target.url.clone())?.with_storage_options(target.storage_options.clone()).load().await {
        Ok(_) => Ok(true),
        Err(deltalake::DeltaTableError::NotATable(_)) => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// Write the visible rows of `rs` as a Delta table at the remote target.
pub async fn write_delta_remote(rs: &cobalt_results::ResultSet, target: &RemoteTarget, opts: &DeltaOptions, progress: &mut (dyn FnMut(crate::Progress) -> bool + Send)) -> Result<crate::ExportStats> {
    register_cloud_handlers();
    let started = std::time::Instant::now();
    let is_table = remote_table_exists(target).await?;
    if opts.mode == crate::DeltaMode::Create && is_table {
        return Err(DeltaError::Exists(std::path::PathBuf::from(target.display())));
    }
    for p in &opts.partition_columns {
        if !rs.schema.fields().iter().any(|f| f.name() == p) {
            return Err(DeltaError::UnknownPartitionColumn(p.clone()));
        }
    }
    let schema: arrow::datatypes::SchemaRef = std::sync::Arc::new(crate::delta_compatible_schema(&rs.schema));
    let kernel_schema: deltalake::kernel::StructType = deltalake::kernel::engine::arrow_conversion::TryIntoKernel::try_into_kernel(schema.as_ref())?;
    let (rows, bytes) = crate::write_inner(rs, &target.url, Some(&target.storage_options), opts, is_table, schema, kernel_schema, progress).await?;
    tracing::info!(target = %target.display(), rows, bytes, mode = ?opts.mode, "remote delta export complete");
    Ok(crate::ExportStats { rows, bytes, elapsed: started.elapsed(), path: std::path::PathBuf::from(target.display()), warnings: Vec::new() })
}

pub fn write_delta_remote_blocking(rs: &cobalt_results::ResultSet, target: &RemoteTarget, opts: &DeltaOptions, progress: &mut (dyn FnMut(crate::Progress) -> bool + Send)) -> Result<crate::ExportStats> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| DeltaError::Io(e))?;
    rt.block_on(write_delta_remote(rs, target, opts, progress))
}

/// Upload a local file (e.g. a Parquet file just written) to the remote target as one object.
pub async fn upload_file(target: &RemoteTarget, local: &Path) -> Result<u64> {
    register_cloud_handlers();
    let bytes = tokio::fs::read(local).await.map_err(DeltaError::Io)?;
    let len = bytes.len() as u64;
    let (store, prefix) = deltalake::logstore::object_store_factories()
        .get(&url::Url::parse(&format!("{}://", target.url.scheme())).map_err(|e| DeltaError::InvalidPath(e.to_string()))?)
        .ok_or_else(|| DeltaError::InvalidPath(format!("no object store for {}", target.url.scheme())))?
        .parse_url_opts(&target.url, &deltalake::logstore::StorageConfig::parse_options(target.storage_options.clone())?)?;
    use deltalake::logstore::object_store::ObjectStoreExt;
    store.put(&prefix, bytes.into()).await.map_err(|e| DeltaError::Delta(e.into()))?;
    Ok(len)
}

pub fn upload_file_blocking(target: &RemoteTarget, local: &Path) -> Result<u64> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| DeltaError::Io(e))?;
    rt.block_on(upload_file(target, local))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn onelake_target_urls() {
        let t = RemoteTarget::onelake("ws-1", "lh-2", "Tables/sales", "tok").unwrap();
        assert_eq!(t.url.as_str(), "abfss://ws-1@onelake.dfs.fabric.microsoft.com/lh-2/Tables/sales");
        assert_eq!(t.display(), "onelake.dfs.fabric.microsoft.com/lh-2/Tables/sales");
        assert_eq!(t.storage_options["use_fabric_endpoint"], "true");
        assert_eq!(t.storage_options["bearer_token"], "tok");
        let f = RemoteTarget::onelake("ws-1", "lh-2", "/Files/out.parquet", "tok").unwrap();
        assert!(f.url.path().ends_with("/Files/out.parquet"));
    }
}
