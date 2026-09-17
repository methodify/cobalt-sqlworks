//! Human and machine summaries of a plan: a one-line statement description for status
//! bars / tooltips, and a compact JSON form for the agent surface.

use crate::icons::icon_for;
use crate::model::{Plan, Statement};
use serde_json::{json, Value};

/// Thousands-separated integer formatting (`1234567` → `1,234,567`).
pub fn fmt_thousands(n: f64) -> String {
    let neg = n < 0.0;
    let whole = n.abs().round() as u64;
    let s = whole.to_string();
    // Split into 3-digit groups from the right (digits are ASCII, so byte slicing is safe).
    let mut groups: Vec<&str> = Vec::with_capacity(s.len() / 3 + 1);
    let mut end = s.len();
    while end > 3 {
        groups.push(&s[end - 3..end]);
        end -= 3;
    }
    groups.push(&s[..end]);
    groups.reverse();
    let mut out = groups.join(",");
    if neg {
        out.insert(0, '-');
    }
    out
}

/// Compact cost formatting: 4 significant-ish decimals, trailing zeros trimmed.
pub fn fmt_cost(c: f64) -> String {
    let s = format!("{c:.4}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() {
        "0".into()
    } else {
        s.to_string()
    }
}

/// Operator label with its object: `Clustered Index Seek on dbo.big (PK__big__…)`.
pub fn node_label(stmt: &Statement, node: usize) -> String {
    let n = &stmt.nodes[node];
    match &n.object {
        Some(o) if !o.display().is_empty() => format!("{} on {}", n.physical_op, o.display()),
        _ => n.physical_op.clone(),
    }
}

/// One-line description: the most expensive root-to-leaf path (leaf first, as data flows),
/// then estimated/actual rows and the statement cost.
///
/// Example: `Clustered Index Seek on dbo.big (PK) → Nested Loops → Sort; est 1,234 rows; cost 0.0123`
pub fn statement_summary(stmt: &Statement) -> String {
    let Some(root) = stmt.root else {
        // Statement text can span lines (`&#10;` in the XML); keep the summary on one line.
        let text = stmt.text.split_whitespace().collect::<Vec<_>>().join(" ");
        let text = text.trim();
        let short: String = text.chars().take(60).collect();
        return if text.is_empty() {
            format!("{} (no plan)", stmt.statement_type)
        } else {
            format!("{}: {}{}", stmt.statement_type, short, if text.chars().count() > 60 { "…" } else { "" })
        };
    };
    // Walk down the costliest child at each level.
    let mut path = vec![root];
    let mut cur = root;
    while let Some(&next) = stmt.nodes[cur]
        .children
        .iter()
        .max_by(|&&a, &&b| {
            stmt.nodes[a]
                .est_subtree_cost
                .partial_cmp(&stmt.nodes[b].est_subtree_cost)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(b.cmp(&a)) // prefer the earlier child on ties
        })
    {
        if path.len() >= 64 {
            break;
        }
        path.push(next);
        cur = next;
    }
    path.reverse();
    const MAX_SHOWN: usize = 6;
    let mut labels: Vec<String> = path.iter().take(MAX_SHOWN).map(|&i| node_label(stmt, i)).collect();
    if path.len() > MAX_SHOWN {
        labels.push("…".into());
    }
    let chain = labels.join(" → ");

    let rows = match stmt.total_actual_rows() {
        Some(a) => format!("{} rows (est {})", fmt_thousands(a as f64), fmt_thousands(stmt.nodes[root].est_rows)),
        None => format!("est {} rows", fmt_thousands(stmt.est_rows.unwrap_or(stmt.nodes[root].est_rows))),
    };
    let mut s = format!("{chain}; {rows}; cost {}", fmt_cost(stmt.subtree_cost));
    let warn_count = stmt.all_warnings().len();
    if warn_count > 0 {
        s.push_str(&format!("; {warn_count} warning{}", if warn_count == 1 { "" } else { "s" }));
    }
    if !stmt.missing_indexes.is_empty() {
        s.push_str("; missing index");
    }
    s
}

fn statement_json(stmt: &Statement) -> Value {
    let nodes: Vec<Value> = stmt
        .nodes
        .iter()
        .map(|n| {
            let mut v = json!({
                "id": n.node_id,
                "index": n.index,
                "op": n.physical_op,
                "logical": n.logical_op,
                "icon": icon_for(&n.physical_op, &n.logical_op, n.object.as_ref()).name(),
                "est_rows": n.est_rows,
                "subtree_cost": n.est_subtree_cost,
                "cost_pct": (n.cost_pct * 10.0).round() / 10.0,
                "parallel": n.parallel,
                "children": n.children,
            });
            let o = v.as_object_mut().expect("object");
            if let Some(obj) = &n.object {
                o.insert("object".into(), Value::String(obj.display()));
            }
            if let Some(a) = &n.actual {
                o.insert("actual_rows".into(), json!(a.rows));
                o.insert("executions".into(), json!(a.executions));
                o.insert("elapsed_ms".into(), json!(a.elapsed_ms));
                if let Some(r) = a.rows_read {
                    o.insert("rows_read".into(), json!(r));
                }
                if let Some(r) = a.logical_reads {
                    o.insert("logical_reads".into(), json!(r));
                }
            }
            if let Some(p) = &n.predicate {
                o.insert("predicate".into(), Value::String(p.clone()));
            }
            if let Some(p) = &n.seek_predicates {
                o.insert("seek_predicates".into(), Value::String(p.clone()));
            }
            if !n.warnings.is_empty() {
                o.insert(
                    "warnings".into(),
                    Value::Array(
                        n.warnings
                            .iter()
                            .map(|w| json!({ "kind": w.kind.label(), "detail": w.detail }))
                            .collect(),
                    ),
                );
            }
            v
        })
        .collect();

    let mut v = json!({
        "id": stmt.id,
        "type": stmt.statement_type,
        "text": stmt.text,
        "cost": stmt.subtree_cost,
        "actual": stmt.is_actual,
        "root": stmt.root,
        "nodes": nodes,
    });
    let o = v.as_object_mut().expect("object");
    if let Some(r) = stmt.est_rows {
        o.insert("est_rows".into(), json!(r));
    }
    if let Some(r) = stmt.total_actual_rows() {
        o.insert("actual_rows".into(), json!(r));
    }
    if let Some(d) = stmt.degree_of_parallelism {
        o.insert("dop".into(), json!(d));
    }
    if let Some((cpu, elapsed)) = stmt.query_time_ms {
        o.insert("cpu_ms".into(), json!(cpu));
        o.insert("elapsed_ms".into(), json!(elapsed));
    }
    if !stmt.warnings.is_empty() {
        o.insert(
            "warnings".into(),
            Value::Array(
                stmt.warnings
                    .iter()
                    .map(|w| json!({ "kind": w.kind.label(), "detail": w.detail }))
                    .collect(),
            ),
        );
    }
    if !stmt.missing_indexes.is_empty() {
        o.insert(
            "missing_indexes".into(),
            Value::Array(
                stmt.missing_indexes
                    .iter()
                    .map(|m| {
                        json!({
                            "impact": m.impact,
                            "table": format!("{}.{}", m.schema, m.table),
                            "equality": m.equality_columns,
                            "inequality": m.inequality_columns,
                            "include": m.included_columns,
                            "sql": m.create_index_sql(),
                        })
                    })
                    .collect(),
            ),
        );
    }
    if !stmt.children.is_empty() {
        o.insert("children".into(), Value::Array(stmt.children.iter().map(statement_json).collect()));
    }
    v
}

/// Compact JSON for the agent surface: statements → nodes with op, object, est/actual rows,
/// cost %, warnings, missing indexes. Nested statements appear under `children`.
pub fn plan_to_json(plan: &Plan) -> Value {
    json!({
        "version": plan.version,
        "build": plan.build,
        "actual": plan.is_actual(),
        "statements": plan.statements.iter().map(statement_json).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thousands() {
        assert_eq!(fmt_thousands(0.0), "0");
        assert_eq!(fmt_thousands(999.0), "999");
        assert_eq!(fmt_thousands(1000.0), "1,000");
        assert_eq!(fmt_thousands(1234567.0), "1,234,567");
        assert_eq!(fmt_thousands(2e6), "2,000,000");
        assert_eq!(fmt_thousands(-1234.0), "-1,234");
    }

    #[test]
    fn cost_format() {
        assert_eq!(fmt_cost(0.0123), "0.0123");
        assert_eq!(fmt_cost(11.9177), "11.9177");
        assert_eq!(fmt_cost(10.0), "10");
        assert_eq!(fmt_cost(0.0), "0");
    }

    #[test]
    fn summary_of_statement_without_plan() {
        let s = Statement { statement_type: "PRINT".into(), text: "PRINT 'x'".into(), ..Default::default() };
        assert_eq!(statement_summary(&s), "PRINT: PRINT 'x'");
        let s = Statement { statement_type: "SET ON/OFF".into(), ..Default::default() };
        assert_eq!(statement_summary(&s), "SET ON/OFF (no plan)");
    }
}
