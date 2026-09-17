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
}

struct UiPrompter {
    cancel: Arc<AtomicBool>,
    device: Arc<parking_lot::Mutex<Option<(String, String)>>>,
    egui: egui::Context,
}

impl Prompter for UiPrompter {
    fn password(&self, _profile: &ConnectionProfile) -> cobalt_auth::Result<Option<Secret>> {
        Ok(None)
    }
    fn open_browser(&self, url: &str) {
        cobalt_auth::entra::open_in_browser(url);
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
            if matches!(profile.auth, AuthMethod::EntraInteractive { .. } | AuthMethod::EntraDeviceCode { .. }) && cx.settings.connections.entra_client_id.trim().is_empty() {
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
            let message = match &profile.auth {
                AuthMethod::EntraInteractive { .. } => "Complete the sign-in in your browser…".to_string(),
                AuthMethod::EntraDeviceCode { .. } => "Requesting a device code…".to_string(),
                AuthMethod::AzureCli { .. } => "Asking Azure CLI for a token…".to_string(),
                _ => "Acquiring token…".to_string(),
            };
            state.dialog = Dialog::AuthWaiting { profile: profile.clone(), purpose: purpose.clone(), message, device: device.clone(), cancel: cancel.clone() };
            let resolver = cx.resolver.clone();
            let tx = cx.auth_tx.clone();
            let egui = cx.egui.clone();
            let prompter = UiPrompter { cancel, device, egui: egui.clone() };
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
    opts.plan = match mode {
        RunMode::EstimatedPlan => PlanMode::Estimated,
        _ if t.actual_plan => PlanMode::Actual,
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

pub fn execute(state: &mut AppState, cx: &Ctx, idx: usize, script: String, opts: ExecOptions, start_line: u32) {
    let Some(t) = state.tabs.get_mut(idx) else { return };
    let run_id = cx.session.new_run();
    let mut view = RunView::new(run_id, opts.plan);
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
    cx.session.send(Command::Run { tab: t.id, run: run_id, script, opts, start_line });
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
            }
        }
        cx.session.send(Command::FetchMore { tab: t.id, rows });
    }
}

pub fn change_database(state: &mut AppState, cx: &Ctx, idx: usize, db: String) {
    if let Some(t) = state.tabs.get(idx) {
        if t.conn.is_connected() {
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
        }
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
    let path = std::path::Path::new(&dir).join(format!("{base}.csv")).to_string_lossy().to_string();
    state.dialog = Dialog::Export(Box::new(ExportDialog {
        tab_index: idx,
        set_index: set,
        format_index: 0,
        path,
        selection_only,
        delta_mode: 0,
        delta_partition: String::new(),
        csv_delimiter: cx.settings.export.csv_delimiter.clone(),
        csv_headers: cx.settings.export.csv_include_headers,
        json_lines: cx.settings.export.json_lines,
        running: false,
        progress: None,
        result: None,
        cancel: Arc::new(AtomicBool::new(false)),
    }));
}

pub fn start_export(state: &mut AppState, cx: &Ctx) {
    let Dialog::Export(d) = &mut state.dialog else { return };
    let Some(t) = state.tabs.get(d.tab_index) else { return };
    let Some(v) = t.run.as_ref().and_then(|r| r.result_sets.get(d.set_index)) else { return };
    let rs = v.rs.clone();
    let selection = if d.selection_only { Some(v.grid.selection.clone()) } else { None };
    let (_, ext) = FORMAT_LABELS[d.format_index];
    let path = PathBuf::from(d.path.trim());
    if path.as_os_str().is_empty() {
        d.result = Some(Err("Choose a file path.".into()));
        return;
    }
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
            let _ = tx.send(ExportDone { result });
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
