//! The Fabric explorer panel: sign-in state, pinned items, workspaces → SQL-capable items.

use crate::fabric::{FabricAction, FabricState, FabricStatus};
use crate::state::Loadable;
use crate::ui::theme::Theme;
use cobalt_fabric::{SqlItem, SqlItemKind, WorkspaceKind};
use egui::{RichText, Sense, Ui, Vec2};
use egui_phosphor::regular as icons;

const ROW_H: f32 = 24.0;

fn kind_icon(kind: SqlItemKind) -> &'static str {
    match kind {
        SqlItemKind::Warehouse | SqlItemKind::WarehouseSnapshot | SqlItemKind::MirroredWarehouse => icons::WAREHOUSE,
        SqlItemKind::Lakehouse => icons::DROP,
        SqlItemKind::SqlDatabase => icons::DATABASE,
        SqlItemKind::MirroredDatabase | SqlItemKind::MirroredCatalog => icons::COPY,
        SqlItemKind::SqlEndpoint => icons::PLUG,
    }
}

pub fn show(ui: &mut Ui, fabric: &mut FabricState, theme: &Theme) -> Vec<FabricAction> {
    let mut actions = Vec::new();
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        ui.label(RichText::new("FABRIC").size(11.0).strong().color(theme.text_muted));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(6.0);
            let ready = fabric.status() == FabricStatus::Ready;
            let r = ui.add_enabled(ready, egui::Button::new(icons::ARROWS_CLOCKWISE).frame(false));
            r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "fabric refresh"));
            if r.on_hover_text("Refresh workspaces").clicked() {
                actions.push(FabricAction::Refresh);
            }
        });
    });

    match fabric.status() {
        FabricStatus::SignedOut => {
            ui.add_space(12.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new(icons::CUBE).size(36.0).color(theme.text_faint));
                ui.add_space(6.0);
                ui.label(RichText::new("Browse your Fabric workspaces").strong());
                ui.label(RichText::new("Warehouses, SQL analytics endpoints, SQL databases and mirrored databases you can reach, one click from a query.").size(12.0).color(theme.text_muted));
                ui.add_space(10.0);
                let label = match &fabric.account {
                    Some(a) => format!("Continue as {}", a.username),
                    None => "Sign in with Microsoft".to_string(),
                };
                let b = ui.add(egui::Button::new(RichText::new(format!("{} {label}", icons::SIGN_IN)).color(egui::Color32::WHITE)).fill(theme.accent));
                b.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "fabric sign in"));
                if b.clicked() {
                    actions.push(FabricAction::SignIn);
                }
            });
            return actions;
        }
        FabricStatus::SigningIn => {
            ui.add_space(12.0);
            ui.vertical_centered(|ui| {
                ui.spinner();
                ui.label(RichText::new("Signing in…").color(theme.text_muted));
            });
            return actions;
        }
        FabricStatus::Error(e) => {
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                ui.add_space(8.0);
                ui.colored_label(theme.error, format!("{} {e}", icons::WARNING));
            });
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                if ui.small_button("Sign in again").clicked() {
                    actions.push(FabricAction::SignIn);
                }
                if ui.small_button("Retry").clicked() {
                    actions.push(FabricAction::Refresh);
                }
            });
            if !matches!(fabric.workspaces, Loadable::Loaded(_)) {
                return actions;
            }
        }
        FabricStatus::Ready => {}
    }

    // account line
    if let Some(a) = &fabric.account {
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(RichText::new(format!("{} {}", icons::USER_CIRCLE, a.username)).size(12.0).color(theme.text_muted));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(6.0);
                if ui.add(egui::Button::new(RichText::new("Sign out").size(11.0)).frame(false)).clicked() {
                    actions.push(FabricAction::SignOut);
                }
            });
        });
    }

    // search
    ui.horizontal(|ui| {
        ui.add_space(6.0);
        let mut s = fabric.search.clone();
        let r = ui.add(egui::TextEdit::singleline(&mut s).hint_text(format!("{} Filter workspaces and items", icons::MAGNIFYING_GLASS)).desired_width(f32::INFINITY));
        r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, true, "fabric filter"));
        if r.changed() {
            actions.push(FabricAction::Search(s));
        }
    });
    let filter = fabric.search.trim().to_lowercase();

    egui::ScrollArea::vertical().id_salt("fabric-tree").auto_shrink([false, false]).show(ui, |ui| {
        // pinned
        if !fabric.pins.is_empty() {
            section(ui, theme, &format!("{} Pinned", icons::PUSH_PIN));
            for p in fabric.pins.clone() {
                if !filter.is_empty() && !p.display_name.to_lowercase().contains(&filter) && !p.workspace_name.to_lowercase().contains(&filter) {
                    continue;
                }
                let kind = SqlItemKind::parse(&p.item_kind).or_else(|| match p.item_kind.as_str() {
                    "SqlDatabase" => Some(SqlItemKind::SqlDatabase),
                    "SqlEndpoint" => Some(SqlItemKind::SqlEndpoint),
                    _ => None,
                });
                let known = fabric.item(&p.item_id).is_some();
                item_row(ui, theme, &p.item_id, &p.display_name, kind, Some(&p.workspace_name), true, known, fabric, &mut actions, 1);
            }
        }

        section(ui, theme, "Workspaces");
        match &fabric.workspaces {
            Loadable::NotLoaded | Loadable::Loading(_) => {
                ui.horizontal(|ui| {
                    ui.add_space(12.0);
                    ui.spinner();
                    ui.label(RichText::new("Loading workspaces…").color(theme.text_muted));
                });
            }
            Loadable::Failed(e) => {
                ui.horizontal_wrapped(|ui| {
                    ui.add_space(12.0);
                    ui.colored_label(theme.error, e);
                });
            }
            Loadable::Loaded(ws) => {
                if ws.is_empty() {
                    ui.horizontal(|ui| {
                        ui.add_space(12.0);
                        ui.label(RichText::new("No Fabric workspaces for this account.").color(theme.text_muted));
                    });
                }
                for w in ws.clone() {
                    let items = fabric.items.get(&w.id);
                    let matches_ws = filter.is_empty() || w.display_name.to_lowercase().contains(&filter);
                    let matching_items: Vec<SqlItem> = items
                        .and_then(|l| l.get())
                        .map(|v| v.iter().filter(|i| !i.kind.is_child_endpoint() && (matches_ws || i.display_name.to_lowercase().contains(&filter))).cloned().collect())
                        .unwrap_or_default();
                    if !filter.is_empty() && !matches_ws && matching_items.is_empty() {
                        continue;
                    }
                    let expanded = fabric.expanded.contains(&w.id) || (!filter.is_empty() && !matching_items.is_empty());
                    let subtitle = match (&w.kind, &w.capacity_region) {
                        (WorkspaceKind::Personal, _) => "personal".to_string(),
                        (_, Some(r)) => r.clone(),
                        _ => String::new(),
                    };
                    let loaded_empty = items.and_then(|l| l.get()).map(|v| v.iter().all(|i| i.kind.is_child_endpoint())).unwrap_or(false);
                    let r = tree_row(ui, theme, 0, expanded, icons::FOLDER, &w.display_name, if loaded_empty { "no SQL items" } else { &subtitle }, &format!("fabric workspace {}", w.display_name));
                    if r.clicked() {
                        actions.push(FabricAction::ToggleWorkspace(w.id.clone()));
                    }
                    if expanded {
                        match items {
                            Some(Loadable::Loaded(_)) => {
                                for it in matching_items {
                                    let pinned = fabric.is_pinned(&it.id);
                                    item_row(ui, theme, &it.id, &it.display_name, Some(it.kind), None, pinned, true, fabric, &mut actions, 1);
                                }
                            }
                            Some(Loadable::Failed(e)) => {
                                ui.horizontal_wrapped(|ui| {
                                    ui.add_space(28.0);
                                    ui.colored_label(theme.error, e);
                                });
                            }
                            _ => {
                                ui.horizontal(|ui| {
                                    ui.add_space(28.0);
                                    ui.spinner();
                                });
                            }
                        }
                    }
                }
            }
        }
        ui.add_space(8.0);
        if let Some(t) = fabric.last_refresh {
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                let s = t.elapsed().as_secs();
                let ago = if s < 60 { "just now".to_string() } else { format!("{} min ago", s / 60) };
                ui.label(RichText::new(format!("Refreshed {ago}")).size(11.0).color(theme.text_faint));
            });
        }
    });
    actions
}

fn section(ui: &mut Ui, theme: &Theme, label: &str) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        ui.label(RichText::new(label).size(11.0).strong().color(theme.text_muted));
    });
}

fn tree_row(ui: &mut Ui, theme: &Theme, depth: usize, expanded: bool, icon: &str, label: &str, subtitle: &str, a11y: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_H), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, a11y.to_string()));
    if resp.hovered() {
        ui.painter().rect_filled(rect, 3.0, theme.bg_hover);
    }
    let x = rect.left() + 8.0 + depth as f32 * 16.0;
    let cy = rect.center().y;
    let font = egui::FontId::proportional(13.0);
    ui.painter().text(egui::pos2(x, cy), egui::Align2::LEFT_CENTER, if expanded { icons::CARET_DOWN } else { icons::CARET_RIGHT }, egui::FontId::proportional(11.0), theme.text_muted);
    ui.painter().text(egui::pos2(x + 16.0, cy), egui::Align2::LEFT_CENTER, icon, font.clone(), theme.accent);
    let g = ui.painter().layout_no_wrap(label.to_string(), font, theme.text);
    let w = g.size().x;
    ui.painter().galley(egui::pos2(x + 36.0, cy - g.size().y / 2.0), g, theme.text);
    if !subtitle.is_empty() {
        ui.painter().text(egui::pos2(x + 36.0 + w + 8.0, cy), egui::Align2::LEFT_CENTER, subtitle, egui::FontId::proportional(11.0), theme.text_faint);
    }
    resp
}

#[allow(clippy::too_many_arguments)]
fn item_row(ui: &mut Ui, theme: &Theme, item_id: &str, name: &str, kind: Option<SqlItemKind>, subtitle: Option<&str>, pinned: bool, known: bool, fabric: &FabricState, actions: &mut Vec<FabricAction>, depth: usize) {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_H), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("fabric item {name}")));
    if resp.hovered() {
        ui.painter().rect_filled(rect, 3.0, theme.bg_hover);
    }
    let x = rect.left() + 8.0 + depth as f32 * 16.0 + 16.0;
    let cy = rect.center().y;
    let font = egui::FontId::proportional(13.0);
    let provisioning = fabric.details.get(item_id).and_then(|d| d.get()).and_then(|t| t.provisioning.clone());
    let not_ready = provisioning.as_deref().map(|p| !p.eq_ignore_ascii_case("Success")).unwrap_or(false);
    let color = if known { theme.text } else { theme.text_faint };
    ui.painter().text(egui::pos2(x, cy), egui::Align2::LEFT_CENTER, kind.map(kind_icon).unwrap_or(icons::DATABASE), font.clone(), if not_ready { theme.text_faint } else { theme.accent });
    let g = ui.painter().layout_no_wrap(name.to_string(), font, color);
    let w = g.size().x;
    ui.painter().galley(egui::pos2(x + 20.0, cy - g.size().y / 2.0), g, color);
    let sub = match (subtitle, kind) {
        (Some(s), Some(k)) => format!("{} · {s}", k.label()),
        (Some(s), None) => s.to_string(),
        (None, Some(k)) => k.label().to_string(),
        _ => String::new(),
    };
    let sub = if not_ready { format!("{sub} · provisioning") } else if !known { format!("{sub} · not found") } else { sub };
    ui.painter().text(egui::pos2(x + 20.0 + w + 8.0, cy), egui::Align2::LEFT_CENTER, sub, egui::FontId::proportional(11.0), theme.text_faint);
    if pinned {
        ui.painter().text(egui::pos2(rect.right() - 10.0, cy), egui::Align2::RIGHT_CENTER, icons::PUSH_PIN, egui::FontId::proportional(11.0), theme.text_faint);
    }
    if resp.double_clicked() && known {
        actions.push(FabricAction::Open { item_id: item_id.to_string() });
    }
    resp.context_menu(|ui| {
        if ui.add_enabled(known, egui::Button::new("Open query")).clicked() {
            actions.push(FabricAction::Open { item_id: item_id.to_string() });
            ui.close();
        }
        if ui.button(if pinned { "Unpin" } else { "Pin" }).clicked() {
            actions.push(FabricAction::TogglePin { item_id: item_id.to_string() });
            ui.close();
        }
        ui.separator();
        if ui.add_enabled(known, egui::Button::new("Save to Servers…")).clicked() {
            actions.push(FabricAction::SaveToServers { item_id: item_id.to_string() });
            ui.close();
        }
        if ui.add_enabled(known, egui::Button::new("Copy connection string")).clicked() {
            actions.push(FabricAction::CopyConnectionString { item_id: item_id.to_string() });
            ui.close();
        }
        if ui.add_enabled(known, egui::Button::new(format!("{} Open in Fabric portal", icons::ARROW_SQUARE_OUT))).clicked() {
            actions.push(FabricAction::OpenInPortal { item_id: item_id.to_string() });
            ui.close();
        }
    });
    resp.on_hover_text("Double-click: open a query · right-click: more");
}
