//! Effects: everything the UI asks the app to *do* — connect (with credential resolution),
//! run, cancel, metadata loads, history, files, hot exit, copy, export, library edits.

use crate::copy::{self, CopyKind};
use crate::session::{Command, MetadataRequest, SessionManager};
use crate::state::*;
use crate::ui::editor::{byte_to_char, char_to_byte};
use crate::ui::results::ResultsAction;
use crate::ui::servers::TreeAction;
use cobalt_auth::{CredentialResolver, Prompter, SecretStore};
use cobalt_core::*;
use cobalt_driver::ScriptKind;
use cobalt_store::{AppPaths, HistoryQuery, HistoryStatus, NewHistoryEntry, Store, TabSnapshot};
use crossbeam_channel::Sender;
use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// Fabric cache key (survives cache clears: `pref:` prefix) holding the last lakehouse exported to.
const LAST_LAKEHOUSE_KEY: &str = "pref:last_lakehouse";

/// Immutable services handed to UI code alongside `&mut AppState`.
pub struct Ctx<'a> {
    pub session: &'a SessionManager,
    pub store: &'a Arc<Store>,
    pub resolver: &'a Arc<CredentialResolver>,
    pub secrets: &'a Arc<dyn SecretStore>,
    pub settings: &'a Settings,
    pub paths: &'a AppPaths,
    pub auth_tx: &'a Sender<AuthDone>,
    pub export_tx: &'a Sender<ExportDone>,
    pub fabric_tx: &'a Sender<crate::fabric::FabricEvent>,
    pub egui: &'a egui::Context,
    pub toasts: &'a RefCell<Vec<(ToastKind, String)>>,
}

impl Ctx<'_> {
    pub fn toast(&self, kind: ToastKind, msg: impl Into<String>) {
        self.toasts.borrow_mut().push((kind, msg.into()));
    }
    pub fn repaint(&self) {
        self.egui.request_repaint();
    }
}

pub struct AuthDone {
    pub purpose: ConnectPurpose,
    pub profile: ConnectionProfile,
    pub result: Result<ResolvedCredentials, String>,
}

pub struct ExportDone {
    pub result: Result<String, String>,
    /// Set for run-to-export: the tab whose run fed the export (the message lands in its Messages).
    pub tab: Option<TabId>,
    /// Local output (first result set) so the message can offer "Open folder".
    pub path: Option<PathBuf>,
}

/// What the command line asked for: `cobalt [file.sql|plan.sqlplan]… [-S server] [-d database]`.
#[derive(Debug, Default, Clone)]
pub struct LaunchArgs {
    pub files: Vec<PathBuf>,
    pub server: Option<String>,
    pub database: Option<String>,
}

impl LaunchArgs {
    /// `None` when nothing was asked for.
    pub fn parse(args: impl Iterator<Item = String>) -> Option<Self> {
        let mut out = LaunchArgs::default();
        let mut args = args.peekable();
        while let Some(a) = args.next() {
            match a.as_str() {
                "-S" | "--server" => out.server = args.next(),
                "-d" | "-D" | "--database" => out.database = args.next(),
                s if s.starts_with("-S") && s.len() > 2 => out.server = Some(s[2..].to_string()),
                s if (s.starts_with("-d") || s.starts_with("-D")) && s.len() > 2 => out.database = Some(s[2..].to_string()),
                s if s.starts_with('-') => tracing::warn!(arg = s, "unknown command-line option ignored"),
                s => out.files.push(PathBuf::from(s.strip_prefix("file://").unwrap_or(s))),
            }
        }
        if out.files.is_empty() && out.server.is_none() && out.database.is_none() { None } else { Some(out) }
    }
}

/// Open the files and/or connection the command line asked for. A `-S` that matches a saved
/// connection (name or server) opens a query tab on it; otherwise the connection editor opens
/// pre-filled so one click saves and connects.
pub fn apply_launch(state: &mut AppState, cx: &Ctx, launch: LaunchArgs) {
    let mut opened_tab: Option<usize> = None;
    if let Some(server) = &launch.server {
        let want = server.trim().to_ascii_lowercase();
        let found = state.library.profiles.iter().find(|p| p.server.eq_ignore_ascii_case(&want) || p.display_name().eq_ignore_ascii_case(&want) || p.name.as_deref().map(|n| n.eq_ignore_ascii_case(&want)).unwrap_or(false)).map(|p| p.id);
        match found {
            Some(id) => {
                let idx = new_query_tab(state, cx, Some(id), launch.database.clone(), None, false);
                opened_tab = Some(idx);
            }
            None => {
                open_connection_dialog(state, cx, None, None, None);
                if let Dialog::Connection(d) = &mut state.dialog {
                    d.profile.server = server.clone();
                    d.profile.database = launch.database.clone();
                }
            }
        }
    }
    for path in &launch.files {
        match path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref() {
            Some("sqlplan") => open_plan_file(state, cx, path.clone()),
            _ => {
                if let (Some(idx), true) = (opened_tab.take(), state.tabs.get(opened_tab.unwrap_or(usize::MAX)).map(|t| t.text.trim().is_empty()).unwrap_or(false)) {
                    // put the first file into the tab that -S just opened
                    match std::fs::read_to_string(path) {
                        Ok(text) => {
                            let t = &mut state.tabs[idx];
                            t.text = text;
                            t.file_path = Some(path.clone());
                            t.title = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "query".into());
                            t.custom_title = true;
                            t.mark_saved();
                        }
                        Err(e) => cx.toast(ToastKind::Error, format!("Could not open {}: {e}", path.display())),
                    }
                } else {
                    open_file(state, cx, Some(path.clone()));
                }
            }
        }
    }
}

/// Rows each result set keeps in the grid while a run streams to an export target.
pub const RUN_EXPORT_PREVIEW_ROWS: u64 = 1_000;

/// A run-to-export target resolved from the Run to File dialog; carried on the tab until the run
/// starts (`execute`), which is also after a reconnect or a read-only-guard confirmation.
pub struct ExportJob {
    pub ext: String,
    pub path: String,
    pub delta_mode: usize,
    pub delta_partition: Vec<String>,
    pub settings: ExportSettings,
    pub fmt: cobalt_results::CellFormatter,
    pub onelake: Option<OneLakeJob>,
}

pub struct OneLakeJob {
    pub item: cobalt_fabric::SqlItem,
    pub name: String,
    pub schema: String,
    pub slot: ProfileId,
    pub tenant: Option<String>,
    pub hint: Option<String>,
}

impl ExportJob {
    /// Human-readable target for the results header and messages.
    pub fn display_target(&self) -> String {
        match &self.onelake {
            Some(o) if self.ext == "delta" => {
                let shown = if o.schema.is_empty() { o.name.clone() } else { format!("{}.{}", o.schema, o.name) };
                format!("{}/Tables/{shown}", o.item.display_name)
            }
            Some(o) => format!("{}/Files/{}", o.item.display_name, o.name),
            None => self.path.clone(),
        }
    }

    /// Local path for result set `index`: the second set and on get a `_2`, `_3`… suffix.
    fn local_path(&self, index: usize) -> PathBuf {
        let p = PathBuf::from(&self.path);
        if index == 0 {
            return p;
        }
        let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let mut name = format!("{stem}_{}", index + 1);
        if let Some(e) = p.extension() {
            name.push('.');
            name.push_str(&e.to_string_lossy());
        }
        let out = p.with_file_name(name);
        // keep the separator style the user typed
        if self.path.contains('/') && !self.path.contains('\\') {
            PathBuf::from(out.to_string_lossy().replace('\\', "/"))
        } else {
            out
        }
    }

    fn onelake_name(&self, index: usize) -> String {
        let base = self.onelake.as_ref().map(|o| o.name.clone()).unwrap_or_default();
        if index == 0 { base } else { format!("{base}_{}", index + 1) }
    }

    fn delta_options(&self, table_name: Option<String>) -> cobalt_export_delta::DeltaOptions {
        let mode = match self.delta_mode {
            1 => cobalt_export_delta::DeltaMode::Overwrite,
            2 => cobalt_export_delta::DeltaMode::Append,
            _ => cobalt_export_delta::DeltaMode::Create,
        };
        cobalt_export_delta::DeltaOptions { mode, partition_columns: self.delta_partition.clone(), table_name, description: None }
    }
}

pub(crate) struct UiPrompter {
    pub(crate) cancel: Arc<AtomicBool>,
    pub(crate) device: Arc<parking_lot::Mutex<Option<(String, String)>>>,
    pub(crate) url: Arc<parking_lot::Mutex<Option<String>>>,
    pub(crate) egui: egui::Context,
}

impl Prompter for UiPrompter {
    fn password(&self, _profile: &ConnectionProfile) -> cobalt_auth::Result<Option<Secret>> {
        Ok(None)
    }
    fn open_browser(&self, url: &str) {
        *self.url.lock() = Some(url.to_string());
        cobalt_auth::entra::open_in_browser(url);
        self.egui.request_repaint();
    }
    fn device_code(&self, prompt: &cobalt_auth::entra::DeviceCodePrompt) {
        *self.device.lock() = Some((prompt.user_code.clone(), prompt.verification_uri.clone()));
        self.egui.request_repaint();
    }
    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

// ---------------------------------------------------------------------------------------------
// Library
// ---------------------------------------------------------------------------------------------

pub fn load_library(state: &mut AppState, cx: &Ctx) {
    match (cx.store.list_groups(), cx.store.list_profiles()) {
        (Ok(g), Ok(p)) => {
            state.library.groups = g;
            state.library.profiles = p;
            if state.library.expanded_groups.is_empty() {
                for g in &state.library.groups {
                    state.library.expanded_groups.insert(g.id);
                }
            }
        }
        (Err(e), _) | (_, Err(e)) => cx.toast(ToastKind::Error, format!("Could not load connections: {e}")),
    }
}

pub fn open_connection_dialog(state: &mut AppState, cx: &Ctx, existing: Option<ProfileId>, group: Option<GroupId>, connect_after: Option<ConnectPurpose>) {
    let (profile, is_new) = match existing.and_then(|id| state.library.profile(id).cloned()) {
        Some(p) => (p, false),
        None => {
            let mut p = ConnectionProfile::new("", AuthMethod::SqlLogin { user: String::new(), password: None });
            p.group = group;
            (p, true)
        }
    };
    open_connection_dialog_from(state, cx, profile, is_new, connect_after);
}

/// The connection editor prefilled with `profile` (new or existing).
pub fn open_connection_dialog_from(state: &mut AppState, cx: &Ctx, profile: ConnectionProfile, is_new: bool, connect_after: Option<ConnectPurpose>) {
    let (tenant, account_hint, sp_client_id) = match &profile.auth {
        AuthMethod::EntraInteractive { tenant, account_hint } => (tenant.clone().unwrap_or_default(), account_hint.clone().unwrap_or_default(), String::new()),
        AuthMethod::EntraDeviceCode { tenant } | AuthMethod::AzureCli { tenant } => (tenant.clone().unwrap_or_default(), String::new(), String::new()),
        AuthMethod::EntraServicePrincipal { tenant, client_id, .. } => (tenant.clone(), String::new(), client_id.clone()),
        _ => (String::new(), String::new(), String::new()),
    };
    let auth_index = auth_index_of(&profile.auth);
    let remember_password = matches!(&profile.auth, AuthMethod::SqlLogin { password: Some(_), .. });
    let recent = cx.store.recent_profiles(8).unwrap_or_default();
    let group_index = profile.group.and_then(|g| state.library.groups.iter().position(|x| x.id == g)).map(|i| i + 1).unwrap_or(0);
    let color_index = profile.color.and_then(|c| Color::PALETTE.iter().position(|p| *p == c));
    state.dialog = Dialog::Connection(Box::new(ConnectionDialog {
        port_text: profile.port.map(|p| p.to_string()).unwrap_or_default(),
        profile,
        is_new,
        password: String::new(),
        remember_password,
        auth_index,
        tenant,
        account_hint,
        sp_client_id,
        sp_secret: String::new(),
        show_advanced: false,
        conn_string: String::new(),
        conn_string_note: None,
        focus_done: false,
        error: None,
        testing: false,
        test_result: None,
        connect_after_save: connect_after,
        recent,
        group_index,
        color_index,
    }));
}

pub const AUTH_LABELS: &[&str] = &["SQL Login", "Microsoft Entra ID (browser sign-in)", "Microsoft Entra ID (device code)", "Azure CLI (az login)", "Windows Authentication", "Entra service principal"];

pub fn auth_index_of(a: &AuthMethod) -> usize {
    match a {
        AuthMethod::SqlLogin { .. } => 0,
        AuthMethod::EntraInteractive { .. } => 1,
        AuthMethod::EntraDeviceCode { .. } => 2,
        AuthMethod::AzureCli { .. } => 3,
        AuthMethod::WindowsIntegrated => 4,
        AuthMethod::EntraServicePrincipal { .. } => 5,
    }
}

/// Build the profile's `AuthMethod` from the dialog fields (without secrets).
pub fn auth_from_dialog(d: &ConnectionDialog) -> AuthMethod {
    let opt = |s: &str| if s.trim().is_empty() { None } else { Some(s.trim().to_string()) };
    match d.auth_index {
        0 => AuthMethod::SqlLogin { user: d.profile.auth.user_name().unwrap_or("").to_string(), password: None },
        1 => AuthMethod::EntraInteractive { tenant: opt(&d.tenant), account_hint: opt(&d.account_hint) },
        2 => AuthMethod::EntraDeviceCode { tenant: opt(&d.tenant) },
        3 => AuthMethod::AzureCli { tenant: opt(&d.tenant) },
        4 => AuthMethod::WindowsIntegrated,
        _ => AuthMethod::EntraServicePrincipal { tenant: d.tenant.trim().to_string(), client_id: d.sp_client_id.trim().to_string(), secret: None },
    }
}

/// Save the connection dialog (validates, persists secrets, upserts profile). Returns the profile.
pub fn save_connection_dialog(state: &mut AppState, cx: &Ctx) -> Option<ConnectionProfile> {
    let Dialog::Connection(d) = &mut state.dialog else { return None };
    if d.profile.server.trim().is_empty() {
        d.error = Some(("Server is required.".into(), None));
        return None;
    }
    let mut p = d.profile.clone();
    p.server = p.server.trim().to_string();
    p.port = d.port_text.trim().parse().ok();
    p.database = p.database.as_ref().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    p.name = p.name.as_ref().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    p.group = if d.group_index == 0 { None } else { state.library.groups.get(d.group_index - 1).map(|g| g.id) };
    p.color = d.color_index.and_then(|i| Color::PALETTE.get(i).copied());
    let mut auth = auth_from_dialog(d);
    if let AuthMethod::SqlLogin { user, password } = &mut auth {
        if user.trim().is_empty() {
            d.error = Some(("User name is required for SQL Login.".into(), None));
            return None;
        }
        if d.remember_password {
            let r = SecretRef::for_profile(&p.id, "password");
            if !d.password.is_empty() {
                if let Err(e) = cx.secrets.set(&r, &Secret::new(d.password.clone())) {
                    cx.toast(ToastKind::Warning, format!("Password not saved to the OS keychain: {e}"));
                } else {
                    *password = Some(r);
                }
            } else if let AuthMethod::SqlLogin { password: Some(old), .. } = &d.profile.auth {
                *password = Some(old.clone());
            }
        } else {
            let _ = cx.secrets.delete(&SecretRef::for_profile(&p.id, "password"));
        }
    }
    if let AuthMethod::EntraServicePrincipal { secret, .. } = &mut auth {
        let r = SecretRef::for_profile(&p.id, "client_secret");
        if !d.sp_secret.is_empty() {
            if cx.secrets.set(&r, &Secret::new(d.sp_secret.clone())).is_ok() {
                *secret = Some(r);
            }
        } else if let AuthMethod::EntraServicePrincipal { secret: Some(old), .. } = &d.profile.auth {
            *secret = Some(old.clone());
        }
    }
    p.auth = auth;
    if p.looks_like_fabric() && matches!(p.auth, AuthMethod::SqlLogin { .. } | AuthMethod::WindowsIntegrated) {
        d.error = Some(("Fabric endpoints require Microsoft Entra ID authentication.".into(), None));
        return None;
    }
    if let Err(e) = cx.store.upsert_profile(&p) {
        d.error = Some((format!("Could not save: {e}"), None));
        return None;
    }
    let after = d.connect_after_save.clone();
    let typed_password = d.password.clone();
    state.dialog = Dialog::None;
    load_library(state, cx);
    if let Some(purpose) = after {
        // use the typed password once even if not remembered
        if let (AuthMethod::SqlLogin { user, .. }, false) = (&p.auth, typed_password.is_empty()) {
            let creds = ResolvedCredentials::SqlLogin { user: user.clone(), password: Secret::new(typed_password) };
            finish_connect(state, cx, p.clone(), creds, purpose);
        } else {
            begin_connect(state, cx, p.clone(), purpose);
        }
    }
    Some(p)
}

pub fn delete_profile(state: &mut AppState, cx: &Ctx, id: ProfileId) {
    let _ = cx.secrets.delete(&SecretRef::for_profile(&id, "password"));
    let _ = cx.resolver.forget(&id);
    if let Err(e) = cx.store.delete_profile(id) {
        cx.toast(ToastKind::Error, format!("Delete failed: {e}"));
    }
    cx.session.send(Command::CloseMetadata { profile: id });
    state.library.servers.remove(&id);
    load_library(state, cx);
}

pub fn save_group(state: &mut AppState, cx: &Ctx, g: &ServerGroup) {
    if let Err(e) = cx.store.upsert_group(g) {
        cx.toast(ToastKind::Error, format!("Could not save group: {e}"));
    }
    state.library.expanded_groups.insert(g.id);
    load_library(state, cx);
}

pub fn delete_group(state: &mut AppState, cx: &Ctx, id: GroupId) {
    if let Err(e) = cx.store.delete_group(id, None) {
        cx.toast(ToastKind::Error, format!("Could not delete group: {e}"));
    }
    load_library(state, cx);
}

pub fn move_profile(state: &mut AppState, cx: &Ctx, id: ProfileId, group: Option<GroupId>) {
    if let Some(mut p) = state.library.profile(id).cloned() {
        p.group = group;
        let _ = cx.store.upsert_profile(&p);
        load_library(state, cx);
    }
}

// ---------------------------------------------------------------------------------------------
// Connecting
// ---------------------------------------------------------------------------------------------

/// Start resolving credentials for `profile`, then connect for `purpose`.
pub fn begin_connect(state: &mut AppState, cx: &Ctx, profile: ConnectionProfile, purpose: ConnectPurpose) {
    if let ConnectPurpose::Tab { tab, .. } = &purpose {
        if let Some(t) = state.tab_mut(*tab) {
            t.conn = ConnState::Connecting;
            t.profile = Some(profile.clone());
        }
    }
    match &profile.auth {
        AuthMethod::SqlLogin { user, password } => {
            let stored = password.as_ref().and_then(|r| cx.secrets.get(r).ok().flatten());
            match stored {
                Some(pw) => finish_connect(state, cx, profile.clone(), ResolvedCredentials::SqlLogin { user: user.clone(), password: pw }, purpose),
                None => {
                    state.dialog = Dialog::Password { profile, password: String::new(), remember: false, purpose, error: None };
                }
            }
        }
        AuthMethod::WindowsIntegrated => finish_connect(state, cx, profile, ResolvedCredentials::WindowsIntegrated, purpose),
        AuthMethod::EntraInteractive { .. } | AuthMethod::EntraDeviceCode { .. } | AuthMethod::AzureCli { .. } | AuthMethod::EntraServicePrincipal { .. } => {
            if matches!(profile.auth, AuthMethod::EntraInteractive { .. } | AuthMethod::EntraDeviceCode { .. }) && cx.settings.connections.effective_entra_client_id().is_empty() {
                let msg = "No Entra client ID is configured. Set one in Settings → Connections (or use Azure CLI authentication after `az login`).";
                cx.toast(ToastKind::Error, msg);
                if let ConnectPurpose::Tab { tab, .. } = &purpose {
                    if let Some(t) = state.tab_mut(*tab) {
                        t.conn = ConnState::Failed { error: msg.into(), hint: None };
                    }
                }
                return;
            }
            let cancel = Arc::new(AtomicBool::new(false));
            let device = Arc::new(parking_lot::Mutex::new(None));
            let url = Arc::new(parking_lot::Mutex::new(None));
            let message = match &profile.auth {
                AuthMethod::EntraInteractive { .. } => "Complete the sign-in in your browser…".to_string(),
                AuthMethod::EntraDeviceCode { .. } => "Requesting a device code…".to_string(),
                AuthMethod::AzureCli { .. } => "Asking Azure CLI for a token…".to_string(),
                _ => "Acquiring token…".to_string(),
            };
            state.dialog = Dialog::AuthWaiting { profile: profile.clone(), purpose: purpose.clone(), message, device: device.clone(), url: url.clone(), cancel: cancel.clone(), started: Instant::now() };
            let resolver = cx.resolver.clone();
            let tx = cx.auth_tx.clone();
            let egui = cx.egui.clone();
            let prompter = UiPrompter { cancel, device, url, egui: egui.clone() };
            let p2 = profile.clone();
            cx.session.spawn(async move {
                let result = resolver.resolve(&p2, &prompter).await.map_err(|e| {
                    let hint = e.hint().map(|h| format!(" — {h}")).unwrap_or_default();
                    format!("{e}{hint}")
                });
                let _ = tx.send(AuthDone { purpose, profile: p2, result });
                egui.request_repaint();
            });
        }
    }
}

pub fn on_auth_done(state: &mut AppState, cx: &Ctx, done: AuthDone) {
    if matches!(state.dialog, Dialog::AuthWaiting { .. }) {
        state.dialog = Dialog::None;
    }
    match done.result {
        Ok(creds) => finish_connect(state, cx, done.profile, creds, done.purpose),
        Err(e) => {
            if !e.contains("cancelled") {
                cx.toast(ToastKind::Error, format!("Sign-in failed: {e}"));
            }
            match done.purpose {
                ConnectPurpose::Tab { tab, .. } => {
                    if let Some(t) = state.tab_mut(tab) {
                        t.conn = ConnState::Failed { error: e, hint: None };
                    }
                }
                ConnectPurpose::Tree { profile } => {
                    let node = state.library.server(profile);
                    node.databases = Loadable::Failed(e);
                    node.expanded = true;
                }
                ConnectPurpose::TestOnly => {
                    if let Dialog::Connection(d) = &mut state.dialog {
                        d.testing = false;
                        d.test_result = Some(Err(e));
                    }
                }
            }
        }
    }
}

/// Password dialog → connect.
pub fn submit_password(state: &mut AppState, cx: &Ctx) {
    let Dialog::Password { profile, password, remember, purpose, .. } = std::mem::take(&mut state.dialog) else { return };
    let AuthMethod::SqlLogin { user, .. } = &profile.auth else { return };
    let mut profile = profile.clone();
    if remember {
        let r = SecretRef::for_profile(&profile.id, "password");
        if cx.secrets.set(&r, &Secret::new(password.clone())).is_ok() {
            profile.auth = AuthMethod::SqlLogin { user: user.clone(), password: Some(r) };
            let _ = cx.store.upsert_profile(&profile);
            if let Some(p) = state.library.profile_mut(profile.id) {
                *p = profile.clone();
            }
        }
    }
    finish_connect(state, cx, profile.clone(), ResolvedCredentials::SqlLogin { user: user.clone(), password: Secret::new(password) }, purpose);
}

pub fn finish_connect(state: &mut AppState, cx: &Ctx, profile: ConnectionProfile, creds: ResolvedCredentials, purpose: ConnectPurpose) {
    let _ = cx.store.touch_profile(profile.id);
    match purpose {
        ConnectPurpose::Tab { tab, database } => {
            if let Some(t) = state.tab_mut(tab) {
                t.profile = Some(profile.clone());
                t.creds = Some(creds.clone());
                t.conn = ConnState::Connecting;
                let db = database.or_else(|| profile.database.clone());
                cx.session.send(Command::Connect { tab, profile: profile.clone(), creds: creds.clone(), database: db });
            }
            // also keep creds for the tree so browsing works without a second prompt
            let node = state.library.server(profile.id);
            if node.creds.is_none() {
                node.creds = Some(creds);
            }
        }
        ConnectPurpose::Tree { profile: pid } => {
            let node = state.library.server(pid);
            node.creds = Some(creds);
            node.expanded = true;
            node.databases = Loadable::NotLoaded;
            request_meta(state, cx, pid, MetadataRequest::Probe, MetaPurpose::Tree);
            let req = request_meta(state, cx, pid, MetadataRequest::ListDatabases, MetaPurpose::Tree);
            if let Some(r) = req {
                state.library.server(pid).databases = Loadable::Loading(r);
            }
        }
        ConnectPurpose::TestOnly => {
            let driver = cx.session.driver();
            let tx = cx.auth_tx.clone();
            let egui = cx.egui.clone();
            cx.session.spawn(async move {
                let res = driver.connect(&profile, &creds, ConnectionRole::Metadata).await;
                let result = match res {
                    Ok(mut c) => {
                        let msg = format!("Connected: {} ({})", c.engine().short_label(), c.engine().version);
                        let _ = c.close().await;
                        Ok(ResolvedCredentials::SqlLogin { user: msg, password: Secret::default() })
                    }
                    Err(e) => Err(format!("{e}{}", e.hint().map(|h| format!("\n{h}")).unwrap_or_default())),
                };
                let _ = tx.send(AuthDone { purpose: ConnectPurpose::TestOnly, profile, result });
                egui.request_repaint();
            });
        }
    }
}

/// Test-connection result arrives through `on_auth_done` with purpose TestOnly and a
/// synthetic `SqlLogin{user: message}` — unpack it here.
pub fn on_test_result(state: &mut AppState, done: &AuthDone) -> bool {
    if !matches!(done.purpose, ConnectPurpose::TestOnly) {
        return false;
    }
    if let Dialog::Connection(d) = &mut state.dialog {
        d.testing = false;
        d.test_result = Some(match &done.result {
            Ok(ResolvedCredentials::SqlLogin { user, .. }) => Ok(user.clone()),
            Ok(_) => Ok("Connected.".into()),
            Err(e) => Err(e.clone()),
        });
    }
    true
}

pub fn test_connection(state: &mut AppState, cx: &Ctx) {
    let Dialog::Connection(d) = &mut state.dialog else { return };
    let mut p = d.profile.clone();
    p.port = d.port_text.trim().parse().ok();
    p.auth = auth_from_dialog(d);
    d.testing = true;
    d.test_result = None;
    if let AuthMethod::SqlLogin { user, .. } = &p.auth {
        let pw = if d.password.is_empty() {
            match &d.profile.auth {
                AuthMethod::SqlLogin { password: Some(r), .. } => cx.secrets.get(r).ok().flatten().unwrap_or_default(),
                _ => Secret::default(),
            }
        } else {
            Secret::new(d.password.clone())
        };
        let creds = ResolvedCredentials::SqlLogin { user: user.clone(), password: pw };
        finish_connect(state, cx, p, creds, ConnectPurpose::TestOnly);
    } else {
        begin_connect(state, cx, p, ConnectPurpose::TestOnly);
    }
}

pub fn disconnect_tab(state: &mut AppState, cx: &Ctx, idx: usize) {
    if let Some(t) = state.tabs.get_mut(idx) {
        cx.session.send(Command::Disconnect { tab: t.id });
        t.conn = ConnState::Disconnected;
        t.creds = None;
    }
}

// ---------------------------------------------------------------------------------------------
// Metadata
// ---------------------------------------------------------------------------------------------

pub fn request_meta(state: &mut AppState, cx: &Ctx, profile: ProfileId, kind: MetadataRequest, purpose: MetaPurpose) -> Option<RequestId> {
    let p = state.library.profile(profile).cloned()?;
    let creds = state.library.server(profile).creds.clone();
    let creds = match creds {
        Some(c) => c,
        None => {
            // not connected for metadata yet → connect first (the request is re-issued by the caller on expand)
            begin_connect(state, cx, p, ConnectPurpose::Tree { profile });
            return None;
        }
    };
    let req = cx.session.new_request();
    state.pending_meta.insert(req, PendingMeta { profile, kind: kind.clone(), purpose });
    cx.session.send(Command::Metadata { req, profile: p, creds, kind });
    Some(req)
}

pub fn tree_action(state: &mut AppState, cx: &Ctx, action: TreeAction) {
    match action {
        TreeAction::NewConnection { group } => open_connection_dialog(state, cx, None, group, None),
        TreeAction::NewGroup => {
            let next = Color::PALETTE[state.library.groups.len() % Color::PALETTE.len()];
            state.dialog = Dialog::Group { group: ServerGroup::new("", next), is_new: true };
        }
        TreeAction::EditGroup(id) => {
            if let Some(g) = state.library.group(id).cloned() {
                state.dialog = Dialog::Group { group: g, is_new: false };
            }
        }
        TreeAction::DeleteGroup(id) => state.dialog = Dialog::ConfirmDeleteGroup { group: id },
        TreeAction::EditProfile(id) => open_connection_dialog(state, cx, Some(id), None, None),
        TreeAction::DeleteProfile(id) => state.dialog = Dialog::ConfirmDeleteProfile { profile: id },
        TreeAction::ConnectServer(id) => {
            if let Some(p) = state.library.profile(id).cloned() {
                begin_connect(state, cx, p, ConnectPurpose::Tree { profile: id });
            }
        }
        TreeAction::DisconnectServer(id) => {
            cx.session.send(Command::CloseMetadata { profile: id });
            let node = state.library.server(id);
            node.creds = None;
            node.databases = Loadable::NotLoaded;
            node.db_nodes.clear();
            node.expanded = false;
        }
        TreeAction::RefreshServer(id) => {
            let node = state.library.server(id);
            node.db_nodes.clear();
            node.databases = Loadable::NotLoaded;
            if let Some(r) = request_meta(state, cx, id, MetadataRequest::ListDatabases, MetaPurpose::Tree) {
                state.library.server(id).databases = Loadable::Loading(r);
            }
        }
        TreeAction::ToggleSystemDbs(id) => {
            let node = state.library.server(id);
            node.show_system_dbs = !node.show_system_dbs;
        }
        TreeAction::ExpandDatabase { profile, database } | TreeAction::RefreshDatabase { profile, database } => {
            let _ = cx.store.invalidate_catalog(profile, Some(&database));
            if let Some(r) = request_meta(state, cx, profile, MetadataRequest::ListObjects { database: database.clone() }, MetaPurpose::Tree) {
                let dbn = state.library.server(profile).db_nodes.entry(database).or_default();
                dbn.objects = Loadable::Loading(r);
                dbn.columns.clear();
                dbn.keys.clear();
                dbn.indexes.clear();
                dbn.parameters.clear();
            }
        }
        TreeAction::LoadObjectChildren { profile, obj, sub } => {
            let kind = match sub {
                SubFolder::Columns => MetadataRequest::ListColumns { obj: obj.clone() },
                SubFolder::Keys => MetadataRequest::ListKeys { obj: obj.clone() },
                SubFolder::Indexes => MetadataRequest::ListIndexes { obj: obj.clone() },
                SubFolder::Parameters => MetadataRequest::ListParameters { obj: obj.clone() },
            };
            if let Some(r) = request_meta(state, cx, profile, kind, MetaPurpose::Tree) {
                let dbn = state.library.server(profile).db_nodes.entry(obj.database.clone()).or_default();
                let oid = obj.object_id.unwrap_or(0);
                match sub {
                    SubFolder::Columns => {
                        dbn.columns.insert(oid, Loadable::Loading(r));
                    }
                    SubFolder::Keys => {
                        dbn.keys.insert(oid, Loadable::Loading(r));
                    }
                    SubFolder::Indexes => {
                        dbn.indexes.insert(oid, Loadable::Loading(r));
                    }
                    SubFolder::Parameters => {
                        dbn.parameters.insert(oid, Loadable::Loading(r));
                    }
                }
            }
        }
        TreeAction::NewQuery { profile, database } => {
            new_query_tab(state, cx, Some(profile), database, None, false);
        }
        TreeAction::SelectTop { profile, obj } => {
            let n = cx.settings.execution.select_top_n;
            let cols = state.library.servers.get(&profile).and_then(|s| s.db_nodes.get(&obj.database)).and_then(|d| d.columns.get(&obj.object_id.unwrap_or(0))).and_then(|l| l.get()).cloned();
            let sql = match cols {
                Some(cols) if !cols.is_empty() => format!("SELECT TOP ({n})\n    {}\nFROM {}", cols.iter().map(|c| quote_ident(&c.name)).collect::<Vec<_>>().join(",\n    "), obj.bracketed()),
                _ => format!("SELECT TOP ({n}) *\nFROM {}", obj.bracketed()),
            };
            let title = format!("{}.{} (top {n})", obj.schema, obj.name);
            new_query_tab(state, cx, Some(profile), Some(obj.database.clone()), Some((title, sql)), true);
        }
        TreeAction::Script { profile, obj, kind } => {
            let title = format!("{}.{} ({:?})", obj.schema, obj.name, kind).to_uppercase();
            request_meta(state, cx, profile, MetadataRequest::Script { obj: obj.clone(), kind }, MetaPurpose::ScriptToTab { title, profile, database: obj.database.clone(), run: false });
        }
        TreeAction::CopyText(s) => {
            cx.egui.copy_text(s);
            state.flash("Copied");
        }
        TreeAction::InsertIntoEditor(s) => insert_at_cursor(state, &s),
        TreeAction::ImportFile { profile, database } => {
            // a tab on that database; the dialog opens once it is connected
            let idx = new_query_tab(state, cx, Some(profile), Some(database), None, false);
            if let Some(t) = state.tabs.get_mut(idx) {
                if t.conn.is_connected() {
                    open_import_dialog(state, cx, idx, None);
                } else {
                    t.pending_import = true;
                }
            }
        }
        TreeAction::MoveProfile { profile, group } => move_profile(state, cx, profile, group),
    }
}

pub fn insert_at_cursor(state: &mut AppState, s: &str) {
    if state.active_tab.is_none() {
        state.new_tab();
    }
    if let Some(t) = state.active_mut() {
        let byte = char_to_byte(&t.text, t.editor.cursor);
        let after = t.editor.cursor + s.chars().count();
        t.editor.pending_edit = Some(PendingEdit::Replace { start: byte, end: byte, text: s.to_string(), cursor_after: Some(after) });
    }
}

// ---------------------------------------------------------------------------------------------
// Tabs
// ---------------------------------------------------------------------------------------------

/// Open a new tab (optionally with text) connected to `profile`/`database`; optionally run it.
pub fn new_query_tab(state: &mut AppState, cx: &Ctx, profile: Option<ProfileId>, database: Option<String>, content: Option<(String, String)>, run_after: bool) -> usize {
    let idx = state.new_tab();
    let tab_id = state.tabs[idx].id;
    if let Some((title, text)) = content {
        let t = &mut state.tabs[idx];
        t.title = title;
        t.custom_title = true;
        t.text = text;
        t.mark_saved();
    }
    state.tabs[idx].editor.request_focus = true;
    if let Some(pid) = profile.and_then(|id| state.library.profile(id).cloned()) {
        state.tabs[idx].pending_run = if run_after { Some(RunMode::All) } else { None };
        // reuse the tree's credentials when we have them
        let creds = state.library.servers.get(&pid.id).and_then(|s| s.creds.clone());
        match creds {
            Some(c) => finish_connect(state, cx, pid, c, ConnectPurpose::Tab { tab: tab_id, database }),
            None => begin_connect(state, cx, pid, ConnectPurpose::Tab { tab: tab_id, database }),
        }
    }
    idx
}

pub fn close_tab(state: &mut AppState, cx: &Ctx, idx: usize, force: bool) {
    let Some(t) = state.tabs.get(idx) else { return };
    if t.is_dirty() && !force && !t.text.trim().is_empty() {
        state.dialog = Dialog::ConfirmClose { tab_index: idx };
        return;
    }
    let t = state.tabs.remove(idx);
    cx.session.send(Command::Disconnect { tab: t.id });
    // keep a restorable snapshot
    let mut snap = TabSnapshot::new(t.id, t.title.clone(), t.text.clone());
    snap.profile_id = t.profile.as_ref().map(|p| p.id);
    snap.database = t.conn.database().map(str::to_string);
    snap.cursor = t.editor.cursor;
    snap.file_path = t.file_path.clone();
    let _ = cx.store.save_tab(&snap);
    let _ = cx.store.close_tab(t.id);
    state.recently_closed_count += 1;
    if state.tabs.is_empty() {
        state.active_tab = None;
    } else {
        let a = state.active_tab.unwrap_or(0);
        state.active_tab = Some(if a >= state.tabs.len() { state.tabs.len() - 1 } else if a > idx { a - 1 } else { a });
    }
}

pub fn reopen_closed_tab(state: &mut AppState, cx: &Ctx) {
    let Ok(list) = cx.store.recently_closed(1) else { return };
    let Some(snap) = list.into_iter().next() else { return };
    let _ = cx.store.restore_tab(snap.tab_id);
    let idx = state.new_tab();
    let t = &mut state.tabs[idx];
    t.id = snap.tab_id;
    t.title = snap.title;
    t.custom_title = true;
    t.text = snap.text;
    t.file_path = snap.file_path;
    t.editor.pending_edit = Some(PendingEdit::SetCursor(snap.cursor));
    t.mark_saved();
    let tab_id = t.id;
    if let Some(pid) = snap.profile_id.and_then(|id| state.library.profile(id).cloned()) {
        begin_connect(state, cx, pid, ConnectPurpose::Tab { tab: tab_id, database: snap.database });
    }
}

/// Persist unsaved tab contents (hot exit). Cheap: only tabs whose text changed since the last snapshot.
pub fn snapshot_tabs(state: &mut AppState, cx: &Ctx, force: bool) {
    for t in &mut state.tabs {
        let h = hash_text(&t.text);
        if force || h != t.snapshot_hash {
            let mut snap = TabSnapshot::new(t.id, t.title.clone(), t.text.clone());
            snap.profile_id = t.profile.as_ref().map(|p| p.id);
            snap.database = t.conn.database().map(str::to_string);
            snap.cursor = t.editor.cursor;
            snap.file_path = t.file_path.clone();
            if cx.store.save_tab(&snap).is_ok() {
                t.snapshot_hash = h;
            }
        }
    }
    state.last_hot_exit_save = Instant::now();
}

pub fn restore_tabs(state: &mut AppState, cx: &Ctx) {
    let Ok(open) = cx.store.load_open_tabs() else { return };
    for snap in open {
        let idx = state.new_tab();
        let t = &mut state.tabs[idx];
        t.id = snap.tab_id;
        t.title = snap.title;
        t.custom_title = !t.title.starts_with("SQLQuery_");
        if let Some(n) = t.title.strip_prefix("SQLQuery_").and_then(|r| r.split([' ', '·']).next()).and_then(|n| n.parse::<usize>().ok()) {
            t.untitled_index = n;
            state.next_untitled = state.next_untitled.max(n + 1);
        }
        t.text = snap.text;
        t.file_path = snap.file_path.clone();
        t.editor.cursor = snap.cursor;
        t.snapshot_hash = hash_text(&t.text);
        if let Some(path) = &snap.file_path {
            if let Ok(disk) = std::fs::read_to_string(path) {
                if disk == t.text {
                    t.mark_saved();
                }
            }
        }
        // remember which profile it was on; connect lazily on first run
        if let Some(p) = snap.profile_id.and_then(|id| state.library.profile(id).cloned()) {
            t.profile = Some(p);
            t.pending_run = None;
        }
        let _ = snap.database;
    }
    if !state.tabs.is_empty() {
        state.active_tab = Some(0);
    }
}

pub fn open_file(state: &mut AppState, cx: &Ctx, path: Option<PathBuf>) {
    let path = match path {
        Some(p) => p,
        None => match rfd::FileDialog::new().add_filter("SQL", &["sql"]).add_filter("Execution plan", &["sqlplan"]).add_filter("All files", &["*"]).pick_file() {
            Some(p) => p,
            None => return,
        },
    };
    if path.extension().map(|e| e.eq_ignore_ascii_case("sqlplan")).unwrap_or(false) {
        open_plan_file(state, cx, path);
        return;
    }
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let text = text.strip_prefix('\u{feff}').map(str::to_string).unwrap_or(text);
            if let Some(i) = state.tabs.iter().position(|t| t.file_path.as_ref() == Some(&path)) {
                state.active_tab = Some(i);
                return;
            }
            let idx = state.new_tab();
            let t = &mut state.tabs[idx];
            t.title = path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "file.sql".into());
            t.custom_title = true;
            t.text = text;
            t.file_path = Some(path);
            t.mark_saved();
            t.editor.request_focus = true;
        }
        Err(e) => cx.toast(ToastKind::Error, format!("Could not open file: {e}")),
    }
}

pub fn open_plan_file(state: &mut AppState, cx: &Ctx, path: PathBuf) {
    match std::fs::read_to_string(&path) {
        Ok(xml) => {
            let idx = state.new_tab();
            let t = &mut state.tabs[idx];
            t.title = path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "plan.sqlplan".into());
            t.custom_title = true;
            let mut run = RunView::new(RunId(0), PlanMode::Estimated);
            run.state = RunViewState::Done;
            let mut pv = PlanView::new(xml);
            pv.statement_index = 0;
            run.plans.push(pv);
            t.run = Some(run);
            t.results_tab = ResultsTab::Plan;
            t.results_fraction = 0.9;
            t.text = "-- Execution plan opened from file. Query text is inside the plan.".into();
            t.mark_saved();
        }
        Err(e) => cx.toast(ToastKind::Error, format!("Could not open plan: {e}")),
    }
}

pub fn save_file(state: &mut AppState, cx: &Ctx, idx: usize, save_as: bool) {
    let Some(t) = state.tabs.get(idx) else { return };
    let path = match (&t.file_path, save_as) {
        (Some(p), false) => p.clone(),
        _ => {
            let mut dlg = rfd::FileDialog::new().add_filter("SQL", &["sql"]).set_file_name(format!("{}.sql", t.title.trim_end_matches(".sql")));
            if let Some(dir) = cx.settings.export.last_dir.as_ref() {
                dlg = dlg.set_directory(dir);
            }
            match dlg.save_file() {
                Some(p) => p,
                None => return,
            }
        }
    };
    let t = &mut state.tabs[idx];
    match std::fs::write(&path, &t.text) {
        Ok(()) => {
            t.file_path = Some(path.clone());
            t.title = path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or(t.title.clone());
            t.custom_title = true;
            t.mark_saved();
            state.flash(format!("Saved {}", path.display()));
        }
        Err(e) => cx.toast(ToastKind::Error, format!("Save failed: {e}")),
    }
}

// ---------------------------------------------------------------------------------------------
// Running
// ---------------------------------------------------------------------------------------------

fn read_only_violation(script: &str) -> Option<String> {
    let lower = script.to_ascii_lowercase();
    for kw in ["insert ", "update ", "delete ", "merge ", "drop ", "alter ", "create ", "truncate ", "exec ", "execute ", "grant ", "deny ", "revoke "] {
        if let Some(pos) = lower.find(kw) {
            // crude: ignore if inside a comment line
            let line_start = lower[..pos].rfind('\n').map(|i| i + 1).unwrap_or(0);
            if lower[line_start..pos].trim_start().starts_with("--") {
                continue;
            }
            return Some(script[pos..].lines().next().unwrap_or("").trim().to_string());
        }
    }
    None
}

pub fn run(state: &mut AppState, cx: &Ctx, idx: usize, mode: RunMode) {
    let Some(t) = state.tabs.get_mut(idx) else { return };
    if t.is_running() {
        return;
    }
    // choose the text
    let (script, start_line) = match mode {
        RunMode::All | RunMode::EstimatedPlan => match t.editor.selection {
            Some((a, b)) if a != b => {
                let ab = char_to_byte(&t.text, a);
                let bb = char_to_byte(&t.text, b);
                (t.text[ab..bb].to_string(), cobalt_sql::statements::line_of(&t.text, ab))
            }
            _ => (t.text.clone(), 1),
        },
        RunMode::Selection => match t.editor.selection {
            Some((a, b)) if a != b => {
                let ab = char_to_byte(&t.text, a);
                let bb = char_to_byte(&t.text, b);
                (t.text[ab..bb].to_string(), cobalt_sql::statements::line_of(&t.text, ab))
            }
            _ => (t.text.clone(), 1),
        },
        RunMode::Current => {
            let cursor = char_to_byte(&t.text, t.editor.cursor);
            match cobalt_sql::statements::statement_at(&t.text, cursor) {
                Some(s) => (t.text[s.start..s.end].to_string(), s.line),
                None => (t.text.clone(), 1),
            }
        }
    };
    if script.trim().is_empty() {
        return;
    }
    let tab_id = t.id;
    // need a connection?
    if !t.conn.is_connected() {
        t.pending_run = Some(mode);
        match t.profile.clone() {
            Some(p) => {
                let db = t.conn.database().map(str::to_string);
                begin_connect(state, cx, p, ConnectPurpose::Tab { tab: tab_id, database: db });
            }
            None => state.dialog = Dialog::ChangeConnection { tab_index: idx },
        }
        return;
    }
    let mut opts = t.exec.clone();
    opts.row_cap = cx.settings.execution.row_cap;
    if opts.timeout_secs == 0 {
        opts.timeout_secs = cx.settings.execution.command_timeout_secs;
    }
    // Engines without SET STATISTICS XML (Fabric Warehouse, Synapse) silently run without a plan.
    let actual_supported = t.conn.capabilities().map(|c| c.actual_plans).unwrap_or(true);
    opts.plan = match mode {
        RunMode::EstimatedPlan => PlanMode::Estimated,
        _ if t.actual_plan && actual_supported => PlanMode::Actual,
        _ => PlanMode::None,
    };
    if let Some(p) = &t.profile {
        if p.read_only_guard && mode != RunMode::EstimatedPlan {
            if let Some(stmt) = read_only_violation(&script) {
                state.dialog = Dialog::ConfirmWrite { tab_index: idx, statement_preview: stmt, script, opts, start_line };
                return;
            }
        }
    }
    execute(state, cx, idx, script, opts, start_line);
}

pub fn execute(state: &mut AppState, cx: &Ctx, idx: usize, script: String, mut opts: ExecOptions, start_line: u32) {
    let Some(t) = state.tabs.get_mut(idx) else { return };
    let job = t.pending_export.take();
    if job.is_some() {
        opts.plan = PlanMode::None; // plans have no place in a file
    }
    let run_id = cx.session.new_run();
    let mut view = RunView::new(run_id, opts.plan);
    view.export_target = job.as_ref().map(|j| j.display_target());
    view.script_hash = hash_text(&t.text);
    // history
    if cx.settings.history.capture {
        let mut e = NewHistoryEntry::new(t.profile.as_ref().map(|p| p.display_name()).unwrap_or_default(), script.clone());
        e.profile_id = t.profile.as_ref().map(|p| p.id);
        e.database = t.conn.database().map(str::to_string);
        e.tab_id = Some(t.id);
        view.history_id = cx.store.add_history(&e).ok();
    }
    t.run = Some(view);
    t.results_visible = true;
    t.results_tab = if opts.plan == PlanMode::Estimated { ResultsTab::Plan } else { ResultsTab::Results };
    t.pending_run = None;
    let tab_id = t.id;
    let sink = job.map(|j| spawn_run_export(state, cx, tab_id, *j));
    cx.session.send(Command::Run { tab: tab_id, run: run_id, script, opts, start_line, sink });
    state.history.loaded = false;
}

pub fn cancel(state: &mut AppState, cx: &Ctx, idx: usize) {
    if let Some(t) = state.tabs.get_mut(idx) {
        if let Some(r) = &mut t.run {
            if r.is_live() {
                r.state = RunViewState::Cancelling;
                cx.session.send(Command::Cancel { tab: t.id });
            }
        }
    }
}

pub fn fetch_more(state: &mut AppState, cx: &Ctx, idx: usize, rows: Option<u64>) {
    if let Some(t) = state.tabs.get_mut(idx) {
        if let Some(r) = &mut t.run {
            if r.state == RunViewState::Paused {
                r.state = RunViewState::Running;
                r.paused_set = None;
                // resume the timer from where it was frozen
                r.started = Instant::now().checked_sub(r.elapsed).unwrap_or_else(Instant::now);
            }
        }
        cx.session.send(Command::FetchMore { tab: t.id, rows });
    }
}

pub fn change_database(state: &mut AppState, cx: &Ctx, idx: usize, db: String) {
    if let Some(t) = state.tabs.get(idx) {
        // Engines without `USE` (SQL database in Fabric, Azure SQL DB) switch by reconnecting.
        let can_use = t.conn.capabilities().map(|c| c.multiple_databases).unwrap_or(true);
        if t.conn.is_connected() && can_use {
            cx.session.send(Command::ChangeDatabase { tab: t.id, database: db });
        } else if let Some(p) = t.profile.clone() {
            let tab = t.id;
            begin_connect(state, cx, p, ConnectPurpose::Tab { tab, database: Some(db) });
        }
    }
}

/// After events are applied: side effects that need the store/session.
pub fn handle_followups(state: &mut AppState, cx: &Ctx, followups: Vec<Followup>) {
    for f in followups {
        match f {
            Followup::LoadTabDatabases(tab) => {
                if let Some(t) = state.tab_mut(tab) {
                    let Some(p) = t.profile.clone() else { continue };
                    t.databases = Loadable::NotLoaded;
                    if state.library.server(p.id).creds.is_none() {
                        if let Some(c) = state.tab_mut(tab).and_then(|t| t.creds.clone()) {
                            state.library.server(p.id).creds = Some(c);
                        }
                    }
                    let req = request_meta(state, cx, p.id, MetadataRequest::ListDatabases, MetaPurpose::TabDatabases { tab });
                    if let (Some(r), Some(t)) = (req, state.tab_mut(tab)) {
                        t.databases = Loadable::Loading(r);
                    }
                    // update the title and run anything queued
                    if let Some(t) = state.tab_mut(tab) {
                        if !t.custom_title {
                            if let Some(db) = t.conn.database() {
                                t.title = format!("SQLQuery_{} · {}", t.untitled_index, db);
                            }
                        }
                    }
                    if let Some(idx) = state.tab_index(tab) {
                        if let Some(mode) = state.tabs[idx].pending_run.take() {
                            run(state, cx, idx, mode);
                        }
                        if std::mem::take(&mut state.tabs[idx].pending_import) {
                            open_import_dialog(state, cx, idx, None);
                        }
                    }
                }
            }
            Followup::LoadCatalog(tab, database) => {
                let Some(p) = state.tab_mut(tab).and_then(|t| t.profile.clone()) else { continue };
                // cache first
                if let Ok(Some((cat, when))) = cx.store.get_catalog(p.id, &database) {
                    let fresh = (chrono::Utc::now() - when).num_minutes() < 30;
                    if let Some(t) = state.tab_mut(tab) {
                        t.catalog = Some(Arc::new(cat));
                        t.catalog_database = Some(database.clone());
                    }
                    if fresh {
                        continue;
                    }
                }
                request_meta(state, cx, p.id, MetadataRequest::LoadCatalog { database: database.clone() }, MetaPurpose::Catalog { tab, database });
            }
            Followup::FinishHistory { history_id, elapsed, rows, cancelled, failed, error } => {
                if let Some(id) = history_id {
                    let status = if cancelled {
                        HistoryStatus::Cancelled
                    } else if failed {
                        HistoryStatus::Error
                    } else {
                        HistoryStatus::Success
                    };
                    let _ = cx.store.finish_history(id, chrono::Utc::now(), Some(elapsed.as_millis() as u64), Some(rows), status, error.as_deref());
                    state.history.loaded = false;
                }
            }
            Followup::CacheCatalog { profile, database, catalog } => {
                let _ = cx.store.put_catalog(profile, &database, &catalog);
            }
            Followup::OpenScriptTab { title, sql, profile, database, run: run_after } => {
                new_query_tab(state, cx, Some(profile), Some(database), Some((title, sql)), run_after);
            }
            Followup::Toast(kind, msg) => cx.toast(kind, msg),
            Followup::RefreshDatabase { profile, database } => {
                // only when the tree already shows that database (otherwise nothing to refresh)
                let shown = state.library.servers.get(&profile).map(|n| n.creds.is_some() && n.db_nodes.contains_key(&database)).unwrap_or(false);
                if shown {
                    tree_action(state, cx, TreeAction::RefreshDatabase { profile, database });
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Import Data (flat file → table)
// ---------------------------------------------------------------------------------------------

/// Open the Import Data dialog for a connected tab. Without `path`, ask for the file first.
pub fn open_import_dialog(state: &mut AppState, cx: &Ctx, idx: usize, path: Option<PathBuf>) {
    let Some(t) = state.tabs.get(idx) else { return };
    if !t.conn.is_connected() {
        cx.toast(ToastKind::Warning, "Connect this tab first: the file is loaded through the tab's connection.");
        return;
    }
    let path = match path {
        Some(p) => p,
        None => match rfd::FileDialog::new().add_filter("Data files", &["csv", "tsv", "txt", "parquet", "pq", "arrow", "feather", "ipc"]).add_filter("All files", &["*"]).pick_file() {
            Some(p) => p,
            None => return,
        },
    };
    let mut d = ImportDialog {
        tab_index: idx,
        path: path.to_string_lossy().to_string(),
        delimiter: String::new(),
        has_header: true,
        inspection: None,
        inspect_error: None,
        columns: Vec::new(),
        schema_name: "dbo".into(),
        table_name: cobalt_import::table_name_from(&path),
        existing: false,
        existing_columns: Loadable::NotLoaded,
        want_existing_columns: false,
        running: false,
        rows_done: 0,
        started: None,
        result: None,
        cancel: Arc::new(AtomicBool::new(false)),
    };
    inspect_import(&mut d);
    state.dialog = Dialog::Import(Box::new(d));
}

fn import_format(d: &ImportDialog) -> cobalt_import::FileFormat {
    let sniffed = cobalt_import::FileFormat::sniff(std::path::Path::new(&d.path));
    match sniffed {
        cobalt_import::FileFormat::Csv { delimiter, .. } => {
            let delim = match d.delimiter.trim() {
                "" => delimiter,
                "\\t" | "tab" => b'\t',
                s => s.bytes().next().unwrap_or(delimiter),
            };
            cobalt_import::FileFormat::Csv { delimiter: delim, has_header: d.has_header }
        }
        other => other,
    }
}

/// (Re)read the file's shape into the dialog.
pub fn inspect_import(d: &mut ImportDialog) {
    let format = import_format(d);
    if let cobalt_import::FileFormat::Csv { delimiter, .. } = &format {
        if d.delimiter.trim().is_empty() {
            d.delimiter = if *delimiter == b'\t' { "\\t".into() } else { (*delimiter as char).to_string() };
        }
    }
    match cobalt_import::inspect(std::path::Path::new(&d.path), &format, 1000, 12) {
        Ok(ins) => {
            d.columns = ins.columns.iter().map(|c| ImportColumnEdit { name: c.name.clone(), sql_type: c.sql_type.to_string(), nullable: c.nullable, include: true, source: format!("{:?}", c.arrow) }).collect();
            d.inspection = Some(ins);
            d.inspect_error = None;
            if d.existing {
                d.want_existing_columns = true;
            }
        }
        Err(e) => {
            d.inspection = None;
            d.inspect_error = Some(e.to_string());
        }
    }
}

/// Existing-table mode: fetch the target table's columns through the tab's connection.
pub fn import_request_existing_columns(state: &mut AppState, cx: &Ctx) {
    let Dialog::Import(d) = &mut state.dialog else { return };
    d.want_existing_columns = false;
    let Some(t) = state.tabs.get(d.tab_index) else { return };
    let (Some(p), Some(db)) = (t.profile.clone(), t.conn.database().map(str::to_string)) else { return };
    let obj = ObjectRef { database: db, schema: d.schema_name.trim().to_string(), name: d.table_name.trim().to_string(), kind: ObjectKind::Table, object_id: None };
    match request_meta(state, cx, p.id, MetadataRequest::ListColumns { obj }, MetaPurpose::ImportColumns) {
        Some(r) => {
            if let Dialog::Import(d) = &mut state.dialog {
                d.existing_columns = Loadable::Loading(r);
            }
        }
        None => {
            if let Dialog::Import(d) = &mut state.dialog {
                d.existing_columns = Loadable::Failed("the tree connection for this server is not open; expand it in Servers and try again".into());
            }
        }
    }
}

/// Validate the dialog, start the reader thread and hand the import to the tab's session actor.
pub fn start_import(state: &mut AppState, cx: &Ctx) {
    let Dialog::Import(d) = &mut state.dialog else { return };
    let Some(ins) = d.inspection.clone() else {
        d.result = Some(Err("Nothing to import: the file could not be read.".into()));
        return;
    };
    let schema_name = d.schema_name.trim().to_string();
    let table_name = d.table_name.trim().to_string();
    if schema_name.is_empty() || table_name.is_empty() {
        d.result = Some(Err("Give the target table a schema and a name.".into()));
        return;
    }
    let mut columns: Vec<ColumnInfo> = Vec::new();
    let mut indexes: Vec<usize> = Vec::new();
    for (i, c) in d.columns.iter().enumerate() {
        if !c.include {
            continue;
        }
        let name = c.name.trim().to_string();
        if name.is_empty() {
            d.result = Some(Err(format!("Column {} has no name.", i + 1)));
            return;
        }
        let Some(ty) = cobalt_import::parse_sql_type(&c.sql_type) else {
            d.result = Some(Err(format!("Column {name}: type \"{}\" is not recognised (try int, bigint, nvarchar(100), decimal(18,2), date, datetime2, bit…).", c.sql_type)));
            return;
        };
        columns.push(ColumnInfo::new(name, ty, c.nullable, columns.len()));
        indexes.push(i);
    }
    if columns.is_empty() {
        d.result = Some(Err("Include at least one column.".into()));
        return;
    }
    let Some(t) = state.tabs.get(d.tab_index) else { return };
    if !t.conn.is_connected() {
        d.result = Some(Err("The tab is not connected.".into()));
        return;
    }
    let tab_id = t.id;
    let create_sql = if d.existing { None } else { Some(cobalt_import::create_table_sql(&schema_name, &table_name, &columns.iter().map(|c| (c.name.clone(), c.sql_type.clone(), c.nullable)).collect::<Vec<_>>())) };
    let table = format!("[{}].[{}]", schema_name.replace(']', "]]"), table_name.replace(']', "]]"));
    let format = import_format(d);
    let path = PathBuf::from(d.path.trim());
    let schema = ins.schema.clone();
    d.running = true;
    d.result = None;
    d.rows_done = 0;
    d.started = Some(Instant::now());
    let cancel = d.cancel.clone();
    cancel.store(false, Ordering::Relaxed);
    // reader thread: file → Arrow batches → bounded channel → session actor
    let (tx, rx) = std::sync::mpsc::sync_channel::<std::result::Result<arrow::array::RecordBatch, String>>(4);
    let cancel2 = cancel.clone();
    std::thread::Builder::new()
        .name("cobalt-import-reader".into())
        .spawn(move || {
            let iter = match cobalt_import::open_batches(&path, &format, schema, cobalt_import::BATCH_ROWS) {
                Ok(it) => it,
                Err(e) => {
                    let _ = tx.send(Err(e.to_string()));
                    return;
                }
            };
            for b in iter {
                if cancel2.load(Ordering::Relaxed) {
                    break;
                }
                let item = b.and_then(|b| cobalt_import::project(&b, &indexes)).map_err(|e| e.to_string());
                let stop = item.is_err();
                if tx.send(item).is_err() || stop {
                    break;
                }
            }
        })
        .ok();
    cx.session.send(Command::Import { tab: tab_id, table, create_sql, columns, rx, cancel });
}

pub fn cancel_import(state: &mut AppState) {
    if let Dialog::Import(d) = &state.dialog {
        d.cancel.store(true, Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------------------------------------
// Results
// ---------------------------------------------------------------------------------------------

pub fn results_action(state: &mut AppState, cx: &Ctx, idx: usize, action: ResultsAction) {
    match action {
        ResultsAction::FetchMore { rows, .. } => fetch_more(state, cx, idx, rows),
        ResultsAction::Cancel => cancel(state, cx, idx),
        ResultsAction::Copy { set, kind } => copy_cells(state, cx, idx, set, kind),
        ResultsAction::Export { set, selection_only } => open_export_dialog(state, cx, idx, set, selection_only),
        ResultsAction::ApplyView { set, spec } => apply_view(state, cx, idx, set, spec),
        ResultsAction::OpenViewer { set, row, col } => {
            if let Some(v) = state.tabs.get_mut(idx).and_then(|t| t.run.as_mut()).and_then(|r| r.result_sets.get_mut(set)) {
                v.grid.viewer = Some((row, col));
            }
        }
        ResultsAction::OpenRecord { set, row, col } => {
            if let Some(v) = state.tabs.get_mut(idx).and_then(|t| t.run.as_mut()).and_then(|r| r.result_sets.get_mut(set)) {
                v.grid.viewer = Some((row, col));
                v.grid.viewer_record = true;
            }
        }
        ResultsAction::JumpToLine(line) => {
            if let Some(t) = state.tabs.get_mut(idx) {
                let byte = t.text.split_inclusive('\n').take(line.saturating_sub(1) as usize).map(|l| l.len()).sum::<usize>();
                let c = byte_to_char(&t.text, byte);
                t.editor.pending_edit = Some(PendingEdit::SetCursor(c));
                state.focus = Focus::Editor;
            }
        }
        ResultsAction::Summarize { .. } => {}
        ResultsAction::PopOut { set } => pop_out_result(state, idx, set),
    }
}

pub fn apply_view(state: &mut AppState, cx: &Ctx, idx: usize, set: usize, spec: cobalt_results::ViewSpec) {
    let Some(v) = state.tabs.get_mut(idx).and_then(|t| t.run.as_mut()).and_then(|r| r.result_sets.get_mut(set)) else { return };
    v.grid.view = spec.clone();
    v.grid.selection = Selection::None;
    v.grid.anchor = None;
    let rs = v.rs.clone();
    if rs.row_count() < 100_000 {
        if let Err(e) = rs.apply_view(spec) {
            cx.toast(ToastKind::Error, format!("Filter failed: {e}"));
        }
    } else {
        v.grid.applying_view = true;
        let egui = cx.egui.clone();
        cx.session.handle().spawn_blocking(move || {
            if let Err(e) = rs.apply_view(spec) {
                tracing::error!(error = %e, "apply_view failed");
            }
            egui.request_repaint();
        });
    }
}

pub fn copy_cells(state: &mut AppState, cx: &Ctx, idx: usize, set: usize, kind: CopyKind) {
    let Some(t) = state.tabs.get(idx) else { return };
    let Some(v) = t.run.as_ref().and_then(|r| r.result_sets.get(set)) else { return };
    let sel = if v.grid.selection == Selection::None { Selection::All } else { v.grid.selection.clone() };
    let count = sel.cell_count(v.rs.visible_count(), v.rs.column_count());
    if count > 5_000_000 {
        cx.toast(ToastKind::Warning, "That's more than 5 million cells — use Save results as… instead.");
        return;
    }
    let table = guess_table_name(&t.text).unwrap_or_default();
    let opts = copy::CopyOptions { fmt: &state.formatter.clone().with_max_chars(0), null_as: &cx.settings.results.copy_null_as, table_name: &table };
    if let Some(text) = copy::build(&v.rs, &sel, kind, &opts) {
        cx.egui.copy_text(text);
        state.flash(format!("Copied {} cell{}", fmt_count(count as u64), if count == 1 { "" } else { "s" }));
    }
}

fn guess_table_name(sql: &str) -> Option<String> {
    let lower = sql.to_ascii_lowercase();
    let pos = lower.find(" from ")? + 6;
    let rest = sql[pos..].trim_start();
    let name: String = rest.chars().take_while(|c| !c.is_whitespace() && *c != ';' && *c != '(' && *c != ',').collect();
    if name.is_empty() { None } else { Some(name) }
}

pub fn pop_out_result(state: &mut AppState, idx: usize, set: usize) {
    let Some(t) = state.tabs.get_mut(idx) else { return };
    let Some(r) = t.run.as_ref() else { return };
    let Some(v) = r.result_sets.get(set) else { return };
    let rs = v.rs.clone();
    let title = format!("{} · result {}", t.title.split(" · ").next().unwrap_or("Result"), set + 1);
    let new_idx = state.new_tab();
    let nt = &mut state.tabs[new_idx];
    nt.title = title;
    nt.custom_title = true;
    nt.text = String::new();
    let mut run = RunView::new(RunId(0), PlanMode::None);
    run.state = RunViewState::Done;
    run.result_sets.push(ResultSetView { rs, grid: GridState::default(), is_plan: false });
    nt.run = Some(run);
    nt.results_fraction = 0.95;
    nt.mark_saved();
}

// ---------------------------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------------------------

pub const FORMAT_LABELS: &[(&str, &str)] = &[
    ("CSV", "csv"),
    ("TSV", "tsv"),
    ("Excel workbook", "xlsx"),
    ("JSON", "json"),
    ("JSON Lines", "jsonl"),
    ("XML", "xml"),
    ("Markdown table", "md"),
    ("Parquet", "parquet"),
    ("Arrow IPC (Feather)", "arrow"),
    ("Delta Lake table (folder)", "delta"),
];

pub fn open_export_dialog(state: &mut AppState, cx: &Ctx, idx: usize, set: usize, selection_only: bool) {
    let Some(t) = state.tabs.get(idx) else { return };
    let base = t.title.split(" · ").next().unwrap_or("results").replace(|c: char| !c.is_alphanumeric() && c != '_' && c != '-', "_");
    let dir = cx.settings.export.last_dir.clone().or_else(|| directories::UserDirs::new().and_then(|u| u.download_dir().map(|d| d.to_string_lossy().to_string()))).unwrap_or_default();
    let format_index = cx.settings.export.last_format.as_deref().and_then(|f| FORMAT_LABELS.iter().position(|(_, e)| *e == f)).unwrap_or(0);
    let ext = FORMAT_LABELS[format_index].1;
    let path = std::path::Path::new(&dir).join(if ext == "delta" { base.clone() } else { format!("{base}.{ext}") }).to_string_lossy().to_string();
    state.dialog = Dialog::Export(Box::new(ExportDialog {
        tab_index: idx,
        set_index: set,
        format_index,
        path,
        selection_only,
        delta_mode: 0,
        delta_partition: String::new(),
        destination: 0,
        onelake_item: cx.store.fabric_cache_get(LAST_LAKEHOUSE_KEY).ok().flatten().map(|(v, _)| v).filter(|v| !v.is_empty()),
        onelake_name: String::new(),
        onelake_schema: String::new(),
        csv_delimiter: cx.settings.export.csv_delimiter.clone(),
        csv_headers: cx.settings.export.csv_include_headers,
        json_lines: cx.settings.export.json_lines,
        running: false,
        progress: None,
        result: None,
        cancel: Arc::new(AtomicBool::new(false)),
        run_mode: None,
    }));
}

/// "Run to File…": the export dialog in run mode for the tab's script (selection or all).
pub fn open_run_to_file(state: &mut AppState, cx: &Ctx, idx: usize, mode: RunMode) {
    if state.tabs.get(idx).map(|t| t.is_running()).unwrap_or(true) {
        cx.toast(ToastKind::Warning, "A query is already running in this tab.");
        return;
    }
    open_export_dialog(state, cx, idx, 0, false);
    if let Dialog::Export(d) = &mut state.dialog {
        d.run_mode = Some(mode);
    }
}

/// Validate the Run to File dialog, stash the target on the tab and start the run.
pub fn start_run_export(state: &mut AppState, cx: &Ctx) {
    let Dialog::Export(d) = &mut state.dialog else { return };
    let Some(mode) = d.run_mode else { return };
    let idx = d.tab_index;
    let Some(t) = state.tabs.get(idx) else { return };
    if t.is_running() {
        d.result = Some(Err("A query is already running in this tab.".into()));
        return;
    }
    let (_, ext) = FORMAT_LABELS[d.format_index];
    let mut settings = cx.settings.export.clone();
    settings.csv_delimiter = d.csv_delimiter.clone();
    settings.csv_include_headers = d.csv_headers;
    settings.json_lines = d.json_lines;
    let onelake = if d.destination == 1 {
        let Some(item_id) = d.onelake_item.clone() else {
            d.result = Some(Err("Choose a lakehouse.".into()));
            return;
        };
        let name = d.onelake_name.trim().trim_matches('/').to_string();
        if name.is_empty() || name.contains(['\\', ':']) {
            d.result = Some(Err(if ext == "delta" { "Give the table a name." } else { "Give the file a name." }.into()));
            return;
        }
        let Some(item) = state.fabric.item(&item_id).cloned() else {
            d.result = Some(Err("That lakehouse is no longer listed; refresh the Fabric panel.".into()));
            return;
        };
        let Some(slot) = state.fabric.slot else {
            d.result = Some(Err("Sign in to Fabric first (Fabric panel).".into()));
            return;
        };
        let _ = cx.store.fabric_cache_put(LAST_LAKEHOUSE_KEY, &item.id);
        let mut schema = d.onelake_schema.trim().trim_matches('/').to_string();
        if schema.is_empty() {
            if let Some(default) = state.fabric.details.get(&item_id).and_then(|d| d.get()).and_then(|t| t.default_schema.clone()) {
                schema = default;
            }
        }
        Some(OneLakeJob { item, name, schema, slot, tenant: cx.settings.connections.entra_default_tenant.clone(), hint: state.fabric.account.as_ref().map(|a| a.username.clone()) })
    } else {
        if d.path.trim().is_empty() {
            d.result = Some(Err("Choose a file path.".into()));
            return;
        }
        None
    };
    state.settings_patch.push(SettingsPatch::LastExportFormat(ext.to_string()));
    let job = ExportJob {
        ext: ext.to_string(),
        path: d.path.trim().to_string(),
        delta_mode: d.delta_mode,
        delta_partition: d.delta_partition.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
        settings,
        fmt: state.formatter.clone().with_max_chars(0),
        onelake,
    };
    if job.onelake.is_none() {
        if let Some(parent) = std::path::Path::new(&job.path).parent() {
            state.settings_patch.push(SettingsPatch::LastExportDir(parent.to_string_lossy().to_string()));
        }
    }
    state.tabs[idx].pending_export = Some(Box::new(job));
    state.dialog = Dialog::None;
    run(state, cx, idx, mode);
    // nothing to run (empty script)? don't leave the target armed for the next plain run
    if state.tabs[idx].run.as_ref().map(|r| !r.is_live()).unwrap_or(true) && state.tabs[idx].pending_run.is_none() && !matches!(state.dialog, Dialog::ConfirmWrite { .. }) {
        if state.tabs[idx].pending_export.take().is_some() {
            cx.toast(ToastKind::Warning, "Nothing to run.");
        }
    }
}

/// OneLake accepts an Azure Storage token; when the registration lacks that permission the Fabric
/// API token (with OneLake.ReadWrite.All) works too. Silent first, then the browser as a last resort.
async fn onelake_token(resolver: Arc<CredentialResolver>, slot: ProfileId, tenant: Option<String>, hint: Option<String>, prompter: UiPrompter) -> Result<String, String> {
    use cobalt_auth::provider::{FABRIC_API_RESOURCE, ONELAKE_RESOURCE};
    if let Ok(Some(ts)) = resolver.resource_token_silent(slot, ONELAKE_RESOURCE, tenant.as_deref()).await {
        return Ok(ts.access.token.expose().to_string());
    }
    if let Ok(Some(ts)) = resolver.resource_token_silent(slot, FABRIC_API_RESOURCE, tenant.as_deref()).await {
        if ts.access.scope.split(' ').any(|s| s.contains("OneLake")) {
            return Ok(ts.access.token.expose().to_string());
        }
    }
    match resolver.onelake_token(slot, tenant.as_deref(), hint.as_deref(), &prompter).await {
        Ok(ts) => Ok(ts.access.token.expose().to_string()),
        Err(e) => Err(format!("OneLake sign-in failed: {e}. Add the delegated permission Azure Storage → user_impersonation (or Power BI Service → OneLake.ReadWrite.All) to the app registration and sign in again.")),
    }
}

/// Start the export thread for a run-to-export and return the sink the session actor feeds.
fn spawn_run_export(state: &mut AppState, cx: &Ctx, tab: TabId, job: ExportJob) -> crate::session::RunSink {
    use crate::session::{RunSink, SinkMsg};
    let (tx, rx) = std::sync::mpsc::sync_channel::<SinkMsg>(8);
    let mut rx = rx;
    let progress_cell = Arc::new(parking_lot::Mutex::new((0usize, 0usize)));
    state.export_progress = Some(progress_cell.clone());
    let egui = cx.egui.clone();
    let done_tx = cx.export_tx.clone();
    // OneLake needs a token: fetch it on the session runtime while the query starts
    let token_rx = job.onelake.as_ref().map(|o| {
        let (ttx, trx) = tokio::sync::oneshot::channel::<Result<String, String>>();
        let prompter = UiPrompter { cancel: Arc::new(AtomicBool::new(false)), device: Arc::new(parking_lot::Mutex::new(None)), url: Arc::new(parking_lot::Mutex::new(None)), egui: egui.clone() };
        let resolver = cx.resolver.clone();
        let (slot, tenant, hint) = (o.slot, o.tenant.clone(), o.hint.clone());
        cx.session.spawn(async move {
            let _ = ttx.send(onelake_token(resolver, slot, tenant, hint, prompter).await);
        });
        trx
    });
    std::thread::Builder::new()
        .name("cobalt-run-export".into())
        .spawn(move || {
            // whatever happens (including a panic in a writer), the run must learn the outcome
            struct DoneGuard {
                tx: Sender<ExportDone>,
                tab: TabId,
                egui: egui::Context,
                result: Option<Result<String, String>>,
                path: Option<PathBuf>,
            }
            impl Drop for DoneGuard {
                fn drop(&mut self) {
                    let result = self.result.take().unwrap_or_else(|| Err("the export thread stopped unexpectedly".into()));
                    let _ = self.tx.send(ExportDone { result, tab: Some(self.tab), path: self.path.take() });
                    self.egui.request_repaint();
                }
            }
            let local_path = if job.onelake.is_none() { Some(job.local_path(0)) } else { None };
            let mut guard = DoneGuard { tx: done_tx, tab, egui: egui.clone(), result: None, path: local_path };
            let started = Instant::now();
            let token = match token_rx {
                Some(trx) => match trx.blocking_recv() {
                    Ok(Ok(t)) => Some(t),
                    Ok(Err(e)) => {
                        guard.result = Some(Err(e));
                        return;
                    }
                    Err(_) => {
                        guard.result = Some(Err("OneLake sign-in did not complete.".into()));
                        return;
                    }
                },
                None => None,
            };
            let egui2 = egui.clone();
            let pc = progress_cell.clone();
            let mut progress = move |p: cobalt_export::Progress| -> bool {
                *pc.lock() = (p.rows_done, p.rows_total);
                egui2.request_repaint();
                true
            };
            let mut lines: Vec<String> = Vec::new();
            let mut error: Option<String> = None;
            let mut sets = 0usize;
            loop {
                match rx.recv().ok() {
                    Some(SinkMsg::SetStart { index, columns, schema }) => {
                        sets += 1;
                        match run_export_set(&job, token.as_deref(), index, columns, schema, &mut rx, &mut progress) {
                            Ok(line) => lines.push(line),
                            Err(e) => {
                                error = Some(e);
                                break;
                            }
                        }
                    }
                    Some(SinkMsg::Failed(m)) => {
                        error = Some(m);
                        break;
                    }
                    Some(SinkMsg::RunEnd) | None => break,
                    Some(SinkMsg::Batch(_)) | Some(SinkMsg::SetEnd) => {}
                }
            }
            // dropping `rx` here tells the actor to stop streaming if it is still running
            drop(rx);
            let _ = sets;
            let result = match error {
                None if lines.is_empty() => Err("The query produced no result set to export.".into()),
                None => Ok(if lines.len() == 1 { lines.remove(0) } else { format!("{} ({:.1}s total)", lines.join("; "), started.elapsed().as_secs_f32()) }),
                Some(e) if e == "cancelled" && lines.is_empty() => Err("Export cancelled; the partial output was removed.".into()),
                Some(e) if e == "cancelled" => Err(format!("Export cancelled; the partial output was removed. Earlier result sets were written: {}", lines.join("; "))),
                Some(e) if lines.is_empty() => Err(e),
                Some(e) => Err(format!("{e} — earlier result sets were written: {}", lines.join("; "))),
            };
            guard.result = Some(result);
        })
        .ok();
    RunSink { tx, preview_rows: RUN_EXPORT_PREVIEW_ROWS }
}

/// Write one streamed result set to the job's target (blocking; called on the export thread).
fn run_export_set(
    job: &ExportJob,
    token: Option<&str>,
    index: usize,
    columns: Vec<ColumnInfo>,
    schema: arrow::datatypes::SchemaRef,
    rx: &mut std::sync::mpsc::Receiver<crate::session::SinkMsg>,
    progress: &mut (dyn FnMut(cobalt_export::Progress) -> bool + Send),
) -> Result<String, String> {
    use crate::session::SinkMsg;
    use cobalt_export::{Source, StreamItem, StreamSource};
    let started = Instant::now();
    let rx: &mut std::sync::mpsc::Receiver<SinkMsg> = rx; // a unique borrow moves into the closure (Receiver is Send, not Sync)
    let source = Source::Stream(StreamSource::new(columns, schema, move || match rx.recv().ok() {
        Some(SinkMsg::Batch(b)) => StreamItem::Batch(b),
        Some(SinkMsg::SetEnd) => StreamItem::End,
        Some(SinkMsg::Failed(m)) => StreamItem::Failed(m),
        Some(SinkMsg::RunEnd) | None => StreamItem::Failed("the query ended before the result set was complete".into()),
        Some(SinkMsg::SetStart { .. }) => StreamItem::Failed("unexpected result set boundary".into()),
    }));
    let ext = job.ext.as_str();
    let size = |b: u64| humansize::format_size(b, humansize::DECIMAL);
    match &job.onelake {
        None => {
            let path = job.local_path(index);
            if ext == "delta" {
                let opts = job.delta_options(None);
                cobalt_export_delta::write_delta_source_blocking(&source, &path, &opts, progress)
                    .map(|s| format!("Wrote {} rows to Delta table {} in {:.1}s", fmt_count(s.rows as u64), path.display(), started.elapsed().as_secs_f32()))
                    .map_err(|e| e.to_string())
            } else {
                let format = cobalt_export::Format::from_extension(ext).unwrap_or(cobalt_export::Format::Csv);
                let opts = cobalt_export::ExportOptions::from_settings(&job.settings);
                cobalt_export::export_source_to_file(&source, format, &path, &opts, &job.fmt, progress)
                    .map(|s| format!("Wrote {} rows ({}) to {} in {:.1}s", fmt_count(s.rows as u64), size(s.bytes), path.display(), started.elapsed().as_secs_f32()))
                    .map_err(|e| e.to_string())
            }
        }
        Some(o) => {
            let token = token.ok_or_else(|| "no OneLake token".to_string())?;
            let name = job.onelake_name(index);
            let relative = if ext == "delta" {
                if o.schema.is_empty() { format!("Tables/{name}") } else { format!("Tables/{}/{name}", o.schema) }
            } else {
                format!("Files/{name}")
            };
            let shown = if ext == "delta" && !o.schema.is_empty() { format!("{}.{name}", o.schema) } else { name.clone() };
            let target = cobalt_export_delta::RemoteTarget::onelake(&o.item.workspace_id, &o.item.id, &relative, token).map_err(|e| e.to_string())?;
            let lh_name = o.item.display_name.clone();
            if ext == "delta" {
                let opts = job.delta_options(Some(name.clone()));
                cobalt_export_delta::write_delta_remote_source_blocking(&source, &target, &opts, progress)
                    .map(|s| format!("Wrote {} rows to {lh_name}/Tables/{shown} in {:.1}s", fmt_count(s.rows as u64), started.elapsed().as_secs_f32()))
                    .map_err(|e| e.to_string())
            } else {
                // stream to a local temp file, then upload it as one object
                let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
                let path = std::env::temp_dir().join(format!("cobalt-onelake-{}-{nanos}.{ext}", std::process::id()));
                let format = cobalt_export::Format::from_extension(ext).unwrap_or(cobalt_export::Format::Csv);
                let opts = cobalt_export::ExportOptions::from_settings(&job.settings);
                let r = cobalt_export::export_source_to_file(&source, format, &path, &opts, &job.fmt, progress).map_err(|e| e.to_string()).and_then(|s| {
                    cobalt_export_delta::upload_file_blocking(&target, &path).map(|bytes| (s.rows, bytes)).map_err(|e| e.to_string())
                });
                let _ = std::fs::remove_file(&path);
                r.map(|(rows, bytes)| format!("Wrote {} rows ({}) to {lh_name}/Files/{name} in {:.1}s", fmt_count(rows as u64), size(bytes), started.elapsed().as_secs_f32()))
            }
        }
    }
}

pub fn start_export(state: &mut AppState, cx: &Ctx) {
    let Dialog::Export(d) = &mut state.dialog else { return };
    let Some(t) = state.tabs.get(d.tab_index) else { return };
    let Some(v) = t.run.as_ref().and_then(|r| r.result_sets.get(d.set_index)) else { return };
    let rs = v.rs.clone();
    let selection = if d.selection_only { Some(v.grid.selection.clone()) } else { None };
    let (_, ext) = FORMAT_LABELS[d.format_index];
    let onelake = if d.destination == 1 {
        let Some(item_id) = d.onelake_item.clone() else {
            d.result = Some(Err("Choose a lakehouse.".into()));
            return;
        };
        let name = d.onelake_name.trim().trim_matches('/').to_string();
        if name.is_empty() || name.contains(['\\', ':']) {
            d.result = Some(Err(if ext == "delta" { "Give the table a name." } else { "Give the file a name." }.into()));
            return;
        }
        let Some(item) = state.fabric.item(&item_id).cloned() else {
            d.result = Some(Err("That lakehouse is no longer listed; refresh the Fabric panel.".into()));
            return;
        };
        let Some(slot) = state.fabric.slot else {
            d.result = Some(Err("Sign in to Fabric first (Fabric panel).".into()));
            return;
        };
        let _ = cx.store.fabric_cache_put(LAST_LAKEHOUSE_KEY, &item.id);
        // default to the lakehouse's schema when it is schema-enabled and none was typed
        let mut schema = d.onelake_schema.trim().trim_matches('/').to_string();
        if schema.is_empty() {
            if let Some(default) = state.fabric.details.get(&item_id).and_then(|d| d.get()).and_then(|t| t.default_schema.clone()) {
                schema = default;
            }
        }
        Some((item, name, slot, schema))
    } else {
        None
    };
    let path = if onelake.is_some() {
        // scratch file for non-Delta formats; Delta writes straight to OneLake
        std::env::temp_dir().join(format!("cobalt-onelake-{}.{}", uuid::Uuid::new_v4(), if ext == "delta" { "delta" } else { &ext }))
    } else {
        PathBuf::from(d.path.trim())
    };
    if path.as_os_str().is_empty() {
        d.result = Some(Err("Choose a file path.".into()));
        return;
    }
    state.settings_patch.push(SettingsPatch::LastExportFormat(ext.to_string()));
    d.running = true;
    d.result = None;
    d.progress = Some((0, rs.visible_count()));
    let cancel = d.cancel.clone();
    cancel.store(false, Ordering::Relaxed);
    let mut settings = cx.settings.export.clone();
    settings.csv_delimiter = d.csv_delimiter.clone();
    settings.csv_include_headers = d.csv_headers;
    settings.json_lines = d.json_lines;
    let delta_mode = d.delta_mode;
    let delta_partition: Vec<String> = d.delta_partition.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    let fmt = state.formatter.clone().with_max_chars(0);
    let tx = cx.export_tx.clone();
    let egui = cx.egui.clone();
    let ext = ext.to_string();
    let progress_cell = Arc::new(parking_lot::Mutex::new((0usize, rs.visible_count())));
    let pc = progress_cell.clone();
    state.export_progress = Some(progress_cell);
    // For a selection, materialize the selected rows/cols into a temporary ResultSet.
    let rs = match selection {
        Some(sel) if sel != Selection::None && sel != Selection::All => match subset(&rs, &sel) {
            Ok(r) => r,
            Err(e) => {
                d.running = false;
                d.result = Some(Err(e));
                return;
            }
        },
        _ => rs,
    };
    if let Some((item, name, slot, schema)) = onelake {
        // OneLake: get a storage token on the session runtime, then write from a blocking thread.
        let resolver = cx.resolver.clone();
        let tenant = cx.settings.connections.entra_default_tenant.clone();
        let hint = state.fabric.account.as_ref().map(|a| a.username.clone());
        let egui2 = egui.clone();
        let tx2 = tx.clone();
        let (ws_id, lh_id, lh_name) = (item.workspace_id.clone(), item.id.clone(), item.display_name.clone());
        let fmt2 = fmt.clone();
        let settings2 = settings.clone();
        let ext2 = ext.clone();
        let rs2 = rs.clone();
        let pc2 = pc.clone();
        let cancel2 = cancel.clone();
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let prompter = UiPrompter { cancel: cancel_flag, device: Arc::new(parking_lot::Mutex::new(None)), url: Arc::new(parking_lot::Mutex::new(None)), egui: egui.clone() };
        cx.session.spawn(async move {
            // OneLake accepts an Azure Storage token; when the registration lacks that permission
            // the Fabric API token (with OneLake.ReadWrite.All) works too. Silent first, then the
            // browser as a last resort.
            use cobalt_auth::provider::{FABRIC_API_RESOURCE, ONELAKE_RESOURCE};
            let mut token: Option<String> = None;
            if let Ok(Some(ts)) = resolver.resource_token_silent(slot, ONELAKE_RESOURCE, tenant.as_deref()).await {
                token = Some(ts.access.token.expose().to_string());
            } else if let Ok(Some(ts)) = resolver.resource_token_silent(slot, FABRIC_API_RESOURCE, tenant.as_deref()).await {
                if ts.access.scope.split(' ').any(|s| s.contains("OneLake")) {
                    token = Some(ts.access.token.expose().to_string());
                }
            }
            let token = match token {
                Some(t) => t,
                None => match resolver.onelake_token(slot, tenant.as_deref(), hint.as_deref(), &prompter).await {
                    Ok(ts) => ts.access.token.expose().to_string(),
                    Err(e) => {
                        let _ = tx2.send(ExportDone { result: Err(format!("OneLake sign-in failed: {e}. Add the delegated permission Azure Storage → user_impersonation (or Power BI Service → OneLake.ReadWrite.All) to the app registration and sign in again.")), tab: None, path: None });
                        egui2.request_repaint();
                        return;
                    }
                },
            };
            // schema-enabled lakehouses keep Delta tables under Tables/<schema>/<table>
            let relative = if ext2 == "delta" {
                if schema.is_empty() { format!("Tables/{name}") } else { format!("Tables/{schema}/{name}") }
            } else {
                format!("Files/{name}")
            };
            let shown = if ext2 == "delta" && !schema.is_empty() { format!("{schema}.{name}") } else { name.clone() };
            let target = match cobalt_export_delta::RemoteTarget::onelake(&ws_id, &lh_id, &relative, &token) {
                Ok(t) => t,
                Err(e) => {
                    let _ = tx2.send(ExportDone { result: Err(e.to_string()), tab: None, path: None });
                    egui2.request_repaint();
                    return;
                }
            };
            let egui3 = egui2.clone();
            let result = tokio::task::spawn_blocking(move || {
                let started = Instant::now();
                let mut progress = |p: cobalt_export::Progress| -> bool {
                    *pc2.lock() = (p.rows_done, p.rows_total);
                    egui3.request_repaint();
                    !cancel2.load(Ordering::Relaxed)
                };
                if ext2 == "delta" {
                    let mode = match delta_mode {
                        1 => cobalt_export_delta::DeltaMode::Overwrite,
                        2 => cobalt_export_delta::DeltaMode::Append,
                        _ => cobalt_export_delta::DeltaMode::Create,
                    };
                    let opts = cobalt_export_delta::DeltaOptions { mode, partition_columns: delta_partition, table_name: Some(name.clone()), description: None };
                    cobalt_export_delta::write_delta_remote_blocking(&rs2, &target, &opts, &mut progress)
                        .map(|s| format!("Wrote {} rows to {lh_name}/Tables/{shown} in {:.1}s", fmt_count(s.rows as u64), started.elapsed().as_secs_f32()))
                        .map_err(|e| e.to_string())
                } else {
                    let format = cobalt_export::Format::from_extension(&ext2).unwrap_or(cobalt_export::Format::Csv);
                    let opts = cobalt_export::ExportOptions::from_settings(&settings2);
                    let r = cobalt_export::export_to_file(&rs2, format, &path, &opts, &fmt2, &mut progress).map_err(|e| e.to_string()).and_then(|s| {
                        cobalt_export_delta::upload_file_blocking(&target, &path).map(|bytes| (s.rows, bytes)).map_err(|e| e.to_string())
                    });
                    let _ = std::fs::remove_file(&path);
                    r.map(|(rows, bytes)| format!("Wrote {} rows ({}) to {lh_name}/Files/{name} in {:.1}s", fmt_count(rows as u64), humansize::format_size(bytes, humansize::DECIMAL), started.elapsed().as_secs_f32()))
                }
            })
            .await
            .unwrap_or_else(|e| Err(format!("export thread failed: {e}")));
            let _ = tx2.send(ExportDone { result, tab: None, path: None });
            egui2.request_repaint();
        });
        return;
    }
    std::thread::Builder::new()
        .name("cobalt-export".into())
        .spawn(move || {
            let started = Instant::now();
            let mut progress = |p: cobalt_export::Progress| -> bool {
                *pc.lock() = (p.rows_done, p.rows_total);
                egui.request_repaint();
                !cancel.load(Ordering::Relaxed)
            };
            let result: Result<String, String> = if ext == "delta" {
                let mode = match delta_mode {
                    1 => cobalt_export_delta::DeltaMode::Overwrite,
                    2 => cobalt_export_delta::DeltaMode::Append,
                    _ => cobalt_export_delta::DeltaMode::Create,
                };
                let opts = cobalt_export_delta::DeltaOptions { mode, partition_columns: delta_partition, table_name: None, description: None };
                cobalt_export_delta::write_delta_blocking(&rs, &path, &opts, &mut progress).map(|s| format!("Wrote {} rows to Delta table {} in {:.1}s", fmt_count(s.rows as u64), path.display(), started.elapsed().as_secs_f32())).map_err(|e| e.to_string())
            } else {
                let format = cobalt_export::Format::from_extension(&ext).unwrap_or(cobalt_export::Format::Csv);
                let opts = cobalt_export::ExportOptions::from_settings(&settings);
                cobalt_export::export_to_file(&rs, format, &path, &opts, &fmt, &mut progress).map(|s| format!("Wrote {} rows ({}) to {} in {:.1}s", fmt_count(s.rows as u64), humansize::format_size(s.bytes, humansize::DECIMAL), path.display(), started.elapsed().as_secs_f32())).map_err(|e| e.to_string())
            };
            let _ = tx.send(ExportDone { result, tab: None, path: None });
            egui.request_repaint();
        })
        .ok();
}

fn subset(rs: &Arc<cobalt_results::ResultSet>, sel: &Selection) -> Result<Arc<cobalt_results::ResultSet>, String> {
    let (rows, cols) = sel.resolve(rs.visible_count(), rs.column_count()).ok_or("empty selection")?;
    let batch = rs.view_to_single_batch().map_err(|e| e.to_string())?;
    let row_idx = arrow::array::UInt32Array::from((*rows.start()..=*rows.end()).map(|r| r as u32).collect::<Vec<_>>());
    let col_idx: Vec<usize> = cols.collect();
    let columns: Vec<cobalt_core::ColumnInfo> = col_idx.iter().enumerate().map(|(i, &c)| { let mut ci = rs.columns[c].clone(); ci.ordinal = i; ci }).collect();
    let arrays = col_idx.iter().map(|&c| arrow::compute::take(batch.column(c), &row_idx, None)).collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?;
    let schema = Arc::new(arrow::datatypes::Schema::new(col_idx.iter().map(|&c| rs.schema.field(c).clone()).collect::<Vec<_>>()));
    let b = arrow::array::RecordBatch::try_new(schema, arrays).map_err(|e| e.to_string())?;
    Ok(cobalt_results::ResultSet::from_batches(0, columns, vec![b]))
}

pub fn on_export_done(state: &mut AppState, cx: &Ctx, done: ExportDone) {
    state.export_progress = None;
    if let Some(tab) = done.tab {
        // run-to-export: the outcome belongs to the run, not to a dialog
        if let Some(r) = state.tab_mut(tab).and_then(|t| t.run.as_mut()) {
            let (text, is_error) = match &done.result {
                Ok(m) => (m.clone(), false),
                Err(e) => (format!("Export failed: {e}"), true),
            };
            let path = if is_error { None } else { done.path.clone() };
            r.messages.push(MessageLine { text, is_error, is_batch_header: false, line: None, at: Instant::now(), path });
        }
        match done.result {
            Ok(m) => cx.toast(ToastKind::Success, m),
            Err(e) => cx.toast(ToastKind::Error, e),
        }
        return;
    }
    if let Dialog::Export(d) = &mut state.dialog {
        d.running = false;
        d.progress = None;
        d.result = Some(done.result.clone());
        if let Ok(msg) = &done.result {
            cx.toast(ToastKind::Success, msg.clone());
            if let Some(parent) = std::path::Path::new(d.path.trim()).parent() {
                state.settings_patch.push(SettingsPatch::LastExportDir(parent.to_string_lossy().to_string()));
            }
        }
    } else {
        match done.result {
            Ok(m) => cx.toast(ToastKind::Success, m),
            Err(e) => cx.toast(ToastKind::Error, e),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// History
// ---------------------------------------------------------------------------------------------

pub fn refresh_history(state: &mut AppState, cx: &Ctx) {
    let q = HistoryQuery { text: if state.history.query.trim().is_empty() { None } else { Some(state.history.query.trim().to_string()) }, starred_only: state.history.starred_only, limit: 300, ..Default::default() };
    match cx.store.search_history(&q) {
        Ok(rows) => {
            state.history.entries = rows
                .into_iter()
                .map(|e| HistoryRow {
                    id: e.id,
                    sql: e.sql,
                    server: e.server,
                    database: e.database,
                    started: e.started_at,
                    duration_ms: e.duration_ms.map(|d| d as i64),
                    rows: e.rows.map(|r| r as i64),
                    status: e.status.as_str().to_string(),
                    error: e.error,
                    starred: e.starred,
                    profile_id: e.profile_id,
                })
                .collect();
            state.history.loaded = true;
        }
        Err(e) => cx.toast(ToastKind::Error, format!("History: {e}")),
    }
}

pub fn import_ads(state: &mut AppState, cx: &Ctx, path: &str) -> Result<String, String> {
    let json = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let import = cobalt_store::parse_ads_settings_detailed(&json).map_err(|e| e.to_string())?;
    let summary = cx.store.import_library(&import.library, true).map_err(|e| e.to_string())?;
    load_library(state, cx);
    Ok(format!("Imported {} group{} and {} connection{}{}", summary.groups, if summary.groups == 1 { "" } else { "s" }, summary.profiles, if summary.profiles == 1 { "" } else { "s" }, if import.skipped.is_empty() { String::new() } else { format!("; skipped {} non-SQL Server connection(s)", import.skipped.len()) }))
}

/// Write every group and connection (never passwords or secrets) to a JSON file.
pub fn export_connections(state: &mut AppState, cx: &Ctx, path: &std::path::Path) {
    let _ = state;
    let result = cx.store.export_library().and_then(|l| l.to_json().map(|j| (l.profiles.len(), j))).map_err(|e| e.to_string()).and_then(|(n, json)| std::fs::write(path, json).map(|_| n).map_err(|e| e.to_string()));
    match result {
        Ok(n) => cx.toast(ToastKind::Success, format!("Exported {n} connection{} to {} (passwords are never included).", if n == 1 { "" } else { "s" }, path.display())),
        Err(e) => cx.toast(ToastKind::Error, format!("Export failed: {e}")),
    }
}

/// Merge groups and connections from a JSON file written by Export Connections.
pub fn import_connections(state: &mut AppState, cx: &Ctx, path: &std::path::Path) {
    let result = std::fs::read_to_string(path)
        .map_err(|e| e.to_string())
        .and_then(|json| cobalt_store::LibraryExport::from_json(&json).map_err(|e| e.to_string()))
        .and_then(|lib| cx.store.import_library(&lib, true).map_err(|e| e.to_string()));
    match result {
        Ok(summary) => {
            load_library(state, cx);
            cx.toast(ToastKind::Success, format!("Imported {} group{} and {} connection{}. Passwords are not carried over; you will be asked on first connect.", summary.groups, if summary.groups == 1 { "" } else { "s" }, summary.profiles, if summary.profiles == 1 { "" } else { "s" }));
        }
        Err(e) => cx.toast(ToastKind::Error, format!("Import failed: {e}")),
    }
}

pub fn maintenance(state: &mut AppState, cx: &Ctx) {
    let _ = cx.store.prune_history(cx.settings.history.retention_days, cx.settings.history.max_entries);
    let _ = cx.store.prune_closed_tabs(50);
    let _ = state;
}

/// Insert the value of a script action into the editor of `idx`.
pub fn script_kind_label(k: ScriptKind) -> &'static str {
    match k {
        ScriptKind::Create => "CREATE",
        ScriptKind::Alter => "ALTER",
        ScriptKind::Drop => "DROP",
        ScriptKind::Select => "SELECT",
        ScriptKind::Execute => "EXECUTE",
    }
}

#[cfg(test)]
mod launch_tests {
    use super::LaunchArgs;

    fn parse(s: &str) -> Option<LaunchArgs> {
        LaunchArgs::parse(s.split_whitespace().map(str::to_string))
    }

    #[test]
    fn nothing_asked() {
        assert!(parse("").is_none());
    }

    #[test]
    fn files_and_connection() {
        let l = parse("a.sql b.sqlplan -S local -d cobalt_test").unwrap();
        assert_eq!(l.files.len(), 2);
        assert_eq!(l.server.as_deref(), Some("local"));
        assert_eq!(l.database.as_deref(), Some("cobalt_test"));
    }

    #[test]
    fn sqlcmd_style_glued_flags_and_file_urls() {
        let l = parse("-Smyserver -Dmydb file:///tmp/x.sql --bogus").unwrap();
        assert_eq!(l.server.as_deref(), Some("myserver"));
        assert_eq!(l.database.as_deref(), Some("mydb"));
        assert_eq!(l.files[0].to_string_lossy(), "/tmp/x.sql");
    }
}
