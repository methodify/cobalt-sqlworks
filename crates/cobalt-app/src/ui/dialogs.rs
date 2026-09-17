//! Modal dialogs: connection editor, password, auth waiting, group editor, confirmations,
//! export, change connection, execution options, rename, error, ADS import — plus the
//! About and Keyboard Shortcuts windows.

use crate::ops::{self, AUTH_LABELS, FORMAT_LABELS};
use crate::state::*;
use crate::ui::shell::Frame;
use crate::ui::theme::Theme;
use crate::ui::widgets::{key_chip, primary_button};
use cobalt_core::*;
use egui::{Key, RichText, Ui, Vec2};
use egui_phosphor::regular as icons;

fn modal<R>(ctx: &egui::Context, theme: &Theme, id: &str, width: f32, add: impl FnOnce(&mut Ui) -> R) -> (R, bool) {
    let max_h = ctx.content_rect().height() - 60.0;
    let m = egui::Modal::new(egui::Id::new(id)).frame(egui::Frame::window(&ctx.global_style()).fill(theme.bg_panel).inner_margin(16.0)).show(ctx, |ui| {
        ui.set_width(width);
        ui.set_max_height(max_h);
        add(ui)
    });
    let close = m.should_close();
    (m.inner, close)
}

pub fn show(ctx: &egui::Context, f: &mut Frame<'_>) {
    let theme = f.theme;
    match std::mem::take(&mut f.state.dialog) {
        Dialog::None => {}
        Dialog::Connection(d) => connection_dialog(ctx, f, d),
        Dialog::Password { profile, mut password, mut remember, purpose, error } => {
            let mut submit = false;
            let mut cancel = false;
            let (_, close) = modal(ctx, theme, "pw", 380.0, |ui| {
                ui.heading(format!("Password for {}", profile.display_name()));
                ui.label(RichText::new(format!("{} on {}", profile.auth.user_name().unwrap_or(""), profile.server)).color(theme.text_muted));
                ui.add_space(8.0);
                let r = ui.add(egui::TextEdit::singleline(&mut password).password(true).desired_width(f32::INFINITY).id(egui::Id::new("pw-field")));
                r.request_focus();
                if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    submit = true;
                }
                ui.checkbox(&mut remember, "Remember in the OS keychain");
                if let Some(e) = &error {
                    ui.colored_label(theme.error, e);
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if primary_button(ui, theme, "Connect", !password.is_empty()).clicked() {
                        submit = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
            if submit {
                f.state.dialog = Dialog::Password { profile, password, remember, purpose, error };
                ops::submit_password(f.state, f.cx);
            } else if cancel || close {
                if let ConnectPurpose::Tab { tab, .. } = &purpose {
                    if let Some(t) = f.state.tab_mut(*tab) {
                        t.conn = ConnState::Disconnected;
                        t.pending_run = None;
                    }
                }
            } else {
                f.state.dialog = Dialog::Password { profile, password, remember, purpose, error };
            }
        }
        Dialog::AuthWaiting { profile, purpose, message, device, url, cancel, started } => {
            let mut cancelled = false;
            let (_, close) = modal(ctx, theme, "auth", 420.0, |ui| {
                ui.heading(format!("Signing in to {}", profile.display_name()));
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(18.0));
                    ui.label(&message);
                });
                let secs = started.elapsed().as_secs();
                ui.label(RichText::new(format!("Waiting {}:{:02} — the sign-in link stays valid for 15 minutes.", secs / 60, secs % 60)).size(11.0).color(theme.text_muted));
                if let Some(u) = url.lock().clone() {
                    ui.horizontal(|ui| {
                        if ui.small_button(format!("{} Open browser again", icons::ARROW_SQUARE_OUT)).clicked() {
                            cobalt_auth::entra::open_in_browser(&u);
                        }
                        if ui.small_button(format!("{} Copy sign-in link", icons::COPY)).clicked() {
                            ui.ctx().copy_text(u.clone());
                        }
                    });
                }
                if let Some((code, url)) = device.lock().clone() {
                    ui.add_space(8.0);
                    ui.label("Open this page and enter the code:");
                    ui.horizontal(|ui| {
                        ui.hyperlink(&url);
                        if ui.small_button("Open").clicked() {
                            cobalt_auth::entra::open_in_browser(&url);
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&code).size(22.0).strong().family(egui::FontFamily::Monospace));
                        if ui.small_button(icons::COPY).clicked() {
                            ui.ctx().copy_text(code.clone());
                        }
                    });
                }
                ui.add_space(10.0);
                if ui.button("Cancel").clicked() {
                    cancelled = true;
                }
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
            });
            if cancelled || close {
                cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                if let ConnectPurpose::Tab { tab, .. } = &purpose {
                    if let Some(t) = f.state.tab_mut(*tab) {
                        t.conn = ConnState::Disconnected;
                        t.pending_run = None;
                    }
                }
            } else {
                f.state.dialog = Dialog::AuthWaiting { profile, purpose, message, device, url, cancel, started };
            }
        }
        Dialog::Group { mut group, is_new } => {
            let mut save = false;
            let mut cancel = false;
            let (_, close) = modal(ctx, theme, "group", 380.0, |ui| {
                ui.heading(if is_new { "New server group" } else { "Edit server group" });
                ui.add_space(6.0);
                ui.label("Name");
                let r = ui.add(egui::TextEdit::singleline(&mut group.name).desired_width(f32::INFINITY));
                if is_new {
                    r.request_focus();
                }
                ui.label("Color");
                ui.horizontal(|ui| {
                    for c in Color::PALETTE {
                        let (rect, resp) = ui.allocate_exact_size(Vec2::new(24.0, 24.0), egui::Sense::click());
                        ui.painter().rect_filled(rect.shrink(2.0), 5.0, Theme::color32(c));
                        if group.color == c {
                            ui.painter().rect_stroke(rect, 6.0, egui::Stroke::new(2.0, theme.text), egui::StrokeKind::Inside);
                        }
                        if resp.clicked() {
                            group.color = c;
                        }
                    }
                });
                ui.label("Description");
                let mut d = group.description.clone().unwrap_or_default();
                if ui.add(egui::TextEdit::singleline(&mut d).desired_width(f32::INFINITY)).changed() {
                    group.description = if d.trim().is_empty() { None } else { Some(d) };
                }
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if primary_button(ui, theme, "Save", !group.name.trim().is_empty()).clicked() {
                        save = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
            if save {
                ops::save_group(f.state, f.cx, &group);
            } else if !cancel && !close {
                f.state.dialog = Dialog::Group { group, is_new };
            }
        }
        Dialog::ConfirmClose { tab_index } => {
            let title = f.state.tabs.get(tab_index).map(|t| t.title.clone()).unwrap_or_default();
            let mut choice = 0;
            let (_, close) = modal(ctx, theme, "confirm-close", 380.0, |ui| {
                ui.heading("Unsaved changes");
                ui.label(format!("\"{title}\" has unsaved changes. Save before closing?"));
                ui.add_space(4.0);
                ui.label(RichText::new("Closed tabs can be restored with Ctrl+Shift+T for a while.").size(11.0).color(theme.text_faint));
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if primary_button(ui, theme, "Save", true).clicked() {
                        choice = 1;
                    }
                    if ui.button("Don't save").clicked() {
                        choice = 2;
                    }
                    if ui.button("Cancel").clicked() {
                        choice = 3;
                    }
                });
            });
            match choice {
                1 => {
                    ops::save_file(f.state, f.cx, tab_index, false);
                    if !f.state.tabs.get(tab_index).map(|t| t.is_dirty()).unwrap_or(false) {
                        ops::close_tab(f.state, f.cx, tab_index, true);
                    }
                }
                2 => ops::close_tab(f.state, f.cx, tab_index, true),
                3 => {}
                _ => {
                    if !close {
                        f.state.dialog = Dialog::ConfirmClose { tab_index };
                    }
                }
            }
        }
        Dialog::ConfirmDeleteProfile { profile } => {
            let name = f.state.library.profile(profile).map(|p| p.display_name()).unwrap_or_default();
            let (choice, close) = modal(ctx, theme, "del-profile", 380.0, |ui| {
                ui.heading("Delete connection");
                ui.label(format!("Delete \"{name}\"? Its saved password is removed from the keychain too."));
                ui.add_space(10.0);
                let mut c = 0;
                ui.horizontal(|ui| {
                    if ui.add(egui::Button::new(RichText::new("Delete").color(egui::Color32::WHITE)).fill(theme.error)).clicked() {
                        c = 1;
                    }
                    if ui.button("Cancel").clicked() {
                        c = 2;
                    }
                });
                c
            });
            match choice {
                1 => ops::delete_profile(f.state, f.cx, profile),
                2 => {}
                _ => {
                    if !close {
                        f.state.dialog = Dialog::ConfirmDeleteProfile { profile };
                    }
                }
            }
        }
        Dialog::ConfirmDeleteGroup { group } => {
            let name = f.state.library.group(group).map(|g| g.name.clone()).unwrap_or_default();
            let (choice, close) = modal(ctx, theme, "del-group", 380.0, |ui| {
                ui.heading("Delete group");
                ui.label(format!("Delete group \"{name}\"? Its connections are kept (ungrouped)."));
                ui.add_space(10.0);
                let mut c = 0;
                ui.horizontal(|ui| {
                    if ui.add(egui::Button::new(RichText::new("Delete").color(egui::Color32::WHITE)).fill(theme.error)).clicked() {
                        c = 1;
                    }
                    if ui.button("Cancel").clicked() {
                        c = 2;
                    }
                });
                c
            });
            match choice {
                1 => ops::delete_group(f.state, f.cx, group),
                2 => {}
                _ => {
                    if !close {
                        f.state.dialog = Dialog::ConfirmDeleteGroup { group };
                    }
                }
            }
        }
        Dialog::ConfirmWrite { tab_index, statement_preview, script, opts, start_line } => {
            let (choice, close) = modal(ctx, theme, "confirm-write", 460.0, |ui| {
                ui.heading(format!("{} Read-only guard", icons::SHIELD_WARNING));
                ui.label("This connection is marked read-only and the script contains a statement that may modify data:");
                ui.add_space(4.0);
                ui.label(RichText::new(&statement_preview).family(egui::FontFamily::Monospace).color(theme.warning));
                ui.add_space(10.0);
                let mut c = 0;
                ui.horizontal(|ui| {
                    if ui.add(egui::Button::new(RichText::new("Run anyway").color(egui::Color32::WHITE)).fill(theme.warning)).clicked() {
                        c = 1;
                    }
                    if ui.button("Cancel").clicked() {
                        c = 2;
                    }
                });
                c
            });
            match choice {
                1 => ops::execute(f.state, f.cx, tab_index, script, opts, start_line),
                2 => {}
                _ => {
                    if !close {
                        f.state.dialog = Dialog::ConfirmWrite { tab_index, statement_preview, script, opts, start_line };
                    }
                }
            }
        }
        Dialog::Export(d) => export_dialog(ctx, f, d),
        Dialog::ChangeConnection { tab_index } => {
            let mut pick: Option<ProfileId> = None;
            let mut new_conn = false;
            let mut cancel = false;
            let profiles = f.state.library.profiles.clone();
            let groups = f.state.library.groups.clone();
            let (_, close) = modal(ctx, theme, "change-conn", 460.0, |ui| {
                ui.heading("Choose a connection");
                ui.add_space(6.0);
                egui::ScrollArea::vertical().max_height(360.0).auto_shrink([false, true]).show(ui, |ui| {
                    let mut list = |ui: &mut Ui, ps: Vec<&ConnectionProfile>| {
                        for p in ps {
                            let color = p.color.or_else(|| p.group.and_then(|g| groups.iter().find(|x| x.id == g)).map(|g| g.color));
                            let r = ui.add(egui::Button::new(format!("{}  {}   {}", icons::HARD_DRIVES, p.display_name(), p.database.clone().unwrap_or_default())).frame(false).min_size(Vec2::new(ui.available_width(), 26.0)));
                            if let Some(c) = color {
                                ui.painter().rect_filled(egui::Rect::from_min_size(r.rect.min, Vec2::new(3.0, r.rect.height())), 2.0, Theme::color32(c));
                            }
                            if r.clicked() {
                                pick = Some(p.id);
                            }
                        }
                    };
                    let ungrouped: Vec<&ConnectionProfile> = profiles.iter().filter(|p| p.group.is_none()).collect();
                    list(ui, ungrouped);
                    for g in &groups {
                        ui.label(RichText::new(&g.name).size(11.0).color(Theme::color32(g.color)).strong());
                        list(ui, profiles.iter().filter(|p| p.group == Some(g.id)).collect());
                    }
                    if profiles.is_empty() {
                        ui.label(RichText::new("No saved connections yet.").color(theme.text_faint));
                    }
                });
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button(format!("{} New connection…", icons::PLUS)).clicked() {
                        new_conn = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
            if let Some(id) = pick {
                let tab = f.state.tabs[tab_index].id;
                if let Some(p) = f.state.library.profile(id).cloned() {
                    ops::begin_connect(f.state, f.cx, p, ConnectPurpose::Tab { tab, database: None });
                }
            } else if new_conn {
                let tab = f.state.tabs[tab_index].id;
                ops::open_connection_dialog(f.state, f.cx, None, None, Some(ConnectPurpose::Tab { tab, database: None }));
            } else if cancel || close {
                if let Some(t) = f.state.tabs.get_mut(tab_index) {
                    t.pending_run = None;
                }
            } else {
                f.state.dialog = Dialog::ChangeConnection { tab_index };
            }
        }
        Dialog::ExecOptions { tab_index, mut opts } => {
            let mut apply = false;
            let mut cancel = false;
            let (_, close) = modal(ctx, theme, "exec-opts", 420.0, |ui| {
                ui.heading("Execution options for this tab");
                ui.add_space(6.0);
                egui::Grid::new("exec-grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                    ui.label("Row cap (0 = unlimited)");
                    ui.add(egui::DragValue::new(&mut opts.row_cap).speed(1000));
                    ui.end_row();
                    ui.label("Timeout seconds (0 = none)");
                    ui.add(egui::DragValue::new(&mut opts.timeout_secs));
                    ui.end_row();
                    ui.label("Isolation level");
                    let labels = ["(session default)", "READ UNCOMMITTED", "READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE", "SNAPSHOT"];
                    let mut idx = match opts.isolation {
                        None => 0,
                        Some(IsolationLevel::ReadUncommitted) => 1,
                        Some(IsolationLevel::ReadCommitted) => 2,
                        Some(IsolationLevel::RepeatableRead) => 3,
                        Some(IsolationLevel::Serializable) => 4,
                        Some(IsolationLevel::Snapshot) => 5,
                    };
                    egui::ComboBox::from_id_salt("iso").selected_text(labels[idx]).show_ui(ui, |ui| {
                        for (i, l) in labels.iter().enumerate() {
                            ui.selectable_value(&mut idx, i, *l);
                        }
                    });
                    opts.isolation = match idx {
                        1 => Some(IsolationLevel::ReadUncommitted),
                        2 => Some(IsolationLevel::ReadCommitted),
                        3 => Some(IsolationLevel::RepeatableRead),
                        4 => Some(IsolationLevel::Serializable),
                        5 => Some(IsolationLevel::Snapshot),
                        _ => None,
                    };
                    ui.end_row();
                });
                ui.checkbox(&mut opts.nocount, "SET NOCOUNT ON");
                ui.checkbox(&mut opts.arithabort, "SET ARITHABORT ON");
                ui.checkbox(&mut opts.xact_abort, "SET XACT_ABORT ON");
                ui.checkbox(&mut opts.statistics_io, "SET STATISTICS IO ON");
                ui.checkbox(&mut opts.statistics_time, "SET STATISTICS TIME ON");
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if primary_button(ui, theme, "Apply", true).clicked() {
                        apply = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
            if apply {
                if let Some(t) = f.state.tabs.get_mut(tab_index) {
                    t.exec = opts;
                }
            } else if !cancel && !close {
                f.state.dialog = Dialog::ExecOptions { tab_index, opts };
            }
        }
        Dialog::Rename { tab_index, mut title } => {
            let mut ok = false;
            let mut cancel = false;
            let (_, close) = modal(ctx, theme, "rename", 360.0, |ui| {
                ui.heading("Rename tab");
                let r = ui.add(egui::TextEdit::singleline(&mut title).desired_width(f32::INFINITY));
                r.request_focus();
                if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    ok = true;
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if primary_button(ui, theme, "OK", !title.trim().is_empty()).clicked() {
                        ok = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
            if ok {
                if let Some(t) = f.state.tabs.get_mut(tab_index) {
                    t.title = title.trim().to_string();
                    t.custom_title = true;
                }
            } else if !cancel && !close {
                f.state.dialog = Dialog::Rename { tab_index, title };
            }
        }
        Dialog::Error { title, message } => {
            let (ok, close) = modal(ctx, theme, "error", 460.0, |ui| {
                ui.heading(RichText::new(&title).color(theme.error));
                ui.label(&message);
                ui.add_space(8.0);
                ui.button("OK").clicked()
            });
            if !ok && !close {
                f.state.dialog = Dialog::Error { title, message };
            }
        }
        Dialog::UpdateAvailable { version, url, notes } => {
            let mut done = false;
            let mut skip = false;
            let (_, close) = modal(ctx, theme, "update", 520.0, |ui| {
                ui.heading(format!("Cobalt SQL Works {version} is available"));
                ui.label(RichText::new(format!("You have {}.", crate::update::CURRENT_VERSION)).color(theme.text_muted));
                if !notes.trim().is_empty() {
                    ui.add_space(6.0);
                    egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                        let shown: String = notes.chars().take(4000).collect();
                        ui.label(RichText::new(shown).size(12.0));
                    });
                }
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.add(egui::Button::new(RichText::new("Open download page").color(egui::Color32::WHITE)).fill(theme.accent)).clicked() {
                        cobalt_auth::entra::open_in_browser(&url);
                        done = true;
                    }
                    if ui.button("Skip this version").clicked() {
                        skip = true;
                        done = true;
                    }
                    if ui.button("Later").clicked() {
                        done = true;
                    }
                });
                false
            });
            if skip {
                f.state.skip_version_request = Some(version.clone());
            }
            if !done && !close {
                f.state.dialog = Dialog::UpdateAvailable { version, url, notes };
            }
        }
        Dialog::AdsImport { mut path, summary, error } => {
            let mut import = false;
            let mut done = false;
            let (_, close) = modal(ctx, theme, "ads", 520.0, |ui| {
                ui.heading("Import Azure Data Studio connections");
                ui.label(RichText::new("Reads server groups and connections from ADS's settings.json. Passwords stay in the OS credential store under ADS's name and aren't migrated; you'll be asked once per connection.").size(12.0).color(theme.text_muted));
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut path).desired_width(380.0));
                    if ui.button("Browse…").clicked() {
                        if let Some(p) = rfd::FileDialog::new().add_filter("settings.json", &["json"]).pick_file() {
                            path = p.to_string_lossy().to_string();
                        }
                    }
                });
                if let Some(s) = &summary {
                    ui.colored_label(theme.success, s);
                }
                if let Some(e) = &error {
                    ui.colored_label(theme.error, e);
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if primary_button(ui, theme, "Import", !path.trim().is_empty() && summary.is_none()).clicked() {
                        import = true;
                    }
                    if ui.button(if summary.is_some() { "Close" } else { "Cancel" }).clicked() {
                        done = true;
                    }
                });
            });
            if import {
                match ops::import_ads(f.state, f.cx, path.trim()) {
                    Ok(s) => f.state.dialog = Dialog::AdsImport { path, summary: Some(s), error: None },
                    Err(e) => f.state.dialog = Dialog::AdsImport { path, summary: None, error: Some(e) },
                }
            } else if !done && !close {
                f.state.dialog = Dialog::AdsImport { path, summary, error };
            }
        }
    }

    // non-modal windows
    if f.state.about_open {
        let mut open = true;
        egui::Window::new("About Cobalt SQL Works").open(&mut open).resizable(false).collapsible(false).show(ctx, |ui| {
            ui.label(RichText::new(format!("{}  Cobalt SQL Works", icons::DATABASE)).size(20.0).color(theme.accent));
            ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
            ui.label("A fast, focused SQL client for SQL Server, Azure SQL and Microsoft Fabric.");
            ui.label(RichText::new("MIT OR Apache-2.0 · Rust + egui").size(11.0).color(theme.text_muted));
            ui.hyperlink("https://github.com/methodify/cobalt-sqlworks");
        });
        f.state.about_open = open;
    }
    if f.state.shortcuts_open {
        let mut open = true;
        egui::Window::new("Keyboard shortcuts").open(&mut open).default_size([520.0, 520.0]).show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                let mut cat = None;
                for c in crate::commands::COMMANDS {
                    if cat != Some(c.category) {
                        cat = Some(c.category);
                        ui.add_space(6.0);
                        ui.label(RichText::new(c.category.label()).strong().color(theme.accent));
                    }
                    ui.horizontal(|ui| {
                        ui.label(c.label);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            key_chip(ui, theme, &f.keymap.shortcut_text(ui.ctx(), c.cmd));
                        });
                    });
                }
            });
        });
        f.state.shortcuts_open = open;
    }
}

fn connection_dialog(ctx: &egui::Context, f: &mut Frame<'_>, mut d: Box<ConnectionDialog>) {
    let theme = f.theme;
    let groups = f.state.library.groups.clone();
    let mut action = 0; // 1 save, 2 save+connect, 3 cancel, 4 test
    let mut load_recent: Option<ConnectionProfile> = None;
    let (_, close) = modal(ctx, theme, "conn", 720.0, |ui| {
        ui.heading(if d.is_new { "New connection" } else { "Edit connection" });
        ui.add_space(6.0);
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(440.0);
                egui::Grid::new("conn-grid").num_columns(2).spacing([10.0, 7.0]).min_col_width(120.0).show(ui, |ui| {
                    ui.label("Server");
                    let r = ui.add(egui::TextEdit::singleline(&mut d.profile.server).hint_text("host, host,port, host\\instance, or Fabric endpoint").desired_width(300.0));
                    if d.is_new && d.profile.server.is_empty() {
                        r.request_focus();
                    }
                    ui.end_row();
                    ui.label("Port");
                    ui.add(egui::TextEdit::singleline(&mut d.port_text).hint_text("1433").desired_width(80.0));
                    ui.end_row();
                    ui.label("Authentication");
                    egui::ComboBox::from_id_salt("auth").width(300.0).selected_text(AUTH_LABELS[d.auth_index]).show_ui(ui, |ui| {
                        for (i, l) in AUTH_LABELS.iter().enumerate() {
                            ui.selectable_value(&mut d.auth_index, i, *l);
                        }
                    });
                    ui.end_row();
                    match d.auth_index {
                        0 => {
                            ui.label("User name");
                            let mut user = d.profile.auth.user_name().unwrap_or("").to_string();
                            if ui.add(egui::TextEdit::singleline(&mut user).desired_width(300.0)).changed() {
                                let pw = match &d.profile.auth {
                                    AuthMethod::SqlLogin { password, .. } => password.clone(),
                                    _ => None,
                                };
                                d.profile.auth = AuthMethod::SqlLogin { user, password: pw };
                            }
                            ui.end_row();
                            ui.label("Password");
                            ui.horizontal(|ui| {
                                ui.add(egui::TextEdit::singleline(&mut d.password).password(true).desired_width(200.0).hint_text(if matches!(d.profile.auth, AuthMethod::SqlLogin { password: Some(_), .. }) { "(saved)" } else { "" }));
                                ui.checkbox(&mut d.remember_password, "Remember");
                            });
                            ui.end_row();
                        }
                        1 | 2 | 3 => {
                            ui.label("Tenant");
                            ui.add(egui::TextEdit::singleline(&mut d.tenant).hint_text("optional: tenant ID or domain").desired_width(300.0));
                            ui.end_row();
                            if d.auth_index == 1 {
                                ui.label("Account hint");
                                ui.add(egui::TextEdit::singleline(&mut d.account_hint).hint_text("optional: you@company.com").desired_width(300.0));
                                ui.end_row();
                            }
                        }
                        5 => {
                            ui.label("Tenant");
                            ui.add(egui::TextEdit::singleline(&mut d.tenant).desired_width(300.0));
                            ui.end_row();
                            ui.label("Client ID");
                            ui.add(egui::TextEdit::singleline(&mut d.sp_client_id).desired_width(300.0));
                            ui.end_row();
                            ui.label("Client secret");
                            ui.add(egui::TextEdit::singleline(&mut d.sp_secret).password(true).desired_width(300.0).hint_text(if matches!(d.profile.auth, AuthMethod::EntraServicePrincipal { secret: Some(_), .. }) { "(saved)" } else { "" }));
                            ui.end_row();
                        }
                        _ => {}
                    }
                    ui.label("Database");
                    let mut db = d.profile.database.clone().unwrap_or_default();
                    if ui.add(egui::TextEdit::singleline(&mut db).hint_text("<default>").desired_width(300.0)).changed() {
                        d.profile.database = if db.trim().is_empty() { None } else { Some(db) };
                    }
                    ui.end_row();
                    ui.label("Name");
                    let mut name = d.profile.name.clone().unwrap_or_default();
                    if ui.add(egui::TextEdit::singleline(&mut name).hint_text("optional friendly name").desired_width(300.0)).changed() {
                        d.profile.name = if name.trim().is_empty() { None } else { Some(name) };
                    }
                    ui.end_row();
                    ui.label("Server group");
                    let gname = if d.group_index == 0 { "(none)".to_string() } else { groups.get(d.group_index - 1).map(|g| g.name.clone()).unwrap_or_default() };
                    egui::ComboBox::from_id_salt("grp").width(300.0).selected_text(gname).show_ui(ui, |ui| {
                        ui.selectable_value(&mut d.group_index, 0, "(none)");
                        for (i, g) in groups.iter().enumerate() {
                            ui.selectable_value(&mut d.group_index, i + 1, &g.name);
                        }
                    });
                    ui.end_row();
                    ui.label("Color");
                    ui.horizontal(|ui| {
                        let (rect, resp) = ui.allocate_exact_size(Vec2::new(24.0, 24.0), egui::Sense::click());
                        ui.painter().rect_stroke(rect.shrink(2.0), 5.0, egui::Stroke::new(1.0, theme.border_strong), egui::StrokeKind::Inside);
                        ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, "–", egui::FontId::proportional(12.0), theme.text_muted);
                        if resp.on_hover_text("Use the group's color").clicked() {
                            d.color_index = None;
                        }
                        for (i, c) in Color::PALETTE.iter().enumerate() {
                            let (rect, resp) = ui.allocate_exact_size(Vec2::new(24.0, 24.0), egui::Sense::click());
                            ui.painter().rect_filled(rect.shrink(2.0), 5.0, Theme::color32(*c));
                            if d.color_index == Some(i) {
                                ui.painter().rect_stroke(rect, 6.0, egui::Stroke::new(2.0, theme.text), egui::StrokeKind::Inside);
                            }
                            if resp.clicked() {
                                d.color_index = Some(i);
                            }
                        }
                    });
                    ui.end_row();
                    ui.label("");
                    ui.checkbox(&mut d.profile.read_only_guard, "Read-only guard (confirm before non-read statements)");
                    ui.end_row();
                });
                ui.add_space(4.0);
                // `open(Some(..))` pins the header to our flag every frame, so the click must
                // toggle the flag or the header snaps shut again.
                let adv = egui::CollapsingHeader::new("Advanced").open(Some(d.show_advanced)).show(ui, |ui| {
                    let o = &mut d.profile.options;
                    egui::Grid::new("adv-grid").num_columns(2).spacing([10.0, 6.0]).min_col_width(120.0).show(ui, |ui| {
                        ui.label("Encrypt");
                        egui::ComboBox::from_id_salt("enc").selected_text(format!("{:?}", o.encrypt)).show_ui(ui, |ui| {
                            ui.selectable_value(&mut o.encrypt, Encrypt::Strict, "Strict (TDS 8)");
                            ui.selectable_value(&mut o.encrypt, Encrypt::Mandatory, "Mandatory");
                            ui.selectable_value(&mut o.encrypt, Encrypt::Optional, "Optional");
                        });
                        ui.end_row();
                        ui.label("");
                        ui.checkbox(&mut o.trust_server_certificate, "Trust server certificate");
                        ui.end_row();
                        ui.label("Host name in certificate");
                        let mut h = o.host_name_in_certificate.clone().unwrap_or_default();
                        if ui.add(egui::TextEdit::singleline(&mut h).desired_width(250.0)).changed() {
                            o.host_name_in_certificate = if h.trim().is_empty() { None } else { Some(h) };
                        }
                        ui.end_row();
                        ui.label("Application name");
                        ui.add(egui::TextEdit::singleline(&mut o.application_name).desired_width(250.0));
                        ui.end_row();
                        ui.label("Connect timeout (s)");
                        ui.add(egui::DragValue::new(&mut o.connect_timeout_secs).range(1..=600));
                        ui.end_row();
                        ui.label("Command timeout (s, 0 = none)");
                        ui.add(egui::DragValue::new(&mut o.command_timeout_secs).range(0..=86400));
                        ui.end_row();
                        ui.label("Application intent");
                        egui::ComboBox::from_id_salt("intent").selected_text(format!("{:?}", o.application_intent)).show_ui(ui, |ui| {
                            ui.selectable_value(&mut o.application_intent, ApplicationIntent::ReadWrite, "ReadWrite");
                            ui.selectable_value(&mut o.application_intent, ApplicationIntent::ReadOnly, "ReadOnly");
                        });
                        ui.end_row();
                        ui.label("");
                        ui.checkbox(&mut o.multi_subnet_failover, "Multi-subnet failover");
                        ui.end_row();
                    });
                });
                if adv.header_response.clicked() {
                    d.show_advanced = !d.show_advanced;
                }
            });
            ui.add_space(16.0);
            ui.vertical(|ui| {
                ui.label(RichText::new("Recent").strong().color(theme.text_muted));
                for p in d.recent.clone() {
                    if ui.add(egui::Button::new(format!("{}\n{}", p.display_name(), p.server)).frame(false).min_size(Vec2::new(200.0, 34.0))).clicked() {
                        load_recent = Some(p);
                    }
                }
                if d.recent.is_empty() {
                    ui.label(RichText::new("—").color(theme.text_faint));
                }
            });
        });
        if let Some((e, hint)) = &d.error {
            ui.colored_label(theme.error, e);
            if let Some(h) = hint {
                ui.label(RichText::new(h).color(theme.text_muted));
            }
        }
        match &d.test_result {
            Some(Ok(m)) => {
                ui.colored_label(theme.success, format!("{} {m}", icons::CHECK_CIRCLE));
            }
            Some(Err(e)) => {
                ui.colored_label(theme.error, format!("{} {e}", icons::X_CIRCLE));
            }
            None => {}
        }
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if primary_button(ui, theme, "Save & Connect", !d.testing).clicked() {
                action = 2;
            }
            if ui.button("Save").clicked() {
                action = 1;
            }
            if ui.add_enabled(!d.testing, egui::Button::new("Test connection")).clicked() {
                action = 4;
            }
            if d.testing {
                ui.add(egui::Spinner::new().size(14.0));
            }
            if ui.button("Cancel").clicked() {
                action = 3;
            }
        });
    });
    if let Some(p) = load_recent {
        let after = d.connect_after_save.clone();
        f.state.dialog = Dialog::None;
        ops::open_connection_dialog(f.state, f.cx, Some(p.id), None, after);
        return;
    }
    match action {
        1 | 2 => {
            let after = d.connect_after_save.clone();
            f.state.dialog = Dialog::Connection(d);
            if action == 2 {
                if let Dialog::Connection(dd) = &mut f.state.dialog {
                    if dd.connect_after_save.is_none() {
                        // connect into a new tab
                        let idx = f.state.new_tab();
                        let tab = f.state.tabs[idx].id;
                        if let Dialog::Connection(dd) = &mut f.state.dialog {
                            dd.connect_after_save = Some(ConnectPurpose::Tab { tab, database: None });
                        }
                    }
                }
                let _ = after;
            } else if let Dialog::Connection(dd) = &mut f.state.dialog {
                dd.connect_after_save = None;
            }
            ops::save_connection_dialog(f.state, f.cx);
        }
        3 => {
            if let Some(ConnectPurpose::Tab { tab, .. }) = &d.connect_after_save {
                if let Some(t) = f.state.tab_mut(*tab) {
                    t.conn = ConnState::Disconnected;
                }
            }
        }
        4 => {
            f.state.dialog = Dialog::Connection(d);
            ops::test_connection(f.state, f.cx);
        }
        _ => {
            if !close {
                f.state.dialog = Dialog::Connection(d);
            }
        }
    }
}

fn export_dialog(ctx: &egui::Context, f: &mut Frame<'_>, mut d: Box<ExportDialog>) {
    let theme = f.theme;
    let mut start = false;
    let mut done = false;
    let running = d.running;
    let (_, close) = modal(ctx, theme, "export", 560.0, |ui| {
        ui.heading("Save results as");
        ui.add_space(6.0);
        egui::Grid::new("export-grid").num_columns(2).spacing([10.0, 8.0]).min_col_width(110.0).show(ui, |ui| {
            ui.label("Format");
            let mut fi = d.format_index;
            egui::ComboBox::from_id_salt("fmt").width(260.0).selected_text(FORMAT_LABELS[fi].0).show_ui(ui, |ui| {
                for (i, (l, _)) in FORMAT_LABELS.iter().enumerate() {
                    ui.selectable_value(&mut fi, i, *l);
                }
            });
            if fi != d.format_index {
                d.format_index = fi;
                let ext = FORMAT_LABELS[fi].1;
                let p = std::path::Path::new(&d.path);
                d.path = if ext == "delta" { p.with_extension("").to_string_lossy().to_string() } else { p.with_extension(ext).to_string_lossy().to_string() };
            }
            ui.end_row();
            let is_delta = FORMAT_LABELS[d.format_index].1 == "delta";
            ui.label("Destination");
            ui.horizontal(|ui| {
                ui.selectable_value(&mut d.destination, 0, "Local file");
                ui.selectable_value(&mut d.destination, 1, format!("{} OneLake lakehouse", icons::CLOUD));
            });
            ui.end_row();
            if d.destination == 1 {
                let lakehouses: Vec<(String, String, String)> = f.state.fabric.items.iter().flat_map(|(ws, l)| l.get().into_iter().flatten().filter(|i| matches!(i.kind, cobalt_fabric::SqlItemKind::Lakehouse)).map(move |i| (i.id.clone(), i.display_name.clone(), ws.clone()))).collect();
                ui.label("Lakehouse");
                if lakehouses.is_empty() {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(match f.state.fabric.status() {
                            crate::fabric::FabricStatus::Ready => "Loading lakehouses…",
                            _ => "Sign in to Fabric to list lakehouses.",
                        }).color(theme.text_muted));
                        if ui.small_button("Open Fabric panel").clicked() {
                            f.state.sidebar_visible = true;
                            f.state.sidebar_view = SidebarView::Fabric;
                        }
                    });
                    crate::fabric::ensure_lakehouses_loaded(f.state, f.cx);
                } else {
                    let selected = d.onelake_item.as_ref().and_then(|id| lakehouses.iter().find(|(i, _, _)| i == id)).map(|(_, n, ws)| format!("{n}  ({})", f.state.fabric.workspace(ws).map(|w| w.display_name.clone()).unwrap_or_default())).unwrap_or_else(|| "choose…".into());
                    egui::ComboBox::from_id_salt("onelake-lh").width(300.0).selected_text(selected).show_ui(ui, |ui| {
                        for (id, name, ws) in &lakehouses {
                            let wsn = f.state.fabric.workspace(ws).map(|w| w.display_name.clone()).unwrap_or_default();
                            if ui.selectable_label(d.onelake_item.as_deref() == Some(id), format!("{name}  ({wsn})")).clicked() {
                                d.onelake_item = Some(id.clone());
                            }
                        }
                    });
                }
                ui.end_row();
                ui.label(if is_delta { "Table name" } else { "File name" });
                ui.add(egui::TextEdit::singleline(&mut d.onelake_name).hint_text(if is_delta { "e.g. sales_export" } else { "e.g. sales_export.parquet" }).desired_width(300.0));
                ui.end_row();
            }
            ui.label(if is_delta { "Table folder" } else { "File" });
            ui.add_enabled_ui(d.destination == 0, |ui| ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut d.path).desired_width(300.0));
                if ui.button("Browse…").clicked() {
                    let ext = FORMAT_LABELS[d.format_index].1;
                    let picked = if is_delta {
                        rfd::FileDialog::new().pick_folder().map(|p| p.join("new_table"))
                    } else {
                        let name = std::path::Path::new(&d.path).file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                        rfd::FileDialog::new().add_filter(FORMAT_LABELS[d.format_index].0, &[ext]).set_file_name(name).save_file()
                    };
                    if let Some(p) = picked {
                        d.path = p.to_string_lossy().to_string();
                    }
                }
            }));
            ui.end_row();
            ui.label("");
            ui.checkbox(&mut d.selection_only, "Selected cells only");
            ui.end_row();
            match FORMAT_LABELS[d.format_index].1 {
                "csv" | "tsv" => {
                    ui.label("Delimiter");
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut d.csv_delimiter).desired_width(40.0));
                        ui.checkbox(&mut d.csv_headers, "Header row");
                    });
                    ui.end_row();
                }
                "json" => {
                    ui.label("");
                    ui.checkbox(&mut d.json_lines, "One object per line (JSON Lines)");
                    ui.end_row();
                }
                "delta" => {
                    ui.label("Mode");
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut d.delta_mode, 0, "Create new");
                        ui.selectable_value(&mut d.delta_mode, 1, "Overwrite");
                        ui.selectable_value(&mut d.delta_mode, 2, "Append");
                    });
                    ui.end_row();
                    ui.label("Partition by");
                    ui.add(egui::TextEdit::singleline(&mut d.delta_partition).hint_text("optional: col1, col2").desired_width(300.0));
                    ui.end_row();
                }
                _ => {}
            }
        });
        if let Some(p) = &f.state.export_progress {
            let (done_rows, total) = *p.lock();
            ui.add(egui::ProgressBar::new(if total == 0 { 0.0 } else { done_rows as f32 / total as f32 }).text(format!("{} / {}", fmt_count(done_rows as u64), fmt_count(total as u64))));
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
        }
        match &d.result {
            Some(Ok(m)) => {
                ui.colored_label(theme.success, m);
            }
            Some(Err(e)) => {
                ui.colored_label(theme.error, e);
            }
            None => {}
        }
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if primary_button(ui, theme, "Export", !running).clicked() {
                start = true;
            }
            if running {
                if ui.button("Cancel export").clicked() {
                    d.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            } else if ui.button(if matches!(d.result, Some(Ok(_))) { "Close" } else { "Cancel" }).clicked() {
                done = true;
            }
            if let (Some(Ok(_)), 0) = (&d.result, d.destination) {
                if ui.button("Open folder").clicked() {
                    if let Some(parent) = std::path::Path::new(&d.path).parent() {
                        let _ = open::that(parent);
                    }
                }
            }
        });
    });
    if start {
        f.state.dialog = Dialog::Export(d);
        ops::start_export(f.state, f.cx);
    } else if done || (close && !running) {
        // closed
    } else {
        f.state.dialog = Dialog::Export(d);
    }
}
