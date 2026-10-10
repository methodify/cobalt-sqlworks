//! The current Spark schema of a Delta table on OneLake, read from its log the way a reader
//! would: the newest commits first (a `metaData` action is written at creation and after every
//! schema change), then the last checkpoint's `metaData` row when log cleanup has removed the
//! early commits (the usual state of a table older than a month). Shared by the LakeSail
//! catalog endpoint (Sail needs the whole nested type of every column) and the completion
//! catalog.

use cobalt_fabric::OneLakeClient;
use serde_json::Value;

/// The `fields` of the table's schema JSON (`StructType` as Spark serialises it), or None when
/// the log cannot be read. `table_dir` is `<lakehouse-id>/Tables/<schema>/<name>` (or without
/// the schema for a plain lakehouse).
pub async fn table_fields(client: &OneLakeClient, workspace_id: &str, table_dir: &str) -> Option<Vec<Value>> {
    let log_dir = format!("{}/_delta_log", table_dir.trim_matches('/'));
    let entries = client.list_dir(workspace_id, &log_dir).await.ok()?;
    let mut commits: Vec<u64> = entries.iter().filter_map(|e| e.name.strip_suffix(".json").and_then(|n| n.parse::<u64>().ok())).collect();
    commits.sort_unstable();
    let checkpoint: Option<u64> = entries.iter().filter_map(|e| e.name.split(".checkpoint").next().and_then(|n| n.parse::<u64>().ok()).filter(|_| e.name.contains(".checkpoint"))).max();
    // newest commits first, down to the checkpoint (what came before it is folded into it)
    let floor = checkpoint.unwrap_or(0);
    for v in commits.iter().rev().take(40) {
        if *v < floor {
            break;
        }
        let path = format!("{log_dir}/{v:020}.json");
        let Ok(text) = client.read_text(workspace_id, &path).await else { continue };
        if let Some(f) = fields_from_commit(&text) {
            return Some(f);
        }
    }
    if let Some(v) = checkpoint {
        // single-part `N.checkpoint.parquet`, or the parts of a multi-part checkpoint
        let prefix = format!("{v:020}.checkpoint");
        let mut parts: Vec<&str> = entries.iter().filter(|e| e.name.starts_with(&prefix) && e.name.ends_with(".parquet")).map(|e| e.name.as_str()).collect();
        parts.sort_unstable();
        for name in parts {
            let Ok(bytes) = client.read_bytes(workspace_id, &format!("{log_dir}/{name}")).await else { continue };
            if let Some(f) = fields_from_checkpoint(bytes) {
                return Some(f);
            }
        }
    }
    None
}

/// The last `metaData.schemaString` in a commit file's actions.
pub fn fields_from_commit(text: &str) -> Option<Vec<Value>> {
    let mut found = None;
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let Some(schema) = v.get("metaData").and_then(|m| m.get("schemaString")).and_then(Value::as_str) else { continue };
        if let Ok(sch) = serde_json::from_str::<Value>(schema) {
            found = sch.get("fields").and_then(Value::as_array).cloned();
        }
    }
    found
}

/// The `metaData` row of a checkpoint parquet: only that column is decoded.
pub fn fields_from_checkpoint(bytes: Vec<u8>) -> Option<Vec<Value>> {
    use arrow::array::{Array, StringArray, StructArray};
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    use parquet::arrow::ProjectionMask;

    let builder = ParquetRecordBatchReaderBuilder::try_new(bytes::Bytes::from(bytes)).ok()?;
    let idx = builder.parquet_schema().root_schema().get_fields().iter().position(|f| f.name() == "metaData")?;
    let mask = ProjectionMask::roots(builder.parquet_schema(), [idx]);
    let reader = builder.with_projection(mask).with_batch_size(1024).build().ok()?;
    for batch in reader.flatten() {
        let col = batch.column_by_name("metaData")?;
        let st = col.as_any().downcast_ref::<StructArray>()?;
        let schema_col = st.column_by_name("schemaString")?;
        let strings = schema_col.as_any().downcast_ref::<StringArray>()?;
        for i in 0..strings.len() {
            if !st.is_null(i) && !strings.is_null(i) {
                if let Ok(sch) = serde_json::from_str::<Value>(strings.value(i)) {
                    if let Some(f) = sch.get("fields").and_then(Value::as_array) {
                        return Some(f.clone());
                    }
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_schema_is_the_last_metadata() {
        let text = r#"{"commitInfo":{"operation":"WRITE"}}
{"metaData":{"id":"a","schemaString":"{\"type\":\"struct\",\"fields\":[{\"name\":\"x\",\"type\":\"integer\",\"nullable\":true,\"metadata\":{}}]}"}}
{"metaData":{"id":"a","schemaString":"{\"type\":\"struct\",\"fields\":[{\"name\":\"x\",\"type\":\"integer\",\"nullable\":true,\"metadata\":{}},{\"name\":\"o\",\"type\":{\"type\":\"struct\",\"fields\":[{\"name\":\"c\",\"type\":\"string\",\"nullable\":true,\"metadata\":{}}]},\"nullable\":true,\"metadata\":{}}]}"}}"#;
        let f = fields_from_commit(text).unwrap();
        assert_eq!(f.len(), 2);
        assert_eq!(f[1]["type"]["type"], "struct");
        assert!(fields_from_commit("{\"add\":{}}").is_none());
    }
}
