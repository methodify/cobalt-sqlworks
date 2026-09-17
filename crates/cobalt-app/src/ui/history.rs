//! Query history sidebar.

use crate::state::AppState;
use crate::ui::theme::Theme;
use crate::ui::widgets::{icon_button, section_title};
use egui::{RichText, Ui};
use egui_phosphor::regular as icons;

#[derive(Debug, Clone)]
pub enum HistoryAction {
    Refresh,
    OpenInTab(i64),
    Run(i64),
    Star(i64, bool),
    Delete(i64),
    ClearAll,
    Copy(String),
}

fn time_ago(t: chrono::DateTime<chrono::Utc>) -> String {
    let d = chrono::Utc::now() - t;
    if d.num_seconds() < 60 {
        "just now".into()
    } else if d.num_minutes() < 60 {
        format!("{} min ago", d.num_minutes())
    } else if d.num_hours() < 24 {
        format!("{} h ago", d.num_hours())
    } else if d.num_days() < 7 {
        format!("{} d ago", d.num_days())
    } else {
        t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string()
    }
}

pub fn show(ui: &mut Ui, state: &mut AppState, theme: &Theme) -> Vec<HistoryAction> {
    let mut actions = Vec::new();
    egui::Frame::new().inner_margin(egui::Margin::symmetric(6, 4)).show(ui, |ui| {
        ui.horizontal(|ui| {
            section_title(ui, theme, "History");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if icon_button(ui, icons::ARROWS_CLOCKWISE, "Refresh", true).clicked() {
                    actions.push(HistoryAction::Refresh);
                }
                let star = if state.history.starred_only { icons::STAR } else { icons::STAR };
                if icon_button(ui, star, "Starred only", true).clicked() {
                    state.history.starred_only = !state.history.starred_only;
                    actions.push(HistoryAction::Refresh);
                }
                if icon_button(ui, icons::TRASH, "Clear history (keeps starred)", true).clicked() {
                    actions.push(HistoryAction::ClearAll);
                }
            });
        });
        let r = ui.add(egui::TextEdit::singleline(&mut state.history.query).hint_text(format!("{} Search history", icons::MAGNIFYING_GLASS)).desired_width(f32::INFINITY));
        if r.changed() {
            actions.push(HistoryAction::Refresh);
        }
    });
    ui.separator();
    if !state.history.loaded {
        actions.push(HistoryAction::Refresh);
    }
    let entries = state.history.entries.clone();
    egui::ScrollArea::vertical().id_salt("history-list").auto_shrink([false, false]).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        for e in &entries {
            let selected = state.history.selected == Some(e.id);
            let first_line: String = e.sql.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with("--")).unwrap_or("").chars().take(90).collect();
            let frame = egui::Frame::new().fill(if selected { theme.bg_selection } else { theme.bg_panel }).inner_margin(egui::Margin::symmetric(8, 5)).corner_radius(4.0);
            let resp = frame
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let (icon, color) = match e.status.as_str() {
                            "success" => (icons::CHECK_CIRCLE, theme.success),
                            "error" => (icons::X_CIRCLE, theme.error),
                            "cancelled" => (icons::PROHIBIT, theme.warning),
                            _ => (icons::CIRCLE_NOTCH, theme.text_muted),
                        };
                        ui.label(RichText::new(icon).color(color));
                        ui.label(RichText::new(&first_line).family(egui::FontFamily::Monospace).size(12.0).color(theme.text));
                    });
                    ui.horizontal(|ui| {
                        let mut meta = format!("{}", e.server);
                        if let Some(db) = &e.database {
                            meta.push_str(&format!(" · {db}"));
                        }
                        meta.push_str(&format!(" · {}", time_ago(e.started)));
                        if let Some(ms) = e.duration_ms {
                            meta.push_str(&format!(" · {:.2}s", ms as f64 / 1000.0));
                        }
                        if let Some(rows) = e.rows {
                            meta.push_str(&format!(" · {} rows", crate::state::fmt_count(rows as u64)));
                        }
                        ui.label(RichText::new(meta).size(10.5).color(theme.text_faint));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let star = if e.starred { icons::STAR } else { icons::STAR };
                            if ui.add(egui::Button::new(RichText::new(star).size(12.0).color(if e.starred { theme.warning } else { theme.text_faint })).frame(false)).clicked() {
                                actions.push(HistoryAction::Star(e.id, !e.starred));
                            }
                        });
                    });
                })
                .response;
            let resp = resp.interact(egui::Sense::click());
            if resp.clicked() {
                state.history.selected = Some(e.id);
            }
            if resp.double_clicked() {
                actions.push(HistoryAction::OpenInTab(e.id));
            }
            resp.on_hover_ui(|ui| {
                ui.set_max_width(600.0);
                ui.label(RichText::new(e.sql.chars().take(1500).collect::<String>()).family(egui::FontFamily::Monospace).size(11.0));
                if let Some(err) = &e.error {
                    ui.label(RichText::new(err).color(theme.error).size(11.0));
                }
            })
            .context_menu(|ui| {
                if ui.button("Open in new tab").clicked() {
                    actions.push(HistoryAction::OpenInTab(e.id));
                    ui.close();
                }
                if ui.button("Open and run").clicked() {
                    actions.push(HistoryAction::Run(e.id));
                    ui.close();
                }
                if ui.button("Copy SQL").clicked() {
                    actions.push(HistoryAction::Copy(e.sql.clone()));
                    ui.close();
                }
                if ui.button(if e.starred { "Unstar" } else { "Star" }).clicked() {
                    actions.push(HistoryAction::Star(e.id, !e.starred));
                    ui.close();
                }
                ui.separator();
                if ui.button(RichText::new("Delete").color(theme.error)).clicked() {
                    actions.push(HistoryAction::Delete(e.id));
                    ui.close();
                }
            });
        }
        if entries.is_empty() && state.history.loaded {
            ui.add_space(12.0);
            ui.vertical_centered(|ui| ui.label(RichText::new("No history yet").color(theme.text_faint)));
        }
        ui.add_space(30.0);
    });
    actions
}
