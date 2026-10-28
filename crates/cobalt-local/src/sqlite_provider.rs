//! A DataFusion table over a table (or view) in a SQLite database file: the rows are read
//! from the file on every scan, only the projected columns and at most the limit, through
//! `cobalt_sqlite::read_table` on a blocking thread. Nothing is copied until a query asks.

use arrow::datatypes::{Field, Schema, SchemaRef};
use async_trait::async_trait;
use cobalt_core::ColumnInfo;
use datafusion::catalog::{Session, TableProvider};
use datafusion::common::{DataFusionError, Result};
use datafusion::datasource::{MemTable, TableType};
use datafusion::logical_expr::Expr;
use datafusion::physical_plan::ExecutionPlan;
use std::any::Any;
use std::path::PathBuf;
use std::sync::Arc;

pub const BATCH_ROWS: usize = 8192;

#[derive(Debug)]
pub struct SqliteTableProvider {
    path: PathBuf,
    table: String,
    columns: Vec<ColumnInfo>,
    schema: SchemaRef,
}

impl SqliteTableProvider {
    pub fn new(path: PathBuf, table: String, columns: Vec<ColumnInfo>) -> Self {
        let schema = Arc::new(Schema::new(columns.iter().map(|c| Field::new(&c.name, c.sql_type.arrow_type(), true)).collect::<Vec<_>>()));
        Self { path, table, columns, schema }
    }
    pub fn path(&self) -> &PathBuf {
        &self.path
    }
    pub fn table(&self) -> &str {
        &self.table
    }
}

#[async_trait]
impl TableProvider for SqliteTableProvider {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn schema(&self) -> SchemaRef {
        Arc::clone(&self.schema)
    }
    fn table_type(&self) -> TableType {
        TableType::Base
    }
    async fn scan(&self, state: &dyn Session, projection: Option<&Vec<usize>>, _filters: &[Expr], limit: Option<usize>) -> Result<Arc<dyn ExecutionPlan>> {
        let path = self.path.clone();
        let table = self.table.clone();
        let columns = self.columns.clone();
        let proj: Option<Vec<usize>> = projection.cloned();
        let projected: SchemaRef = match &proj {
            Some(p) => Arc::new(self.schema.project(p)?),
            None => Arc::clone(&self.schema),
        };
        let batches = tokio::task::spawn_blocking(move || cobalt_sqlite::read_table(&path, &table, &columns, proj.as_deref(), limit, BATCH_ROWS))
            .await
            .map_err(|e| DataFusionError::External(Box::new(e)))?
            .map_err(DataFusionError::Execution)?;
        // the batches carry the SQL types' Arrow types, which match the projected schema
        let batches: Vec<arrow::array::RecordBatch> = batches.into_iter().map(|b| crate::engine::conform(b, &projected)).collect::<std::result::Result<_, String>>().map_err(DataFusionError::Execution)?;
        let mem = MemTable::try_new(projected, vec![batches])?;
        mem.scan(state, None, &[], None).await
    }
}
