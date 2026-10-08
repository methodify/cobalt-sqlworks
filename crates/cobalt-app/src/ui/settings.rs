//! Settings window: edits a draft `Settings`; the app applies and saves it.

use crate::commands::{self, parse_shortcut, shortcut_label, COMMANDS};
use crate::runtime::{RuntimeAction, RuntimeUi};
use crate::ui::theme::Theme;
use cobalt_core::{ExecOptions, QueryShortcut, ResultLayout, Settings, ThemeChoice};
use cobalt_runtime::{ComponentState, JdkVendor};
use crate::ui::widgets::{mnemonic_button, ButtonStyle};
use egui::{Color32, RichText, Stroke, Ui, Vec2};
use egui_phosphor::regular as icons;

pub enum SettingsAction {
    Apply(Settings),
    Close,
    /// Save the draft, then act on the Spark runtime (the window stays open).
    Runtime(RuntimeAction, Settings),
}

/// One page of the settings window.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tab {
    Appearance, Editor, Execution, Results, Connections, Notebooks, Keyboard, Advanced,
}

const TABS: &[(Tab, &str, &str)] = &[
    (Tab::Appearance, icons::PALETTE, "Appearance"),
    (Tab::Editor, icons::CODE, "Editor"),
    (Tab::Execution, icons::PLAY, "Query execution"),
    (Tab::Results, icons::TABLE, "Results & export"),
    (Tab::Connections, icons::PLUGS_CONNECTED, "Connections"),
    (Tab::Notebooks, icons::NOTEBOOK, "Notebooks & Spark"),
    (Tab::Keyboard, icons::KEYBOARD, "Keyboard"),
    (Tab::Advanced, icons::GEAR_SIX, "Advanced"),
];

pub fn show(ctx: &egui::Context, draft: &mut Settings, theme: &Theme, paths_info: &str, runtime: &mut RuntimeUi, scroll_to: Option<&str>) -> Option<SettingsAction> {
    let mut action = None;
    let mut open = true;
    let tab_key = egui::Id::new("settings-tab");
    let mut tab: Tab = ctx.memory(|m| m.data.get_temp(tab_key)).unwrap_or(Tab::Appearance);
    if scroll_to == Some("Spark runtime") {
        tab = Tab::Notebooks;
    }
    crate::ui::chrome::Window::new("Settings").id(egui::Id::new("settings-window")).open(&mut open).default_size([860.0, 620.0]).min_size([640.0, 420.0]).resizable(true).show(ctx, theme, |ui| {
        let total_h = ui.available_height();
        ui.horizontal_top(|ui| {
            // navigation
            ui.allocate_ui_with_layout(Vec2::new(176.0, total_h - 44.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                for (t, icon, label) in TABS {
                    let selected = tab == *t;
                    let text = RichText::new(format!("{icon}   {label}")).size(13.0).color(if selected { theme.text } else { theme.text_muted });
                    // min_size in the column's top-down(Min) layout keeps the label left-aligned
                    // (add_sized would centre it)
                    let r = ui.add(egui::Button::new(text).min_size(Vec2::new(168.0, 30.0)).fill(if selected { theme.bg_selection } else { Color32::TRANSPARENT }).stroke(Stroke::NONE).corner_radius(6.0));
                    r.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, selected, format!("settings tab {label}")));
                    if r.clicked() {
                        tab = *t;
                    }
                }
            });
            ui.add_space(8.0);
            let sep_x = ui.cursor().left();
            ui.painter().line_segment([egui::pos2(sep_x, ui.cursor().top()), egui::pos2(sep_x, ui.cursor().top() + total_h - 44.0)], Stroke::new(1.0, theme.border));
            ui.add_space(12.0);
            // page
            ui.vertical(|ui| {
                ui.set_width(ui.available_width());
                egui::ScrollArea::vertical().id_salt(("settings-page", tab as u8)).auto_shrink([false, false]).max_height(total_h - 44.0).show(ui, |ui| {
                    ui.set_width(ui.available_width() - 8.0);
                    ui.spacing_mut().item_spacing.y = 6.0;
                    match tab {
                            Tab::Appearance => {
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

            ui.checkbox(&mut draft.appearance.spid_in_tab_title, "Show the connection's SPID in tab titles");
            ui.checkbox(&mut draft.appearance.show_welcome, "Show the getting-started pane next to a new empty query tab");
                            }
                            Tab::Editor => {
                            section(ui, theme, "Editor");
            ui.horizontal(|ui| {
                ui.label("Tab size");
                ui.add(egui::DragValue::new(&mut draft.editor.tab_size).range(1..=8));
                ui.checkbox(&mut draft.editor.insert_spaces, "Insert spaces");
                ui.checkbox(&mut draft.editor.word_wrap, "Word wrap");
            });
            ui.checkbox(&mut draft.editor.auto_close_brackets, "Auto-close brackets and quotes").on_hover_text("Typing ( [ { ' \" ` inserts the partner with the caret between; typing the closer over it steps past; Backspace in an empty pair removes both. Pairs are not inserted before a word character, nor quotes right after one or inside an open string.");
            ui.checkbox(&mut draft.editor.auto_surround, "Wrap the selection when typing a bracket or quote").on_hover_text("With text selected, ( [ { ' \" ` wrap it instead of replacing it — at every cursor — and the selection stays on the wrapped text, so a second bracket wraps again. A closing bracket still replaces the selection.");
            ui.checkbox(&mut draft.editor.highlight_current_statement, "Highlight the current statement");
            ui.checkbox(&mut draft.editor.completion_enabled, "IntelliSense");
            ui.checkbox(&mut draft.editor.completion_on_type, "Suggest while typing");
            ui.checkbox(&mut draft.editor.uppercase_keywords_on_complete, "Uppercase keywords on completion");
            ui.horizontal(|ui| {
                ui.label("Format document:");
                ui.checkbox(&mut draft.editor.format_uppercase_keywords, "Uppercase keywords");
                ui.label("indent");
                ui.add(egui::DragValue::new(&mut draft.editor.format_indent).range(1..=8));
                ui.label("blank lines between statements");
                ui.add(egui::DragValue::new(&mut draft.editor.format_blank_lines).range(0..=3));
            });
            ui.label(egui::RichText::new("Your own snippets: edit snippets.toml next to settings.toml (see the template there); it is reloaded automatically. Tab walks the placeholders.").size(11.0).color(theme.text_muted));
                            }
                            Tab::Execution => {
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
            ui.label(RichText::new("Session options for new tabs ((default) = leave the server's setting)").size(11.0).color(theme.text_muted));
            egui::Grid::new("settings-tri").num_columns(4).spacing([12.0, 4.0]).show(ui, |ui| {
                tri_state(ui, "ANSI_NULLS", &mut draft.execution.session.ansi_nulls);
                tri_state(ui, "ANSI_PADDING", &mut draft.execution.session.ansi_padding);
                ui.end_row();
                tri_state(ui, "ANSI_WARNINGS", &mut draft.execution.session.ansi_warnings);
                tri_state(ui, "QUOTED_IDENTIFIER", &mut draft.execution.session.quoted_identifier);
                ui.end_row();
                tri_state(ui, "CONCAT_NULL_YIELDS_NULL", &mut draft.execution.session.concat_null_yields_null);
                tri_state(ui, "NUMERIC_ROUNDABORT", &mut draft.execution.session.numeric_roundabort);
                ui.end_row();
                tri_state(ui, "IMPLICIT_TRANSACTIONS", &mut draft.execution.session.implicit_transactions);
                ui.end_row();
            });
            lock_and_deadlock(ui, &mut draft.execution.session);
                            section(ui, theme, "Query shortcuts");
            ui.label(RichText::new("A key combination runs the SQL in the current tab; {sel} is the selected text or the word at the caret (quotes doubled).").size(11.0).color(theme.text_muted));
            let mut remove: Option<usize> = None;
            egui::Grid::new("query-shortcuts").num_columns(3).spacing([8.0, 4.0]).show(ui, |ui| {
                for (i, q) in draft.execution.query_shortcuts.iter_mut().enumerate() {
                    let ok = q.keys.trim().is_empty() || parse_shortcut(&q.keys).is_some();
                    let r = ui.add(egui::TextEdit::singleline(&mut q.keys).desired_width(90.0).hint_text("Alt+F1"));
                    if !ok {
                        ui.painter().rect_stroke(r.rect, 2.0, egui::Stroke::new(1.0, theme.error), egui::StrokeKind::Outside);
                    }
                    ui.add(egui::TextEdit::singleline(&mut q.sql).desired_width(360.0).font(egui::FontId::monospace(12.0)).hint_text("EXEC sp_help N'{sel}'"));
                    if ui.small_button(egui_phosphor::regular::X).clicked() {
                        remove = Some(i);
                    }
                    ui.end_row();
                }
            });
            if let Some(i) = remove {
                draft.execution.query_shortcuts.remove(i);
            }
            ui.horizontal(|ui| {
                if ui.small_button("Add shortcut").clicked() {
                    draft.execution.query_shortcuts.push(QueryShortcut::default());
                }
                if ui.small_button("Restore defaults").clicked() {
                    draft.execution.query_shortcuts = QueryShortcut::defaults();
                }
            });
                            }
                            Tab::Results => {
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
                            }
                            Tab::Connections => {
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
                            }
                            Tab::Notebooks => {
                            section(ui, theme, "Libraries");
                            libraries_ui(ui, theme, draft, runtime, paths_info, &mut action);
                            section(ui, theme, "Notebooks");
            ui.horizontal(|ui| {
                ui.label("New notebooks start as");
                ui.selectable_value(&mut draft.notebooks.default_language, "sql".to_string(), "SQL");
                ui.selectable_value(&mut draft.notebooks.default_language, "pyspark".to_string(), "PySpark");
            });
            ui.horizontal(|ui| {
                ui.label("Rows saved per result set in the .ipynb");
                ui.add(egui::DragValue::new(&mut draft.notebooks.max_output_rows).range(0..=100_000).speed(50));
                ui.label("Grid height (rows)");
                ui.add(egui::DragValue::new(&mut draft.notebooks.grid_rows).range(3..=60));
            });
            ui.horizontal(|ui| {
                ui.label("Rows a Spark DataFrame brings back");
                ui.add(egui::DragValue::new(&mut draft.notebooks.spark_row_limit).range(1..=1_000_000).speed(100));
                ui.label(RichText::new("(bare expressions and %%sql cells; display(df) shows 1,000 like Fabric unless given limit=)").size(11.0).color(theme.text_muted));
            });
            ui.label(RichText::new("Saved rows travel inside the notebook as Arrow (plus HTML/Markdown previews for other tools), so grids come back when the notebook is reopened.").size(11.0).color(theme.text_muted));
            section(ui, theme, "Session lifecycle");
            ui.label(RichText::new("One local Spark session serves every notebook; each notebook runs in its own context (variables, temp views and default lakehouse of its own; tables and clones shared). These settings decide when the session starts and ends on its own.").size(11.0).color(theme.text_muted));
            ui.horizontal(|ui| {
                ui.label("Start the session");
                for (v, label, hint) in [("notebook_open", "when a Spark notebook opens", "A new or opened notebook on the Local Spark kernel brings the session up in the background, so its first cell does not wait (20–60 s). Costs the driver's memory while you read."), ("app_start", "when Cobalt starts", "A plain session starts a minute after launch; a lakehouse notebook rebinds it on first use."), ("first_cell", "only when a cell runs", "Nothing starts until you run something.")] {
                    if ui.selectable_label(draft.spark.early_start == v, label).on_hover_text(hint).clicked() {
                        draft.spark.early_start = v.into();
                    }
                }
            });
            ui.horizontal(|ui| {
                ui.label("End the session");
                for (v, label, hint) in [("idle", "after idle minutes", "Stops when no cell has run and no preload has been active for the given time; the next cell starts a fresh session."), ("keep", "only when I stop it", "The session runs until Stop, Restart or Cobalt exits."), ("last_notebook", "when the last Spark notebook closes", "Closing the last notebook on the Local Spark kernel stops the session.")] {
                    if ui.selectable_label(draft.spark.lifecycle == v, label).on_hover_text(hint).clicked() {
                        draft.spark.lifecycle = v.into();
                    }
                }
                if draft.spark.lifecycle == "idle" {
                    ui.add(egui::DragValue::new(&mut draft.spark.idle_minutes).range(5..=24 * 60).speed(5).suffix(" min"));
                }
            });
            ui.horizontal(|ui| {
                ui.label("Lakehouse Files");
                for (v, label, hint) in [("lazy", "stream from OneLake, fetch on first open", "Spark reads and writes Files/ straight from OneLake (nothing is copied; writes need write-through). Python's /lakehouse/default/Files fetches a single file the first time it is opened and lists folders from OneLake. Native readers that bypass Python's open (DuckDB, Arrow files) need the folder pulled first: sync_files(paths=[...])."), ("mirror", "mirror folders locally", "The worker's full mirror: folders you pull with sync_files live under the runtime's state folder and Spark's Files/ points there. Use when you need whole folders on disk."), ] {
                    if ui.selectable_label(draft.spark.files_mode == v, label).on_hover_text(hint).clicked() {
                        draft.spark.files_mode = v.into();
                    }
                }
                ui.label(RichText::new("(applies to the next session)").size(11.0).color(theme.text_faint));
            });
            ui.label(RichText::new("Per lakehouse (lakehouse button on a notebook): what to preload when a session attaches it and whether to keep its clones between sessions.").size(11.0).color(theme.text_muted));
                            section(ui, theme, "Spark runtime");
            spark_runtime(ui, theme, draft, runtime, &mut action);
                            }
                            Tab::Keyboard => {
                            section(ui, theme, "Keyboard shortcuts");
            ui.label(RichText::new("Type a combination such as Ctrl+Shift+R, F6 or Alt+Enter; clear the box to unbind; Reset returns the default.").size(11.0).color(theme.text_muted));
            egui::Grid::new("keybindings").num_columns(3).spacing([10.0, 3.0]).striped(true).show(ui, |ui| {
                for c in COMMANDS {
                    ui.label(c.label);
                    let default_text = c.default_key.map(shortcut_label).unwrap_or_default();
                    let mut text = draft.keybindings.get(c.id).cloned().unwrap_or_else(|| default_text.clone());
                    let r = ui.add(egui::TextEdit::singleline(&mut text).desired_width(150.0).hint_text("unbound"));
                    if r.changed() {
                        if text == default_text {
                            draft.keybindings.remove(c.id);
                        } else {
                            draft.keybindings.insert(c.id.to_string(), text.clone());
                        }
                    }
                    let valid = text.trim().is_empty() || parse_shortcut(&text).is_some();
                    if !valid {
                        ui.painter().rect_stroke(r.rect, 2.0, egui::Stroke::new(1.0, theme.error), egui::StrokeKind::Outside);
                    }
                    let overridden = draft.keybindings.contains_key(c.id);
                    if ui.add_enabled(overridden, egui::Button::new("Reset").small()).clicked() {
                        draft.keybindings.remove(c.id);
                    }
                    ui.end_row();
                }
            });
            let _ = commands::info;
                            }
                            Tab::Advanced => {
                            section(ui, theme, "Updates");
            ui.checkbox(&mut draft.updates.check_on_startup, "Check for updates at start-up (one anonymous request to GitHub)");
            if let Some(v) = draft.updates.skipped_version.clone() {
                ui.horizontal(|ui| {
                    ui.label(format!("Skipping version {v}"));
                    if ui.small_button("Stop skipping").clicked() {
                        draft.updates.skipped_version = None;
                    }
                });
            }
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
            ui.horizontal(|ui| {
                ui.label("Renderer");
                let choices: [(&str, &str); 3] = [("auto", "Auto (GPU; Mesa OpenGL when there is no GPU)"), ("wgpu-only", "GPU only (wgpu, WARP without a GPU)"), ("opengl", "OpenGL (Mesa llvmpipe if present next to the exe)")];
                let current = if draft.advanced.renderer == "wgpu" { "auto" } else { draft.advanced.renderer.as_str() };
                let label = choices.iter().find(|(k, _)| *k == current).map(|(_, l)| *l).unwrap_or("Auto");
                egui::ComboBox::from_id_salt("renderer").width(360.0).selected_text(label).show_ui(ui, |ui| {
                    for (k, l) in choices {
                        ui.selectable_value(&mut draft.advanced.renderer, k.to_string(), l);
                    }
                });
            });
            ui.label(RichText::new(format!("Renderer changes apply at the next start. Now: {}{}", crate::gpu::adapter_label().unwrap_or("?"), if crate::gpu::is_software() { " · software rendering" } else { "" })).size(11.0).color(theme.text_faint));
            ui.label(RichText::new(paths_info).size(11.0).color(theme.text_faint));
                            }
                    }
                    ui.add_space(8.0);
                });
            });
        });
        ui.separator();
        ui.horizontal(|ui| {
            if mnemonic_button(ui, theme, "&Save", ButtonStyle::Primary, true, None) {
                action = Some(SettingsAction::Apply(draft.clone()));
            }
            if mnemonic_button(ui, theme, "&Cancel", ButtonStyle::Normal, true, Some(egui::Key::Escape)) {
                action = Some(SettingsAction::Close);
            }
            if ui.button("Reset this page to defaults").on_hover_text("Only the settings on the current page").clicked() {
                let d = Settings::default();
                match tab {
                    Tab::Appearance => draft.appearance = d.appearance,
                    Tab::Editor => draft.editor = d.editor,
                    Tab::Execution => draft.execution = d.execution,
                    Tab::Results => {
                        draft.results = d.results;
                        draft.export = d.export;
                    }
                    Tab::Connections => {
                        draft.connections = d.connections;
                        draft.history = d.history;
                    }
                    Tab::Notebooks => {
                        draft.notebooks = d.notebooks;
                        draft.spark = d.spark;
                    }
                    Tab::Keyboard => draft.keybindings = d.keybindings,
                    Tab::Advanced => {
                        draft.updates = d.updates;
                        draft.advanced = d.advanced;
                    }
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new("Changes apply when you save").size(11.0).color(theme.text_faint));
            });
        });
    });
    ctx.memory_mut(|m| m.data.insert_temp(tab_key, tab));
    if !open {
        action = Some(SettingsAction::Close);
    }
    action
}

fn section(ui: &mut Ui, theme: &Theme, title: &str) -> egui::Response {
    ui.add_space(6.0);
    let r = ui.label(RichText::new(title).size(15.0).strong().color(theme.text));
    ui.add_space(2.0);
    ui.painter().line_segment([ui.cursor().left_top(), egui::pos2(ui.cursor().left_top().x + ui.available_width(), ui.cursor().top())], Stroke::new(1.0, theme.border));
    ui.add_space(6.0);
    r
}

/// Settings → Notebooks & Spark → Libraries: Python packages and Java libraries for the runtime.
fn libraries_ui(ui: &mut Ui, theme: &Theme, draft: &mut Settings, runtime: &mut RuntimeUi, _paths_info: &str, action: &mut Option<SettingsAction>) {
    ui.label(RichText::new("Python packages are installed into the runtime's environment with uv and are importable in Spark sessions (restart a running session). Jar files and Maven coordinates are handed to each Spark session as it starts: jars join spark.jars, Maven packages are resolved by Ivy with their dependencies (the first start with a new package waits on the download).").size(12.0).color(theme.text_muted));
    ui.add_space(4.0);
    let (py_status, jar_status) = {
        // status comes from the saved settings' runtime folder; the lists edited here are the draft
        let paths = crate::runtime::dirs(draft, &cobalt_store::AppPaths::new().unwrap_or_else(|_| cobalt_store::AppPaths::for_test(std::env::temp_dir().join("cobalt-paths"))));
        let _ = paths;
        let cache_paths = cobalt_store::AppPaths::new().unwrap_or_else(|_| cobalt_store::AppPaths::for_test(std::env::temp_dir().join("cobalt-paths")));
        runtime.library_status(draft, &cache_paths)
    };
    let busy = runtime.job.is_some();
    // ---- Python ----
    ui.label(RichText::new("Python packages").strong());
    let mut remove: Option<usize> = None;
    for (i, spec) in draft.spark.python_packages.clone().iter().enumerate() {
        ui.horizontal(|ui| {
            let installed = py_status.iter().find(|(s, _)| s == spec).and_then(|(_, v)| v.clone());
            let (icon, color) = match &installed {
                Some(_) => (egui_phosphor::regular::CHECK_CIRCLE, theme.success),
                None => (egui_phosphor::regular::CIRCLE_DASHED, theme.text_faint),
            };
            ui.label(RichText::new(icon).color(color));
            ui.label(RichText::new(spec).monospace().size(12.0));
            if let Some(v) = installed {
                ui.label(RichText::new(format!("installed {v}")).size(11.0).color(theme.text_muted));
            } else {
                ui.label(RichText::new("not installed").size(11.0).color(theme.text_faint));
            }
            if ui.small_button(egui_phosphor::regular::X).on_hover_text("Remove from the list (does not uninstall)").clicked() {
                remove = Some(i);
            }
        });
    }
    if let Some(i) = remove {
        draft.spark.python_packages.remove(i);
    }
    let py_input = egui::Id::new("lib-py-input");
    let mut text: String = ui.memory(|m| m.data.get_temp(py_input)).unwrap_or_default();
    ui.horizontal(|ui| {
        let r = ui.add(egui::TextEdit::singleline(&mut text).hint_text("package, package==1.2, or a path to a .whl").desired_width(360.0));
        let submit = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if (ui.small_button("Add").clicked() || submit) && !text.trim().is_empty() {
            draft.spark.python_packages.push(text.trim().to_string());
            text.clear();
        }
        if ui.small_button("Add wheel…").clicked() {
            if let Some(p) = rfd::FileDialog::new().add_filter("Python packages", &["whl", "gz", "zip"]).pick_file() {
                draft.spark.python_packages.push(p.to_string_lossy().to_string());
            }
        }
    });
    ui.memory_mut(|m| m.data.insert_temp(py_input, text));
    ui.add_space(6.0);
    // ---- Java ----
    ui.label(RichText::new("Java libraries").strong());
    let mut remove_jar: Option<usize> = None;
    for (i, j) in draft.spark.jars.clone().iter().enumerate() {
        ui.horizontal(|ui| {
            let ok = jar_status.iter().find(|(s, _)| s == j).map(|(_, ok)| *ok).unwrap_or(false);
            ui.label(RichText::new(if ok { egui_phosphor::regular::CHECK_CIRCLE } else { egui_phosphor::regular::WARNING }).color(if ok { theme.success } else { theme.warning }));
            ui.label(RichText::new(j).monospace().size(12.0));
            if !ok {
                ui.label(RichText::new("file not found").size(11.0).color(theme.warning));
            }
            if ui.small_button(egui_phosphor::regular::X).clicked() {
                remove_jar = Some(i);
            }
        });
    }
    if let Some(i) = remove_jar {
        draft.spark.jars.remove(i);
    }
    let mut remove_mvn: Option<usize> = None;
    for (i, m) in draft.spark.maven.clone().iter().enumerate() {
        ui.horizontal(|ui| {
            let ok = jar_status.iter().find(|(s, _)| s == m).map(|(_, ok)| *ok).unwrap_or(false);
            let valid = cobalt_runtime::libraries::MavenCoord::parse(m).is_some();
            let _ = ok;
            let (icon, color, note) = if !valid { (egui_phosphor::regular::WARNING, theme.error, "not group:artifact:version") } else { (egui_phosphor::regular::CHECK_CIRCLE, theme.success, "resolved at session start") };
            ui.label(RichText::new(icon).color(color));
            ui.label(RichText::new(format!("{} {m}", egui_phosphor::regular::PACKAGE)).monospace().size(12.0));
            ui.label(RichText::new(note).size(11.0).color(theme.text_muted));
            if ui.small_button(egui_phosphor::regular::X).clicked() {
                remove_mvn = Some(i);
            }
        });
    }
    if let Some(i) = remove_mvn {
        draft.spark.maven.remove(i);
    }
    let mvn_input = egui::Id::new("lib-mvn-input");
    let mut mtext: String = ui.memory(|m| m.data.get_temp(mvn_input)).unwrap_or_default();
    ui.horizontal(|ui| {
        let r = ui.add(egui::TextEdit::singleline(&mut mtext).hint_text("Maven coordinate: group:artifact:version").desired_width(360.0));
        let submit = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if (ui.small_button("Add").clicked() || submit) && !mtext.trim().is_empty() {
            draft.spark.maven.push(mtext.trim().to_string());
            mtext.clear();
        }
        if ui.small_button("Add jar…").clicked() {
            if let Some(files) = rfd::FileDialog::new().add_filter("Java libraries", &["jar"]).pick_files() {
                for p in files {
                    draft.spark.jars.push(p.to_string_lossy().to_string());
                }
            }
        }
    });
    ui.memory_mut(|m| m.data.insert_temp(mvn_input, mtext));
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        let any = !draft.spark.python_packages.is_empty();
        let b = ui.add_enabled(!busy && any, egui::Button::new("Install Python packages"));
        b.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "install libraries"));
        if b.on_hover_text("Saves these settings and installs the Python packages into the environment (jars and Maven packages need no install step)").clicked() {
            *action = Some(SettingsAction::Runtime(RuntimeAction::InstallLibraries, draft.clone()));
        }
        if let Some(job) = &runtime.job {
            if job.kind == crate::runtime::JobKind::Libraries {
                ui.spinner();
                ui.label(RichText::new(job.step.as_ref().map(|(_, l)| l.clone()).unwrap_or_else(|| "Working…".into())).size(12.0).color(theme.text_muted));
            }
        }
    });
}

fn component_row(ui: &mut Ui, theme: &Theme, name: &str, st: &ComponentState) {
    let (icon, color, what) = match st {
        ComponentState::Managed { .. } => (egui_phosphor::regular::CHECK_CIRCLE, theme.success, "installed by Cobalt"),
        ComponentState::Adopted { .. } => (egui_phosphor::regular::CHECK_CIRCLE, theme.success, "found on this machine"),
        ComponentState::Missing { .. } => (egui_phosphor::regular::CIRCLE_DASHED, theme.text_faint, "missing"),
    };
    ui.label(RichText::new(icon).color(color));
    ui.label(RichText::new(name).strong());
    ui.label(RichText::new(st.detail()).size(12.0));
    ui.label(RichText::new(what).size(11.0).color(theme.text_faint));
    ui.end_row();
}

/// Settings → Spark runtime: profile and JDK choices, component status, install / smoke test /
/// remove, and the live log of the running job.
fn spark_runtime(ui: &mut Ui, theme: &Theme, draft: &mut Settings, runtime: &mut RuntimeUi, action: &mut Option<SettingsAction>) {
    let manifest = cobalt_runtime::Manifest::embedded();
    ui.label(RichText::new("Cobalt provisions a local Spark that matches a Fabric runtime (uv, Python, pyspark + delta-spark via local-spark-mcp, and a non-Oracle JDK) into its own folder. Nothing is downloaded until you ask. PySpark and Spark SQL notebook cells run on it (kernel button on the notebook toolbar); this page installs and verifies the runtime.").size(12.0).color(theme.text_muted));
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label("Runtime profile");
        let current = draft.spark.profile.clone();
        egui::ComboBox::from_id_salt("spark-profile").width(130.0).selected_text(&current).show_ui(ui, |ui| {
            for (name, p) in &manifest.profiles {
                ui.selectable_value(&mut draft.spark.profile, name.clone(), name).on_hover_text(p.describe());
            }
        });
        if let Ok(p) = manifest.profile(&draft.spark.profile) {
            ui.label(RichText::new(p.describe()).size(11.0).color(theme.text_muted));
        }
    });
    ui.horizontal(|ui| {
        ui.label("JDK");
        let vendor = if draft.spark.jdk_vendor.eq_ignore_ascii_case("temurin") { JdkVendor::Temurin } else { JdkVendor::Microsoft };
        egui::ComboBox::from_id_salt("spark-jdk").width(220.0).selected_text(vendor.label()).show_ui(ui, |ui| {
            for v in JdkVendor::ALL {
                if ui.selectable_label(vendor == v, v.label()).clicked() {
                    draft.spark.jdk_vendor = match v {
                        JdkVendor::Microsoft => "microsoft".into(),
                        JdkVendor::Temurin => "temurin".into(),
                    };
                }
            }
        });
        ui.label("Driver memory");
        ui.add(egui::TextEdit::singleline(&mut draft.spark.driver_memory).desired_width(50.0));
    });
    // "Use what I have": pick an existing JDK
    if let Some(st) = &runtime.status {
        if !st.jdk_candidates.is_empty() {
            ui.horizontal(|ui| {
                ui.label("Java on this machine");
                let current = draft.spark.java_home.clone().unwrap_or_default();
                let label = if current.is_empty() { "Let Cobalt install one".to_string() } else { current.clone() };
                egui::ComboBox::from_id_salt("spark-java-home").width(420.0).selected_text(RichText::new(label).size(12.0)).show_ui(ui, |ui| {
                    if ui.selectable_label(current.is_empty(), "Let Cobalt install one").clicked() {
                        draft.spark.java_home = None;
                    }
                    for c in &st.jdk_candidates {
                        if c.source == "managed" {
                            continue;
                        }
                        let p = c.home.to_string_lossy().to_string();
                        if ui.selectable_label(current == p, RichText::new(c.label()).size(12.0)).clicked() {
                            draft.spark.java_home = Some(p.clone());
                        }
                    }
                });
            });
        }
    }
    ui.horizontal(|ui| {
        ui.label("Folder");
        let mut dir = draft.spark.runtime_dir.clone().unwrap_or_default();
        let r = ui.add(egui::TextEdit::singleline(&mut dir).hint_text("default: the app's local data folder").desired_width(380.0));
        if r.changed() {
            draft.spark.runtime_dir = if dir.trim().is_empty() { None } else { Some(dir) };
        }
        if ui.small_button("Open").clicked() {
            *action = Some(SettingsAction::Runtime(RuntimeAction::OpenFolder, draft.clone()));
        }
    });
    ui.add_space(6.0);
    match &runtime.status {
        None => {
            ui.horizontal(|ui| {
                ui.label(RichText::new(if runtime.status_pending { "Checking what is installed…" } else { "Status unknown" }).color(theme.text_muted));
                if !runtime.status_pending && ui.small_button("Check").clicked() {
                    *action = Some(SettingsAction::Runtime(RuntimeAction::Refresh, draft.clone()));
                }
            });
        }
        Some(st) => {
            egui::Grid::new("spark-status").num_columns(4).spacing([8.0, 4.0]).show(ui, |ui| {
                component_row(ui, theme, "uv", &st.uv);
                component_row(ui, theme, "Python", &st.python);
                component_row(ui, theme, "Spark", &st.env);
                component_row(ui, theme, "Java", &st.jdk);
            });
            ui.horizontal(|ui| {
                let ready = st.is_ready();
                let msg = if ready && st.warm { format!("Ready — Spark {} verified", st.spark_version.clone().unwrap_or_default()) } else if ready { "Installed; run the smoke test to verify".to_string() } else { "Not installed".to_string() };
                ui.label(RichText::new(msg).strong().color(if ready { theme.success } else { theme.text_muted }));
                ui.label(RichText::new(format!("· {} on disk", cobalt_runtime::fmt_bytes(st.disk_bytes))).size(11.0).color(theme.text_faint));
                if st.profile != draft.spark.profile {
                    ui.label(RichText::new(format!("(status is for {}; save to re-check)", st.profile)).size(11.0).color(theme.warning));
                }
            });
            if let Some(e) = &st.last_error {
                ui.label(RichText::new(format!("Last run failed: {e}")).size(12.0).color(theme.error));
            }
            if let Some(h) = &st.health {
                let text = if h.ok { format!("Healthcheck OK{}", h.protocol_version.map(|v| format!(" · worker protocol v{v}")).unwrap_or_default()) } else { format!("Healthcheck: {} problem{}", h.problems.len(), if h.problems.len() == 1 { "" } else { "s" }) };
                ui.label(RichText::new(text).size(12.0).color(if h.ok { theme.success } else { theme.error }));
                for p in &h.problems {
                    ui.label(RichText::new(format!("  {} {p}", egui_phosphor::regular::WARNING)).size(11.0).color(theme.error));
                }
                for w in &h.warnings {
                    ui.label(RichText::new(format!("  {w}")).size(11.0).color(theme.warning));
                }
            }
        }
    }
    ui.add_space(4.0);
    let busy = runtime.job.is_some();
    ui.horizontal(|ui| {
        let ready = runtime.status.as_ref().map(|s| s.is_ready()).unwrap_or(false);
        // what the install will actually do: present components are kept, only the rest is fetched
        let pinned = manifest.local_spark_mcp.version.clone();
        let (label, plan_text) = match runtime.status.as_ref() {
            Some(st) => {
                let kept = |c: &cobalt_runtime::ComponentState| matches!(c, cobalt_runtime::ComponentState::Managed { .. } | cobalt_runtime::ComponentState::Adopted { .. });
                let mut lines: Vec<String> = Vec::new();
                lines.push(format!("uv: {}", if kept(&st.uv) { "kept" } else { "downloaded" }));
                lines.push(format!("Python: {}", if kept(&st.python) { "kept" } else { "installed by uv" }));
                let pkg_update = st.package_version.as_deref().map(|v| v != pinned).unwrap_or(false);
                lines.push(match (&st.package_version, kept(&st.env)) {
                    (Some(v), false) if pkg_update => format!("Spark package: local-spark-mcp {v} → {pinned} (pyspark/delta-spark kept)"),
                    (_, true) => format!("Spark package: local-spark-mcp {pinned} reinstalled over itself (fast, cached)"),
                    _ => format!("Spark package: local-spark-mcp {pinned} + pyspark/delta-spark installed"),
                });
                lines.push(format!("JDK: {}", if kept(&st.jdk) { "kept" } else { "downloaded" }));
                lines.push("then one Spark start to cache its jars (~20 s)".into());
                let label = if pkg_update && kept(&st.uv) && kept(&st.python) && kept(&st.jdk) { format!("Update Spark to {pinned}") } else if ready { "Reinstall / update".to_string() } else { "Install for me".to_string() };
                (label, lines.join("
"))
            }
            None => ("Install for me".to_string(), "Downloads what is missing (uv, Python, pyspark/delta-spark, a JDK), then starts Spark once so its jars are cached.".to_string()),
        };
        let b = ui.add_enabled(!busy, egui::Button::new(label));
        b.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "install spark runtime"));
        if b.on_hover_text(format!("What this will do:
{plan_text}
Hash-checked and resumable; nothing present is downloaded again.")).clicked() {
            *action = Some(SettingsAction::Runtime(RuntimeAction::Install, draft.clone()));
        }
        let b = ui.add_enabled(!busy && ready, egui::Button::new("Run smoke test"));
        b.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "spark smoke test"));
        if b.on_hover_text("Starts a Spark session in the worker and runs SELECT 1").clicked() {
            *action = Some(SettingsAction::Runtime(RuntimeAction::SmokeTest, draft.clone()));
        }
        if ui.add_enabled(busy, egui::Button::new("Cancel")).clicked() {
            *action = Some(SettingsAction::Runtime(RuntimeAction::Cancel, draft.clone()));
        }
        if ui.add_enabled(!busy, egui::Button::new("Re-check")).clicked() {
            *action = Some(SettingsAction::Runtime(RuntimeAction::Refresh, draft.clone()));
        }
        if ui.add_enabled(!busy, egui::Button::new(RichText::new("Remove runtime").color(theme.error))).on_hover_text("Deletes everything Cobalt installed in the runtime folder (tools found on the machine are untouched)").clicked() {
            *action = Some(SettingsAction::Runtime(RuntimeAction::Remove, draft.clone()));
        }
        if ui.small_button("Log file").clicked() {
            *action = Some(SettingsAction::Runtime(RuntimeAction::OpenLog, draft.clone()));
        }
    });
    if let Some(st) = &runtime.status {
        ui.horizontal(|ui| {
            let mb = st.mirror_bytes as f64 / 1_048_576.0;
            ui.label(RichText::new(format!("Lakehouse Files mirror: {}", if st.mirror_bytes == 0 { "empty".to_string() } else if mb < 1024.0 { format!("{mb:.0} MB") } else { format!("{:.1} GB", mb / 1024.0) })).size(12.0).color(theme.text_muted));
            if st.mirror_bytes > 0 && ui.add_enabled(!busy, egui::Button::new("Clear").small()).on_hover_text("Deletes the lazily fetched files and pulled folders of every lakehouse (local writes not yet pushed go with them); they are fetched again on first use. Stop the Spark session first.").clicked() {
                *action = Some(SettingsAction::Runtime(RuntimeAction::ClearMirror, draft.clone()));
            }
        });
    }
    if let Some(job) = &runtime.job {
        ui.add_space(4.0);
        let (label, frac) = match (&job.step, &job.bytes) {
            (Some((_, l)), Some((done, Some(total)))) if *total > 0 => (format!("{l} — {} / {}", cobalt_runtime::fmt_bytes(*done), cobalt_runtime::fmt_bytes(*total)), Some(*done as f32 / *total as f32)),
            (Some((_, l)), Some((done, None))) => (format!("{l} — {}", cobalt_runtime::fmt_bytes(*done)), None),
            (Some((_, l)), None) => (l.clone(), None),
            _ => ("Starting…".to_string(), None),
        };
        let bar = match frac {
            Some(f) => egui::ProgressBar::new(f).text(RichText::new(label).size(12.0)),
            None => egui::ProgressBar::new(0.0).animate(true).text(RichText::new(label).size(12.0)),
        };
        ui.add(bar);
        ui.label(RichText::new(format!("{}s elapsed", job.started.elapsed().as_secs())).size(11.0).color(theme.text_faint));
    }
    if let Some(r) = &runtime.last_result {
        match r {
            Ok(m) => ui.label(RichText::new(m).size(12.0).color(theme.success)),
            Err(e) => ui.add(egui::Label::new(RichText::new(e).size(12.0).color(theme.error)).wrap()),
        };
    }
    if let Some(s) = &runtime.last_smoke {
        ui.label(RichText::new(s).size(12.0).color(theme.text_muted));
    }
    let has_log = runtime.job.as_ref().map(|j| !j.log.is_empty()).unwrap_or(false);
    if has_log || runtime.log_open {
        ui.checkbox(&mut runtime.log_open, "Show log");
        if runtime.log_open {
            if let Some(job) = &runtime.job {
                egui::Frame::new().fill(theme.bg_editor).inner_margin(6.0).show(ui, |ui| {
                    egui::ScrollArea::vertical().id_salt("spark-log").max_height(180.0).stick_to_bottom(true).auto_shrink([false, true]).show(ui, |ui| {
                        ui.style_mut().override_font_id = Some(egui::FontId::monospace(11.0));
                        for l in job.log.iter().rev().take(400).collect::<Vec<_>>().into_iter().rev() {
                            ui.label(l);
                        }
                    });
                });
            }
        }
    }
}

/// A `(default) / ON / OFF` combo for a session option; two grid cells (label + combo).
pub fn tri_state(ui: &mut Ui, name: &str, value: &mut Option<bool>) {
    ui.label(RichText::new(name).monospace().size(12.0));
    let text = match value {
        None => "(default)",
        Some(true) => "ON",
        Some(false) => "OFF",
    };
    egui::ComboBox::from_id_salt(("tri", name)).width(90.0).selected_text(text).show_ui(ui, |ui| {
        ui.selectable_value(value, None, "(default)");
        ui.selectable_value(value, Some(true), "ON");
        ui.selectable_value(value, Some(false), "OFF");
    });
}

/// Lock timeout and deadlock priority controls.
pub fn lock_and_deadlock(ui: &mut Ui, opts: &mut ExecOptions) {
    ui.horizontal(|ui| {
        let mut set = opts.lock_timeout_ms.is_some();
        if ui.checkbox(&mut set, "LOCK_TIMEOUT (ms, -1 = wait)").changed() {
            opts.lock_timeout_ms = if set { Some(opts.lock_timeout_ms.unwrap_or(-1)) } else { None };
        }
        if let Some(ms) = opts.lock_timeout_ms.as_mut() {
            ui.add(egui::DragValue::new(ms).range(-1..=i32::MAX).speed(100));
        }
        ui.add_space(12.0);
        ui.label("DEADLOCK_PRIORITY");
        let text = match opts.deadlock_priority {
            None => "(default)".to_string(),
            Some(-5) => "LOW".into(),
            Some(0) => "NORMAL".into(),
            Some(5) => "HIGH".into(),
            Some(n) => n.to_string(),
        };
        egui::ComboBox::from_id_salt("deadlock").width(90.0).selected_text(text).show_ui(ui, |ui| {
            ui.selectable_value(&mut opts.deadlock_priority, None, "(default)");
            ui.selectable_value(&mut opts.deadlock_priority, Some(-5), "LOW");
            ui.selectable_value(&mut opts.deadlock_priority, Some(0), "NORMAL");
            ui.selectable_value(&mut opts.deadlock_priority, Some(5), "HIGH");
        });
    });
}
