//! A DataFusion table over a `cobalt_results::ResultSet`: the chunks the grid already holds,
//! read in order through the store's spill cache, projected to the columns the query asks for.
//! No copy is made: the provider holds the same `Arc` the tab holds.

use arrow::array::RecordBatch;
use arrow::datatypes::SchemaRef;
use async_trait::async_trait;
use cobalt_results::ResultSet;
use datafusion::catalog::{Session, TableProvider};
use datafusion::common::{DataFusionError, Result};
use datafusion::datasource::TableType;
use datafusion::execution::{SendableRecordBatchStream, TaskContext};
use datafusion::logical_expr::Expr;
use datafusion::physical_expr::EquivalenceProperties;
use datafusion::physical_plan::execution_plan::{Boundedness, EmissionType};
use datafusion::physical_plan::stream::RecordBatchStreamAdapter;
use datafusion::physical_plan::{DisplayAs, DisplayFormatType, ExecutionPlan, Partitioning, PlanProperties};
use std::any::Any;
use std::fmt;
use std::sync::Arc;

#[derive(Debug)]
pub struct ResultSetProvider {
    rs: Arc<ResultSet>,
}

impl ResultSetProvider {
    pub fn new(rs: Arc<ResultSet>) -> Self {
        Self { rs }
    }
    pub fn result_set(&self) -> &Arc<ResultSet> {
        &self.rs
    }
}

#[async_trait]
impl TableProvider for ResultSetProvider {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn schema(&self) -> SchemaRef {
        Arc::clone(&self.rs.schema)
    }
    fn table_type(&self) -> TableType {
        TableType::Base
    }
    async fn scan(&self, _state: &dyn Session, projection: Option<&Vec<usize>>, _filters: &[Expr], limit: Option<usize>) -> Result<Arc<dyn ExecutionPlan>> {
        Ok(Arc::new(ResultSetExec::new(Arc::clone(&self.rs), projection.cloned(), limit)?))
    }
}

/// The scan: every chunk of the result set (as many as exist when the scan starts), in order.
#[derive(Debug)]
pub struct ResultSetExec {
    rs: Arc<ResultSet>,
    schema: SchemaRef,
    projection: Option<Vec<usize>>,
    limit: Option<usize>,
    cache: Arc<PlanProperties>,
}

impl ResultSetExec {
    fn new(rs: Arc<ResultSet>, projection: Option<Vec<usize>>, limit: Option<usize>) -> Result<Self> {
        let schema: SchemaRef = match &projection {
            Some(p) => Arc::new(rs.schema.project(p).map_err(|e| DataFusionError::ArrowError(Box::new(e), None))?),
            None => Arc::clone(&rs.schema),
        };
        let props = PlanProperties::new(EquivalenceProperties::new(Arc::clone(&schema)), Partitioning::UnknownPartitioning(1), EmissionType::Incremental, Boundedness::Bounded);
        Ok(Self { rs, schema, projection, limit, cache: Arc::new(props) })
    }
}

impl DisplayAs for ResultSetExec {
    fn fmt_as(&self, _t: DisplayFormatType, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "ResultSetExec: rows={}, chunks={}", self.rs.row_count(), self.rs.chunk_count())?;
        if let Some(p) = &self.projection {
            write!(f, ", projection={p:?}")?;
        }
        if let Some(l) = self.limit {
            write!(f, ", limit={l}")?;
        }
        Ok(())
    }
}

impl ExecutionPlan for ResultSetExec {
    fn name(&self) -> &str {
        "ResultSetExec"
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn properties(&self) -> &Arc<PlanProperties> {
        &self.cache
    }
    fn children(&self) -> Vec<&Arc<dyn ExecutionPlan>> {
        Vec::new()
    }
    fn with_new_children(self: Arc<Self>, _children: Vec<Arc<dyn ExecutionPlan>>) -> Result<Arc<dyn ExecutionPlan>> {
        Ok(self)
    }
    fn execute(&self, _partition: usize, _context: Arc<TaskContext>) -> Result<SendableRecordBatchStream> {
        let rs = Arc::clone(&self.rs);
        let n = rs.chunk_count();
        let projection = self.projection.clone();
        let limit = self.limit;
        let mut emitted = 0usize;
        let iter = (0..n).filter_map(move |i| {
            if let Some(l) = limit {
                if emitted >= l {
                    return None;
                }
            }
            let mut b: RecordBatch = rs.chunk(i)?;
            if let Some(p) = &projection {
                b = match b.project(p) {
                    Ok(b) => b,
                    Err(e) => return Some(Err(DataFusionError::ArrowError(Box::new(e), None))),
                };
            }
            if let Some(l) = limit {
                let take = (l - emitted).min(b.num_rows());
                if take < b.num_rows() {
                    b = b.slice(0, take);
                }
            }
            emitted += b.num_rows();
            Some(Ok(b))
        });
        Ok(Box::pin(RecordBatchStreamAdapter::new(Arc::clone(&self.schema), futures_util::stream::iter(iter))))
    }
}
