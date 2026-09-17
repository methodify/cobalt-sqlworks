//! Settings window: edits a draft `Settings`; the app applies and saves it.

use crate::ui::theme::Theme;
use cobalt_core::{ResultLayout, Settings, ThemeChoice};
use egui::{RichText, Ui};

pub enum SettingsAction {
    Apply(Settings),
    Close,
}

pub fn show(ctx: &egui::Context, draft: &mut Settings, theme: &Theme, paths_info: &str) -> Option<SettingsAction> {
    let mut action = None;
    let mut open = true;
    egui::Window::new("Settings").id(egui::Id::new("settings-window")).open(&mut open).default_size([640.0, 560.0]).resizable(true).show(ctx, |ui| {
        egui::ScrollArea::vertical().auto_shrink([false, false]).max_height(480.0).show(ui, |ui| {
            section(ui, theme, "Appearance");
            ui.horizontal(|ui| {
                ui.label("Theme");
                for (c, label) in [(ThemeChoice::System, "Follow system"), (ThemeChoice::Light, "Cobalt Light"), (ThemeChoice::Dark, "Cobalt Dark")] {
                    ui.selectable_value(&mut draft.appearance.theme, c, label);
                }
            });
            ui.horizontal(|ui| {
                ui.label("UI scale");
                ui.add(egui::Slider::new(&mut draft.appearance.ui_scale, 0.75..=2.0).step_by(0.05));
            });
            ui.horizontal(|ui| {
                ui.label("Editor font size");
                ui.add(egui::DragValue::new(&mut draft.appearance.editor_font_size).range(9.0..=32.0));
                ui.label("Grid font size");
                ui.add(egui::DragValue::new(&mut draft.appearance.grid_font_size).range(9.0..=28.0));
            });

            section(ui, theme, "Editor");
            ui.horizontal(|ui| {
                ui.label("Tab size");
                ui.add(egui::DragValue::new(&mut draft.editor.tab_size).range(1..=8));
                ui.checkbox(&mut draft.editor.insert_spaces, "Insert spaces");
                ui.checkbox(&mut draft.editor.word_wrap, "Word wrap");
            });
            ui.checkbox(&mut draft.editor.highlight_current_statement, "Highlight the current statement");
            ui.checkbox(&mut draft.editor.completion_enabled, "IntelliSense");
            ui.checkbox(&mut draft.editor.completion_on_type, "Suggest while typing");
            ui.checkbox(&mut draft.editor.uppercase_keywords_on_complete, "Uppercase keywords on completion");

            section(ui, theme, "Execution");
            ui.horizontal(|ui| {
                ui.label("Row cap per result set (0 = unlimited)");
                ui.add(egui::DragValue::new(&mut draft.execution.row_cap).range(0..=100_000_000).speed(1000));
            });
            ui.horizontal(|ui| {
                ui.label("Command timeout (seconds, 0 = none)");
                ui.add(egui::DragValue::new(&mut draft.execution.command_timeout_secs).range(0..=86_400));
            });
            ui.horizontal(|ui| {
                ui.label("Select Top N rows");
                ui.add(egui::DragValue::new(&mut draft.execution.select_top_n).range(1..=1_000_000).speed(100));
            });

            section(ui, theme, "Results");
            ui.horizontal(|ui| {
                ui.label("Layout");
                ui.selectable_value(&mut draft.results.layout, ResultLayout::Stacked, "Stacked");
                ui.selectable_value(&mut draft.results.layout, ResultLayout::Tabs, "Tabs");
            });
            ui.horizontal(|ui| {
                ui.label("NULL text");
                ui.add(egui::TextEdit::singleline(&mut draft.results.null_text).desired_width(80.0));
                ui.checkbox(&mut draft.results.bit_as_number, "bit as 1/0");
                ui.checkbox(&mut draft.results.show_row_numbers, "Row numbers");
            });
            ui.horizontal(|ui| {
                ui.label("Max column width");
                ui.add(egui::DragValue::new(&mut draft.results.max_column_width).range(60.0..=2000.0));
                ui.label("Date/time format");
                ui.add(egui::TextEdit::singleline(&mut draft.results.datetime_format).desired_width(180.0));
            });
            ui.horizontal(|ui| {
                ui.label("Copy NULL as");
                ui.add(egui::TextEdit::singleline(&mut draft.results.copy_null_as).desired_width(80.0));
            });

            section(ui, theme, "Export defaults");
            ui.horizontal(|ui| {
                ui.label("CSV delimiter");
                ui.add(egui::TextEdit::singleline(&mut draft.export.csv_delimiter).desired_width(40.0));
                ui.checkbox(&mut draft.export.csv_include_headers, "Headers");
                ui.checkbox(&mut draft.export.csv_bom, "UTF-8 BOM");
                ui.checkbox(&mut draft.export.csv_quote_all, "Quote all");
            });
            ui.horizontal(|ui| {
                ui.checkbox(&mut draft.export.json_pretty, "Pretty JSON");
                ui.checkbox(&mut draft.export.excel_freeze_header, "Excel: freeze header");
                ui.checkbox(&mut draft.export.excel_autofilter, "Excel: autofilter");
            });
            ui.horizontal(|ui| {
                ui.label("Parquet compression");
                egui::ComboBox::from_id_salt("pq-comp").selected_text(&draft.export.parquet_compression).show_ui(ui, |ui| {
                    for c in ["zstd", "snappy", "lz4", "none"] {
                        ui.selectable_value(&mut draft.export.parquet_compression, c.to_string(), c);
                    }
                });
                ui.checkbox(&mut draft.export.open_after_save, "Open file after save");
            });

            section(ui, theme, "Connections");
            ui.label(RichText::new("Microsoft Entra ID sign-in uses a public-client app registration. Cobalt's default is used when this is empty; set your own if your tenant requires it.").size(11.0).color(theme.text_muted));
            ui.horizontal(|ui| {
                ui.label("Entra client ID");
                ui.add(egui::TextEdit::singleline(&mut draft.connections.entra_client_id).desired_width(300.0).hint_text("00000000-0000-0000-0000-000000000000"));
            });
            ui.horizontal(|ui| {
                ui.label("Default tenant");
                let mut t = draft.connections.entra_default_tenant.clone().unwrap_or_default();
                if ui.add(egui::TextEdit::singleline(&mut t).desired_width(300.0).hint_text("organizations, common, or a tenant ID/domain")).changed() {
                    draft.connections.entra_default_tenant = if t.trim().is_empty() { None } else { Some(t.trim().to_string()) };
                }
            });
            ui.checkbox(&mut draft.connections.reconnect_on_run, "Reconnect automatically when a run finds the session closed");

            section(ui, theme, "History");
            ui.checkbox(&mut draft.history.capture, "Record query history");
            ui.horizontal(|ui| {
                ui.label("Keep for (days)");
                ui.add(egui::DragValue::new(&mut draft.history.retention_days).range(1..=3650));
                ui.label("Max entries");
                ui.add(egui::DragValue::new(&mut draft.history.max_entries).range(100..=1_000_000).speed(100));
            });

            section(ui, theme, "Advanced");
            ui.horizontal(|ui| {
                ui.label("Result memory budget (MB)");
                let mut mb = (draft.advanced.memory_budget_bytes / (1024 * 1024)) as u64;
                if ui.add(egui::DragValue::new(&mut mb).range(64..=65536).speed(64)).changed() {
                    draft.advanced.memory_budget_bytes = mb * 1024 * 1024;
                }
            });
            ui.horizontal(|ui| {
                ui.label("Log level");
                egui::ComboBox::from_id_salt("log-level").selected_text(&draft.advanced.log_level).show_ui(ui, |ui| {
                    for c in ["error", "warn", "info", "debug", "trace"] {
                        ui.selectable_value(&mut draft.advanced.log_level, c.to_string(), c);
                    }
                });
            });
            ui.label(RichText::new(paths_info).size(11.0).color(theme.text_faint));
        });
        ui.separator();
        ui.horizontal(|ui| {
            if crate::ui::widgets::primary_button(ui, theme, "Save", true).clicked() {
                action = Some(SettingsAction::Apply(draft.clone()));
            }
            if ui.button("Cancel").clicked() {
                action = Some(SettingsAction::Close);
            }
            if ui.button("Reset to defaults").clicked() {
                *draft = Settings::default();
            }
        });
    });
    if !open {
        action = Some(SettingsAction::Close);
    }
    action
}

fn section(ui: &mut Ui, theme: &Theme, title: &str) {
    ui.add_space(8.0);
    ui.label(RichText::new(title).strong().color(theme.accent));
    ui.separator();
}
