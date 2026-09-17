//! Execution plan viewer: statement selector, zoomable node graph, properties pane, top operations.

use crate::state::{EditorTab, PlanView};
use crate::ui::theme::Theme;
use crate::ui::widgets::icon_button;
use cobalt_core::Settings;
use cobalt_plan::{IconCategory, LayoutOptions, Metric, OpIcon, Statement};
use egui::{Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke, Ui, Vec2};
use egui_phosphor::regular as icons;
use std::sync::Arc;

const NODE_W: f32 = 168.0;
const NODE_H: f32 = 66.0;

fn ensure_parsed(pv: &mut PlanView) {
    if pv.parsed.is_none() && pv.error.is_none() {
        match cobalt_plan::parse(&pv.xml) {
            Ok(p) => pv.parsed = Some(Arc::new(p)),
            Err(e) => pv.error = Some(e.to_string()),
        }
        pv.layout = None;
    }
    if pv.layout.is_none() {
        if let Some(p) = &pv.parsed {
            let stmts = p.all_statements();
            if let Some(s) = stmts.get(pv.shown_statement.min(stmts.len().saturating_sub(1))) {
                let l = cobalt_plan::layout(s, &LayoutOptions { node_w: NODE_W, node_h: NODE_H, h_gap: 70.0, v_gap: 18.0 });
                pv.layout = Some(Arc::new(l));
            }
        }
    }
}

fn icon_glyph(icon: OpIcon) -> &'static str {
    use OpIcon::*;
    match icon {
        TableScan => icons::TABLE,
        ClusteredIndexScan | IndexScan | ColumnstoreIndexScan => icons::LIST_MAGNIFYING_GLASS,
        ClusteredIndexSeek | IndexSeek => icons::CROSSHAIR,
        KeyLookup | RidLookup => icons::KEY,
        NestedLoops => icons::ARROWS_CLOCKWISE,
        HashMatch => icons::HASH,
        MergeJoin => icons::GIT_MERGE,
        AdaptiveJoin => icons::SHUFFLE,
        Sort => icons::SORT_ASCENDING,
        Filter => icons::FUNNEL,
        ComputeScalar => icons::FUNCTION,
        StreamAggregate | HashAggregate | WindowAggregate => icons::SIGMA,
        Top => icons::ARROW_LINE_UP,
        Concatenation => icons::ROWS,
        Parallelism => icons::ARROWS_SPLIT,
        TableSpool | IndexSpool | RowCountSpool => icons::SPIRAL,
        Insert | Update | Delete | Merge => icons::PENCIL_SIMPLE,
        Assert => icons::WARNING_CIRCLE,
        SequenceProject | Segment => icons::LIST_NUMBERS,
        ConstantScan => icons::DOT,
        TableValuedFunction => icons::FUNCTION,
        RemoteQuery => icons::CLOUD,
        Collapse | Split | Switch | UdxOp => icons::GIT_BRANCH,
        Result => icons::FLAG_CHECKERED,
        Cursor => icons::CURSOR,
        Other => icons::CIRCLE,
    }
}

fn category_color(theme: &Theme, c: IconCategory) -> Color32 {
    match c {
        IconCategory::Scan => theme.plan.cat_scan,
        IconCategory::Seek => theme.plan.cat_seek,
        IconCategory::Join => theme.plan.cat_join,
        IconCategory::Aggregate => theme.plan.cat_aggregate,
        IconCategory::Sort => theme.plan.cat_sort,
        IconCategory::Dml => theme.plan.cat_dml,
        IconCategory::Parallel => theme.plan.cat_parallel,
        IconCategory::Spool => theme.plan.cat_spool,
        IconCategory::Scalar => theme.plan.cat_scalar,
        IconCategory::Other => theme.plan.cat_other,
    }
}

pub fn show(ui: &mut Ui, tab: &mut EditorTab, theme: &Theme, settings: &Settings) {
    let Some(run) = tab.run.as_mut() else { return };
    if run.plans.is_empty() {
        ui.centered_and_justified(|ui| ui.label(RichText::new("No plan").color(theme.text_faint)));
        return;
    }
    // which PlanView? one per statement result set; a selector when several
    let plan_count = run.plans.len();
    let sel_id = egui::Id::new(("plan-sel", tab.id));
    let mut selected: usize = ui.ctx().memory(|m| m.data.get_temp(sel_id)).unwrap_or(0);
    selected = selected.min(plan_count - 1);
    for pv in run.plans.iter_mut() {
        ensure_parsed(pv);
    }
    let pv = &mut run.plans[selected];
    let mut save_xml: Option<String> = None;
    let mut open_xml: Option<String> = None;

    egui::Frame::new().fill(theme.bg_panel).show(ui, |ui| {
        ui.set_min_size(ui.available_size());
        // toolbar
        egui::Frame::new().fill(theme.bg_sidebar).inner_margin(egui::Margin::symmetric(6, 3)).show(ui, |ui| {
            ui.horizontal(|ui| {
                if plan_count > 1 {
                    ui.label(RichText::new("Plan").color(theme.text_muted));
                    egui::ComboBox::from_id_salt(sel_id.with("combo")).selected_text(format!("Result {}", selected + 1)).show_ui(ui, |ui| {
                        for i in 0..plan_count {
                            ui.selectable_value(&mut selected, i, format!("Result {}", i + 1));
                        }
                    });
                }
                if let Some(p) = &pv.parsed {
                    let stmts = p.all_statements();
                    if stmts.len() > 1 {
                        ui.label(RichText::new("Statement").color(theme.text_muted));
                        let cur = pv.shown_statement.min(stmts.len() - 1);
                        let mut new_stmt = cur;
                        egui::ComboBox::from_id_salt(sel_id.with("stmt")).width(320.0).selected_text(format!("{}: {}", cur + 1, stmts[cur].text.chars().take(50).collect::<String>().replace('\n', " "))).show_ui(ui, |ui| {
                            for (i, s) in stmts.iter().enumerate() {
                                ui.selectable_value(&mut new_stmt, i, format!("{}: {}  ({:.4})", i + 1, s.text.chars().take(80).collect::<String>().replace('\n', " "), s.subtree_cost));
                            }
                        });
                        if new_stmt != cur {
                            pv.shown_statement = new_stmt;
                            pv.layout = None;
                            pv.selected_node = None;
                            ensure_parsed(pv);
                        }
                    }
                }
                ui.separator();
                if icon_button(ui, icons::MAGNIFYING_GLASS_PLUS, "Zoom in", true).clicked() {
                    pv.zoom = (pv.zoom * 1.2).min(4.0);
                }
                if icon_button(ui, icons::MAGNIFYING_GLASS_MINUS, "Zoom out", true).clicked() {
                    pv.zoom = (pv.zoom / 1.2).max(0.15);
                }
                if icon_button(ui, icons::ARROWS_IN, "Zoom to fit", true).clicked() {
                    pv.zoom = 0.0;
                }
                ui.separator();
                ui.add(egui::TextEdit::singleline(&mut pv.find).hint_text(format!("{} Find node", icons::MAGNIFYING_GLASS)).desired_width(160.0));
                ui.separator();
                if icon_button(ui, icons::FLOPPY_DISK, "Save plan as .sqlplan", true).clicked() {
                    save_xml = Some(pv.xml.clone());
                }
                if icon_button(ui, icons::CODE, "Show plan XML", true).clicked() {
                    open_xml = Some(pv.xml.clone());
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.toggle_value(&mut pv.show_properties, "Properties");
                    ui.toggle_value(&mut pv.show_top_ops, "Top operations");
                });
            });
        });
        if let Some(e) = &pv.error {
            ui.colored_label(theme.error, format!("Could not parse the plan: {e}"));
            return;
        }
        let Some(plan) = pv.parsed.clone() else { return };
        let stmts = plan.all_statements();
        let Some(stmt) = stmts.get(pv.shown_statement.min(stmts.len().saturating_sub(1))).copied() else { return };
        let Some(layout) = pv.layout.clone() else { return };

        // header: statement summary
        egui::Frame::new().inner_margin(egui::Margin::symmetric(8, 4)).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                let mut summary = format!("Cost {:.4}", stmt.subtree_cost);
                if let Some(d) = stmt.degree_of_parallelism {
                    summary.push_str(&format!(" · DOP {d}"));
                }
                if let Some((cpu, el)) = stmt.query_time_ms {
                    summary.push_str(&format!(" · CPU {cpu} ms · elapsed {el} ms"));
                }
                if let Some(m) = stmt.memory_grant_kb {
                    summary.push_str(&format!(" · memory grant {} KB", m));
                }
                if let Some(ct) = stmt.compile_time_ms {
                    summary.push_str(&format!(" · compile {ct} ms"));
                }
                if stmt.is_actual {
                    summary.push_str(" · actual");
                } else {
                    summary.push_str(" · estimated");
                }
                ui.label(RichText::new(summary).size(12.0).color(theme.text_muted));
                let text: String = stmt.text.chars().take(160).collect();
                ui.label(RichText::new(text.replace('\n', " ")).family(egui::FontFamily::Monospace).size(11.5).color(theme.text)).on_hover_text(&stmt.text);
            });
            for mi in &stmt.missing_indexes {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{} Missing index (impact {:.1}%): {}.{} ({}){}", icons::LIGHTBULB, mi.impact, mi.schema, mi.table, mi.equality_columns.iter().chain(mi.inequality_columns.iter()).cloned().collect::<Vec<_>>().join(", "), if mi.included_columns.is_empty() { String::new() } else { format!(" INCLUDE ({})", mi.included_columns.join(", ")) })).color(theme.warning).size(12.0));
                    if ui.small_button("Copy CREATE INDEX").clicked() {
                        ui.ctx().copy_text(mi.create_index_sql());
                    }
                });
            }
            for w in &stmt.warnings {
                ui.label(RichText::new(format!("{} {}", icons::WARNING, w.detail)).color(theme.warning).size(12.0));
            }
        });

        let avail = ui.available_rect_before_wrap();
        let props_w = if pv.show_properties { 320.0f32.min(avail.width() * 0.4) } else { 0.0 };
        let top_h = if pv.show_top_ops { 180.0f32.min(avail.height() * 0.4) } else { 0.0 };
        let canvas_rect = Rect::from_min_size(avail.min, Vec2::new(avail.width() - props_w, avail.height() - top_h));
        canvas(ui, canvas_rect, pv, stmt, &layout, theme, settings);
        if pv.show_properties {
            let r = Rect::from_min_max(Pos2::new(canvas_rect.right(), avail.top()), Pos2::new(avail.right(), canvas_rect.bottom()));
            let mut pui = ui.new_child(egui::UiBuilder::new().max_rect(r).layout(egui::Layout::top_down(egui::Align::Min)));
            pui.set_clip_rect(r);
            properties(&mut pui, pv, stmt, theme);
        }
        if pv.show_top_ops {
            let r = Rect::from_min_max(Pos2::new(avail.left(), canvas_rect.bottom()), avail.max);
            let mut tui = ui.new_child(egui::UiBuilder::new().max_rect(r).layout(egui::Layout::top_down(egui::Align::Min)));
            tui.set_clip_rect(r);
            top_operations(&mut tui, pv, stmt, theme);
        }
    });
    ui.ctx().memory_mut(|m| m.data.insert_temp(sel_id, selected));
    if let Some(xml) = save_xml {
        if let Some(p) = rfd::FileDialog::new().add_filter("Execution plan", &["sqlplan"]).set_file_name("plan.sqlplan").save_file() {
            let _ = std::fs::write(p, xml);
        }
    }
    if let Some(xml) = open_xml {
        ui.ctx().copy_text(xml);
        ui.ctx().memory_mut(|m| m.data.insert_temp(egui::Id::new("plan-xml-copied"), true));
    }
}

fn canvas(ui: &mut Ui, rect: Rect, pv: &mut PlanView, stmt: &Statement, layout: &cobalt_plan::Layout, theme: &Theme, _settings: &Settings) {
    let painter = ui.painter().with_clip_rect(rect);
    painter.rect_filled(rect, 0.0, theme.bg_editor);
    let resp = ui.interact(rect, egui::Id::new(("plan-canvas", stmt.id, pv.statement_index)), Sense::click_and_drag());
    // zoom to fit
    if pv.zoom <= 0.0 {
        let (w, h) = layout.size;
        let z = ((rect.width() - 40.0) / w.max(1.0)).min((rect.height() - 40.0) / h.max(1.0)).clamp(0.15, 1.25);
        pv.zoom = z;
        pv.pan = Vec2::new(20.0, ((rect.height() - h * z) / 2.0).max(20.0));
    }
    if resp.dragged() {
        pv.pan += resp.drag_delta();
    }
    if resp.hovered() {
        let (scroll, zoom_delta) = ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta()));
        let ctrl = ui.input(|i| i.modifiers.command);
        if zoom_delta != 1.0 || (ctrl && scroll.y != 0.0) {
            let factor = if zoom_delta != 1.0 { zoom_delta } else if scroll.y > 0.0 { 1.1 } else { 1.0 / 1.1 };
            let mouse = ui.input(|i| i.pointer.hover_pos()).unwrap_or(rect.center());
            let before = (mouse - rect.min - pv.pan) / pv.zoom;
            pv.zoom = (pv.zoom * factor).clamp(0.15, 4.0);
            pv.pan = mouse - rect.min - before * pv.zoom;
        } else if scroll != Vec2::ZERO {
            pv.pan += scroll;
        }
    }
    let z = pv.zoom;
    let origin = rect.min + pv.pan;
    let to_screen = |x: f32, y: f32| Pos2::new(origin.x + x * z, origin.y + y * z);
    let find = pv.find.trim().to_lowercase();
    let matches: Vec<usize> = if find.is_empty() { vec![] } else { stmt.find_nodes(&find) };

    // edges
    for e in &layout.edges {
        let (Some(fr), Some(tr)) = (layout.rect_of(e.from), layout.rect_of(e.to)) else { continue };
        // parent (from) is left; child (to) is right
        let a = to_screen(fr.x + fr.w, fr.y + fr.h / 2.0);
        let b = to_screen(tr.x, tr.y + tr.h / 2.0);
        let rows = e.actual_rows.unwrap_or(e.est_rows).max(0.0);
        let thickness = ((1.0 + (rows + 1.0).log10() * 1.6).clamp(1.0, 10.0) as f32) * z.clamp(0.5, 1.5);
        let mid = (a.x + b.x) / 2.0;
        let pts = [a, Pos2::new(mid, a.y), Pos2::new(mid, b.y), b];
        painter.add(egui::Shape::line(pts.to_vec(), Stroke::new(thickness, theme.plan.edge)));
        // arrow head pointing left into the parent
        let ah = 6.0 * z.clamp(0.5, 1.5);
        painter.add(egui::Shape::convex_polygon(vec![a, Pos2::new(a.x + ah * 1.6, a.y - ah), Pos2::new(a.x + ah * 1.6, a.y + ah)], theme.plan.edge, Stroke::NONE));
        if z > 0.5 {
            let label = cobalt_plan::fmt_thousands(rows);
            painter.text(Pos2::new(mid, (a.y + b.y) / 2.0 - 8.0 * z), Align2::CENTER_BOTTOM, label, FontId::proportional(10.0 * z.clamp(0.7, 1.4)), theme.text_faint);
        }
    }

    // nodes
    let mut hovered: Option<usize> = None;
    let mut clicked: Option<usize> = None;
    for (ni, r) in &layout.rects {
        let node = &stmt.nodes[*ni];
        let top_left = to_screen(r.x, r.y);
        let nrect = Rect::from_min_size(top_left, Vec2::new(r.w * z, r.h * z));
        if !rect.intersects(nrect) {
            continue;
        }
        let cost_t = (node.cost_pct / 100.0) as f32;
        let bg = theme.cost_color(cost_t.powf(0.6));
        let selected = pv.selected_node == Some(*ni);
        let is_match = matches.contains(ni);
        painter.rect_filled(nrect, 6.0 * z, bg);
        let border = if selected { Stroke::new(2.0, theme.plan.node_selected) } else if is_match { Stroke::new(2.0, theme.warning) } else { Stroke::new(1.0, theme.plan.node_border) };
        painter.rect_stroke(nrect, 6.0 * z, border, egui::StrokeKind::Outside);
        let icon = cobalt_plan::icon_for(&node.physical_op, &node.logical_op, node.object.as_ref());
        let cat = category_color(theme, icon.category());
        if z >= 0.35 {
            painter.text(nrect.left_top() + Vec2::new(14.0 * z, 16.0 * z), Align2::CENTER_CENTER, icon_glyph(icon), FontId::proportional(18.0 * z), cat);
            let fs = (12.0 * z).max(6.0);
            let name = if node.physical_op == node.logical_op || node.logical_op.is_empty() { node.physical_op.clone() } else { format!("{} ({})", node.physical_op, node.logical_op) };
            let name_galley = painter.layout(name, FontId::proportional(fs), theme.text, (r.w - 30.0) * z);
            painter.galley(nrect.left_top() + Vec2::new(28.0 * z, 6.0 * z), name_galley, theme.text);
            let mut line2 = node.object.as_ref().map(|o| o.display()).unwrap_or_default();
            if line2.len() > 40 {
                line2 = format!("{}…", &line2[..40]);
            }
            painter.text(nrect.left_top() + Vec2::new(28.0 * z, 30.0 * z), Align2::LEFT_TOP, line2, FontId::proportional(fs - 1.0), theme.text_muted);
            let cost = format!("Cost: {:.0}%", node.cost_pct);
            painter.text(nrect.left_bottom() + Vec2::new(8.0 * z, -6.0 * z), Align2::LEFT_BOTTOM, cost, FontId::proportional(fs - 1.0), theme.text);
            let rows_txt = match &node.actual {
                Some(a) => format!("{} of {}", cobalt_plan::fmt_thousands(a.rows as f64), cobalt_plan::fmt_thousands(node.est_rows)),
                None => cobalt_plan::fmt_thousands(node.est_rows),
            };
            let rows_color = match &node.actual {
                Some(a) if (a.rows as f64 > node.est_rows * 10.0 && a.rows > 100) || ((a.rows as f64) * 10.0 < node.est_rows && node.est_rows > 100.0) => theme.error,
                _ => theme.text_muted,
            };
            painter.text(nrect.right_bottom() + Vec2::new(-8.0 * z, -6.0 * z), Align2::RIGHT_BOTTOM, rows_txt, FontId::proportional(fs - 1.0), rows_color);
            if !node.warnings.is_empty() {
                painter.text(nrect.right_top() + Vec2::new(-10.0 * z, 12.0 * z), Align2::CENTER_CENTER, icons::WARNING, FontId::proportional(14.0 * z), theme.warning);
            }
            if node.parallel {
                painter.text(nrect.right_top() + Vec2::new(-24.0 * z, 12.0 * z), Align2::CENTER_CENTER, icons::ARROWS_SPLIT, FontId::proportional(12.0 * z), theme.plan.cat_parallel);
            }
        }
        if let Some(pos) = ui.input(|i| i.pointer.hover_pos()) {
            if nrect.contains(pos) && rect.contains(pos) {
                hovered = Some(*ni);
                if resp.clicked() {
                    clicked = Some(*ni);
                }
            }
        }
    }
    if let Some(c) = clicked {
        pv.selected_node = Some(c);
    } else if resp.clicked() {
        pv.selected_node = None;
    }
    if let Some(h) = hovered {
        let node = &stmt.nodes[h];
        egui::Tooltip::always_open(ui.ctx().clone(), ui.layer_id(), egui::Id::new(("plan-tip", h)), egui::PopupAnchor::Pointer).show(|ui| {
            ui.set_max_width(420.0);
            ui.label(RichText::new(format!("{} — {}", node.physical_op, node.logical_op)).strong());
            if let Some(o) = &node.object {
                ui.label(o.display());
            }
            egui::Grid::new(("tip-grid", h)).num_columns(2).spacing([12.0, 2.0]).show(ui, |ui| {
                let row = |ui: &mut Ui, k: &str, v: String| {
                    ui.label(RichText::new(k).color(theme.text_muted));
                    ui.label(v);
                    ui.end_row();
                };
                row(ui, "Estimated rows", cobalt_plan::fmt_thousands(node.est_rows));
                if let Some(a) = &node.actual {
                    row(ui, "Actual rows", cobalt_plan::fmt_thousands(a.rows as f64));
                    row(ui, "Executions", a.executions.to_string());
                    row(ui, "Elapsed", format!("{} ms", a.elapsed_ms));
                    row(ui, "CPU", format!("{} ms", a.cpu_ms));
                    if let Some(lr) = a.logical_reads {
                        row(ui, "Logical reads", lr.to_string());
                    }
                }
                row(ui, "Est. cost", format!("{:.0}% ({:.4})", node.cost_pct, stmt.own_cost(h)));
                row(ui, "Subtree cost", format!("{:.4}", node.est_subtree_cost));
                row(ui, "Est. CPU / IO", format!("{:.5} / {:.5}", node.est_cpu, node.est_io));
                if let Some(m) = &node.execution_mode {
                    row(ui, "Mode", m.clone());
                }
            });
            if let Some(p) = &node.predicate {
                ui.label(RichText::new(format!("Predicate: {}", p.chars().take(300).collect::<String>())).size(11.0).color(theme.text_muted));
            }
            if let Some(p) = &node.seek_predicates {
                ui.label(RichText::new(format!("Seek: {}", p.chars().take(300).collect::<String>())).size(11.0).color(theme.text_muted));
            }
            for w in &node.warnings {
                ui.label(RichText::new(format!("{} {}", icons::WARNING, w.detail)).size(11.0).color(theme.warning));
            }
        });
    }
    // zoom badge
    painter.text(rect.right_top() + Vec2::new(-8.0, 8.0), Align2::RIGHT_TOP, format!("{:.0}%", z * 100.0), FontId::proportional(11.0), theme.text_faint);
    if ui.ctx().memory(|m| m.data.get_temp::<bool>(egui::Id::new("plan-xml-copied")).unwrap_or(false)) {
        painter.text(rect.left_top() + Vec2::new(8.0, 8.0), Align2::LEFT_TOP, "Plan XML copied to clipboard", FontId::proportional(11.0), theme.accent);
        ui.ctx().request_repaint_after(std::time::Duration::from_secs(3));
        if ui.input(|i| i.time) as i64 % 7 == 0 {
            ui.ctx().memory_mut(|m| m.data.remove::<bool>(egui::Id::new("plan-xml-copied")));
        }
    }
}

fn properties(ui: &mut Ui, pv: &mut PlanView, stmt: &Statement, theme: &Theme) {
    egui::Frame::new().fill(theme.bg_panel).inner_margin(6.0).show(ui, |ui| {
        ui.set_min_size(ui.available_size());
        let Some(ni) = pv.selected_node else {
            ui.label(RichText::new("Select an operator to see its properties.").color(theme.text_faint));
            return;
        };
        let Some(node) = stmt.nodes.get(ni) else { return };
        ui.label(RichText::new(format!("{}", node.physical_op)).strong());
        if let Some(o) = &node.object {
            ui.label(RichText::new(o.display()).size(12.0).color(theme.text_muted));
        }
        ui.separator();
        let filter_id = egui::Id::new(("prop-filter", stmt.id));
        let mut filter: String = ui.ctx().memory(|m| m.data.get_temp(filter_id)).unwrap_or_default();
        ui.add(egui::TextEdit::singleline(&mut filter).hint_text("Filter properties").desired_width(f32::INFINITY));
        ui.ctx().memory_mut(|m| m.data.insert_temp(filter_id, filter.clone()));
        let f = filter.to_lowercase();
        let mut props: Vec<(String, String)> = Vec::new();
        props.push(("Physical Operation".into(), node.physical_op.clone()));
        props.push(("Logical Operation".into(), node.logical_op.clone()));
        props.push(("Estimated Number of Rows".into(), cobalt_plan::fmt_thousands(node.est_rows)));
        if let Some(a) = &node.actual {
            props.push(("Actual Number of Rows".into(), cobalt_plan::fmt_thousands(a.rows as f64)));
            props.push(("Actual Executions".into(), a.executions.to_string()));
            props.push(("Actual Elapsed (ms)".into(), a.elapsed_ms.to_string()));
            props.push(("Actual CPU (ms)".into(), a.cpu_ms.to_string()));
            if let Some(x) = a.logical_reads {
                props.push(("Logical Reads".into(), x.to_string()));
            }
            if let Some(x) = a.physical_reads {
                props.push(("Physical Reads".into(), x.to_string()));
            }
            if let Some(x) = a.rows_read {
                props.push(("Rows Read".into(), x.to_string()));
            }
            props.push(("Threads".into(), a.threads.to_string()));
        }
        props.push(("Estimated Operator Cost".into(), format!("{:.6} ({:.0}%)", stmt.own_cost(ni), node.cost_pct)));
        props.push(("Estimated Subtree Cost".into(), format!("{:.6}", node.est_subtree_cost)));
        props.push(("Estimated CPU Cost".into(), format!("{:.6}", node.est_cpu)));
        props.push(("Estimated I/O Cost".into(), format!("{:.6}", node.est_io)));
        if let Some(x) = node.est_executions {
            props.push(("Estimated Executions".into(), format!("{x}")));
        }
        if let Some(x) = node.avg_row_size {
            props.push(("Average Row Size (bytes)".into(), x.to_string()));
        }
        props.push(("Parallel".into(), node.parallel.to_string()));
        if let Some(m) = &node.execution_mode {
            props.push(("Execution Mode".into(), m.clone()));
        }
        if let Some(p) = &node.predicate {
            props.push(("Predicate".into(), p.clone()));
        }
        if let Some(p) = &node.seek_predicates {
            props.push(("Seek Predicates".into(), p.clone()));
        }
        if !node.output_list.is_empty() {
            props.push(("Output List".into(), node.output_list.join(", ")));
        }
        for w in &node.warnings {
            props.push(("Warning".into(), w.detail.clone()));
        }
        for (k, v) in &node.properties {
            if !props.iter().any(|(pk, _)| pk.eq_ignore_ascii_case(k)) {
                props.push((k.clone(), v.clone()));
            }
        }
        egui::ScrollArea::vertical().id_salt(("props-scroll", stmt.id)).auto_shrink([false, false]).show(ui, |ui| {
            egui::Grid::new(("props-grid", stmt.id, ni)).num_columns(2).striped(true).spacing([10.0, 3.0]).min_col_width(90.0).show(ui, |ui| {
                for (k, v) in props.iter().filter(|(k, v)| f.is_empty() || k.to_lowercase().contains(&f) || v.to_lowercase().contains(&f)) {
                    ui.label(RichText::new(k).size(11.5).color(theme.text_muted));
                    let shown = if v.chars().count() > 200 { format!("{}…", v.chars().take(200).collect::<String>()) } else { v.clone() };
                    let r = ui.add(egui::Label::new(RichText::new(shown).size(11.5)).sense(Sense::click()).wrap());
                    if r.clicked() {
                        ui.ctx().copy_text(v.clone());
                    }
                    r.on_hover_text(format!("{v}\n\n(click to copy)"));
                    ui.end_row();
                }
            });
        });
    });
}

const TOP_COLS: &[&str] = &["Operation", "Object", "Est. rows", "Actual rows", "Cost %", "Subtree cost", "Elapsed ms", "Reads"];

fn top_operations(ui: &mut Ui, pv: &mut PlanView, stmt: &Statement, theme: &Theme) {
    egui::Frame::new().fill(theme.bg_panel).inner_margin(4.0).show(ui, |ui| {
        ui.set_min_size(ui.available_size());
        let (sort_col, desc) = pv.top_ops_sort;
        let mut nodes: Vec<usize> = (0..stmt.nodes.len()).collect();
        let key = |i: usize| -> f64 {
            let n = &stmt.nodes[i];
            match sort_col {
                2 => n.est_rows,
                3 => n.actual.as_ref().map(|a| a.rows as f64).unwrap_or(-1.0),
                4 => n.cost_pct,
                5 => n.est_subtree_cost,
                6 => n.actual.as_ref().map(|a| a.elapsed_ms as f64).unwrap_or(-1.0),
                7 => n.actual.as_ref().and_then(|a| a.logical_reads).map(|r| r as f64).unwrap_or(-1.0),
                _ => 0.0,
            }
        };
        if sort_col >= 2 {
            nodes.sort_by(|a, b| key(*b).partial_cmp(&key(*a)).unwrap_or(std::cmp::Ordering::Equal));
            if !desc {
                nodes.reverse();
            }
        } else {
            nodes.sort_by(|a, b| {
                let (na, nb) = (&stmt.nodes[*a], &stmt.nodes[*b]);
                let (ka, kb) = if sort_col == 0 { (&na.physical_op, &nb.physical_op) } else { (&na.physical_op, &nb.physical_op) };
                let o = ka.cmp(kb);
                if desc { o.reverse() } else { o }
            });
        }
        let mut new_sort = pv.top_ops_sort;
        let mut select: Option<usize> = None;
        egui::ScrollArea::both().id_salt(("topops", stmt.id)).auto_shrink([false, false]).show(ui, |ui| {
            egui::Grid::new(("topops-grid", stmt.id)).num_columns(TOP_COLS.len()).striped(true).spacing([14.0, 2.0]).show(ui, |ui| {
                for (i, c) in TOP_COLS.iter().enumerate() {
                    let arrow = if sort_col == i { if desc { " ▼" } else { " ▲" } } else { "" };
                    if ui.add(egui::Button::new(RichText::new(format!("{c}{arrow}")).size(11.5).strong()).frame(false)).clicked() {
                        new_sort = if sort_col == i { (i, !desc) } else { (i, true) };
                    }
                }
                ui.end_row();
                for ni in nodes {
                    let n = &stmt.nodes[ni];
                    let selected = pv.selected_node == Some(ni);
                    let color = if selected { theme.accent } else { theme.text };
                    let cells = [
                        n.physical_op.clone(),
                        n.object.as_ref().map(|o| o.display()).unwrap_or_default(),
                        cobalt_plan::fmt_thousands(n.est_rows),
                        n.actual.as_ref().map(|a| cobalt_plan::fmt_thousands(a.rows as f64)).unwrap_or_default(),
                        format!("{:.1}", n.cost_pct),
                        format!("{:.4}", n.est_subtree_cost),
                        n.actual.as_ref().map(|a| a.elapsed_ms.to_string()).unwrap_or_default(),
                        n.actual.as_ref().and_then(|a| a.logical_reads).map(|r| r.to_string()).unwrap_or_default(),
                    ];
                    for c in cells {
                        let r = ui.add(egui::Label::new(RichText::new(c).size(11.5).color(color)).sense(Sense::click()));
                        if r.clicked() {
                            select = Some(ni);
                        }
                    }
                    ui.end_row();
                }
            });
        });
        pv.top_ops_sort = new_sort;
        if let Some(s) = select {
            pv.selected_node = Some(s);
            pv.show_properties = true;
        }
    });
}

pub fn metric_label(m: Metric) -> &'static str {
    match m {
        Metric::Cost => "Cost",
        Metric::SubtreeCost => "Subtree cost",
        Metric::ActualRows => "Actual rows",
        Metric::EstRows => "Estimated rows",
        Metric::ActualElapsed => "Actual elapsed",
        Metric::ActualCpu => "Actual CPU",
        Metric::RowsRead => "Rows read",
    }
}
