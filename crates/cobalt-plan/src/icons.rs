//! Operator → icon mapping, plus a coarse category for tinting.
//!
//! The icon set is Cobalt's own (SSMS-shaped, monochrome). This module only decides
//! *which* icon a node gets; the app maps `OpIcon` to an image.

use crate::model::PlanObject;
use serde::{Deserialize, Serialize};

/// Icon identifiers for the plan canvas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OpIcon {
    TableScan,
    ClusteredIndexScan,
    IndexScan,
    ClusteredIndexSeek,
    IndexSeek,
    KeyLookup,
    RidLookup,
    NestedLoops,
    HashMatch,
    MergeJoin,
    Sort,
    Filter,
    ComputeScalar,
    StreamAggregate,
    HashAggregate,
    Top,
    Concatenation,
    Parallelism,
    TableSpool,
    IndexSpool,
    RowCountSpool,
    Insert,
    Update,
    Delete,
    Merge,
    Assert,
    SequenceProject,
    Segment,
    WindowAggregate,
    ConstantScan,
    TableValuedFunction,
    RemoteQuery,
    Collapse,
    Split,
    Switch,
    UdxOp,
    ColumnstoreIndexScan,
    AdaptiveJoin,
    Result,
    Cursor,
    Other,
}

/// Coarse operator family, used to tint icons.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum IconCategory {
    Scan,
    Seek,
    Join,
    Aggregate,
    Sort,
    Dml,
    Parallel,
    Spool,
    Scalar,
    Other,
}

impl OpIcon {
    pub fn category(&self) -> IconCategory {
        use OpIcon::*;
        match self {
            TableScan | ClusteredIndexScan | IndexScan | ColumnstoreIndexScan | ConstantScan
            | TableValuedFunction | RemoteQuery => IconCategory::Scan,
            ClusteredIndexSeek | IndexSeek | KeyLookup | RidLookup => IconCategory::Seek,
            NestedLoops | HashMatch | MergeJoin | AdaptiveJoin | Concatenation => IconCategory::Join,
            StreamAggregate | HashAggregate | WindowAggregate => IconCategory::Aggregate,
            Sort | Top => IconCategory::Sort,
            Insert | Update | Delete | Merge => IconCategory::Dml,
            Parallelism => IconCategory::Parallel,
            TableSpool | IndexSpool | RowCountSpool => IconCategory::Spool,
            Filter | ComputeScalar | Assert | SequenceProject | Segment | Collapse | Split
            | Switch | UdxOp => IconCategory::Scalar,
            Result | Cursor | Other => IconCategory::Other,
        }
    }

    /// Stable snake_case name (asset file stem / agent JSON).
    pub fn name(&self) -> &'static str {
        use OpIcon::*;
        match self {
            TableScan => "table_scan",
            ClusteredIndexScan => "clustered_index_scan",
            IndexScan => "index_scan",
            ClusteredIndexSeek => "clustered_index_seek",
            IndexSeek => "index_seek",
            KeyLookup => "key_lookup",
            RidLookup => "rid_lookup",
            NestedLoops => "nested_loops",
            HashMatch => "hash_match",
            MergeJoin => "merge_join",
            Sort => "sort",
            Filter => "filter",
            ComputeScalar => "compute_scalar",
            StreamAggregate => "stream_aggregate",
            HashAggregate => "hash_aggregate",
            Top => "top",
            Concatenation => "concatenation",
            Parallelism => "parallelism",
            TableSpool => "table_spool",
            IndexSpool => "index_spool",
            RowCountSpool => "row_count_spool",
            Insert => "insert",
            Update => "update",
            Delete => "delete",
            Merge => "merge",
            Assert => "assert",
            SequenceProject => "sequence_project",
            Segment => "segment",
            WindowAggregate => "window_aggregate",
            ConstantScan => "constant_scan",
            TableValuedFunction => "table_valued_function",
            RemoteQuery => "remote_query",
            Collapse => "collapse",
            Split => "split",
            Switch => "switch",
            UdxOp => "udx",
            ColumnstoreIndexScan => "columnstore_index_scan",
            AdaptiveJoin => "adaptive_join",
            Result => "result",
            Cursor => "cursor",
            Other => "other",
        }
    }
}

fn is_aggregate_logical(logical: &str) -> bool {
    matches!(
        logical,
        "aggregate" | "partial aggregate" | "flow distinct" | "distinct" | "distinct sort"
    )
}

/// Pick the icon for an operator. `physical_op` is matched case-insensitively; the logical
/// op refines Hash Match (join vs aggregate) and the object refines scans (columnstore, heap).
pub fn icon_for(physical_op: &str, logical_op: &str, object: Option<&PlanObject>) -> OpIcon {
    let phys = physical_op.trim().to_ascii_lowercase();
    let logical = logical_op.trim().to_ascii_lowercase();
    let index_kind = object
        .and_then(|o| o.index_kind.as_deref())
        .map(|k| k.to_ascii_lowercase())
        .unwrap_or_default();
    let columnstore = index_kind.contains("columnstore")
        || object.and_then(|o| o.index_kind.as_deref()).is_none() && phys.contains("columnstore");

    match phys.as_str() {
        "table scan" => OpIcon::TableScan,
        "clustered index scan" => {
            if columnstore {
                OpIcon::ColumnstoreIndexScan
            } else {
                OpIcon::ClusteredIndexScan
            }
        }
        "index scan" => {
            if columnstore {
                OpIcon::ColumnstoreIndexScan
            } else {
                OpIcon::IndexScan
            }
        }
        "columnstore index scan" => OpIcon::ColumnstoreIndexScan,
        "clustered index seek" => {
            if logical == "key lookup" {
                OpIcon::KeyLookup
            } else {
                OpIcon::ClusteredIndexSeek
            }
        }
        "index seek" => OpIcon::IndexSeek,
        "key lookup" => OpIcon::KeyLookup,
        "rid lookup" => OpIcon::RidLookup,
        "nested loops" => OpIcon::NestedLoops,
        "hash match" => {
            if is_aggregate_logical(&logical) {
                OpIcon::HashAggregate
            } else {
                OpIcon::HashMatch
            }
        }
        "merge join" => OpIcon::MergeJoin,
        "adaptive join" => OpIcon::AdaptiveJoin,
        "sort" => OpIcon::Sort,
        "filter" => OpIcon::Filter,
        "compute scalar" => OpIcon::ComputeScalar,
        "stream aggregate" => OpIcon::StreamAggregate,
        "hash aggregate" => OpIcon::HashAggregate,
        "window aggregate" => OpIcon::WindowAggregate,
        "top" => OpIcon::Top,
        "concatenation" => OpIcon::Concatenation,
        "parallelism" => OpIcon::Parallelism,
        "table spool" => OpIcon::TableSpool,
        "index spool" | "nonclustered index spool" => OpIcon::IndexSpool,
        "row count spool" => OpIcon::RowCountSpool,
        "table insert" | "clustered index insert" | "index insert" | "insert" => OpIcon::Insert,
        "table update" | "clustered index update" | "index update" | "update" => OpIcon::Update,
        "table delete" | "clustered index delete" | "index delete" | "delete" => OpIcon::Delete,
        "table merge" | "clustered index merge" | "index merge" | "merge" => OpIcon::Merge,
        "assert" => OpIcon::Assert,
        "sequence project" => OpIcon::SequenceProject,
        "segment" => OpIcon::Segment,
        "constant scan" => OpIcon::ConstantScan,
        "table-valued function" | "table valued function" => OpIcon::TableValuedFunction,
        "remote query" | "remote scan" | "remote insert" | "remote update" | "remote delete"
        | "remote index scan" | "remote index seek" => OpIcon::RemoteQuery,
        "collapse" => OpIcon::Collapse,
        "split" => OpIcon::Split,
        "switch" => OpIcon::Switch,
        "udx" => OpIcon::UdxOp,
        "result" => OpIcon::Result,
        "fetch query" | "population query" | "refresh query" | "snapshot" | "dynamic"
        | "fast forward" | "keyset" | "cursor" => OpIcon::Cursor,
        _ => {
            if phys.contains("cursor") {
                OpIcon::Cursor
            } else if phys.contains("spool") {
                OpIcon::TableSpool
            } else if phys.contains("scan") {
                OpIcon::TableScan
            } else if phys.contains("seek") {
                OpIcon::IndexSeek
            } else if phys.contains("join") {
                OpIcon::NestedLoops
            } else if phys.contains("aggregate") {
                OpIcon::StreamAggregate
            } else if phys.contains("insert") {
                OpIcon::Insert
            } else if phys.contains("update") {
                OpIcon::Update
            } else if phys.contains("delete") {
                OpIcon::Delete
            } else {
                OpIcon::Other
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(kind: &str) -> PlanObject {
        PlanObject { index_kind: Some(kind.into()), ..Default::default() }
    }

    #[test]
    fn common_ops_map() {
        assert_eq!(icon_for("Clustered Index Seek", "Clustered Index Seek", None), OpIcon::ClusteredIndexSeek);
        assert_eq!(icon_for("Index Seek", "Index Seek", None), OpIcon::IndexSeek);
        assert_eq!(icon_for("Clustered Index Scan", "Clustered Index Scan", None), OpIcon::ClusteredIndexScan);
        assert_eq!(icon_for("Index Scan", "Index Scan", None), OpIcon::IndexScan);
        assert_eq!(icon_for("Table Scan", "Table Scan", None), OpIcon::TableScan);
        assert_eq!(icon_for("Key Lookup", "Clustered Index Seek", None), OpIcon::KeyLookup);
        assert_eq!(icon_for("RID Lookup", "RID Lookup", None), OpIcon::RidLookup);
        assert_eq!(icon_for("Nested Loops", "Inner Join", None), OpIcon::NestedLoops);
        assert_eq!(icon_for("Merge Join", "Inner Join", None), OpIcon::MergeJoin);
        assert_eq!(icon_for("Sort", "TopN Sort", None), OpIcon::Sort);
        assert_eq!(icon_for("Top", "Top", None), OpIcon::Top);
        assert_eq!(icon_for("Filter", "Filter", None), OpIcon::Filter);
        assert_eq!(icon_for("Compute Scalar", "Compute Scalar", None), OpIcon::ComputeScalar);
        assert_eq!(icon_for("Stream Aggregate", "Aggregate", None), OpIcon::StreamAggregate);
        assert_eq!(icon_for("Window Aggregate", "Window Aggregate", None), OpIcon::WindowAggregate);
        assert_eq!(icon_for("Parallelism", "Gather Streams", None), OpIcon::Parallelism);
        assert_eq!(icon_for("Constant Scan", "Constant Scan", None), OpIcon::ConstantScan);
        assert_eq!(icon_for("Table Insert", "Insert", None), OpIcon::Insert);
        assert_eq!(icon_for("Clustered Index Update", "Update", None), OpIcon::Update);
        assert_eq!(icon_for("Table Delete", "Delete", None), OpIcon::Delete);
        assert_eq!(icon_for("Concatenation", "Concatenation", None), OpIcon::Concatenation);
        assert_eq!(icon_for("Table Spool", "Lazy Spool", None), OpIcon::TableSpool);
        assert_eq!(icon_for("Adaptive Join", "Inner Join", None), OpIcon::AdaptiveJoin);
        assert_eq!(icon_for("Table-valued function", "Table-valued function", None), OpIcon::TableValuedFunction);
    }

    #[test]
    fn hash_match_splits_on_logical_op() {
        assert_eq!(icon_for("Hash Match", "Inner Join", None), OpIcon::HashMatch);
        assert_eq!(icon_for("Hash Match", "Right Semi Join", None), OpIcon::HashMatch);
        assert_eq!(icon_for("Hash Match", "Aggregate", None), OpIcon::HashAggregate);
        assert_eq!(icon_for("Hash Match", "Partial Aggregate", None), OpIcon::HashAggregate);
        assert_eq!(icon_for("Hash Match", "Flow Distinct", None), OpIcon::HashAggregate);
    }

    #[test]
    fn columnstore_from_object_kind() {
        assert_eq!(icon_for("Clustered Index Scan", "Clustered Index Scan", Some(&obj("Clustered"))), OpIcon::ClusteredIndexScan);
        assert_eq!(icon_for("Index Scan", "Index Scan", Some(&obj("NonClusteredColumnStore"))), OpIcon::ColumnstoreIndexScan);
        assert_eq!(icon_for("Columnstore Index Scan", "Columnstore Index Scan", None), OpIcon::ColumnstoreIndexScan);
    }

    #[test]
    fn unknown_ops_fall_back_by_keyword() {
        assert_eq!(icon_for("Weird Future Scan", "?", None), OpIcon::TableScan);
        assert_eq!(icon_for("Something Else", "?", None), OpIcon::Other);
        assert_eq!(icon_for("", "", None), OpIcon::Other);
        assert_eq!(icon_for("Fetch Query", "Fetch Query", None), OpIcon::Cursor);
    }

    #[test]
    fn categories_and_names() {
        assert_eq!(OpIcon::KeyLookup.category(), IconCategory::Seek);
        assert_eq!(OpIcon::HashAggregate.category(), IconCategory::Aggregate);
        assert_eq!(OpIcon::Parallelism.category(), IconCategory::Parallel);
        assert_eq!(OpIcon::Update.category(), IconCategory::Dml);
        assert_eq!(OpIcon::ComputeScalar.category(), IconCategory::Scalar);
        assert_eq!(OpIcon::Other.category(), IconCategory::Other);
        assert_eq!(OpIcon::ClusteredIndexSeek.name(), "clustered_index_seek");
    }
}
