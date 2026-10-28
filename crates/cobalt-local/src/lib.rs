//! The local query engine: DataFusion in-process over the app's own data — result sets of open
//! tabs (`provider`), files and Delta folders (slice B), scratch and lakehouse tables (slice D).
//! Design: `docs/design/local_query.md`.

pub mod diff;
pub mod engine;
pub mod explore;
pub mod provider;
pub mod sqlite_provider;
pub mod sugar;

pub use engine::{columns_for, conform, quote_ident, quote_table, sanitize_name, sql_type_for, table_ref, Engine, TableInfo, TableSource, ENGINE_VERSION, LATEST, SCRATCH_PREFIX, SCRATCH_SCHEMA};
pub use provider::ResultSetProvider;

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::{Int32Array, StringArray};
    use arrow::datatypes::{DataType, Field, Schema};
    use arrow::record_batch::RecordBatch;
    use cobalt_core::{ColumnInfo, SqlType};
    use cobalt_results::ResultSet;
    use futures_util::StreamExt;
    use std::sync::Arc;

    fn result(index: usize, rows: i32) -> Arc<ResultSet> {
        let columns = vec![ColumnInfo::new("id", SqlType::Int, true, 0), ColumnInfo::new("name", SqlType::NVarChar { len: Some(50) }, true, 1)];
        let schema = Arc::new(Schema::new(vec![Field::new("id", DataType::Int32, true), Field::new("name", DataType::Utf8, true)]));
        let mut batches = Vec::new();
        for start in (0..rows).step_by(3) {
            let ids: Vec<i32> = (start..(start + 3).min(rows)).collect();
            let names: Vec<String> = ids.iter().map(|i| format!("n{i}")).collect();
            batches.push(RecordBatch::try_new(Arc::clone(&schema), vec![Arc::new(Int32Array::from(ids)), Arc::new(StringArray::from(names))]).unwrap());
        }
        ResultSet::from_batches(index, columns, batches)
    }

    #[tokio::test]
    async fn queries_join_and_aggregate_over_results() {
        let e = Engine::new(Arc::new(cobalt_results::MemoryBudget::unlimited()), 256 << 20, std::env::temp_dir().join("cobalt-local-test")).unwrap();
        e.register_result("prod", result(0, 10), TableSource::Result { tab: "1".into(), set: 0, title: "prod".into() });
        e.register_result("dev", result(0, 7), TableSource::Result { tab: "2".into(), set: 0, title: "dev".into() });
        let df = e.sql("SELECT count(*) AS n FROM prod p JOIN dev d ON p.id = d.id").await.unwrap();
        let rows = df.collect().await.unwrap();
        let n = rows[0].column(0).as_any().downcast_ref::<arrow::array::Int64Array>().unwrap().value(0);
        assert_eq!(n, 7);
        // `_` is the latest registration
        let df = e.sql("SELECT count(*) FROM _").await.unwrap();
        let rows = df.collect().await.unwrap();
        assert_eq!(rows[0].column(0).as_any().downcast_ref::<arrow::array::Int64Array>().unwrap().value(0), 7);
        // streaming scan with a projection and limit
        let df = e.sql("SELECT name FROM prod LIMIT 4").await.unwrap();
        let mut s = df.execute_stream().await.unwrap();
        let mut total = 0;
        while let Some(b) = s.next().await {
            total += b.unwrap().num_rows();
        }
        assert_eq!(total, 4);
        assert!(e.functions().iter().any(|f| f == "date_trunc"));
        let plan = e.explain("SELECT * FROM prod").await.unwrap();
        assert!(plan.contains("ResultSetExec"));
        // rename, deregister by tab
        e.rename("prod", "Prod Orders").unwrap();
        assert!(e.has_table("prod_orders") && !e.has_table("prod"));
        e.deregister_tab("2");
        assert!(!e.has_table("dev"));
        assert_eq!(e.tables().len(), 1);
    }

    #[tokio::test]
    async fn files_and_delta_and_statements_are_tables() {
        let dir = std::env::temp_dir().join(format!("cobalt-local-files-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let e = Engine::new(Arc::new(cobalt_results::MemoryBudget::unlimited()), 256 << 20, dir.join("spill")).unwrap();
        // CSV with a sniffed semicolon delimiter
        let csv = dir.join("sales.csv");
        std::fs::write(&csv, "id;name;amount\n1;Ann;10.5\n2;Bob;20\n3;Cy;7\n").unwrap();
        let info = e.register_file("sales", &csv).await.unwrap();
        assert_eq!(info.columns.len(), 3);
        assert!(!info.rows_known);
        let n = e.sql("SELECT sum(amount) FROM sales").await.unwrap().collect().await.unwrap();
        assert_eq!(n[0].column(0).as_any().downcast_ref::<arrow::array::Float64Array>().unwrap().value(0), 37.5);
        // Parquet, with the footer's row count
        let schema = Arc::new(Schema::new(vec![Field::new("id", DataType::Int32, true), Field::new("name", DataType::Utf8, true)]));
        let batch = RecordBatch::try_new(Arc::clone(&schema), vec![Arc::new(Int32Array::from(vec![1, 2, 3, 4])), Arc::new(StringArray::from(vec!["a", "b", "c", "d"]))]).unwrap();
        let pq = dir.join("rows.parquet");
        {
            let f = std::fs::File::create(&pq).unwrap();
            let mut w = datafusion::parquet::arrow::ArrowWriter::try_new(f, Arc::clone(&schema), None).unwrap();
            w.write(&batch).unwrap();
            w.close().unwrap();
        }
        let info = e.register_file("rows", &pq).await.unwrap();
        assert!(info.rows_known && info.rows == 4);
        assert_eq!(e.latest().as_deref(), Some("rows"));
        // a Delta table written by delta-rs
        let delta_dir = dir.join("dt");
        std::fs::create_dir_all(&delta_dir).unwrap();
        let url = url::Url::from_directory_path(&delta_dir).unwrap();
        #[allow(deprecated)]
        deltalake::DeltaOps::try_from_url(url).await.unwrap().write(vec![batch.clone()]).await.unwrap();
        let info = e.register_file("dt", &delta_dir).await.unwrap();
        assert_eq!(info.columns.len(), 2);
        let n = e.sql("SELECT count(*) FROM dt JOIN rows USING (id)").await.unwrap().collect().await.unwrap();
        assert_eq!(n[0].column(0).as_any().downcast_ref::<arrow::array::Int64Array>().unwrap().value(0), 4);
        // a table made by a statement shows up after a sync
        e.sql("CREATE TABLE made AS SELECT id * 10 AS x FROM rows").await.unwrap().collect().await.unwrap();
        e.sync_from_context().await;
        let t = e.tables();
        assert!(t.iter().any(|t| t.name == "made" && t.source == TableSource::Statement));
        // a folder without a log is refused
        assert!(e.register_file("nope", &dir).await.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn scratch_tables_persist() {
        let dir = std::env::temp_dir().join(format!("cobalt-local-scratch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let e = Engine::new(Arc::new(cobalt_results::MemoryBudget::unlimited()), 256 << 20, dir.join("spill")).unwrap();
        assert_eq!(e.load_scratch(dir.join("scratch")).await, 0);
        e.register_result("prod", result(0, 9), TableSource::Result { tab: "1".into(), set: 0, title: "prod".into() });
        let info = e.save_scratch("Prod Snapshot", result(0, 9)).await.unwrap();
        assert_eq!(info.name, "scratch.prod_snapshot");
        assert!(info.rows_known && info.rows == 9);
        let n = e.sql("SELECT count(*) FROM scratch.prod_snapshot").await.unwrap().collect().await.unwrap();
        assert_eq!(n[0].column(0).as_any().downcast_ref::<arrow::array::Int64Array>().unwrap().value(0), 9);
        // a CTAS into scratch is persisted on sync
        e.sql("CREATE TABLE scratch.evens AS SELECT id FROM prod WHERE id % 2 = 0").await.unwrap().collect().await.unwrap();
        e.sync_from_context().await;
        assert!(dir.join("scratch").join("evens.parquet").is_file());
        assert!(e.tables().iter().any(|t| t.name == "scratch.evens" && matches!(t.source, TableSource::Scratch { .. })));
        // a fresh engine finds both on disk
        let e2 = Engine::new(Arc::new(cobalt_results::MemoryBudget::unlimited()), 256 << 20, dir.join("spill2")).unwrap();
        assert_eq!(e2.load_scratch(dir.join("scratch")).await, 2);
        let n = e2.sql("SELECT count(*) FROM scratch.evens").await.unwrap().collect().await.unwrap();
        assert_eq!(n[0].column(0).as_any().downcast_ref::<arrow::array::Int64Array>().unwrap().value(0), 5);
        // drop removes the file
        e2.sql("DROP TABLE scratch.evens").await.unwrap().collect().await.unwrap();
        e2.sync_from_context().await;
        assert!(!dir.join("scratch").join("evens.parquet").exists());
        e2.remove_scratch("scratch.prod_snapshot").unwrap();
        assert!(!dir.join("scratch").join("prod_snapshot.parquet").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scratch_names_quote_by_part() {
        assert_eq!(quote_table("orders"), "\"orders\"");
        assert_eq!(quote_table("scratch.orders_v2"), "scratch.\"orders_v2\"");
        assert_eq!(table_ref("orders"), "orders");
        assert_eq!(table_ref("scratch.orders_v2"), "scratch.orders_v2");
        assert_eq!(table_ref("scratch.Mixed Case"), "scratch.\"Mixed Case\"");
        assert_eq!(table_ref("Caps"), "\"Caps\"");
    }

    #[test]
    fn names_are_sanitised() {
        assert_eq!(sanitize_name("SQLQuery_1 · cobalt_test"), "sqlquery_1_cobalt_test");
        assert_eq!(sanitize_name("2024 sales.csv"), "t2024_sales_csv");
        assert_eq!(sanitize_name("Explore_7 · RetailDemo"), "explore_7_retaildemo");
    }
}
