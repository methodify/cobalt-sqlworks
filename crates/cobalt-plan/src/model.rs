//! The plan model: a parsed showplan document as plain data.
//!
//! A [`Plan`] holds top-level [`Statement`]s; each statement owns an arena of [`Node`]s
//! (operator tree, `root` is an index into `nodes`) and may own nested statements
//! (`children`: the branches of an `IF`, the statements of an executed procedure, the
//! extra operations of a cursor).

use serde::{Deserialize, Serialize};

/// A parsed showplan document.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Plan {
    /// Top-level statements of the batch, in document order.
    pub statements: Vec<Statement>,
    /// The original XML, verbatim (for "Show plan XML" / "Save as .sqlplan").
    pub source_xml: String,
    /// `ShowPlanXML/@Version` (e.g. `1.564`).
    pub version: String,
    /// `ShowPlanXML/@Build` (e.g. `16.0.4295.3`).
    pub build: String,
}

impl Plan {
    /// Every statement in the document, depth first (a statement before its children).
    pub fn all_statements(&self) -> Vec<&Statement> {
        fn walk<'a>(s: &'a Statement, out: &mut Vec<&'a Statement>) {
            out.push(s);
            for c in &s.children {
                walk(c, out);
            }
        }
        let mut out = Vec::new();
        for s in &self.statements {
            walk(s, &mut out);
        }
        out
    }

    /// True when any statement carries run-time (actual) information.
    pub fn is_actual(&self) -> bool {
        self.all_statements().iter().any(|s| s.is_actual)
    }

    /// Sum of the estimated subtree costs of the top-level statements.
    pub fn total_cost(&self) -> f64 {
        self.statements.iter().map(|s| s.subtree_cost).sum()
    }
}

/// Which showplan statement element a [`Statement`] came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum StatementKind {
    /// `StmtSimple` (SELECT/INSERT/…, EXEC, SET, PRINT, DDL).
    #[default]
    Simple,
    /// `StmtCond` (`IF … ELSE …`); the condition's plan is this statement's tree,
    /// the branches are `children`.
    Cond,
    /// `StmtCursor` (`DECLARE/OPEN/FETCH/CLOSE/DEALLOCATE CURSOR`).
    Cursor,
    /// `StmtUseDb`.
    UseDb,
    /// `StmtReceive` (Service Broker `RECEIVE`).
    Receive,
    /// Anything else under `Statements`.
    Other,
}

/// One statement of the batch with its operator tree.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Statement {
    /// `@StatementId`.
    pub id: i32,
    /// `@StatementText`.
    pub text: String,
    /// `@StatementType` (`SELECT`, `EXECUTE PROC`, `COND WITH QUERY`, …).
    pub statement_type: String,
    pub kind: StatementKind,
    /// `@StatementSubTreeCost`, falling back to the root operator's subtree cost.
    pub subtree_cost: f64,
    /// `@StatementEstRows`.
    pub est_rows: Option<f64>,
    /// `@QueryHash`.
    pub query_hash: Option<String>,
    /// `@QueryPlanHash`.
    pub plan_hash: Option<String>,
    /// `QueryPlan/@DegreeOfParallelism`.
    pub degree_of_parallelism: Option<u32>,
    /// `QueryPlan/@MemoryGrant` (KB).
    pub memory_grant_kb: Option<u64>,
    /// `QueryPlan/@CompileTime` (ms).
    pub compile_time_ms: Option<u64>,
    /// `QueryPlan/@CompileCPU` (ms).
    pub compile_cpu_ms: Option<u64>,
    /// `QueryPlan/@CachedPlanSize` (KB).
    pub cached_plan_size_kb: Option<u64>,
    /// `@StatementOptmLevel` (`TRIVIAL` / `FULL`).
    pub optimization_level: Option<String>,
    /// `@StatementOptmEarlyAbortReason`.
    pub early_abort_reason: Option<String>,
    /// `@CardinalityEstimationModelVersion`.
    pub cardinality_estimation_model: Option<String>,
    /// `QueryPlan/@NonParallelPlanReason`.
    pub non_parallel_reason: Option<String>,
    /// `QueryPlan/ParameterList`.
    pub parameters: Vec<PlanParameter>,
    /// Statement-level warnings (`QueryPlan/Warnings`). Operator warnings live on nodes.
    pub warnings: Vec<Warning>,
    /// `QueryPlan/MissingIndexes`.
    pub missing_indexes: Vec<MissingIndex>,
    /// `QueryPlan/WaitStats` (actual plans).
    pub wait_stats: Vec<WaitStat>,
    /// `QueryPlan/QueryTimeStats` as `(cpu_ms, elapsed_ms)` (actual plans).
    pub query_time_ms: Option<(u64, u64)>,
    /// Statement-level key/value properties (statement and `QueryPlan` attributes,
    /// `MemoryGrantInfo.*`, `OptimizerHardwareDependentProperties.*`, `TraceFlags`, …).
    pub properties: Vec<(String, String)>,
    /// Operator arena. `Node::index` equals the position in this vector.
    pub nodes: Vec<Node>,
    /// Index of the root operator, if the statement has a query plan.
    pub root: Option<usize>,
    /// True when run-time information is present (actual plan).
    pub is_actual: bool,
    /// Nested statements: `IF` branches, executed procedure bodies, extra cursor operations.
    pub children: Vec<Statement>,
}

/// Metric used by [`Statement::most_expensive`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Metric {
    /// Operator's own estimated cost (subtree minus children).
    Cost,
    SubtreeCost,
    ActualRows,
    EstRows,
    ActualElapsed,
    ActualCpu,
    RowsRead,
}

impl Statement {
    /// True if the statement has an operator tree.
    pub fn has_plan(&self) -> bool {
        self.root.is_some() && !self.nodes.is_empty()
    }

    /// Own (not subtree) estimated cost of a node.
    pub fn own_cost(&self, node: usize) -> f64 {
        let n = &self.nodes[node];
        let children: f64 = n.children.iter().map(|&c| self.nodes[c].est_subtree_cost).sum();
        (n.est_subtree_cost - children).max(0.0)
    }

    /// Node indexes sorted by own estimated cost, descending (ties by node order).
    pub fn top_operations(&self) -> Vec<usize> {
        let mut v: Vec<usize> = (0..self.nodes.len()).collect();
        v.sort_by(|&a, &b| {
            self.own_cost(b)
                .partial_cmp(&self.own_cost(a))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.cmp(&b))
        });
        v
    }

    /// Case-insensitive search over physical/logical operator names, object names,
    /// and predicate text. Returns node indexes in arena order.
    pub fn find_nodes(&self, term: &str) -> Vec<usize> {
        let term = term.trim().to_lowercase();
        if term.is_empty() {
            return Vec::new();
        }
        let hit = |s: &str| s.to_lowercase().contains(&term);
        self.nodes
            .iter()
            .filter(|n| {
                hit(&n.physical_op)
                    || hit(&n.logical_op)
                    || n.object.as_ref().is_some_and(|o| hit(&o.display()))
                    || n.predicate.as_deref().is_some_and(hit)
                    || n.seek_predicates.as_deref().is_some_and(hit)
            })
            .map(|n| n.index)
            .collect()
    }

    /// The node with the largest value for `metric`, if any node has a value.
    pub fn most_expensive(&self, metric: Metric) -> Option<usize> {
        let value = |n: &Node| -> Option<f64> {
            match metric {
                Metric::Cost => Some(self.own_cost(n.index)),
                Metric::SubtreeCost => Some(n.est_subtree_cost),
                Metric::EstRows => Some(n.est_rows),
                Metric::ActualRows => n.actual.as_ref().map(|a| a.rows as f64),
                Metric::ActualElapsed => n.actual.as_ref().map(|a| a.elapsed_ms as f64),
                Metric::ActualCpu => n.actual.as_ref().map(|a| a.cpu_ms as f64),
                Metric::RowsRead => n
                    .actual
                    .as_ref()
                    .and_then(|a| a.rows_read)
                    .map(|r| r as f64)
                    .or(n.est_rows_read),
            }
        };
        let mut best: Option<(usize, f64)> = None;
        for n in &self.nodes {
            if let Some(v) = value(n) {
                if best.is_none_or(|(_, bv)| v > bv) {
                    best = Some((n.index, v));
                }
            }
        }
        best.map(|(i, _)| i)
    }

    /// Depth of a node (root = 0).
    pub fn depth(&self, node: usize) -> usize {
        let mut d = 0;
        let mut cur = self.nodes.get(node).and_then(|n| n.parent);
        while let Some(p) = cur {
            d += 1;
            cur = self.nodes[p].parent;
        }
        d
    }

    /// Actual rows returned by the root operator, if this is an actual plan.
    pub fn total_actual_rows(&self) -> Option<u64> {
        self.root
            .and_then(|r| self.nodes.get(r))
            .and_then(|n| n.actual.as_ref())
            .map(|a| a.rows)
    }

    /// All warnings: statement-level ones (node `None`) and operator ones (node `Some`).
    pub fn all_warnings(&self) -> Vec<(Option<usize>, &Warning)> {
        let mut out: Vec<(Option<usize>, &Warning)> =
            self.warnings.iter().map(|w| (None, w)).collect();
        for n in &self.nodes {
            out.extend(n.warnings.iter().map(|w| (Some(n.index), w)));
        }
        out
    }

    /// Nodes in pre-order (parent before children, children in plan order).
    pub fn preorder(&self) -> Vec<usize> {
        let mut out = Vec::with_capacity(self.nodes.len());
        let mut stack: Vec<usize> = self.root.into_iter().collect();
        while let Some(i) = stack.pop() {
            out.push(i);
            for &c in self.nodes[i].children.iter().rev() {
                stack.push(c);
            }
        }
        out
    }
}

/// One operator of a statement's plan.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Node {
    /// Position in `Statement::nodes`.
    pub index: usize,
    /// `RelOp/@NodeId` (unique within a statement, not across a document).
    pub node_id: i32,
    pub parent: Option<usize>,
    /// Child operators in plan order (first child = outer/build input).
    pub children: Vec<usize>,
    /// `@PhysicalOp`, with lookups renamed to `Key Lookup` / `RID Lookup` as SSMS does.
    /// The raw value is in `properties["PhysicalOp"]`.
    pub physical_op: String,
    /// `@LogicalOp`.
    pub logical_op: String,
    /// `@EstimateRows` (per execution).
    pub est_rows: f64,
    /// `@EstimateRowsAllExecs` when present.
    pub est_rows_all_executions: Option<f64>,
    /// `@EstimatedRowsRead`.
    pub est_rows_read: Option<f64>,
    /// `@EstimateExecutions` when present.
    pub est_executions: Option<f64>,
    pub est_cpu: f64,
    pub est_io: f64,
    /// `@EstimatedTotalSubtreeCost`.
    pub est_subtree_cost: f64,
    pub est_rebinds: Option<f64>,
    pub est_rewinds: Option<f64>,
    pub avg_row_size: Option<u32>,
    /// `@Parallel`.
    pub parallel: bool,
    /// `@EstimatedExecutionMode` (`Row` / `Batch`).
    pub execution_mode: Option<String>,
    /// The table/index this operator touches.
    pub object: Option<PlanObject>,
    /// Run-time counters aggregated across threads (actual plans).
    pub actual: Option<ActualStats>,
    pub warnings: Vec<Warning>,
    /// `Predicate/ScalarOperator/@ScalarString`.
    pub predicate: Option<String>,
    /// Rendered seek predicates (`Seek Keys[1]: Prefix: … = Scalar Operator(…)`).
    pub seek_predicates: Option<String>,
    /// `OutputList/ColumnReference`, rendered.
    pub output_list: Vec<String>,
    /// `DefinedValues/DefinedValue`, rendered as `column = expression` or `column`.
    pub defined_values: Vec<String>,
    /// Every attribute and scalar child of the operator, flattened; nested groups joined with `.`.
    pub properties: Vec<(String, String)>,
    /// Own cost as a percentage (0–100) of the statement's subtree cost.
    pub cost_pct: f64,
}

impl Node {
    /// `properties` lookup by key (first match).
    pub fn property(&self, key: &str) -> Option<&str> {
        self.properties
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// True when this operator is a Key Lookup / RID Lookup.
    pub fn is_lookup(&self) -> bool {
        self.physical_op == "Key Lookup" || self.physical_op == "RID Lookup"
    }

    /// Rows for edge thickness / row-delta display: actual when present, else estimated.
    pub fn display_rows(&self) -> f64 {
        self.actual
            .as_ref()
            .map(|a| a.rows as f64)
            .unwrap_or(self.est_rows_all_executions.unwrap_or(self.est_rows))
    }
}

/// The database object an operator touches (`Object` element).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanObject {
    pub database: Option<String>,
    pub schema: Option<String>,
    pub table: Option<String>,
    pub index: Option<String>,
    pub alias: Option<String>,
    /// `@IndexKind` (`Clustered`, `NonClustered`, `Heap`, `ViewClustered`, …).
    pub index_kind: Option<String>,
    pub column: Option<String>,
}

impl PlanObject {
    /// `schema.table` (or just `table`), bracket-free.
    pub fn table_name(&self) -> String {
        match (&self.schema, &self.table) {
            (Some(s), Some(t)) => format!("{s}.{t}"),
            (None, Some(t)) => t.clone(),
            _ => String::new(),
        }
    }

    /// `schema.table (index)` / `schema.table` / `schema.table (index) [alias]` – for node labels.
    pub fn display(&self) -> String {
        let mut s = self.table_name();
        if let Some(ix) = &self.index {
            if s.is_empty() {
                s = ix.clone();
            } else {
                s.push_str(&format!(" ({ix})"));
            }
        }
        if let Some(c) = &self.column {
            if !s.is_empty() {
                s.push('.');
            }
            s.push_str(c);
        }
        if let Some(a) = &self.alias {
            s.push_str(&format!(" [{a}]"));
        }
        s
    }
}

/// Run-time counters for an operator, aggregated across threads.
///
/// Sums: `rows`, `rows_read`, `executions`, `cpu_ms`, reads, scans.
/// Max across threads: `elapsed_ms`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ActualStats {
    pub rows: u64,
    pub rows_read: Option<u64>,
    pub executions: u64,
    /// Max `ActualElapsedms` over threads.
    pub elapsed_ms: u64,
    /// Sum of `ActualCPUms` over threads.
    pub cpu_ms: u64,
    pub logical_reads: Option<u64>,
    pub physical_reads: Option<u64>,
    pub read_aheads: Option<u64>,
    pub scans: Option<u64>,
    pub rebinds: Option<u64>,
    pub rewinds: Option<u64>,
    /// Number of `RunTimeCountersPerThread` entries (thread 0 = coordinator in parallel plans).
    pub threads: u32,
    /// `@ActualExecutionMode` of the first thread (`Row` / `Batch`).
    pub execution_mode: Option<String>,
}

/// Known warning categories. Unknown ones keep their element name in `Other`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WarningKind {
    NoJoinPredicate,
    SpillToTempDb,
    ColumnsWithNoStatistics,
    PlanAffectingConvert,
    MemoryGrant,
    UnmatchedIndexes,
    SortSpill,
    HashSpill,
    ExchangeSpill,
    WaitingForMemory,
    Other(String),
}

impl WarningKind {
    /// Short human label.
    pub fn label(&self) -> String {
        match self {
            Self::NoJoinPredicate => "No join predicate".into(),
            Self::SpillToTempDb => "Spill to tempdb".into(),
            Self::ColumnsWithNoStatistics => "Columns with no statistics".into(),
            Self::PlanAffectingConvert => "Implicit conversion".into(),
            Self::MemoryGrant => "Memory grant".into(),
            Self::UnmatchedIndexes => "Unmatched filtered indexes".into(),
            Self::SortSpill => "Sort spill".into(),
            Self::HashSpill => "Hash spill".into(),
            Self::ExchangeSpill => "Exchange spill".into(),
            Self::WaitingForMemory => "Waited for memory grant".into(),
            Self::Other(s) => s.clone(),
        }
    }
}

/// A warning attached to a statement or an operator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Warning {
    pub kind: WarningKind,
    /// Attribute summary (`SpillLevel=8, SpilledThreadCount=1; GrantedMemoryKb=…`).
    pub detail: String,
}

/// A missing-index recommendation.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MissingIndex {
    /// `MissingIndexGroup/@Impact` (percent).
    pub impact: f64,
    pub database: String,
    pub schema: String,
    pub table: String,
    pub equality_columns: Vec<String>,
    pub inequality_columns: Vec<String>,
    pub included_columns: Vec<String>,
}

impl MissingIndex {
    /// `CREATE NONCLUSTERED INDEX … ON [schema].[table] (…) INCLUDE (…);` with a generated name.
    pub fn create_index_sql(&self) -> String {
        let bracket = |s: &str| format!("[{}]", s.replace(']', "]]"));
        let key_cols: Vec<&String> = self
            .equality_columns
            .iter()
            .chain(self.inequality_columns.iter())
            .collect();
        let name = {
            let mut n = format!("IX_{}", self.table);
            for c in &key_cols {
                n.push('_');
                n.push_str(c);
            }
            n
        };
        let mut sql = format!(
            "CREATE NONCLUSTERED INDEX {} ON {}.{} ({})",
            bracket(&name),
            bracket(&self.schema),
            bracket(&self.table),
            key_cols.iter().map(|c| bracket(c)).collect::<Vec<_>>().join(", ")
        );
        if !self.included_columns.is_empty() {
            sql.push_str(&format!(
                " INCLUDE ({})",
                self.included_columns
                    .iter()
                    .map(|c| bracket(c))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        sql.push(';');
        sql
    }
}

/// A parameter of the statement (`ParameterList/ColumnReference`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanParameter {
    pub name: String,
    pub data_type: String,
    pub compiled_value: Option<String>,
    pub runtime_value: Option<String>,
}

/// One row of `QueryPlan/WaitStats`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitStat {
    pub wait_type: String,
    pub wait_time_ms: u64,
    pub wait_count: u64,
}
