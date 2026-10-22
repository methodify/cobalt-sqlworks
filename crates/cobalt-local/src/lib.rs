//! The local query engine: DataFusion in-process over the app's own data — result sets of open
//! tabs (`provider`), files and Delta folders (slice B), scratch and lakehouse tables (slice D).
//! Design: `docs/design/local_query.md`.

pub mod engine;
pub mod provider;
pub mod sugar;

pub use engine::{columns_for, conform, sanitize_name, sql_type_for, Engine, TableInfo, TableSource, ENGINE_VERSION, LATEST};
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

    #[test]
    fn names_are_sanitised() {
        assert_eq!(sanitize_name("SQLQuery_1 · cobalt_test"), "sqlquery_1_cobalt_test");
        assert_eq!(sanitize_name("2024 sales.csv"), "t2024_sales_csv");
        assert_eq!(sanitize_name("Explore_7 · RetailDemo"), "explore_7_retaildemo");
    }
}
