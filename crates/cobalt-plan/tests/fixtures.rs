//! Fixture-driven tests: every `.sqlplan` under `tests/fixtures` was captured from the
//! `cobalt-mssql` container (see `tests/fixtures/README.md` for the exact SQL).

use cobalt_plan::*;
use std::path::PathBuf;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn load(name: &str) -> Plan {
    let path = fixture_dir().join(format!("{name}.sqlplan"));
    let xml = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    parse(&xml).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn all_fixture_names() -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(fixture_dir())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "sqlplan"))
        .map(|p| p.file_stem().unwrap().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

fn ops(stmt: &Statement) -> Vec<&str> {
    stmt.nodes.iter().map(|n| n.physical_op.as_str()).collect()
}

fn root_op(stmt: &Statement) -> &str {
    &stmt.nodes[stmt.root.expect("root")].physical_op
}

// ───────────────────────────── corpus-wide invariants ─────────────────────────────

#[test]
fn corpus_has_at_least_the_required_fixtures() {
    let names = all_fixture_names();
    assert!(names.len() >= 12, "{names:?}");
    for required in [
        "simple_seek",
        "hash_join_aggregate",
        "nested_loops_seek",
        "key_lookup_actual",
        "parallel",
        "batch_three",
        "actual_sort",
        "missing_index",
        "implicit_convert",
        "sort_spill_actual",
        "exec_p_multi",
        "stmt_cond",
        "cursor",
    ] {
        assert!(names.iter().any(|n| n == required), "missing fixture {required}");
    }
}

#[test]
fn every_fixture_parses_with_version_and_build() {
    for name in all_fixture_names() {
        let plan = load(&name);
        assert_eq!(plan.version, "1.564", "{name}");
        assert!(plan.build.starts_with("16."), "{name}: {}", plan.build);
        assert!(!plan.statements.is_empty(), "{name}");
        assert!(plan.source_xml.contains("<ShowPlanXML"), "{name}");
    }
}

#[test]
fn every_tree_is_consistent() {
    for name in all_fixture_names() {
        let plan = load(&name);
        for s in plan.all_statements() {
            if let Some(r) = s.root {
                assert_eq!(r, 0, "{name}: root is the first arena entry");
                assert!(s.nodes[r].parent.is_none());
                assert_eq!(s.preorder().len(), s.nodes.len(), "{name}: every node reachable");
            }
            for n in &s.nodes {
                assert_eq!(n.index, s.nodes.iter().position(|m| std::ptr::eq(m, n)).unwrap());
                for &c in &n.children {
                    assert_eq!(s.nodes[c].parent, Some(n.index), "{name}");
                }
                if let Some(p) = n.parent {
                    assert!(s.nodes[p].children.contains(&n.index), "{name}");
                }
                assert!(!n.physical_op.is_empty(), "{name}");
                assert!(n.est_subtree_cost >= 0.0);
                assert!(n.properties.iter().any(|(k, _)| k == "NodeId"), "{name}: raw attrs kept");
            }
        }
    }
}

#[test]
fn cost_pct_sums_to_100_per_statement() {
    for name in all_fixture_names() {
        let plan = load(&name);
        for s in plan.all_statements().into_iter().filter(|s| s.has_plan()) {
            let sum: f64 = s.nodes.iter().map(|n| n.cost_pct).sum();
            assert!((sum - 100.0).abs() < 1.0, "{name} stmt {}: cost_pct sum {sum}", s.id);
            for n in &s.nodes {
                assert!((0.0..=100.0).contains(&n.cost_pct), "{name}");
            }
        }
    }
}

#[test]
fn layout_has_no_overlaps_for_any_fixture() {
    let opts = LayoutOptions::default();
    for name in all_fixture_names() {
        let plan = load(&name);
        for s in plan.all_statements() {
            let l = layout(s, &opts);
            assert_eq!(l.rects.len(), s.nodes.len(), "{name} stmt {}", s.id);
            for (i, (_, a)) in l.rects.iter().enumerate() {
                for (_, b) in &l.rects[i + 1..] {
                    assert!(!a.intersects(b), "{name} stmt {}: overlap {a:?} {b:?}", s.id);
                }
            }
            if let Some(r) = s.root {
                let rr = l.rect_of(r).unwrap();
                assert_eq!(rr.x, 0.0, "{name}: root at the left edge");
            }
            for e in &l.edges {
                let (pr, cr) = (l.rect_of(e.from).unwrap(), l.rect_of(e.to).unwrap());
                assert!(cr.x > pr.x, "{name}: children are right of parents");
                assert!(e.rows >= 0.0);
            }
            assert_eq!(l.edges.len(), s.nodes.len().saturating_sub(usize::from(s.root.is_some())));
            for (_, r) in &l.rects {
                assert!(r.right() <= l.size.0 + 0.01 && r.bottom() <= l.size.1 + 0.01, "{name}");
            }
        }
    }
}

#[test]
fn layout_is_deterministic() {
    let plan = load("hash_join_aggregate");
    let a = layout(&plan.statements[0], &LayoutOptions::default());
    let b = layout(&plan.statements[0], &LayoutOptions::default());
    assert_eq!(a, b);
}

#[test]
fn every_node_gets_an_icon_that_is_not_other() {
    for name in all_fixture_names() {
        let plan = load(&name);
        for s in plan.all_statements() {
            for n in &s.nodes {
                let icon = icon_for(&n.physical_op, &n.logical_op, n.object.as_ref());
                assert_ne!(icon, OpIcon::Other, "{name}: {} / {}", n.physical_op, n.logical_op);
            }
        }
    }
}

#[test]
fn plan_to_json_round_trips_the_corpus() {
    for name in all_fixture_names() {
        let plan = load(&name);
        let j = plan_to_json(&plan);
        assert_eq!(j["version"], "1.564");
        let stmts = j["statements"].as_array().unwrap();
        assert_eq!(stmts.len(), plan.statements.len(), "{name}");
        for (js, s) in stmts.iter().zip(&plan.statements) {
            assert_eq!(js["nodes"].as_array().unwrap().len(), s.nodes.len());
        }
        // Must serialize without panicking and stay compact-ish.
        let text = serde_json::to_string(&j).unwrap();
        assert!(text.len() < plan.source_xml.len() * 2, "{name}");
    }
}

#[test]
fn statement_summary_is_one_line_for_the_corpus() {
    for name in all_fixture_names() {
        let plan = load(&name);
        for s in plan.all_statements() {
            let summary = statement_summary(s);
            assert!(!summary.is_empty() && !summary.contains('\n'), "{name}: {summary:?}");
            if s.has_plan() {
                assert!(summary.contains("cost "), "{name}: {summary}");
            }
        }
    }
}

// ───────────────────────────── individual fixtures ─────────────────────────────

#[test]
fn simple_seek() {
    let plan = load("simple_seek");
    assert_eq!(plan.statements.len(), 1);
    let s = &plan.statements[0];
    assert_eq!(s.kind, StatementKind::Simple);
    assert_eq!(s.statement_type, "SELECT");
    assert_eq!(s.id, 1);
    assert_eq!(s.nodes.len(), 1);
    assert_eq!(root_op(s), "Clustered Index Seek");
    let n = &s.nodes[0];
    assert!((n.cost_pct - 100.0).abs() < 1e-6);
    assert!(!n.parallel);
    assert_eq!(n.execution_mode.as_deref(), Some("Row"));
    let o = n.object.as_ref().unwrap();
    assert_eq!(o.database.as_deref(), Some("cobalt_test"));
    assert_eq!(o.schema.as_deref(), Some("dbo"));
    assert_eq!(o.table.as_deref(), Some("big"));
    assert_eq!(o.index_kind.as_deref(), Some("Clustered"));
    assert!(o.index.as_deref().unwrap().starts_with("PK__big__"));
    assert_eq!(o.table_name(), "dbo.big");
    let sp = n.seek_predicates.as_deref().expect("seek predicate");
    assert!(sp.starts_with("Seek Keys[1]: Prefix: "), "{sp}");
    assert!(sp.contains("[cobalt_test].[dbo].[big].[id] = Scalar Operator("), "{sp}");
    assert_eq!(n.output_list.len(), 2);
    assert_eq!(n.output_list[0], "[cobalt_test].[dbo].[big].[id]");
    assert_eq!(s.parameters.len(), 1);
    assert_eq!(s.parameters[0].name, "@1");
    assert_eq!(s.parameters[0].data_type, "tinyint");
    assert_eq!(s.parameters[0].compiled_value.as_deref(), Some("(42)"));
    assert!(s.parameters[0].runtime_value.is_none());
    assert!(!s.is_actual);
    assert!(s.query_hash.is_some() && s.plan_hash.is_some());
    assert_eq!(s.optimization_level.as_deref(), Some("TRIVIAL"));
    assert!(s.cached_plan_size_kb.is_some());
    assert!(n.property("Ordered").is_some(), "op-element attrs are flattened to the top level");
    assert_eq!(n.property("Object.Table"), Some("[big]"));
    assert!(statement_summary(s).starts_with("Clustered Index Seek on dbo.big (PK__big__"));
}

#[test]
fn hash_join_aggregate() {
    let plan = load("hash_join_aggregate");
    let s = &plan.statements[0];
    assert_eq!(s.nodes.len(), 7);
    assert_eq!(root_op(s), "Hash Match");
    assert_eq!(s.nodes[0].logical_op, "Inner Join");
    let hash_matches: Vec<&Node> = s.nodes.iter().filter(|n| n.physical_op == "Hash Match").collect();
    assert_eq!(hash_matches.len(), 3);
    assert!(hash_matches.iter().any(|n| n.logical_op == "Aggregate"));
    assert_eq!(s.nodes[0].children.len(), 2, "hash join has build + probe");
    assert!(s.nodes[0].property("HashKeysBuild").is_some());
    assert!(s.nodes[0].property("HashKeysProbe").is_some());
    assert_eq!(s.degree_of_parallelism, None);
    assert_eq!(s.non_parallel_reason.as_deref(), Some("MaxDOPSetToOne"));
    assert!(s.nodes.iter().all(|n| !n.parallel));
    let scans: Vec<&Node> = s.nodes.iter().filter(|n| n.physical_op == "Clustered Index Scan").collect();
    assert_eq!(scans.len(), 2);
    assert!(scans.iter().any(|n| n.predicate.as_deref().is_some_and(|p| p.contains("amount"))));
    assert_eq!(s.missing_indexes.len(), 1);
    let mi = &s.missing_indexes[0];
    assert_eq!(mi.inequality_columns, vec!["amount"]);
    assert_eq!(mi.included_columns, vec!["category"]);
    assert!(icon_for(&s.nodes[0].physical_op, &s.nodes[0].logical_op, None) == OpIcon::HashMatch);
    // Top operations: the two 2M-row scans are the most expensive.
    let top = s.top_operations();
    assert_eq!(top.len(), 7);
    assert_eq!(s.nodes[top[0]].physical_op, "Clustered Index Scan");
    assert_eq!(s.nodes[top[1]].physical_op, "Clustered Index Scan");
    assert!(s.own_cost(top[0]) >= s.own_cost(top[6]));
}

#[test]
fn nested_loops_seek() {
    let plan = load("nested_loops_seek");
    let s = &plan.statements[0];
    assert_eq!(s.nodes.len(), 5);
    assert_eq!(root_op(s), "Top");
    let nl = s.nodes.iter().find(|n| n.physical_op == "Nested Loops").unwrap();
    assert_eq!(nl.children.len(), 2);
    assert_eq!(nl.logical_op, "Inner Join");
    assert!(nl.property("OuterReferences").is_some(), "{:?}", nl.properties);
    let seeks: Vec<&Node> = s.nodes.iter().filter(|n| n.physical_op == "Clustered Index Seek").collect();
    assert_eq!(seeks.len(), 2);
    assert!(seeks.iter().all(|n| n.seek_predicates.is_some()));
    let range = seeks.iter().find(|n| n.seek_predicates.as_deref().unwrap().contains("Start:")).unwrap();
    let sp = range.seek_predicates.as_deref().unwrap();
    assert!(sp.contains(">= Scalar Operator(") && sp.contains("End:") && sp.contains("<= Scalar Operator("), "{sp}");
    assert_eq!(s.depth(nl.index), 1);
    assert_eq!(s.depth(seeks[1].index), 2);
    assert_eq!(s.depth(0), 0);
    assert!(s.est_rows.is_some());
}

#[test]
fn key_lookup_actual() {
    let plan = load("key_lookup_actual");
    assert!(plan.is_actual());
    let s = &plan.statements[0];
    assert!(s.is_actual);
    assert_eq!(ops(s), vec!["Top", "Nested Loops", "Index Seek", "Key Lookup"]);
    let lookup = &s.nodes[3];
    assert!(lookup.is_lookup());
    assert_eq!(lookup.property("PhysicalOp"), Some("Clustered Index Seek"), "raw op kept");
    assert_eq!(lookup.property("Lookup"), Some("1"));
    assert_eq!(icon_for(&lookup.physical_op, &lookup.logical_op, lookup.object.as_ref()), OpIcon::KeyLookup);
    assert_eq!(lookup.object.as_ref().unwrap().index_kind.as_deref(), Some("Clustered"));
    let seek = &s.nodes[2];
    assert_eq!(seek.object.as_ref().unwrap().index.as_deref(), Some("IX_tmp_big_cat"));
    assert_eq!(s.nodes[1].children, vec![2, 3]);
    let a = lookup.actual.as_ref().unwrap();
    assert!(a.executions >= 20, "lookup executes once per outer row: {a:?}");
    assert_eq!(a.rows, 20);
    assert_eq!(s.total_actual_rows(), Some(20));
    assert_eq!(s.most_expensive(Metric::ActualRows).map(|i| &s.nodes[i].physical_op), Some(&"Index Seek".to_string()));
    let summary = statement_summary(s);
    assert!(summary.contains("20 rows (est"), "{summary}");
}

#[test]
fn parallel_plan() {
    let plan = load("parallel");
    let s = &plan.statements[0];
    assert_eq!(s.nodes.len(), 4);
    assert_eq!(root_op(s), "Parallelism");
    assert_eq!(s.nodes[0].logical_op, "Gather Streams");
    assert!(s.nodes.iter().all(|n| n.parallel), "every operator is flagged Parallel in this plan");
    // Estimated plans carry no DegreeOfParallelism on QueryPlan; the optimizer's view is a property.
    assert_eq!(s.degree_of_parallelism, None);
    assert!(s
        .properties
        .iter()
        .any(|(k, v)| k == "OptimizerHardwareDependentProperties.EstimatedAvailableDegreeOfParallelism" && v == "8"));
    assert_eq!(icon_for("Parallelism", "Gather Streams", None).category(), IconCategory::Parallel);
    assert!(s.nodes.iter().any(|n| n.property("MemoryFractions.Input").is_some()));
    let agg = s.nodes.iter().find(|n| n.logical_op == "Aggregate").unwrap();
    assert_eq!(icon_for(&agg.physical_op, &agg.logical_op, None), OpIcon::HashAggregate);
}

#[test]
fn batch_of_three_statements() {
    let plan = load("batch_three");
    assert_eq!(plan.statements.len(), 3);
    let ids: Vec<i32> = plan.statements.iter().map(|s| s.id).collect();
    assert_eq!(ids, vec![1, 2, 3]);
    assert!(plan.statements.iter().all(|s| s.has_plan()));
    assert_eq!(root_op(&plan.statements[0]), "Clustered Index Seek");
    assert_eq!(root_op(&plan.statements[1]), "Top");
    assert!(plan.statements[2].nodes.iter().any(|n| n.physical_op == "Filter"));
    assert!(plan.statements[2].nodes.iter().any(|n| n.physical_op == "Hash Match"));
    let total: usize = plan.statements.iter().map(|s| s.nodes.len()).sum();
    assert_eq!(total, 8);
    assert!(plan.statements[1].text.contains("TOP (5)"));
    assert!(plan.total_cost() > 0.0);
    assert_eq!(plan.all_statements().len(), 3);
}

#[test]
fn actual_plan_has_runtime_information() {
    let plan = load("actual_sort");
    let s = &plan.statements[0];
    assert!(s.is_actual);
    assert_eq!(ops(s), vec!["Top", "Parallelism", "Sort", "Clustered Index Scan"]);
    assert_eq!(s.degree_of_parallelism, Some(16));
    assert_eq!(s.memory_grant_kb, Some(30720));
    assert_eq!(s.query_time_ms, Some((242, 23)));
    assert_eq!(s.wait_stats.len(), 4);
    assert_eq!(s.wait_stats[0].wait_type, "CXSYNC_PORT");
    assert_eq!(s.wait_stats[0].wait_time_ms, 121);
    assert_eq!(s.wait_stats[0].wait_count, 17);
    assert_eq!(s.total_actual_rows(), Some(1000));
    let scan = &s.nodes[3];
    let a = scan.actual.as_ref().unwrap();
    assert_eq!(a.threads, 17);
    assert_eq!(a.rows, 117_647, "CAT3 rows summed across threads");
    assert!(a.logical_reads.unwrap() > 10_000);
    assert!(a.scans.unwrap() >= 16);
    assert!(a.elapsed_ms <= 23 && a.elapsed_ms > 0);
    assert!(a.cpu_ms >= a.elapsed_ms, "cpu is summed across threads");
    assert_eq!(a.execution_mode.as_deref(), Some("Row"));
    assert!(scan.property("RunTimeInformation[16].ActualRows").is_some());
    assert_eq!(scan.property("ActualRows"), Some("117647"));
    let sort = &s.nodes[2];
    assert_eq!(sort.logical_op, "TopN Sort");
    assert!(sort.actual.as_ref().unwrap().rows >= 1000);
    assert_eq!(s.most_expensive(Metric::ActualElapsed), Some(0));
    assert_eq!(s.most_expensive(Metric::RowsRead), Some(3));
    // Sorting 117k rows on 16 threads costs more summed CPU than the scan itself.
    assert_eq!(s.most_expensive(Metric::ActualCpu), Some(2));
    assert_eq!(s.missing_indexes.len(), 1);
    assert!(s.properties.iter().any(|(k, _)| k == "MemoryGrantInfo.GrantedMemory"));
    assert!(s.properties.iter().any(|(k, _)| k == "OptimizerHardwareDependentProperties.EstimatedAvailableDegreeOfParallelism"));
    let l = layout(s, &LayoutOptions::default());
    let e = l.edges.iter().find(|e| e.to == 3).unwrap();
    assert_eq!(e.actual_rows, Some(117_647.0));
    assert_eq!(e.rows, 117_647.0);
    let j = plan_to_json(&plan);
    assert_eq!(j["actual"], true);
    assert_eq!(j["statements"][0]["actual_rows"], 1000);
    assert_eq!(j["statements"][0]["nodes"][3]["actual_rows"], 117_647);
}

#[test]
fn missing_index_recommendation_and_sql() {
    let plan = load("missing_index");
    let s = &plan.statements[0];
    assert_eq!(s.missing_indexes.len(), 1);
    let mi = &s.missing_indexes[0];
    assert!(mi.impact > 50.0, "{}", mi.impact);
    assert_eq!(mi.database, "cobalt_test");
    assert_eq!(mi.schema, "dbo");
    assert_eq!(mi.table, "big");
    assert_eq!(mi.equality_columns, vec!["amount"]);
    assert_eq!(mi.inequality_columns, vec!["created"]);
    assert!(mi.included_columns.is_empty());
    assert_eq!(
        mi.create_index_sql(),
        "CREATE NONCLUSTERED INDEX [IX_big_amount_created] ON [dbo].[big] ([amount], [created]);"
    );
    assert!(statement_summary(s).ends_with("; missing index"));
    let j = plan_to_json(&plan);
    assert!(j["statements"][0]["missing_indexes"][0]["sql"].as_str().unwrap().starts_with("CREATE NONCLUSTERED INDEX"));
}

#[test]
fn missing_index_sql_with_includes() {
    let plan = load("actual_sort");
    let mi = &plan.statements[0].missing_indexes[0];
    assert_eq!(mi.equality_columns, vec!["category"]);
    assert_eq!(mi.included_columns, vec!["amount", "note"]);
    assert_eq!(
        mi.create_index_sql(),
        "CREATE NONCLUSTERED INDEX [IX_big_category] ON [dbo].[big] ([category]) INCLUDE ([amount], [note]);"
    );
}

#[test]
fn implicit_conversion_warning() {
    let plan = load("implicit_convert");
    let s = &plan.statements[0];
    let converts: Vec<&Warning> = s
        .all_warnings()
        .into_iter()
        .map(|(_, w)| w)
        .filter(|w| w.kind == WarningKind::PlanAffectingConvert)
        .collect();
    assert_eq!(converts.len(), 2);
    assert!(converts.iter().any(|w| w.detail.starts_with("Cardinality Estimate: CONVERT_IMPLICIT")), "{converts:?}");
    assert!(converts.iter().any(|w| w.detail.starts_with("Seek Plan: ")));
    assert_eq!(s.warnings.len(), 2, "these are statement-level (QueryPlan/Warnings)");
    assert!(statement_summary(s).contains("2 warnings"));
    let scan = s.nodes.iter().find(|n| n.physical_op == "Clustered Index Scan").unwrap();
    assert!(scan.predicate.as_deref().unwrap().contains("CONVERT_IMPLICIT"));
    let j = plan_to_json(&plan);
    assert_eq!(j["statements"][0]["warnings"].as_array().unwrap().len(), 2);
}

#[test]
fn sort_spill_and_memory_grant_warnings() {
    let plan = load("sort_spill_actual");
    let s = &plan.statements[0];
    assert!(s.is_actual);
    assert_eq!(ops(s), vec!["Hash Match", "Window Aggregate", "Sort", "Clustered Index Scan"]);
    let sort = &s.nodes[2];
    assert_eq!(sort.warnings.len(), 1, "{:?}", sort.warnings);
    assert_eq!(sort.warnings[0].kind, WarningKind::SortSpill);
    assert!(sort.warnings[0].detail.contains("SpillLevel=8"), "{}", sort.warnings[0].detail);
    assert!(sort.warnings[0].detail.contains("WritesToTempDb="), "{}", sort.warnings[0].detail);
    assert!(sort.property("Warnings.SpillToTempDb.SpillLevel").is_some());
    assert!(s.warnings.iter().any(|w| w.kind == WarningKind::MemoryGrant));
    assert_eq!(s.all_warnings().len(), 2);
    assert_eq!(s.total_actual_rows(), Some(1));
    assert_eq!(s.nodes[3].actual.as_ref().unwrap().rows, 2_000_000);
    assert_eq!(s.non_parallel_reason.as_deref(), Some("MaxDOPSetToOne"));
    assert_eq!(icon_for(&s.nodes[1].physical_op, &s.nodes[1].logical_op, None), OpIcon::WindowAggregate);
}

#[test]
fn exec_proc_yields_the_procs_statements() {
    let plan = load("exec_p_multi");
    assert_eq!(plan.statements.len(), 1);
    let exec = &plan.statements[0];
    assert_eq!(exec.statement_type, "EXECUTE PROC");
    assert!(!exec.has_plan());
    assert_eq!(exec.children.len(), 6);
    assert!(exec.properties.iter().any(|(k, v)| k == "StoredProc.ProcName" && v == "dbo.p_multi"));
    let types: Vec<&str> = exec.children.iter().map(|c| c.statement_type.as_str()).collect();
    assert_eq!(types, vec!["SET ON/OFF", "PRINT", "SELECT", "RAISERROR", "SELECT", "PRINT"]);
    let ids: Vec<i32> = exec.children.iter().map(|c| c.id).collect();
    assert_eq!(ids, vec![2, 3, 4, 5, 6, 7]);
    let selects: Vec<&Statement> = exec.children.iter().filter(|c| c.has_plan()).collect();
    assert_eq!(selects.len(), 2);
    assert_eq!(root_op(selects[0]), "Top");
    assert_eq!(root_op(selects[1]), "Parallelism");
    assert!(selects[0].text.contains("TOP (@n)"));
    assert_eq!(plan.all_statements().len(), 7);
    let top = &selects[0].nodes[0];
    assert!(top.property("TopExpression").is_some_and(|v| v.contains("[@n]")), "{:?}", top.properties);
    let j = plan_to_json(&plan);
    assert_eq!(j["statements"][0]["children"].as_array().unwrap().len(), 6);
}

#[test]
fn if_else_gives_condition_plan_and_branches() {
    let plan = load("stmt_cond");
    assert_eq!(plan.statements.len(), 1);
    let cond = &plan.statements[0];
    assert_eq!(cond.kind, StatementKind::Cond);
    assert_eq!(cond.statement_type, "COND WITH QUERY");
    assert!(cond.has_plan(), "the IF condition's plan");
    assert_eq!(root_op(cond), "Compute Scalar");
    assert!(cond.nodes.iter().any(|n| n.physical_op == "Constant Scan"));
    assert_eq!(cond.children.len(), 2);
    assert_eq!(cond.children[0].properties[0], ("Branch".to_string(), "Then".to_string()));
    assert_eq!(cond.children[1].properties[0], ("Branch".to_string(), "Else".to_string()));
    assert!(cond.children.iter().all(|c| c.has_plan()));
    assert_eq!(root_op(&cond.children[0]), "Top");
    assert_eq!(root_op(&cond.children[1]), "Top");
    let total: usize = plan.all_statements().iter().map(|s| s.nodes.len()).sum();
    assert_eq!(total, 8);
}

#[test]
fn cursor_statements() {
    let plan = load("cursor");
    assert_eq!(plan.statements.len(), 5);
    assert!(plan.statements.iter().all(|s| s.kind == StatementKind::Cursor));
    let declare = &plan.statements[0];
    assert_eq!(declare.statement_type, "DECLARE CURSOR");
    assert!(declare.has_plan());
    assert_eq!(root_op(declare), "Clustered Index Seek");
    assert_eq!(declare.non_parallel_reason.as_deref(), Some("NoParallelFastForwardCursor"));
    assert!(declare.properties.iter().any(|(k, v)| k == "CursorPlan.CursorName" && v == "c"));
    assert!(declare.properties.iter().any(|(k, v)| k == "Operation" && v == "FetchQuery"));
    assert!(plan.statements[1..].iter().all(|s| !s.has_plan()));
    assert_eq!(plan.statements[4].statement_type, "DEALLOCATE CURSOR");
    assert_eq!(layout(&plan.statements[1], &LayoutOptions::default()).rects.len(), 0);
    let sp = declare.nodes[0].seek_predicates.as_deref().unwrap();
    assert!(sp.contains("End: [cobalt_test].[dbo].[big].[id] < Scalar Operator((10))"), "{sp}");
}

// ───────────────────────────── helper behaviour on real plans ─────────────────────────────

#[test]
fn find_nodes_is_case_insensitive_over_ops_objects_and_predicates() {
    let plan = load("hash_join_aggregate");
    let s = &plan.statements[0];
    assert_eq!(s.find_nodes("hash match").len(), 3);
    assert_eq!(s.find_nodes("HASH").len(), 3);
    assert_eq!(s.find_nodes("aggregate").len(), 2, "logical op of the two hash aggregates");
    assert_eq!(s.find_nodes("dbo.big").len(), 2, "object names");
    assert!(!s.find_nodes("14000").is_empty(), "predicate text");
    assert!(s.find_nodes("").is_empty());
    assert!(s.find_nodes("no such thing").is_empty());
    let seek = load("simple_seek");
    // Auto-parameterised: the seek predicate references [@1] (compiled value (42) is in ParameterList).
    assert_eq!(seek.statements[0].find_nodes("[@1]").len(), 1, "seek predicate text");
    assert_eq!(seek.statements[0].find_nodes("convert_implicit").len(), 1);
}

#[test]
fn most_expensive_by_each_metric() {
    let plan = load("actual_sort");
    let s = &plan.statements[0];
    assert_eq!(s.most_expensive(Metric::Cost), Some(3), "the scan dominates");
    assert_eq!(s.most_expensive(Metric::SubtreeCost), Some(0), "the root has the full subtree");
    assert_eq!(s.most_expensive(Metric::EstRows), Some(3));
    assert_eq!(s.most_expensive(Metric::ActualRows), Some(3));
    let est = load("simple_seek");
    assert_eq!(est.statements[0].most_expensive(Metric::ActualRows), None, "no actuals in an estimated plan");
    assert_eq!(est.statements[0].most_expensive(Metric::Cost), Some(0));
    let empty = Statement::default();
    assert_eq!(empty.most_expensive(Metric::Cost), None);
    assert!(empty.top_operations().is_empty());
}

#[test]
fn top_operations_orders_by_own_cost_desc() {
    let plan = load("actual_sort");
    let s = &plan.statements[0];
    let top = s.top_operations();
    assert_eq!(top.len(), 4);
    for w in top.windows(2) {
        assert!(s.own_cost(w[0]) >= s.own_cost(w[1]));
    }
    assert_eq!(top[0], 3);
    assert!((s.nodes[3].cost_pct - 89.0).abs() < 2.0, "{}", s.nodes[3].cost_pct);
}

#[test]
fn source_xml_survives_bom_and_whitespace_wrapping() {
    let raw = std::fs::read_to_string(fixture_dir().join("simple_seek.sqlplan")).unwrap();
    let wrapped = format!("\u{feff}\r\n\t  {raw}\n\n");
    let plan = parse(&wrapped).unwrap();
    assert_eq!(plan.statements.len(), 1);
    assert_eq!(plan.source_xml, wrapped, "source is kept verbatim");
}

#[test]
fn truncated_fixture_is_an_error_not_a_panic() {
    let raw = std::fs::read_to_string(fixture_dir().join("hash_join_aggregate.sqlplan")).unwrap();
    for cut in [10usize, 100, 1000, raw.len() / 2, raw.len() - 20] {
        let r = parse(&raw[..cut]);
        assert!(r.is_err(), "cut at {cut} should fail");
    }
}

#[test]
fn model_is_serde_round_trippable() {
    let plan = load("key_lookup_actual");
    let text = serde_json::to_string(&plan.statements[0]).unwrap();
    let back: Statement = serde_json::from_str(&text).unwrap();
    assert_eq!(back.nodes.len(), plan.statements[0].nodes.len());
    assert_eq!(back.nodes[3].physical_op, "Key Lookup");
    assert_eq!(back.nodes[3].actual, plan.statements[0].nodes[3].actual);
}
