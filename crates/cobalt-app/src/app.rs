//! `CobaltApp`: owns services (store, session, auth), settings, theme, and the UI state;
//! runs the per-frame loop: drain events → apply → shortcuts → shell → toasts → housekeeping.

use crate::commands::Keymap;
use crate::ops::{self, AuthDone, Ctx, ExportDone};
use crate::session::SessionManager;
use crate::state::{AppState, SettingsPatch, ToastKind};
use crate::ui::shell::{self, Frame};
use crate::ui::theme::Theme;
use cobalt_auth::{CredentialResolver, SecretStore};
use cobalt_core::{Settings, ThemeChoice};
use cobalt_results::{CellFormatter, MemoryBudget};
use cobalt_store::{AppPaths, Store};
use crossbeam_channel::{Receiver, Sender};
use std::cell::RefCell;
use std::sync::Arc;
use std::time::{Duration, Instant};


/// Build a `Ctx` borrowing only the service fields (never `state`), so callers can hold
/// `&mut self.state` at the same time.
macro_rules! make_ctx {
    ($app:expr, $egui:expr, $toasts:expr) => {
        Ctx {
            session: &$app.session,
            store: &$app.store,
            resolver: &$app.resolver,
            secrets: &$app.secrets,
            settings: &$app.settings,
            paths: &$app.paths,
            auth_tx: &$app.auth_tx,
            export_tx: &$app.export_tx,
            egui: $egui,
            toasts: $toasts,
        }
    };
}

pub struct CobaltApp {
    pub state: AppState,
    pub session: SessionManager,
    pub store: Arc<Store>,
    pub resolver: Arc<CredentialResolver>,
    pub secrets: Arc<dyn SecretStore>,
    pub settings: Settings,
    pub paths: AppPaths,
    pub theme: Theme,
    pub keymap: Keymap,
    pub toasts: egui_notify::Toasts,
    pub budget: Arc<MemoryBudget>,
    auth_tx: Sender<AuthDone>,
    auth_rx: Receiver<AuthDone>,
    export_tx: Sender<ExportDone>,
    export_rx: Receiver<ExportDone>,
    update_tx: crossbeam_channel::Sender<crate::update::UpdateOutcome>,
    update_rx: Receiver<crate::update::UpdateOutcome>,
    update_started: bool,
    last_maintenance: Instant,
    applied_scale: f32,
    applied_theme: Option<bool>,
    frames: u64,
}

impl CobaltApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let paths = AppPaths::new().expect("app directories");
        let _ = paths.ensure_dirs();
        let _ = paths.clean_spill_dir();
        let settings = cobalt_store::load_settings(&paths.settings_file());
        let store = Arc::new(Store::open(&paths.db_file()).unwrap_or_else(|e| {
            tracing::error!(error = %e, "could not open the store; using in-memory");
            Store::open_in_memory().expect("in-memory store")
        }));
        let secrets: Arc<dyn SecretStore> = cobalt_auth::default_secret_store().unwrap_or_else(|e| {
            tracing::warn!(error = %e, "no secret store; passwords will not be remembered");
            Arc::new(cobalt_auth::MemoryStore::new())
        });
        let resolver = Arc::new(CredentialResolver::new(secrets.clone(), cobalt_auth::entra::EntraConfig::from_settings(&settings.connections)));
        let budget = Arc::new(MemoryBudget::new(settings.advanced.memory_budget_bytes as usize));
        let egui_ctx = cc.egui_ctx.clone();
        let repaint: Arc<dyn Fn() + Send + Sync> = Arc::new(move || egui_ctx.request_repaint());
        let driver: Arc<dyn cobalt_driver::Driver> = Arc::new(cobalt_driver::mssql::MssqlDriver::new());
        let session = SessionManager::start(driver, budget.clone(), paths.spill_dir(), repaint);

        // fonts
        let mut fonts = egui::FontDefinitions::default();
        egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
        cc.egui_ctx.set_fonts(fonts);

        let system_dark = cc.egui_ctx.system_theme().map(|t| t == egui::Theme::Dark).unwrap_or(true);
        let theme = Theme::for_choice(settings.appearance.theme, system_dark);
        cc.egui_ctx.set_visuals(theme.visuals());
        cc.egui_ctx.set_zoom_factor(settings.appearance.ui_scale);
        let mut style = (*cc.egui_ctx.global_style()).clone();
        style.spacing.item_spacing = egui::vec2(6.0, 4.0);
        style.spacing.button_padding = egui::vec2(8.0, 3.0);
        style.text_styles.insert(egui::TextStyle::Body, egui::FontId::proportional(settings.appearance.ui_font_size));
        style.text_styles.insert(egui::TextStyle::Button, egui::FontId::proportional(settings.appearance.ui_font_size));
        style.text_styles.insert(egui::TextStyle::Monospace, egui::FontId::monospace(settings.appearance.ui_font_size));
        cc.egui_ctx.set_global_style(style);

        let (auth_tx, auth_rx) = crossbeam_channel::unbounded();
        let (export_tx, export_rx) = crossbeam_channel::unbounded();
        let (update_tx, update_rx) = crossbeam_channel::unbounded();
        let mut state = AppState::new();
        state.formatter = CellFormatter::from_settings(&settings.results);
        let toasts = egui_notify::Toasts::default().with_anchor(egui_notify::Anchor::BottomRight).with_margin(egui::vec2(12.0, 32.0));
        let mut app = Self {
            state,
            session,
            store,
            resolver,
            secrets,
            settings,
            paths,
            theme,
            keymap: Keymap::default(),
            toasts,
            budget,
            auth_tx,
            auth_rx,
            export_tx,
            export_rx,
            update_tx,
            update_rx,
            update_started: false,
            last_maintenance: Instant::now(),
            applied_scale: 1.0,
            applied_theme: None,
            frames: 0,
        };
        app.applied_scale = app.settings.appearance.ui_scale;
        app.applied_theme = Some(app.theme.is_dark());
        {
            let toasts = RefCell::new(Vec::new());
            let cx = make_ctx!(app, &cc.egui_ctx, &toasts);
            ops::load_library(&mut app.state, &cx);
            ops::restore_tabs(&mut app.state, &cx);
            ops::maintenance(&mut app.state, &cx);
            drop(cx);
            app.flush_toasts(toasts.into_inner());
        }
        if app.state.tabs.is_empty() {
            app.state.new_tab();
        }
        app
    }

    fn flush_toasts(&mut self, list: Vec<(ToastKind, String)>) {
        for (kind, msg) in list {
            let t = match kind {
                ToastKind::Info => self.toasts.info(msg),
                ToastKind::Success => self.toasts.success(msg),
                ToastKind::Warning => self.toasts.warning(msg),
                ToastKind::Error => self.toasts.error(msg),
            };
            t.duration(Some(Duration::from_secs(if kind == ToastKind::Error { 9 } else { 4 }))).closable(true);
        }
    }

    pub fn apply_settings(&mut self, ctx: &egui::Context, new: Settings) {
        let theme_changed = new.appearance.theme != self.settings.appearance.theme;
        self.settings = new;
        let _ = cobalt_store::save_settings(&self.paths.settings_file(), &self.settings);
        self.state.formatter = CellFormatter::from_settings(&self.settings.results);
        self.budget.set_limit(self.settings.advanced.memory_budget_bytes as usize);
        self.resolver.set_config(cobalt_auth::entra::EntraConfig::from_settings(&self.settings.connections));
        if theme_changed || self.applied_theme.is_none() {
            let system_dark = ctx.system_theme().map(|t| t == egui::Theme::Dark).unwrap_or(true);
            self.theme = Theme::for_choice(self.settings.appearance.theme, system_dark);
            ctx.set_visuals(self.theme.visuals());
            self.applied_theme = Some(self.theme.is_dark());
        }
        if (self.settings.appearance.ui_scale - self.applied_scale).abs() > 0.001 {
            ctx.set_zoom_factor(self.settings.appearance.ui_scale);
            self.applied_scale = self.settings.appearance.ui_scale;
        }
    }

    /// One frame of logic: events, auth/export completions, shortcuts, housekeeping.
    fn logic(&mut self, ctx: &egui::Context) {
        self.frames += 1;
        let toasts = RefCell::new(Vec::new());
        let mut update_outcomes: Vec<crate::update::UpdateOutcome> = Vec::new();
        let mut skip_request: Option<String> = None;
        {
            let cx = make_ctx!(self, ctx, &toasts);
            // session events
            let events = self.session.drain();
            let mut followups = Vec::new();
            for ev in events {
                followups.extend(self.state.apply_event(ev));
            }
            ops::handle_followups(&mut self.state, &cx, followups);
            // auth completions
            while let Ok(done) = self.auth_rx.try_recv() {
                if ops::on_test_result(&mut self.state, &done) {
                    continue;
                }
                ops::on_auth_done(&mut self.state, &cx, done);
            }
            while let Ok(done) = self.export_rx.try_recv() {
                ops::on_export_done(&mut self.state, &cx, done);
            }
            // update check: once shortly after start-up (if enabled), or on request from Help
            let startup_due = !self.update_started && self.frames > 120 && self.settings.updates.check_on_startup;
            if startup_due || self.state.update_check_requested {
                let manual = self.state.update_check_requested;
                self.state.update_check_requested = false;
                self.update_started = true;
                crate::update::spawn_check(&self.session, self.update_tx.clone(), manual);
            }
            while let Ok(outcome) = self.update_rx.try_recv() {
                update_outcomes.push(outcome);
            }
            skip_request = self.state.skip_version_request.take();
            // hot exit
            if self.state.last_hot_exit_save.elapsed() > Duration::from_secs(5) {
                ops::snapshot_tabs(&mut self.state, &cx, false);
            }
            if self.last_maintenance.elapsed() > Duration::from_secs(600) {
                ops::maintenance(&mut self.state, &cx);
                self.last_maintenance = Instant::now();
            }
        }
        self.flush_toasts(toasts.into_inner());
        for outcome in update_outcomes {
            use crate::update::UpdateOutcome::*;
            match outcome {
                Available { info, manual } => {
                    let skipped = self.settings.updates.skipped_version.as_deref() == Some(info.version.as_str());
                    if manual || !skipped {
                        self.state.dialog = crate::state::Dialog::UpdateAvailable { version: info.version, url: info.url, notes: info.notes };
                    }
                }
                UpToDate { manual: true } => {
                    self.toasts.success(format!("You're on the latest version ({}).", crate::update::CURRENT_VERSION)).closable(true);
                }
                Failed { error, manual: true } => {
                    self.toasts.warning(format!("Could not check for updates: {error}")).closable(true);
                }
                UpToDate { manual: false } | Failed { manual: false, .. } => {}
            }
        }
        if let Some(v) = skip_request {
            let mut s = self.settings.clone();
            s.updates.skipped_version = Some(v);
            self.apply_settings(ctx, s);
        }
        // follow the OS theme when set to System
        if self.settings.appearance.theme == ThemeChoice::System {
            let system_dark = ctx.system_theme().map(|t| t == egui::Theme::Dark).unwrap_or(true);
            if self.applied_theme != Some(system_dark) {
                self.theme = Theme::for_choice(ThemeChoice::System, system_dark);
                ctx.set_visuals(self.theme.visuals());
                self.applied_theme = Some(system_dark);
            }
        }
    }

    fn handle_settings_patches(&mut self, ctx: &egui::Context) {
        let patches = std::mem::take(&mut self.state.settings_patch);
        if patches.is_empty() {
            return;
        }
        let mut s = self.settings.clone();
        for p in patches {
            match p {
                SettingsPatch::LastExportDir(d) => s.export.last_dir = Some(d),
                SettingsPatch::Theme(t) => s.appearance.theme = t,
                SettingsPatch::UiScale(z) => s.appearance.ui_scale = z,
            }
        }
        self.apply_settings(ctx, s);
    }
}

impl eframe::App for CobaltApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.logic(&ctx);
        if !self.state.injected_events.is_empty() {
            let evs = std::mem::take(&mut self.state.injected_events);
            ctx.input_mut(|i| {
                for e in evs {
                    if let egui::Event::Key { key, pressed: true, modifiers, .. } = &e {
                        i.keys_down.insert(*key);
                        i.modifiers = *modifiers;
                    }
                    i.events.push(e);
                }
            });
            ctx.request_repaint();
        }

        // keyboard shortcuts (not while a modal dialog or the palette owns the keyboard)
        let mut cmds = Vec::new();
        if !self.state.dialog.is_open() && !self.state.palette_open {
            cmds = self.keymap.consume(&ctx);
        } else if self.state.palette_open {
            // allow toggling the palette off
            if ctx.input_mut(|i| i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::P))) {
                self.state.palette_open = false;
            }
        }
        // Ctrl+C on a focused grid
        if !self.state.dialog.is_open() && !self.state.palette_open {
            let copy_event = ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy)));
            if copy_event && ctx.memory(|m| m.focused()).is_none() {
                let grid_focused = self.state.active().and_then(|t| t.run.as_ref()).map(|r| r.result_sets.iter().any(|s| s.grid.focused)).unwrap_or(false);
                if grid_focused {
                    cmds.push(crate::commands::Command::CopySelection);
                }
            }
        }

        let toasts = RefCell::new(Vec::new());
        {
            let theme = self.theme.clone();
            let keymap = &self.keymap;
            let cx = Ctx {
                session: &self.session,
                store: &self.store,
                resolver: &self.resolver,
                secrets: &self.secrets,
                settings: &self.settings,
                paths: &self.paths,
                auth_tx: &self.auth_tx,
                export_tx: &self.export_tx,
                egui: &ctx,
                toasts: &toasts,
            };
            let mut f = Frame { state: &mut self.state, cx: &cx, theme: &theme, keymap };
            for c in cmds {
                shell::dispatch(&mut f, c);
            }
            shell::show(ui, &mut f);
        }
        self.flush_toasts(toasts.into_inner());

        // settings window
        if self.state.settings_open {
            if self.state.settings_draft.is_none() {
                self.state.settings_draft = Some(self.settings.clone());
            }
            let info = format!("Settings: {}\nData: {}\nSpill: {}", self.paths.settings_file().display(), self.paths.db_file().display(), self.paths.spill_dir().display());
            let mut draft = self.state.settings_draft.take().unwrap();
            let theme = self.theme.clone();
            match crate::ui::settings::show(&ctx, &mut draft, &theme, &info) {
                Some(crate::ui::settings::SettingsAction::Apply(s)) => {
                    self.apply_settings(&ctx, s);
                    self.state.settings_open = false;
                    self.state.settings_draft = None;
                }
                Some(crate::ui::settings::SettingsAction::Close) => {
                    self.state.settings_open = false;
                    self.state.settings_draft = None;
                }
                None => self.state.settings_draft = Some(draft),
            }
        }
        self.handle_settings_patches(&ctx);
        self.toasts.show(&ctx);

        // keep animating while anything is live
        let live = self.state.tabs.iter().any(|t| t.is_running() || matches!(t.conn, crate::state::ConnState::Connecting));
        if live {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    fn on_exit(&mut self) {
        let egui = egui::Context::default();
        let toasts = RefCell::new(Vec::new());
        let cx = make_ctx!(self, &egui, &toasts);
        ops::snapshot_tabs(&mut self.state, &cx, true);
        self.session.send(crate::session::Command::Shutdown);
    }
}

impl CobaltApp {
    pub(crate) fn auth_tx_ref(&self) -> Sender<AuthDone> {
        self.auth_tx.clone()
    }
    pub(crate) fn export_tx_ref(&self) -> Sender<ExportDone> {
        self.export_tx.clone()
    }
}
