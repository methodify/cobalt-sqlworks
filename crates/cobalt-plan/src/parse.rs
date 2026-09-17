//! Showplan XML → [`Plan`].
//!
//! Strategy: read the document into a tiny generic DOM with quick-xml (so unknown elements
//! never break parsing), then walk it. Every attribute of an operator and every scalar
//! child becomes a `(key, value)` property; the well-known ones also populate typed fields.

use crate::error::PlanError;
use crate::model::*;
use quick_xml::events::Event;
use quick_xml::Reader;

// ───────────────────────────────── generic DOM ─────────────────────────────────

#[derive(Debug, Default)]
struct Elem {
    name: String,
    attrs: Vec<(String, String)>,
    children: Vec<Elem>,
}

impl Elem {
    fn attr(&self, key: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }
    fn child(&self, name: &str) -> Option<&Elem> {
        self.children.iter().find(|c| c.name == name)
    }
    fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Elem> + 'a {
        self.children.iter().filter(move |c| c.name == name)
    }
    fn attr_f64(&self, key: &str) -> Option<f64> {
        self.attr(key).and_then(parse_f64)
    }
    fn attr_u64(&self, key: &str) -> Option<u64> {
        self.attr(key).and_then(parse_u64)
    }
    fn attr_bool(&self, key: &str) -> bool {
        self.attr(key).is_some_and(parse_bool)
    }
}

fn parse_f64(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok()
}

fn parse_u64(s: &str) -> Option<u64> {
    let s = s.trim();
    s.parse::<u64>()
        .ok()
        .or_else(|| s.parse::<f64>().ok().filter(|f| *f >= 0.0).map(|f| f.round() as u64))
}

fn parse_bool(s: &str) -> bool {
    matches!(s.trim(), "1" | "true" | "True" | "TRUE")
}

/// Strip one layer of `[…]` quoting.
fn unbracket(s: &str) -> String {
    let t = s.trim();
    if t.len() >= 2 && t.starts_with('[') && t.ends_with(']') {
        t[1..t.len() - 1].replace("]]", "]")
    } else {
        t.to_string()
    }
}

fn read_dom(xml: &str) -> Result<Elem, PlanError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut stack: Vec<Elem> = vec![Elem { name: String::from("#document"), ..Default::default() }];

    let xml_err = |reader: &Reader<&[u8]>, e: &dyn std::fmt::Display| PlanError::Xml {
        position: reader.error_position(),
        message: e.to_string(),
    };

    loop {
        let ev = reader.read_event().map_err(|e| xml_err(&reader, &e))?;
        let is_empty = matches!(ev, Event::Empty(_));
        match ev {
            Event::Start(start) | Event::Empty(start) => {
                let mut elem = Elem {
                    name: start.local_name().as_ref().to_string(),
                    ..Default::default()
                };
                for attr in start.attributes() {
                    let attr = attr.map_err(|e| xml_err(&reader, &e))?;
                    let key = attr.key.local_name().as_ref().to_string();
                    let val = quick_xml::escape::unescape(&attr.value)
                        .map(|v| v.into_owned())
                        .unwrap_or_else(|_| attr.value.to_string());
                    elem.attrs.push((key, val));
                }
                if is_empty {
                    stack.last_mut().expect("document root").children.push(elem);
                } else {
                    stack.push(elem);
                }
            }
            Event::End(_) if stack.len() > 1 => {
                let done = stack.pop().expect("non-empty");
                stack.last_mut().expect("document root").children.push(done);
            }
            Event::Eof => break,
            // Showplan carries everything in attributes; text/entities/PIs are ignored.
            _ => {}
        }
    }
    if stack.len() != 1 {
        return Err(PlanError::Xml {
            position: reader.error_position(),
            message: format!("unexpected end of document inside <{}>", stack.last().map(|e| e.name.as_str()).unwrap_or("?")),
        });
    }
    Ok(stack.pop().expect("document"))
}

// ───────────────────────────────── rendering helpers ─────────────────────────────────

/// `[db].[schema].[table].[col]` (+ ` as [alias].[col]`), or a bare column/expression name.
fn fmt_column_ref(e: &Elem) -> String {
    let column = e.attr("Column").unwrap_or("");
    let mut out = String::new();
    for key in ["Server", "Database", "Schema", "Table"] {
        if let Some(v) = e.attr(key) {
            if !out.is_empty() {
                out.push('.');
            }
            out.push_str(v);
        }
    }
    if out.is_empty() {
        out.push_str(column);
    } else if !column.is_empty() {
        out.push('.');
        if column.starts_with('[') {
            out.push_str(column);
        } else {
            out.push('[');
            out.push_str(column);
            out.push(']');
        }
        if let Some(alias) = e.attr("Alias") {
            out.push_str(&format!(" as {alias}.[{}]", unbracket(column)));
        }
    }
    out
}

fn column_refs(e: &Elem) -> Vec<String> {
    e.children_named("ColumnReference").map(fmt_column_ref).collect()
}

fn scalar_string(e: &Elem) -> Option<String> {
    e.children_named("ScalarOperator")
        .filter_map(|s| s.attr("ScalarString"))
        .map(str::to_string)
        .reduce(|a, b| format!("{a}, {b}"))
}

fn attr_summary(e: &Elem) -> String {
    e.attrs
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn scan_op(scan_type: &str) -> &'static str {
    match scan_type {
        "EQ" => "=",
        "NE" => "<>",
        "GT" => ">",
        "GE" => ">=",
        "LT" => "<",
        "LE" => "<=",
        _ => "?",
    }
}

fn render_seek_part(part: &Elem, label: &str) -> String {
    let cols = part
        .child("RangeColumns")
        .map(column_refs)
        .unwrap_or_default()
        .join(", ");
    let exprs = part
        .child("RangeExpressions")
        .and_then(scalar_string)
        .unwrap_or_default();
    let op = part.attr("ScanType").map(scan_op).unwrap_or("=");
    format!("{label}: {cols} {op} Scalar Operator({exprs})")
}

fn render_seek_keys(keys: &Elem) -> String {
    let mut parts = Vec::new();
    for c in &keys.children {
        match c.name.as_str() {
            "Prefix" => parts.push(render_seek_part(c, "Prefix")),
            "StartRange" => parts.push(render_seek_part(c, "Start")),
            "EndRange" => parts.push(render_seek_part(c, "End")),
            _ => {}
        }
    }
    parts.join(", ")
}

/// `Seek Keys[1]: Prefix: … = Scalar Operator(…); Seek Keys[2]: …`
fn render_seek_predicates(sp: &Elem) -> String {
    let mut out = Vec::new();
    let mut n = 0;
    for pred in &sp.children {
        let keys: Vec<&Elem> = pred.children_named("SeekKeys").collect();
        if keys.is_empty() {
            n += 1;
            out.push(format!("Seek Keys[{n}]: {}", render_seek_keys(pred)));
        } else {
            for k in keys {
                n += 1;
                out.push(format!("Seek Keys[{n}]: {}", render_seek_keys(k)));
            }
        }
    }
    out.join("; ")
}

fn render_defined_values(dv: &Elem) -> Vec<String> {
    dv.children_named("DefinedValue")
        .map(|d| {
            let cols = column_refs(d).join(", ");
            match scalar_string(d) {
                Some(s) if !cols.is_empty() => format!("{cols} = {s}"),
                Some(s) => s,
                None => cols,
            }
        })
        .collect()
}

/// Generic flattening: attributes become `prefix + key`; scalar children become
/// `prefix + Child` = rendered value; nested elements recurse with `Child.` appended.
/// `RelOp` children are never descended (they are tree edges, not properties).
fn flatten(e: &Elem, prefix: &str, out: &mut Vec<(String, String)>) {
    for (k, v) in &e.attrs {
        out.push((format!("{prefix}{k}"), v.clone()));
    }
    let own_key = prefix.trim_end_matches('.').to_string();
    let cols = column_refs(e);
    if !cols.is_empty() && !own_key.is_empty() {
        out.push((own_key.clone(), cols.join(", ")));
    }
    if let Some(s) = scalar_string(e) {
        if !own_key.is_empty() {
            out.push((own_key.clone(), s));
        }
    }
    for c in &e.children {
        match c.name.as_str() {
            "RelOp" | "ColumnReference" | "ScalarOperator" => {}
            "SeekPredicates" => out.push((format!("{prefix}SeekPredicates"), render_seek_predicates(c))),
            "DefinedValues" => out.push((format!("{prefix}DefinedValues"), render_defined_values(c).join("; "))),
            _ => flatten(c, &format!("{prefix}{}.", c.name), out),
        }
    }
}

// ───────────────────────────────── warnings ─────────────────────────────────

fn parse_warnings(w: &Elem) -> Vec<Warning> {
    let mut out = Vec::new();
    for (k, v) in &w.attrs {
        if !parse_bool(v) {
            continue;
        }
        let kind = match k.as_str() {
            "NoJoinPredicate" => WarningKind::NoJoinPredicate,
            "UnmatchedIndexes" => WarningKind::UnmatchedIndexes,
            other => WarningKind::Other(other.to_string()),
        };
        out.push(Warning { kind, detail: format!("{k}={v}") });
    }
    let has = |n: &str| w.child(n);
    for c in &w.children {
        let (kind, detail) = match c.name.as_str() {
            "SpillToTempDb" => {
                let (kind, details) = if let Some(d) = has("SortSpillDetails") {
                    (WarningKind::SortSpill, Some(d))
                } else if let Some(d) = has("HashSpillDetails") {
                    (WarningKind::HashSpill, Some(d))
                } else if let Some(d) = has("ExchangeSpillDetails") {
                    (WarningKind::ExchangeSpill, Some(d))
                } else {
                    (WarningKind::SpillToTempDb, None)
                };
                let mut detail = attr_summary(c);
                if let Some(d) = details {
                    detail.push_str("; ");
                    detail.push_str(&attr_summary(d));
                }
                (kind, detail)
            }
            "SortSpillDetails" | "HashSpillDetails" | "ExchangeSpillDetails" => continue,
            "PlanAffectingConvert" => (
                WarningKind::PlanAffectingConvert,
                format!(
                    "{}: {}",
                    c.attr("ConvertIssue").unwrap_or("Convert"),
                    c.attr("Expression").unwrap_or("")
                ),
            ),
            "ColumnsWithNoStatistics" => (WarningKind::ColumnsWithNoStatistics, column_refs(c).join(", ")),
            "MemoryGrantWarning" => (WarningKind::MemoryGrant, attr_summary(c)),
            "Wait" => (WarningKind::WaitingForMemory, attr_summary(c)),
            other => (WarningKind::Other(other.to_string()), attr_summary(c)),
        };
        out.push(Warning { kind, detail });
    }
    out
}

// ───────────────────────────────── run-time info ─────────────────────────────────

fn parse_runtime(rt: &Elem) -> Option<ActualStats> {
    let threads: Vec<&Elem> = rt.children_named("RunTimeCountersPerThread").collect();
    if threads.is_empty() {
        return None;
    }
    let mut a = ActualStats { threads: threads.len() as u32, ..Default::default() };
    let sum_opt = |cur: &mut Option<u64>, t: &Elem, key: &str| {
        if let Some(v) = t.attr_u64(key) {
            *cur = Some(cur.unwrap_or(0) + v);
        }
    };
    for t in &threads {
        a.rows += t.attr_u64("ActualRows").unwrap_or(0);
        a.executions += t.attr_u64("ActualExecutions").unwrap_or(0);
        a.cpu_ms += t.attr_u64("ActualCPUms").unwrap_or(0);
        a.elapsed_ms = a.elapsed_ms.max(t.attr_u64("ActualElapsedms").unwrap_or(0));
        sum_opt(&mut a.rows_read, t, "ActualRowsRead");
        sum_opt(&mut a.logical_reads, t, "ActualLogicalReads");
        sum_opt(&mut a.physical_reads, t, "ActualPhysicalReads");
        sum_opt(&mut a.read_aheads, t, "ActualReadAheads");
        sum_opt(&mut a.scans, t, "ActualScans");
        sum_opt(&mut a.rebinds, t, "ActualRebinds");
        sum_opt(&mut a.rewinds, t, "ActualRewinds");
    }
    a.execution_mode = threads[0].attr("ActualExecutionMode").map(str::to_string);
    Some(a)
}

fn runtime_properties(rt: &Elem, a: &ActualStats, out: &mut Vec<(String, String)>) {
    let push = |out: &mut Vec<(String, String)>, k: &str, v: String| out.push((k.to_string(), v));
    push(out, "ActualRows", a.rows.to_string());
    if let Some(v) = a.rows_read {
        push(out, "ActualRowsRead", v.to_string());
    }
    push(out, "ActualExecutions", a.executions.to_string());
    push(out, "ActualElapsedms", a.elapsed_ms.to_string());
    push(out, "ActualCPUms", a.cpu_ms.to_string());
    for (k, v) in [
        ("ActualLogicalReads", a.logical_reads),
        ("ActualPhysicalReads", a.physical_reads),
        ("ActualReadAheads", a.read_aheads),
        ("ActualScans", a.scans),
        ("ActualRebinds", a.rebinds),
        ("ActualRewinds", a.rewinds),
    ] {
        if let Some(v) = v {
            push(out, k, v.to_string());
        }
    }
    if let Some(m) = &a.execution_mode {
        push(out, "ActualExecutionMode", m.clone());
    }
    for t in rt.children_named("RunTimeCountersPerThread") {
        let id = t.attr("Thread").unwrap_or("?");
        for (k, v) in &t.attrs {
            if k != "Thread" {
                out.push((format!("RunTimeInformation[{id}].{k}"), v.clone()));
            }
        }
    }
}

// ───────────────────────────────── operators ─────────────────────────────────

/// Children of `RelOp` that are not the operator-specific element.
const ANCILLARY: &[&str] = &[
    "OutputList",
    "Warnings",
    "MemoryFractions",
    "RunTimeInformation",
    "RunTimePartitionSummary",
    "InternalInfo",
];

fn parse_object(o: &Elem) -> PlanObject {
    let get = |k: &str| o.attr(k).map(unbracket).filter(|s| !s.is_empty());
    PlanObject {
        database: get("Database"),
        schema: get("Schema"),
        table: get("Table"),
        index: get("Index"),
        alias: get("Alias"),
        index_kind: get("IndexKind"),
        column: get("Column"),
    }
}

fn build_node(rel: &Elem, parent: Option<usize>, nodes: &mut Vec<Node>) -> usize {
    let index = nodes.len();
    let mut n = Node {
        index,
        parent,
        node_id: rel.attr("NodeId").and_then(|s| s.parse().ok()).unwrap_or(-1),
        physical_op: rel.attr("PhysicalOp").unwrap_or("").to_string(),
        logical_op: rel.attr("LogicalOp").unwrap_or("").to_string(),
        est_rows: rel.attr_f64("EstimateRows").unwrap_or(0.0),
        est_rows_all_executions: rel.attr_f64("EstimateRowsAllExecs"),
        est_rows_read: rel.attr_f64("EstimatedRowsRead"),
        est_executions: rel.attr_f64("EstimateExecutions"),
        est_cpu: rel.attr_f64("EstimateCPU").unwrap_or(0.0),
        est_io: rel.attr_f64("EstimateIO").unwrap_or(0.0),
        est_subtree_cost: rel.attr_f64("EstimatedTotalSubtreeCost").unwrap_or(0.0),
        est_rebinds: rel.attr_f64("EstimateRebinds"),
        est_rewinds: rel.attr_f64("EstimateRewinds"),
        avg_row_size: rel.attr_u64("AvgRowSize").map(|v| v as u32),
        parallel: rel.attr_bool("Parallel"),
        execution_mode: rel.attr("EstimatedExecutionMode").map(str::to_string),
        ..Default::default()
    };
    for (k, v) in &rel.attrs {
        n.properties.push((k.clone(), v.clone()));
    }
    nodes.push(n);

    let mut children = Vec::new();
    let mut props: Vec<(String, String)> = Vec::new();
    let mut object = None;
    let mut predicate = None;
    let mut seek_predicates = None;
    let mut output_list = Vec::new();
    let mut defined_values = Vec::new();
    let mut warnings = Vec::new();
    let mut actual = None;
    let mut lookup = false;

    for c in &rel.children {
        match c.name.as_str() {
            "OutputList" => {
                output_list = column_refs(c);
                props.push(("OutputList".into(), output_list.join(", ")));
            }
            "Warnings" => {
                warnings.extend(parse_warnings(c));
                flatten(c, "Warnings.", &mut props);
            }
            "RunTimeInformation" => {
                if let Some(a) = parse_runtime(c) {
                    runtime_properties(c, &a, &mut props);
                    actual = Some(a);
                }
            }
            name if ANCILLARY.contains(&name) => flatten(c, &format!("{name}."), &mut props),
            _ => {
                // The operator-specific element (IndexScan, Hash, NestedLoops, …).
                if c.attr_bool("Lookup") {
                    lookup = true;
                }
                for (k, v) in &c.attrs {
                    props.push((k.clone(), v.clone()));
                }
                for g in &c.children {
                    match g.name.as_str() {
                        "RelOp" => {
                            let ci = build_node(g, Some(index), nodes);
                            children.push(ci);
                        }
                        "Object" => {
                            if object.is_none() {
                                object = Some(parse_object(g));
                            }
                            flatten(g, "Object.", &mut props);
                        }
                        "Predicate" => {
                            let s = scalar_string(g).unwrap_or_default();
                            props.push(("Predicate".into(), s.clone()));
                            predicate = Some(s);
                        }
                        "SeekPredicates" | "SeekPredicate" => {
                            let s = if g.name == "SeekPredicates" {
                                render_seek_predicates(g)
                            } else {
                                format!("Seek Keys[1]: {}", render_seek_keys(g))
                            };
                            props.push(("SeekPredicates".into(), s.clone()));
                            seek_predicates = Some(s);
                        }
                        "DefinedValues" => {
                            defined_values = render_defined_values(g);
                            props.push(("DefinedValues".into(), defined_values.join("; ")));
                        }
                        "ColumnReference" | "ScalarOperator" => {}
                        _ => flatten(g, &format!("{}.", g.name), &mut props),
                    }
                }
                let cols = column_refs(c);
                if !cols.is_empty() {
                    props.push((c.name.clone(), cols.join(", ")));
                }
                if let Some(s) = scalar_string(c) {
                    props.push((c.name.clone(), s));
                }
            }
        }
    }

    let n = &mut nodes[index];
    if lookup {
        let heap = object
            .as_ref()
            .and_then(|o: &PlanObject| o.index_kind.as_deref())
            .is_some_and(|k| k.eq_ignore_ascii_case("Heap"));
        n.physical_op = if heap || n.physical_op == "RID Lookup" {
            "RID Lookup".to_string()
        } else {
            "Key Lookup".to_string()
        };
    }
    n.children = children;
    n.object = object;
    n.predicate = predicate;
    n.seek_predicates = seek_predicates;
    n.output_list = output_list;
    n.defined_values = defined_values;
    n.warnings = warnings;
    n.actual = actual;
    n.properties.extend(props);
    index
}

// ───────────────────────────────── statements ─────────────────────────────────

fn parse_missing_indexes(mi: &Elem) -> Vec<MissingIndex> {
    let mut out = Vec::new();
    for group in mi.children_named("MissingIndexGroup") {
        let impact = group.attr_f64("Impact").unwrap_or(0.0);
        for ix in group.children_named("MissingIndex") {
            let mut m = MissingIndex {
                impact,
                database: ix.attr("Database").map(unbracket).unwrap_or_default(),
                schema: ix.attr("Schema").map(unbracket).unwrap_or_default(),
                table: ix.attr("Table").map(unbracket).unwrap_or_default(),
                ..Default::default()
            };
            for cg in ix.children_named("ColumnGroup") {
                let cols: Vec<String> = cg
                    .children_named("Column")
                    .filter_map(|c| c.attr("Name"))
                    .map(unbracket)
                    .collect();
                match cg.attr("Usage").unwrap_or("") {
                    "EQUALITY" => m.equality_columns.extend(cols),
                    "INEQUALITY" => m.inequality_columns.extend(cols),
                    _ => m.included_columns.extend(cols),
                }
            }
            out.push(m);
        }
    }
    out
}

fn parse_query_plan(qp: &Elem, stmt: &mut Statement) {
    stmt.degree_of_parallelism = qp.attr_u64("DegreeOfParallelism").map(|v| v as u32);
    stmt.memory_grant_kb = qp.attr_u64("MemoryGrant");
    stmt.compile_time_ms = qp.attr_u64("CompileTime");
    stmt.compile_cpu_ms = qp.attr_u64("CompileCPU");
    stmt.cached_plan_size_kb = qp.attr_u64("CachedPlanSize");
    stmt.non_parallel_reason = qp.attr("NonParallelPlanReason").map(str::to_string);
    for (k, v) in &qp.attrs {
        stmt.properties.push((k.clone(), v.clone()));
    }
    for c in &qp.children {
        match c.name.as_str() {
            "RelOp" => {
                if stmt.root.is_none() {
                    let r = build_node(c, None, &mut stmt.nodes);
                    stmt.root = Some(r);
                }
            }
            "MissingIndexes" => stmt.missing_indexes.extend(parse_missing_indexes(c)),
            "Warnings" => {
                stmt.warnings.extend(parse_warnings(c));
                flatten(c, "Warnings.", &mut stmt.properties);
            }
            "ParameterList" => {
                for p in c.children_named("ColumnReference") {
                    stmt.parameters.push(PlanParameter {
                        name: p.attr("Column").unwrap_or("").to_string(),
                        data_type: p.attr("ParameterDataType").unwrap_or("").to_string(),
                        compiled_value: p.attr("ParameterCompiledValue").map(str::to_string),
                        runtime_value: p.attr("ParameterRuntimeValue").map(str::to_string),
                    });
                }
                let s = stmt
                    .parameters
                    .iter()
                    .map(|p| {
                        format!(
                            "{} {} = {}",
                            p.name,
                            p.data_type,
                            p.runtime_value.as_deref().or(p.compiled_value.as_deref()).unwrap_or("?")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("; ");
                stmt.properties.push(("ParameterList".into(), s));
            }
            "WaitStats" => {
                for w in c.children_named("Wait") {
                    stmt.wait_stats.push(WaitStat {
                        wait_type: w.attr("WaitType").unwrap_or("").to_string(),
                        wait_time_ms: w.attr_u64("WaitTimeMs").unwrap_or(0),
                        wait_count: w.attr_u64("WaitCount").unwrap_or(0),
                    });
                }
                flatten(c, "WaitStats.", &mut stmt.properties);
            }
            "QueryTimeStats" => {
                stmt.query_time_ms = Some((
                    c.attr_u64("CpuTime").unwrap_or(0),
                    c.attr_u64("ElapsedTime").unwrap_or(0),
                ));
                flatten(c, "QueryTimeStats.", &mut stmt.properties);
            }
            other => flatten(c, &format!("{other}."), &mut stmt.properties),
        }
    }
}

fn finish_statement(stmt: &mut Statement) {
    if let Some(r) = stmt.root {
        if stmt.subtree_cost <= 0.0 {
            stmt.subtree_cost = stmt.nodes[r].est_subtree_cost;
        }
    }
    stmt.is_actual = stmt.query_time_ms.is_some() || stmt.nodes.iter().any(|n| n.actual.is_some());
    let denom = if stmt.subtree_cost > 0.0 {
        stmt.subtree_cost
    } else {
        stmt.root.map(|r| stmt.nodes[r].est_subtree_cost).unwrap_or(0.0)
    };
    for i in 0..stmt.nodes.len() {
        let own = stmt.own_cost(i);
        stmt.nodes[i].cost_pct = if denom > 0.0 {
            (own / denom * 100.0).clamp(0.0, 100.0)
        } else {
            0.0
        };
    }
}

fn parse_statements(list: &Elem) -> Vec<Statement> {
    list.children.iter().map(parse_statement).collect()
}

fn parse_statement(e: &Elem) -> Statement {
    let kind = match e.name.as_str() {
        "StmtSimple" => StatementKind::Simple,
        "StmtCond" => StatementKind::Cond,
        "StmtCursor" => StatementKind::Cursor,
        "StmtUseDb" => StatementKind::UseDb,
        "StmtReceive" => StatementKind::Receive,
        _ => StatementKind::Other,
    };
    let mut stmt = Statement {
        id: e.attr("StatementId").and_then(|s| s.parse().ok()).unwrap_or(0),
        text: e.attr("StatementText").unwrap_or("").to_string(),
        statement_type: e.attr("StatementType").unwrap_or("").to_string(),
        kind,
        subtree_cost: e.attr_f64("StatementSubTreeCost").unwrap_or(0.0),
        est_rows: e.attr_f64("StatementEstRows"),
        query_hash: e.attr("QueryHash").map(str::to_string),
        plan_hash: e.attr("QueryPlanHash").map(str::to_string),
        optimization_level: e.attr("StatementOptmLevel").map(str::to_string),
        early_abort_reason: e.attr("StatementOptmEarlyAbortReason").map(str::to_string),
        cardinality_estimation_model: e.attr("CardinalityEstimationModelVersion").map(str::to_string),
        ..Default::default()
    };
    stmt.properties.push(("Statement".into(), e.name.clone()));
    for (k, v) in &e.attrs {
        stmt.properties.push((k.clone(), v.clone()));
    }

    for c in &e.children {
        match c.name.as_str() {
            "QueryPlan" => parse_query_plan(c, &mut stmt),
            // StmtCond
            "Condition" => {
                if let Some(qp) = c.child("QueryPlan") {
                    parse_query_plan(qp, &mut stmt);
                }
            }
            "Then" | "Else" => {
                if let Some(list) = c.child("Statements") {
                    for mut child in parse_statements(list) {
                        child.properties.insert(0, ("Branch".into(), c.name.clone()));
                        stmt.children.push(child);
                    }
                }
            }
            // EXEC proc / UDF bodies
            "StoredProc" | "UDF" => {
                for (k, v) in &c.attrs {
                    stmt.properties.push((format!("{}.{k}", c.name), v.clone()));
                }
                if let Some(list) = c.child("Statements") {
                    stmt.children.extend(parse_statements(list));
                }
            }
            // StmtCursor
            "CursorPlan" => {
                flatten_attrs_only(c, "CursorPlan.", &mut stmt.properties);
                let mut first = true;
                for op in c.children_named("Operation") {
                    let op_type = op.attr("OperationType").unwrap_or("").to_string();
                    if first {
                        first = false;
                        stmt.properties.push(("Operation".into(), op_type));
                        if let Some(qp) = op.child("QueryPlan") {
                            parse_query_plan(qp, &mut stmt);
                        }
                    } else {
                        let mut child = Statement {
                            id: stmt.id,
                            text: stmt.text.clone(),
                            statement_type: op_type.clone(),
                            kind: StatementKind::Cursor,
                            ..Default::default()
                        };
                        child.properties.push(("Operation".into(), op_type));
                        if let Some(qp) = op.child("QueryPlan") {
                            parse_query_plan(qp, &mut child);
                        }
                        finish_statement(&mut child);
                        stmt.children.push(child);
                    }
                }
            }
            // A bare nested statement list (StmtReceive etc.)
            "Statements" => stmt.children.extend(parse_statements(c)),
            other => flatten(c, &format!("{other}."), &mut stmt.properties),
        }
    }
    finish_statement(&mut stmt);
    stmt
}

fn flatten_attrs_only(e: &Elem, prefix: &str, out: &mut Vec<(String, String)>) {
    for (k, v) in &e.attrs {
        out.push((format!("{prefix}{k}"), v.clone()));
    }
}

// ───────────────────────────────── entry point ─────────────────────────────────

/// Parse a showplan XML document (from `SET SHOWPLAN_XML ON`, `SET STATISTICS XML ON`,
/// a `.sqlplan` file, or `sys.dm_exec_query_plan`) into a [`Plan`].
///
/// Never panics on unknown structure: unknown elements are folded into `properties`.
pub fn parse(xml: &str) -> Result<Plan, PlanError> {
    let trimmed = xml.trim_start_matches('\u{feff}').trim();
    if trimmed.is_empty() {
        return Err(PlanError::Empty);
    }
    let doc = read_dom(trimmed)?;
    let root = doc
        .children
        .iter()
        .find(|c| c.name == "ShowPlanXML")
        .or_else(|| doc.children.first())
        .ok_or_else(|| PlanError::NotShowplan("no root element".into()))?;
    if root.name != "ShowPlanXML" {
        return Err(PlanError::NotShowplan(format!("root element is <{}>", root.name)));
    }

    let mut plan = Plan {
        source_xml: xml.to_string(),
        version: root.attr("Version").unwrap_or("").to_string(),
        build: root.attr("Build").unwrap_or("").to_string(),
        statements: Vec::new(),
    };
    for seq in root.children_named("BatchSequence") {
        for batch in seq.children_named("Batch") {
            for list in batch.children_named("Statements") {
                plan.statements.extend(parse_statements(list));
            }
        }
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unbracket_strips_one_layer() {
        assert_eq!(unbracket("[dbo]"), "dbo");
        assert_eq!(unbracket("dbo"), "dbo");
        assert_eq!(unbracket("[a]]b]"), "a]b");
        assert_eq!(unbracket(""), "");
    }

    #[test]
    fn numbers_parse_in_showplan_forms() {
        assert_eq!(parse_f64("2e+06"), Some(2_000_000.0));
        assert_eq!(parse_u64("30720"), Some(30720));
        assert_eq!(parse_u64("1.0"), Some(1));
        assert_eq!(parse_u64("x"), None);
        assert!(parse_bool("1") && parse_bool("true") && !parse_bool("0"));
    }

    #[test]
    fn empty_input_is_an_error() {
        assert!(matches!(parse(""), Err(PlanError::Empty)));
        assert!(matches!(parse("\u{feff}  \n"), Err(PlanError::Empty)));
    }

    #[test]
    fn non_showplan_root_is_an_error() {
        assert!(matches!(parse("<foo/>"), Err(PlanError::NotShowplan(_))));
    }

    #[test]
    fn malformed_xml_is_an_error() {
        let r = parse("<ShowPlanXML><BatchSequence><Batch></BatchSequence>");
        assert!(matches!(r, Err(PlanError::Xml { .. })), "{r:?}");
        let r = parse("<ShowPlanXML><BatchSequence>");
        assert!(matches!(r, Err(PlanError::Xml { .. })), "{r:?}");
    }

    #[test]
    fn minimal_document_parses_with_bom_and_unknown_elements() {
        let xml = "\u{feff}\n<ShowPlanXML xmlns=\"http://schemas.microsoft.com/sqlserver/2004/07/showplan\" Version=\"1.5\" Build=\"9\">\
          <BatchSequence><Batch><Statements>\
            <StmtSimple StatementText=\"SELECT 1\" StatementId=\"1\" StatementType=\"SELECT\" StatementSubTreeCost=\"0.5\">\
              <Mystery Foo=\"bar\"><Deeper X=\"1\"/></Mystery>\
              <QueryPlan CachedPlanSize=\"8\">\
                <RelOp NodeId=\"0\" PhysicalOp=\"Compute Scalar\" LogicalOp=\"Compute Scalar\" EstimateRows=\"1\" EstimatedTotalSubtreeCost=\"0.5\" Parallel=\"0\">\
                  <FutureThing Alpha=\"1\"/>\
                  <ComputeScalar>\
                    <RelOp NodeId=\"1\" PhysicalOp=\"Constant Scan\" LogicalOp=\"Constant Scan\" EstimateRows=\"1\" EstimatedTotalSubtreeCost=\"0.1\" Parallel=\"0\">\
                      <ConstantScan/>\
                    </RelOp>\
                  </ComputeScalar>\
                </RelOp>\
              </QueryPlan>\
            </StmtSimple>\
          </Statements></Batch></BatchSequence></ShowPlanXML>";
        let plan = parse(xml).unwrap();
        assert_eq!(plan.version, "1.5");
        assert_eq!(plan.build, "9");
        assert_eq!(plan.statements.len(), 1);
        let s = &plan.statements[0];
        assert_eq!(s.nodes.len(), 2);
        assert_eq!(s.root, Some(0));
        assert_eq!(s.nodes[0].children, vec![1]);
        assert_eq!(s.nodes[1].parent, Some(0));
        assert!(s.properties.iter().any(|(k, v)| k == "Mystery.Foo" && v == "bar"));
        assert!(s.properties.iter().any(|(k, v)| k == "Mystery.Deeper.X" && v == "1"));
        assert!(s.nodes[0].properties.iter().any(|(k, _)| k == "Alpha"));
        assert!((s.nodes[0].cost_pct - 80.0).abs() < 1e-9);
        assert!((s.nodes[1].cost_pct - 20.0).abs() < 1e-9);
        assert!(!s.is_actual);
    }

    #[test]
    fn column_ref_rendering() {
        let e = Elem {
            name: "ColumnReference".into(),
            attrs: vec![
                ("Database".into(), "[db]".into()),
                ("Schema".into(), "[dbo]".into()),
                ("Table".into(), "[big]".into()),
                ("Alias".into(), "[b]".into()),
                ("Column".into(), "id".into()),
            ],
            children: vec![],
        };
        assert_eq!(fmt_column_ref(&e), "[db].[dbo].[big].[id] as [b].[id]");
        let bare = Elem { name: "ColumnReference".into(), attrs: vec![("Column".into(), "Expr1004".into())], children: vec![] };
        assert_eq!(fmt_column_ref(&bare), "Expr1004");
    }
}
