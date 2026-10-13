//! The model explorer: a semantic model's tables (columns, measures, hierarchies) and
//! relationships as a tree, drawn under the model's row in the Fabric panel. Rows insert DAX
//! into the active DAX tab or run quick queries. Design: `docs/design/semantic_models.md` §3.1.

use super::fabric::tree_row;
use super::theme::Theme;
use crate::state::Loadable;
use cobalt_core::tabular::{dax_column_ref, dax_measure_ref, dax_table_ref, Column, Measure, ModelCatalog, Table, TabularTarget};
use egui::{RichText, Ui};
use egui_phosphor::regular as ph;
use std::collections::HashSet;
use std::sync::Arc;

/// What a model tree row asks for.
#[derive(Clone, Debug)]
pub enum ModelAction {
    /// Open (or reuse) a DAX tab with this text; run it when `run`.
    Query { target: TabularTarget, text: String, run: bool },
    /// Insert at the caret of the active DAX tab on the model, else open one.
    Insert { target: TabularTarget, text: String },
    Copy(String),
    Refresh { target: TabularTarget },
    ToggleHidden,
    /// Open or close a tree node (`<key>/tables`, `<key>/t/<table>` …).
    ToggleNode(String),
}

const MAX_HOVER_EXPR: usize = 600;

/// Draw the model explorer for `target` at `depth`. `expanded` holds open node ids.
pub fn show(ui: &mut Ui, theme: &Theme, target: &TabularTarget, catalog: Option<&Loadable<Arc<ModelCatalog>>>, expanded: &HashSet<String>, show_hidden: bool, depth: usize, actions: &mut Vec<ModelAction>) {
    let key = target.key();
    match catalog {
        Some(Loadable::Loaded(cat)) => tree(ui, theme, target, cat, &key, expanded, show_hidden, depth, actions),
        Some(Loadable::Failed(e)) => {
            ui.horizontal(|ui| {
                ui.add_space(14.0 * depth as f32 + 8.0);
                ui.label(RichText::new(format!("{} {}", ph::WARNING, first_line(e))).size(12.0).color(theme.warning)).on_hover_text(e);
                if ui.small_button("Retry").clicked() {
                    actions.push(ModelAction::Refresh { target: target.clone() });
                }
            });
        }
        _ => {
            ui.horizontal(|ui| {
                ui.add_space(14.0 * depth as f32 + 8.0);
                ui.spinner();
                ui.label(RichText::new("loading the model…").size(12.0).color(theme.text_muted));
            });
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
        }
    }
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").chars().take(90).collect()
}

#[allow(clippy::too_many_arguments)]
fn tree(ui: &mut Ui, theme: &Theme, target: &TabularTarget, cat: &ModelCatalog, key: &str, expanded: &HashSet<String>, show_hidden: bool, depth: usize, actions: &mut Vec<ModelAction>) {
    let tables: Vec<&Table> = cat.tables.iter().filter(|t| show_hidden || !t.hidden).collect();
    let hidden_tables = cat.tables.len() - tables.len();
    // Tables folder
    let tid = format!("{key}/tables");
    let open = expanded.contains(&tid);
    let sub = if hidden_tables > 0 && !show_hidden { format!("{} · {hidden_tables} hidden", tables.len()) } else { tables.len().to_string() };
    let r = tree_row(ui, theme, depth, open, if open { ph::CARET_DOWN } else { ph::CARET_RIGHT }, "Tables", &sub, &format!("model tables {}", target.model()));
    if r.clicked() {
        actions.push(ModelAction::ToggleNode(tid.clone()));
    }
    r.context_menu(|ui| root_menu(ui, target, cat, show_hidden, actions));
    if open {
        for t in &tables {
            table_node(ui, theme, target, cat, key, t, expanded, show_hidden, depth + 1, actions);
        }
    }
    // Relationships folder
    let rid = format!("{key}/rel");
    let open = expanded.contains(&rid);
    let active = cat.relationships.iter().filter(|r| r.active).count();
    let sub = if active == cat.relationships.len() { cat.relationships.len().to_string() } else { format!("{} · {active} active", cat.relationships.len()) };
    let r = tree_row(ui, theme, depth, open, if open { ph::CARET_DOWN } else { ph::CARET_RIGHT }, "Relationships", &sub, &format!("model relationships {}", target.model()));
    if r.clicked() {
        actions.push(ModelAction::ToggleNode(rid.clone()));
    }
    if open {
        for rel in &cat.relationships {
            let label = format!("{}[{}] {} {}[{}]", rel.from_table, rel.from_column, if rel.cross_filter.to_ascii_lowercase().contains("both") { ph::ARROWS_LEFT_RIGHT } else { ph::ARROW_RIGHT }, rel.to_table, rel.to_column);
            let card = format!("{}:{}", short_card(&rel.from_cardinality), short_card(&rel.to_cardinality));
            let sub = if rel.active { card } else { format!("{card} · inactive") };
            let icon = if rel.active { ph::LINK } else { ph::LINK_BREAK };
            let r = tree_row(ui, theme, depth + 1, false, icon, &label, &sub, &format!("relationship {label}"));
            r.on_hover_text(format!("{} ({}) → {} ({})\ncross filter: {}\n{}", rel.from_table, rel.from_cardinality, rel.to_table, rel.to_cardinality, rel.cross_filter, if rel.active { "active" } else { "inactive (USERELATIONSHIP activates it in a query)" }))
                .context_menu(|ui| {
                    if ui.button("Insert USERELATIONSHIP(…)").clicked() {
                        actions.push(ModelAction::Insert { target: target.clone(), text: format!("USERELATIONSHIP({}, {})", dax_column_ref(&rel.from_table, &rel.from_column), dax_column_ref(&rel.to_table, &rel.to_column)) });
                        ui.close();
                    }
                });
        }
        if cat.relationships.is_empty() {
            muted(ui, theme, depth + 1, "no relationships");
        }
    }
    if !cat.calculation_groups.is_empty() {
        let cid = format!("{key}/cg");
        let open = expanded.contains(&cid);
        let r = tree_row(ui, theme, depth, open, if open { ph::CARET_DOWN } else { ph::CARET_RIGHT }, "Calculation groups", &cat.calculation_groups.len().to_string(), &format!("model calculation groups {}", target.model()));
        if r.clicked() {
            actions.push(ModelAction::ToggleNode(cid.clone()));
        }
        if open {
            for g in &cat.calculation_groups {
                let r = tree_row(ui, theme, depth + 1, false, ph::CALCULATOR, &g.table, &format!("{} items", g.items.len()), &format!("calculation group {}", g.table));
                r.on_hover_text(g.items.join("\n"));
            }
        }
    }
}

fn short_card(c: &str) -> &'static str {
    if c.to_ascii_lowercase().starts_with("many") { "*" } else { "1" }
}

fn muted(ui: &mut Ui, theme: &Theme, depth: usize, text: &str) {
    ui.horizontal(|ui| {
        ui.add_space(14.0 * depth as f32 + 8.0);
        ui.label(RichText::new(text).size(12.0).color(theme.text_faint));
    });
}

fn root_menu(ui: &mut Ui, target: &TabularTarget, cat: &ModelCatalog, show_hidden: bool, actions: &mut Vec<ModelAction>) {
    if ui.button("New DAX query").clicked() {
        actions.push(ModelAction::Query { target: target.clone(), text: String::new(), run: false });
        ui.close();
    }
    if ui.button("Model metadata (INFO.VIEW)").on_hover_text("Tables, columns, measures and relationships as result sets").clicked() {
        actions.push(ModelAction::Query { target: target.clone(), text: crate::tabularq::q_info(), run: true });
        ui.close();
    }
    if ui.button("Define all measures").on_hover_text("A DEFINE block with every measure's expression, to edit and evaluate").clicked() {
        actions.push(ModelAction::Query { target: target.clone(), text: crate::tabularq::q_define_all(cat, None), run: false });
        ui.close();
    }
    ui.separator();
    if ui.checkbox(&mut show_hidden.clone(), "Show hidden").clicked() {
        actions.push(ModelAction::ToggleHidden);
        ui.close();
    }
    if ui.button(format!("{} Refresh model", ph::ARROWS_CLOCKWISE)).clicked() {
        actions.push(ModelAction::Refresh { target: target.clone() });
        ui.close();
    }
}

#[allow(clippy::too_many_arguments)]
fn table_node(ui: &mut Ui, theme: &Theme, target: &TabularTarget, cat: &ModelCatalog, key: &str, t: &Table, expanded: &HashSet<String>, show_hidden: bool, depth: usize, actions: &mut Vec<ModelAction>) {
    let id = format!("{key}/t/{}", t.name);
    let open = expanded.contains(&id);
    let cols = t.columns.iter().filter(|c| show_hidden || !c.hidden).count();
    let meas = t.measures.iter().filter(|m| show_hidden || !m.hidden).count();
    let mut sub = format!("{cols} col");
    if meas > 0 {
        sub.push_str(&format!(" · {meas} meas"));
    }
    if let Some(sm) = &t.storage_mode {
        if !sm.is_empty() && sm != "Import" {
            sub.push_str(&format!(" · {sm}"));
        }
    }
    if t.hidden {
        sub.push_str(" · hidden");
    }
    let icon = if t.expression.is_some() { ph::FUNCTION } else { ph::TABLE };
    let r = tree_row(ui, theme, depth, open, icon, &t.name, &sub, &format!("model table {}", t.name));
    if r.clicked() {
        actions.push(ModelAction::ToggleNode(id.clone()));
    }
    if r.double_clicked() {
        actions.push(ModelAction::Query { target: target.clone(), text: crate::tabularq::q_top_rows(&t.name, 100), run: true });
    }
    let r = r.on_hover_text(table_hover(t));
    r.context_menu(|ui| {
        if ui.button("Top 100 rows").clicked() {
            actions.push(ModelAction::Query { target: target.clone(), text: crate::tabularq::q_top_rows(&t.name, 100), run: true });
            ui.close();
        }
        if ui.button("Count rows").clicked() {
            actions.push(ModelAction::Query { target: target.clone(), text: crate::tabularq::q_count_rows(&t.name), run: true });
            ui.close();
        }
        if meas > 0 && ui.button("Define this table's measures").clicked() {
            actions.push(ModelAction::Query { target: target.clone(), text: crate::tabularq::q_define_all(cat, Some(&t.name)), run: false });
            ui.close();
        }
        ui.separator();
        if ui.button(format!("Insert {}", dax_table_ref(&t.name))).clicked() {
            actions.push(ModelAction::Insert { target: target.clone(), text: dax_table_ref(&t.name) });
            ui.close();
        }
        if ui.button("Copy name").clicked() {
            actions.push(ModelAction::Copy(dax_table_ref(&t.name)));
            ui.close();
        }
    });
    if !open {
        return;
    }
    for c in t.columns.iter().filter(|c| show_hidden || !c.hidden) {
        column_row(ui, theme, target, t, c, depth + 1, actions);
    }
    for m in t.measures.iter().filter(|m| show_hidden || !m.hidden) {
        measure_row(ui, theme, target, t, m, depth + 1, actions);
    }
    for h in t.hierarchies.iter().filter(|h| show_hidden || !h.hidden) {
        let hid = format!("{id}/h/{}", h.name);
        let open = expanded.contains(&hid);
        let r = tree_row(ui, theme, depth + 1, open, ph::TREE_STRUCTURE, &h.name, &format!("hierarchy · {} levels", h.levels.len()), &format!("hierarchy {}", h.name));
        if r.clicked() {
            actions.push(ModelAction::ToggleNode(hid.clone()));
        }
        if open {
            for l in &h.levels {
                let r = tree_row(ui, theme, depth + 2, false, ph::DOT_OUTLINE, &l.name, &l.column, &format!("level {}", l.name));
                r.context_menu(|ui| {
                    if ui.button(format!("Insert {}", dax_column_ref(&t.name, &l.column))).clicked() {
                        actions.push(ModelAction::Insert { target: target.clone(), text: dax_column_ref(&t.name, &l.column) });
                        ui.close();
                    }
                });
            }
        }
    }
    if cols + meas == 0 {
        muted(ui, theme, depth + 1, "nothing visible");
    }
}

fn table_hover(t: &Table) -> String {
    let mut s = format!("{}\n{} columns · {} measures · {} hierarchies", t.name, t.columns.len(), t.measures.len(), t.hierarchies.len());
    if let Some(sm) = &t.storage_mode {
        s.push_str(&format!("\nstorage: {sm}"));
    }
    if let Some(d) = t.description.as_deref().filter(|d| !d.trim().is_empty()) {
        s.push_str(&format!("\n\n{d}"));
    }
    if let Some(e) = t.expression.as_deref().filter(|e| !e.trim().is_empty()) {
        s.push_str(&format!("\n\ncalculated table:\n{}", clip(e)));
    }
    s
}

fn clip(e: &str) -> String {
    if e.chars().count() > MAX_HOVER_EXPR {
        let mut s: String = e.chars().take(MAX_HOVER_EXPR).collect();
        s.push('…');
        s
    } else {
        e.to_string()
    }
}

fn column_row(ui: &mut Ui, theme: &Theme, target: &TabularTarget, t: &Table, c: &Column, depth: usize, actions: &mut Vec<ModelAction>) {
    let icon = if c.key {
        ph::KEY
    } else if c.calculated {
        ph::FUNCTION
    } else {
        match c.data_type {
            cobalt_core::tabular::TabularDataType::String => ph::TEXT_AA,
            cobalt_core::tabular::TabularDataType::DateTime => ph::CALENDAR_BLANK,
            cobalt_core::tabular::TabularDataType::Boolean => ph::TOGGLE_LEFT,
            cobalt_core::tabular::TabularDataType::Int64 | cobalt_core::tabular::TabularDataType::Double | cobalt_core::tabular::TabularDataType::Decimal => ph::HASH,
            _ => ph::COLUMNS,
        }
    };
    let mut sub = c.data_type.label().to_string();
    if c.hidden {
        sub.push_str(" · hidden");
    }
    let r = tree_row(ui, theme, depth, false, icon, &c.name, &sub, &format!("column {}[{}]", t.name, c.name));
    if r.double_clicked() {
        actions.push(ModelAction::Insert { target: target.clone(), text: dax_column_ref(&t.name, &c.name) });
    }
    let r = r.on_hover_text(column_hover(t, c));
    r.context_menu(|ui| {
        if ui.button("Distinct values").clicked() {
            actions.push(ModelAction::Query { target: target.clone(), text: crate::tabularq::q_distinct(&t.name, &c.name), run: true });
            ui.close();
        }
        if ui.button("Summary (rows, distinct, blanks, min, max)").clicked() {
            actions.push(ModelAction::Query { target: target.clone(), text: crate::tabularq::q_column_summary(&t.name, &c.name), run: true });
            ui.close();
        }
        ui.separator();
        if ui.button(format!("Insert {}", dax_column_ref(&t.name, &c.name))).clicked() {
            actions.push(ModelAction::Insert { target: target.clone(), text: dax_column_ref(&t.name, &c.name) });
            ui.close();
        }
        if ui.button("Copy reference").clicked() {
            actions.push(ModelAction::Copy(dax_column_ref(&t.name, &c.name)));
            ui.close();
        }
    });
}

fn column_hover(t: &Table, c: &Column) -> String {
    let mut s = format!("{}\n{}", dax_column_ref(&t.name, &c.name), c.data_type.label());
    if c.key {
        s.push_str(" · key");
    }
    if c.hidden {
        s.push_str(" · hidden");
    }
    if let Some(f) = c.format_string.as_deref().filter(|f| !f.is_empty()) {
        s.push_str(&format!("\nformat: {f}"));
    }
    if let Some(sb) = c.sort_by_column.as_deref().filter(|f| !f.is_empty()) {
        s.push_str(&format!("\nsort by: {sb}"));
    }
    if let Some(sm) = c.summarize_by.as_deref().filter(|f| !f.is_empty() && *f != "Default") {
        s.push_str(&format!("\nsummarize by: {sm}"));
    }
    if let Some(f) = c.display_folder.as_deref().filter(|f| !f.is_empty()) {
        s.push_str(&format!("\nfolder: {f}"));
    }
    if let Some(d) = c.description.as_deref().filter(|d| !d.trim().is_empty()) {
        s.push_str(&format!("\n\n{d}"));
    }
    if let Some(e) = c.expression.as_deref().filter(|e| !e.trim().is_empty()) {
        s.push_str(&format!("\n\n= {}", clip(e)));
    }
    s
}

fn measure_row(ui: &mut Ui, theme: &Theme, target: &TabularTarget, t: &Table, m: &Measure, depth: usize, actions: &mut Vec<ModelAction>) {
    let mut sub = String::from("measure");
    if let Some(f) = m.format_string.as_deref().filter(|f| !f.is_empty()) {
        sub = format!("measure · {f}");
    }
    if m.hidden {
        sub.push_str(" · hidden");
    }
    let r = tree_row(ui, theme, depth, false, ph::SIGMA, &m.name, &sub, &format!("measure {}", m.name));
    if r.double_clicked() {
        actions.push(ModelAction::Insert { target: target.clone(), text: dax_measure_ref(&m.name) });
    }
    let r = r.on_hover_text(measure_hover(t, m));
    r.context_menu(|ui| {
        if ui.button("Evaluate").clicked() {
            actions.push(ModelAction::Query { target: target.clone(), text: crate::tabularq::q_evaluate_measure(&m.name), run: true });
            ui.close();
        }
        ui.menu_button("Evaluate by…", |ui| {
            egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                for tb in &cat_tables_for_menu(t) {
                    for c in tb.columns.iter().filter(|c| !c.hidden) {
                        if ui.button(format!("{}[{}]", tb.name, c.name)).clicked() {
                            actions.push(ModelAction::Query { target: target.clone(), text: crate::tabularq::q_measure_by(&m.name, &tb.name, &c.name), run: true });
                            ui.close();
                        }
                    }
                }
            });
        });
        if ui.button("Define measure").on_hover_text("DEFINE MEASURE with the expression, to edit it in the query").clicked() {
            actions.push(ModelAction::Query { target: target.clone(), text: crate::tabularq::q_define_measure(&t.name, &m.name, m.expression.as_deref()), run: false });
            ui.close();
        }
        ui.separator();
        if ui.button(format!("Insert {}", dax_measure_ref(&m.name))).clicked() {
            actions.push(ModelAction::Insert { target: target.clone(), text: dax_measure_ref(&m.name) });
            ui.close();
        }
        if m.expression.as_deref().map(|e| !e.trim().is_empty()).unwrap_or(false) && ui.button("Copy expression").clicked() {
            actions.push(ModelAction::Copy(m.expression.clone().unwrap_or_default()));
            ui.close();
        }
        if ui.button("Copy reference").clicked() {
            actions.push(ModelAction::Copy(dax_measure_ref(&m.name)));
            ui.close();
        }
    });
}

/// "Evaluate by…" offers the measure's own table's columns (the common case: a dimension
/// column on the same table, or the fact table's own attributes).
fn cat_tables_for_menu(t: &Table) -> Vec<Table> {
    vec![t.clone()]
}

fn measure_hover(t: &Table, m: &Measure) -> String {
    let mut s = format!("{} · {}\n{}", dax_measure_ref(&m.name), t.name, m.data_type.label());
    if let Some(f) = m.format_string.as_deref().filter(|f| !f.is_empty()) {
        s.push_str(&format!("\nformat: {f}"));
    }
    if let Some(f) = m.display_folder.as_deref().filter(|f| !f.is_empty()) {
        s.push_str(&format!("\nfolder: {f}"));
    }
    if let Some(d) = m.description.as_deref().filter(|d| !d.trim().is_empty()) {
        s.push_str(&format!("\n\n{d}"));
    }
    match m.expression.as_deref().filter(|e| !e.trim().is_empty()) {
        Some(e) => s.push_str(&format!("\n\n= {}", clip(e))),
        None => s.push_str("\n\n(expression not visible with this access)"),
    }
    s
}
