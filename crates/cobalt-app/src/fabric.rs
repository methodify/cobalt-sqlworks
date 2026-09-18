//! Fabric explorer: state, async loading against the Fabric REST API, and the operations the
//! panel can trigger (open, pin, save to Servers). The REST client lives in `cobalt-fabric`;
//! tokens come from `cobalt-auth` on the account behind one Entra profile (the "account slot").

use crate::ops::{self, Ctx, UiPrompter};
use crate::state::{AppState, ConnectPurpose, Dialog, Loadable, SidebarView, ToastKind};
use cobalt_auth::provider::FABRIC_API_RESOURCE;
use cobalt_auth::EntraAccount;
use cobalt_core::*;
use cobalt_fabric::{Capacity, FabricClient, FabricError, SqlItem, SqlItemKind, SqlTarget, Workspace};
use crate::ui::servers::TreeAction;
use cobalt_store::FabricPin;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;

const KV_ACCOUNT_SLOT: &str = "fabric.account_profile";
const CACHE_WORKSPACES: &str = "fabric:workspaces";
/// Workspaces/items older than this are refreshed in the background when the panel opens.
const STALE_SECS: i64 = 15 * 60;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FabricStatus {
    SignedOut,
    SigningIn,
    Ready,
    Error(String),
}

#[derive(Default)]
pub struct FabricState {
    pub status: Option<FabricStatus>,
    pub account: Option<EntraAccount>,
    /// The Entra profile whose refresh token serves the Fabric API.
    pub slot: Option<ProfileId>,
    pub workspaces: Loadable<Vec<Workspace>>,
    pub items: HashMap<String, Loadable<Vec<SqlItem>>>,
    pub details: HashMap<String, Loadable<SqlTarget>>,
    pub expanded: HashSet<String>,
    pub pins: Vec<FabricPin>,
    pub recent: Vec<FabricPin>,
    pub capacities: HashMap<String, Capacity>,
    /// Items whose inline object explorer is open.
    pub expanded_items: HashSet<String>,
    /// Item ids waiting for their detail before the explorer opens.
    pub expand_after_detail: HashSet<String>,
    /// SQL-endpoint items waiting for a sibling's detail to learn the workspace host.
    pub endpoint_after_detail: HashMap<String, Vec<String>>,
    pub search: String,
    pub last_refresh: Option<Instant>,
    pub loaded_once: bool,
    /// Item ids for which "open a tab" is queued until the detail arrives.
    pub open_after_detail: HashSet<String>,
    /// Item ids for which "save to servers" is queued until the detail arrives.
    pub save_after_detail: HashSet<String>,
}

impl FabricState {
    pub fn status(&self) -> FabricStatus {
        self.status.clone().unwrap_or(FabricStatus::SignedOut)
    }
    pub fn workspace(&self, id: &str) -> Option<&Workspace> {
        self.workspaces.get().and_then(|w| w.iter().find(|w| w.id == id))
    }
    pub fn item(&self, id: &str) -> Option<&SqlItem> {
        self.items.values().filter_map(|l| l.get()).flatten().find(|i| i.id == id)
    }
    pub fn is_pinned(&self, item_id: &str) -> bool {
        self.pins.iter().any(|p| p.item_id == item_id)
    }
    /// The SQL analytics endpoint item that belongs to a SQL database (same name, same workspace).
    pub fn endpoint_child(&self, db: &SqlItem) -> Option<&SqlItem> {
        self.items.get(&db.workspace_id).and_then(|l| l.get()).and_then(|v| v.iter().find(|i| i.kind == SqlItemKind::SqlEndpoint && i.display_name == db.display_name))
    }
    /// The datawarehouse host shared by a workspace's SQL endpoints, from any loaded detail.
    pub fn workspace_endpoint_host(&self, workspace_id: &str) -> Option<String> {
        self.items.get(workspace_id).and_then(|l| l.get()).and_then(|v| {
            v.iter().filter(|i| matches!(i.kind, SqlItemKind::Lakehouse | SqlItemKind::Warehouse | SqlItemKind::MirroredDatabase | SqlItemKind::MirroredWarehouse)).find_map(|i| self.details.get(&i.id).and_then(|d| d.get()).map(|t| t.server.clone()))
        })
    }
    /// Profile id of an item's inline explorer, when it has been created.
    pub fn explorer_profile(&self, item_id: &str) -> ProfileId {
        ephemeral_id(item_id)
    }
}

/// Results from the background tasks.
pub enum FabricEvent {
    SignedIn { slot: ProfileId, account: EntraAccount },
    SignInFailed(String),
    NeedSignIn,
    Workspaces(Result<Vec<Workspace>, String>),
    Account(EntraAccount),
    Items { workspace_id: String, result: Result<Vec<SqlItem>, String> },
    Detail { item_id: String, result: Result<SqlTarget, String> },
    Capacities(Vec<Capacity>),
}

/// What the panel asks for.
#[derive(Clone, Debug)]
pub enum FabricAction {
    SignIn,
    SignOut,
    Refresh,
    ToggleWorkspace(String),
    Open { item_id: String },
    TogglePin { item_id: String },
    SaveToServers { item_id: String },
    CopyConnectionString { item_id: String },
    OpenInPortal { item_id: String },
    Search(String),
    /// Toggle the inline object explorer under an item.
    ToggleItem { item_id: String },
    /// Open the export dialog for the active tab's results with this lakehouse preselected.
    ExportHere { item_id: String },
    /// An action from the inline object explorer.
    Tree(TreeAction),
}

fn fabric_error_text(e: &FabricError) -> String {
    match e {
        FabricError::Unauthorized(_) => "Fabric rejected the token; sign in again.".into(),
        other => other.to_string(),
    }
}

fn tenant_hint(cx: &Ctx) -> Option<String> {
    cx.settings.connections.entra_default_tenant.clone()
}

// ---------------------------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------------------------

/// Called when the panel becomes visible: restore pins and cache, then refresh if stale.
pub fn on_panel_shown(state: &mut AppState, cx: &Ctx) {
    if state.fabric.loaded_once {
        return;
    }
    state.fabric.loaded_once = true;
    state.fabric.pins = cx.store.fabric_pins().unwrap_or_default();
    state.fabric.recent = cx.store.fabric_recent(6).unwrap_or_default();
    state.fabric.slot = cx.store.get_kv::<String>(KV_ACCOUNT_SLOT).ok().flatten().and_then(|s| ProfileId::parse(&s));
    let mut stale = true;
    if let Ok(Some((json, fetched))) = cx.store.fabric_cache_get(CACHE_WORKSPACES) {
        if let Ok(ws) = serde_json::from_str::<Vec<Workspace>>(&json) {
            state.fabric.workspaces = Loadable::Loaded(ws);
            stale = (chrono::Utc::now() - fetched).num_seconds() > STALE_SECS;
        }
    }
    if state.fabric.slot.is_some() {
        state.fabric.status = Some(FabricStatus::Ready);
        if stale || !matches!(state.fabric.workspaces, Loadable::Loaded(_)) {
            load_workspaces(state, cx);
        }
        load_items_for_pins(state, cx);
    } else {
        // no account yet: try any Entra profile silently before asking the user
        state.fabric.status = Some(FabricStatus::SignedOut);
        try_silent_adopt(state, cx);
    }
}

/// Load the items of every workspace holding a pinned or recent item, so those rows resolve (and
/// open) without the user expanding their workspace first. No-op for workspaces already loading.
fn load_items_for_pins(state: &mut AppState, cx: &Ctx) {
    let mut ids: Vec<String> = Vec::new();
    for p in state.fabric.pins.iter().chain(state.fabric.recent.iter()) {
        let known = state.fabric.workspace(&p.workspace_id).is_some();
        let needs = state.fabric.items.get(&p.workspace_id).map(|l| l.needs_load()).unwrap_or(true);
        if known && needs && !ids.contains(&p.workspace_id) {
            ids.push(p.workspace_id.clone());
        }
    }
    for id in ids {
        load_items(state, cx, &id);
    }
}

/// Make sure every workspace's items are loaded (the export dialog lists lakehouses from them).
pub fn ensure_lakehouses_loaded(state: &mut AppState, cx: &Ctx) {
    on_panel_shown(state, cx);
    let ws: Vec<String> = state.fabric.workspaces.get().map(|w| w.iter().map(|w| w.id.clone()).collect()).unwrap_or_default();
    for id in ws {
        if state.fabric.items.get(&id).map(|l| l.needs_load()).unwrap_or(true) {
            load_items(state, cx, &id);
        }
    }
}

pub fn action(state: &mut AppState, cx: &Ctx, a: FabricAction) {
    match a {
        FabricAction::SignIn => sign_in(state, cx),
        FabricAction::SignOut => sign_out(state, cx),
        FabricAction::Refresh => {
            state.fabric.items.clear();
            state.fabric.details.clear();
            load_workspaces(state, cx);
        }
        FabricAction::ToggleWorkspace(id) => {
            if !state.fabric.expanded.remove(&id) {
                state.fabric.expanded.insert(id.clone());
                if state.fabric.items.get(&id).map(|l| l.needs_load()).unwrap_or(true) {
                    load_items(state, cx, &id);
                }
            }
        }
        FabricAction::Open { item_id } => {
            match state.fabric.details.get(&item_id).and_then(|d| d.get()).cloned() {
                Some(target) => open_item(state, cx, &item_id, target),
                None => {
                    state.fabric.open_after_detail.insert(item_id.clone());
                    load_detail(state, cx, &item_id);
                }
            }
        }
        FabricAction::TogglePin { item_id } => toggle_pin(state, cx, &item_id),
        FabricAction::SaveToServers { item_id } => match state.fabric.details.get(&item_id).and_then(|d| d.get()).cloned() {
            Some(target) => save_to_servers(state, cx, &item_id, target),
            None => {
                state.fabric.save_after_detail.insert(item_id.clone());
                load_detail(state, cx, &item_id);
            }
        },
        FabricAction::CopyConnectionString { item_id } => match state.fabric.details.get(&item_id).and_then(|d| d.get()) {
            Some(t) => {
                cx.egui.copy_text(format!("Server={};Database={};Authentication=Active Directory Interactive;Encrypt=True", t.server, t.database));
                cx.toast(ToastKind::Success, "Connection string copied.");
            }
            None => {
                load_detail(state, cx, &item_id);
                cx.toast(ToastKind::Info, "Fetching connection details… try again in a moment.");
            }
        },
        FabricAction::OpenInPortal { item_id } => {
            if let Some(item) = state.fabric.item(&item_id).cloned() {
                let seg = match item.kind {
                    SqlItemKind::Warehouse => "warehouses",
                    SqlItemKind::Lakehouse => "lakehouses",
                    SqlItemKind::SqlDatabase => "sqldatabases",
                    SqlItemKind::MirroredDatabase => "mirroreddatabases",
                    SqlItemKind::MirroredWarehouse => "mirroredwarehouses",
                    _ => "items",
                };
                cobalt_auth::entra::open_in_browser(&format!("https://app.fabric.microsoft.com/groups/{}/{}/{}", item.workspace_id, seg, item.id));
            }
        }
        FabricAction::Search(s) => state.fabric.search = s,
        FabricAction::ToggleItem { item_id } => {
            if state.fabric.expanded_items.remove(&item_id) {
                return;
            }
            state.fabric.expanded_items.insert(item_id.clone());
            match state.fabric.details.get(&item_id).and_then(|d| d.get()).cloned() {
                Some(target) => ensure_item_explorer(state, cx, &item_id, target),
                None => {
                    state.fabric.expand_after_detail.insert(item_id.clone());
                    load_detail(state, cx, &item_id);
                }
            }
        }
        FabricAction::ExportHere { item_id } => export_here(state, cx, &item_id),
        FabricAction::Tree(a) => ops::tree_action(state, cx, a),
    }
}

/// Make the inline explorer's profile exist and connected (metadata connection), like expanding a
/// server in the Servers tree.
fn ensure_item_explorer(state: &mut AppState, cx: &Ctx, item_id: &str, target: SqlTarget) {
    if !target.is_ready() {
        cx.toast(ToastKind::Warning, "The SQL endpoint is still provisioning.");
        state.fabric.expanded_items.remove(item_id);
        return;
    }
    let Some(profile) = ephemeral_profile(state, cx, item_id, &target) else { return };
    let id = profile.id;
    if state.library.profile(id).is_none() {
        state.library.ephemeral.push(profile);
    }
    let node = state.library.server(id);
    node.expanded = true;
    let connected = node.creds.is_some();
    let needs = node.databases.needs_load();
    if !connected {
        ops::tree_action(state, cx, TreeAction::ConnectServer(id));
    } else if needs {
        ops::tree_action(state, cx, TreeAction::RefreshServer(id));
    }
}

fn export_here(state: &mut AppState, cx: &Ctx, item_id: &str) {
    let Some(idx) = state.active_tab else {
        cx.toast(ToastKind::Warning, "Run a query first, then export its results here.");
        return;
    };
    let has_results = state.tabs[idx].run.as_ref().map(|r| r.result_sets.iter().any(|s| !s.is_plan)).unwrap_or(false);
    if !has_results {
        cx.toast(ToastKind::Warning, "The active tab has no results to export yet.");
        return;
    }
    load_detail(state, cx, item_id);
    ops::open_export_dialog(state, cx, idx, 0, false);
    if let Dialog::Export(d) = &mut state.dialog {
        d.destination = 1;
        d.onelake_item = Some(item_id.to_string());
        d.onelake_schema.clear();
        if let Some(i) = ops::FORMAT_LABELS.iter().position(|(_, e)| *e == "delta") {
            d.format_index = i;
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Sign-in
// ---------------------------------------------------------------------------------------------

/// Without a slot yet: see whether any Entra profile's refresh token already carries Fabric
/// consent. Silent; the first that works becomes the slot.
fn try_silent_adopt(state: &mut AppState, cx: &Ctx) {
    let candidates: Vec<ProfileId> = state
        .library
        .profiles
        .iter()
        .filter(|p| matches!(p.auth, AuthMethod::EntraInteractive { .. } | AuthMethod::EntraDeviceCode { .. }))
        .filter(|p| cx.resolver.is_remembered(&p.id))
        .map(|p| p.id)
        .collect();
    if candidates.is_empty() {
        return;
    }
    let resolver = cx.resolver.clone();
    let tx = cx.fabric_tx.clone();
    let egui = cx.egui.clone();
    let tenant = tenant_hint(cx);
    cx.session.spawn(async move {
        for id in candidates {
            if let Ok(Some(ts)) = resolver.resource_token_silent(id, FABRIC_API_RESOURCE, tenant.as_deref()).await {
                let _ = tx.send(FabricEvent::SignedIn { slot: id, account: ts.account });
                egui.request_repaint();
                return;
            }
        }
        let _ = tx.send(FabricEvent::NeedSignIn);
        egui.request_repaint();
    });
}

pub fn sign_in(state: &mut AppState, cx: &Ctx) {
    if cx.settings.connections.effective_entra_client_id().is_empty() {
        cx.toast(ToastKind::Error, "No Entra client ID is configured (Settings → Connections).");
        return;
    }
    // reuse an existing slot, or pick the first Entra profile, or mint a dedicated slot id
    let slot = state
        .fabric
        .slot
        .or_else(|| state.library.profiles.iter().find(|p| matches!(p.auth, AuthMethod::EntraInteractive { .. })).map(|p| p.id))
        .unwrap_or_else(ProfileId::new);
    let hint = state.fabric.account.as_ref().map(|a| a.username.clone());
    state.fabric.status = Some(FabricStatus::SigningIn);

    let cancel = Arc::new(AtomicBool::new(false));
    let device = Arc::new(parking_lot::Mutex::new(None));
    let url = Arc::new(parking_lot::Mutex::new(None));
    let mut placeholder = ConnectionProfile::new("api.fabric.microsoft.com", AuthMethod::EntraInteractive { tenant: tenant_hint(cx), account_hint: hint.clone() });
    placeholder.name = Some("Microsoft Fabric".into());
    state.dialog = Dialog::AuthWaiting {
        profile: placeholder,
        purpose: ConnectPurpose::TestOnly,
        message: "Complete the Fabric sign-in in your browser… (consent covers SQL and Fabric)".into(),
        device: device.clone(),
        url: url.clone(),
        cancel: cancel.clone(),
        started: Instant::now(),
    };
    let prompter = UiPrompter { cancel, device, url, egui: cx.egui.clone() };
    let resolver = cx.resolver.clone();
    let tx = cx.fabric_tx.clone();
    let egui = cx.egui.clone();
    let tenant = tenant_hint(cx);
    cx.session.spawn(async move {
        let r = resolver.fabric_api_token(slot, tenant.as_deref(), hint.as_deref(), &prompter).await;
        let ev = match r {
            Ok(ts) => FabricEvent::SignedIn { slot, account: ts.account },
            Err(e) => FabricEvent::SignInFailed(format!("{e}")),
        };
        let _ = tx.send(ev);
        egui.request_repaint();
    });
}

fn sign_out(state: &mut AppState, cx: &Ctx) {
    let _ = cx.store.set_kv(KV_ACCOUNT_SLOT, &"");
    let _ = cx.store.fabric_cache_clear();
    state.fabric = FabricState { pins: std::mem::take(&mut state.fabric.pins), loaded_once: true, status: Some(FabricStatus::SignedOut), ..Default::default() };
}

// ---------------------------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------------------------

/// A Fabric API token for the slot, silently; `None` → the caller reports `NeedSignIn`.
async fn token(resolver: &cobalt_auth::CredentialResolver, slot: ProfileId, tenant: Option<&str>) -> Result<(String, EntraAccount), Option<String>> {
    match resolver.resource_token_silent(slot, FABRIC_API_RESOURCE, tenant).await {
        Ok(Some(ts)) => Ok((ts.access.token.expose().to_string(), ts.account)),
        Ok(None) => Err(None),
        Err(e) => Err(Some(e.to_string())),
    }
}

pub fn load_workspaces(state: &mut AppState, cx: &Ctx) {
    let Some(slot) = state.fabric.slot else {
        state.fabric.status = Some(FabricStatus::SignedOut);
        return;
    };
    if !matches!(state.fabric.workspaces, Loadable::Loaded(_)) {
        state.fabric.workspaces = Loadable::Loading(cx.session.new_request());
    }
    let resolver = cx.resolver.clone();
    let tx = cx.fabric_tx.clone();
    let egui = cx.egui.clone();
    let tenant = tenant_hint(cx);
    cx.session.spawn(async move {
        let ev = match token(&resolver, slot, tenant.as_deref()).await {
            Ok((tok, account)) => {
                let _ = tx.send(FabricEvent::Account(account));
                FabricEvent::Workspaces(FabricClient::new(tok).list_workspaces().await.map_err(|e| fabric_error_text(&e)))
            }
            Err(None) => FabricEvent::NeedSignIn,
            Err(Some(e)) => FabricEvent::Workspaces(Err(e)),
        };
        let _ = tx.send(ev);
        egui.request_repaint();
    });
}

/// Capacity names/SKUs; silently skipped when the registration lacks `Capacity.Read.All`.
pub fn load_capacities(state: &mut AppState, cx: &Ctx) {
    let Some(slot) = state.fabric.slot else { return };
    let resolver = cx.resolver.clone();
    let tx = cx.fabric_tx.clone();
    let egui = cx.egui.clone();
    let tenant = tenant_hint(cx);
    cx.session.spawn(async move {
        if let Ok((tok, _)) = token(&resolver, slot, tenant.as_deref()).await {
            match FabricClient::new(tok).list_capacities().await {
                Ok(caps) => {
                    let _ = tx.send(FabricEvent::Capacities(caps));
                    egui.request_repaint();
                }
                Err(e) => tracing::info!(error = %e, "capacity names unavailable (Capacity.Read.All not granted?)"),
            }
        }
    });
}

pub fn load_items(state: &mut AppState, cx: &Ctx, workspace_id: &str) {
    let Some(slot) = state.fabric.slot else { return };
    state.fabric.items.insert(workspace_id.to_string(), Loadable::Loading(cx.session.new_request()));
    let resolver = cx.resolver.clone();
    let tx = cx.fabric_tx.clone();
    let egui = cx.egui.clone();
    let tenant = tenant_hint(cx);
    let ws = workspace_id.to_string();
    cx.session.spawn(async move {
        let result = match token(&resolver, slot, tenant.as_deref()).await {
            Ok((tok, _)) => FabricClient::new(tok).list_sql_items(&ws).await.map_err(|e| fabric_error_text(&e)),
            Err(None) => Err("Sign in to Fabric first.".into()),
            Err(Some(e)) => Err(e),
        };
        let _ = tx.send(FabricEvent::Items { workspace_id: ws, result });
        egui.request_repaint();
    });
}

pub fn load_detail(state: &mut AppState, cx: &Ctx, item_id: &str) {
    let Some(slot) = state.fabric.slot else { return };
    let Some(item) = state.fabric.item(item_id).cloned() else { return };
    if item.kind == SqlItemKind::SqlEndpoint {
        // The endpoint item carries no properties; its host is the workspace's shared
        // datawarehouse host and its database is the item's name.
        if let Some(host) = state.fabric.workspace_endpoint_host(&item.workspace_id) {
            let target = SqlTarget { server: host, database: item.display_name.clone(), provisioning: None, onelake_tables_path: None, default_schema: None, collation: None };
            on_event(state, cx, FabricEvent::Detail { item_id: item_id.to_string(), result: Ok(target) });
        } else {
            let sibling = state.fabric.items.get(&item.workspace_id).and_then(|l| l.get()).and_then(|v| v.iter().find(|i| matches!(i.kind, SqlItemKind::Lakehouse | SqlItemKind::Warehouse | SqlItemKind::MirroredDatabase)).map(|i| i.id.clone()));
            match sibling {
                Some(sid) => {
                    state.fabric.endpoint_after_detail.entry(sid.clone()).or_default().push(item_id.to_string());
                    state.fabric.details.insert(item_id.to_string(), Loadable::Loading(cx.session.new_request()));
                    load_detail(state, cx, &sid);
                }
                None => {
                    state.fabric.details.insert(item_id.to_string(), Loadable::Failed("no SQL endpoint host known for this workspace".into()));
                }
            }
        }
        return;
    }
    if state.fabric.details.get(item_id).map(|d| d.is_loading()).unwrap_or(false) {
        return;
    }
    state.fabric.details.insert(item_id.to_string(), Loadable::Loading(cx.session.new_request()));
    let resolver = cx.resolver.clone();
    let tx = cx.fabric_tx.clone();
    let egui = cx.egui.clone();
    let tenant = tenant_hint(cx);
    let id = item_id.to_string();
    cx.session.spawn(async move {
        let result = match token(&resolver, slot, tenant.as_deref()).await {
            Ok((tok, _)) => FabricClient::new(tok).item_detail(&item).await.map(|d| d.target).map_err(|e| fabric_error_text(&e)),
            Err(None) => Err("Sign in to Fabric first.".into()),
            Err(Some(e)) => Err(e),
        };
        let _ = tx.send(FabricEvent::Detail { item_id: id, result });
        egui.request_repaint();
    });
}

pub fn on_event(state: &mut AppState, cx: &Ctx, ev: FabricEvent) {
    match ev {
        FabricEvent::SignedIn { slot, account } => {
            if matches!(state.dialog, Dialog::AuthWaiting { .. }) {
                state.dialog = Dialog::None;
            }
            state.fabric.slot = Some(slot);
            state.fabric.account = Some(account);
            state.fabric.status = Some(FabricStatus::Ready);
            let _ = cx.store.set_kv(KV_ACCOUNT_SLOT, &slot.to_string());
            load_workspaces(state, cx);
        }
        FabricEvent::SignInFailed(e) => {
            if matches!(state.dialog, Dialog::AuthWaiting { .. }) {
                state.dialog = Dialog::None;
            }
            state.fabric.status = Some(if e.contains("ancelled") { FabricStatus::SignedOut } else { FabricStatus::Error(e) });
        }
        FabricEvent::Account(a) => {
            if a != EntraAccount::default() {
                state.fabric.account = Some(a);
            }
        }
        FabricEvent::Capacities(caps) => {
            state.fabric.capacities = caps.into_iter().map(|c| (c.id.clone(), c)).collect();
        }
        FabricEvent::NeedSignIn => {
            state.fabric.status = Some(FabricStatus::SignedOut);
            state.fabric.workspaces = Loadable::NotLoaded;
        }
        FabricEvent::Workspaces(result) => match result {
            Ok(ws) => {
                if let Ok(json) = serde_json::to_string(&ws) {
                    let _ = cx.store.fabric_cache_put(CACHE_WORKSPACES, &json);
                }
                // keep pin labels current
                for p in &state.fabric.pins {
                    if let Some(w) = ws.iter().find(|w| w.id == p.workspace_id) {
                        if w.display_name != p.workspace_name {
                            let _ = cx.store.fabric_pin_relabel(&p.item_id, &p.display_name, &w.display_name);
                        }
                    }
                }
                state.fabric.workspaces = Loadable::Loaded(ws);
                state.fabric.status = Some(FabricStatus::Ready);
                state.fabric.last_refresh = Some(Instant::now());
                state.fabric.pins = cx.store.fabric_pins().unwrap_or_default();
                // refresh items of workspaces already open
                let open: Vec<String> = state.fabric.expanded.iter().cloned().collect();
                for w in open {
                    load_items(state, cx, &w);
                }
                load_items_for_pins(state, cx);
                load_capacities(state, cx);
            }
            Err(e) => {
                if !matches!(state.fabric.workspaces, Loadable::Loaded(_)) {
                    state.fabric.workspaces = Loadable::Failed(e.clone());
                }
                state.fabric.status = Some(FabricStatus::Error(e));
            }
        },
        FabricEvent::Items { workspace_id, result } => {
            state.fabric.items.insert(workspace_id, match result {
                Ok(v) => Loadable::Loaded(v),
                Err(e) => Loadable::Failed(e),
            });
        }
        FabricEvent::Detail { item_id, result } => match result {
            Ok(target) => {
                state.fabric.details.insert(item_id.clone(), Loadable::Loaded(target.clone()));
                if let Some(endpoints) = state.fabric.endpoint_after_detail.remove(&item_id) {
                    for ep in endpoints {
                        state.fabric.details.remove(&ep);
                        load_detail(state, cx, &ep);
                    }
                }
                if state.fabric.expand_after_detail.remove(&item_id) {
                    ensure_item_explorer(state, cx, &item_id, target.clone());
                }
                if state.fabric.open_after_detail.remove(&item_id) {
                    open_item(state, cx, &item_id, target.clone());
                }
                if state.fabric.save_after_detail.remove(&item_id) {
                    save_to_servers(state, cx, &item_id, target);
                }
            }
            Err(e) => {
                state.fabric.open_after_detail.remove(&item_id);
                state.fabric.save_after_detail.remove(&item_id);
                state.fabric.expand_after_detail.remove(&item_id);
                state.fabric.expanded_items.remove(&item_id);
                state.fabric.details.insert(item_id, Loadable::Failed(e.clone()));
                cx.toast(ToastKind::Error, e);
            }
        },
    }
}

// ---------------------------------------------------------------------------------------------
// Opening items
// ---------------------------------------------------------------------------------------------

/// Deterministic profile id for a Fabric item, so reopening the same item reuses tabs' state,
/// credentials and history rows.
fn ephemeral_id(item_id: &str) -> ProfileId {
    ProfileId(uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, format!("cobalt-fabric-item:{item_id}").as_bytes()))
}

/// Build the profile a query tab connects with, sharing the slot's refresh token so no extra
/// sign-in is needed.
fn ephemeral_profile(state: &AppState, cx: &Ctx, item_id: &str, target: &SqlTarget) -> Option<ConnectionProfile> {
    let item = state.fabric.item(item_id)?.clone();
    let ws_name = state.fabric.workspace(&item.workspace_id).map(|w| w.display_name.clone()).unwrap_or_default();
    let (server, port) = match target.server.split_once(',') {
        Some((h, p)) => (h.to_string(), p.parse::<u16>().ok()),
        None => (target.server.clone(), None),
    };
    let mut p = ConnectionProfile::new(server, AuthMethod::EntraInteractive { tenant: tenant_hint(cx), account_hint: state.fabric.account.as_ref().map(|a| a.username.clone()) });
    p.id = ephemeral_id(item_id);
    p.port = port;
    p.database = Some(target.database.clone());
    let label = if item.kind == SqlItemKind::SqlEndpoint { format!("{} (SQL endpoint)", item.display_name) } else { item.display_name.clone() };
    p.name = Some(if ws_name.is_empty() { label } else { format!("{label} · {ws_name}") });
    // share the account's refresh token with the ephemeral profile id
    if let Some(slot) = state.fabric.slot {
        let from = cobalt_auth::CredentialResolver::refresh_token_ref(&slot);
        let to = cobalt_auth::CredentialResolver::refresh_token_ref(&p.id);
        if let Ok(Some(rt)) = cx.secrets.get(&from) {
            let _ = cx.secrets.set(&to, &rt);
        }
    }
    Some(p)
}

fn open_item(state: &mut AppState, cx: &Ctx, item_id: &str, target: SqlTarget) {
    if !target.is_ready() {
        cx.toast(ToastKind::Warning, format!("The SQL endpoint is still provisioning ({}).", target.provisioning.as_deref().unwrap_or("unknown")));
        return;
    }
    let Some(profile) = ephemeral_profile(state, cx, item_id, &target) else { return };
    let db = profile.database.clone();
    let id = profile.id;
    state.library.ephemeral.retain(|p| p.id != id);
    state.library.ephemeral.push(profile);
    ops::new_query_tab(state, cx, Some(id), db, None, false);
    if let Some(item) = state.fabric.item(item_id).cloned() {
        let ws_name = state.fabric.workspace(&item.workspace_id).map(|w| w.display_name.clone()).unwrap_or_default();
        let _ = cx.store.fabric_touch_recent(&FabricPin { workspace_id: item.workspace_id.clone(), item_id: item.id.clone(), item_kind: format!("{:?}", item.kind), display_name: item.display_name.clone(), workspace_name: ws_name, position: 0 });
        state.fabric.recent = cx.store.fabric_recent(6).unwrap_or_default();
    }
}

fn toggle_pin(state: &mut AppState, cx: &Ctx, item_id: &str) {
    if state.fabric.is_pinned(item_id) {
        let _ = cx.store.fabric_unpin(item_id);
    } else if let Some(item) = state.fabric.item(item_id).cloned() {
        let ws_name = state.fabric.workspace(&item.workspace_id).map(|w| w.display_name.clone()).unwrap_or_default();
        let pin = FabricPin { workspace_id: item.workspace_id.clone(), item_id: item.id.clone(), item_kind: format!("{:?}", item.kind), display_name: item.display_name.clone(), workspace_name: ws_name, position: 0 };
        let _ = cx.store.fabric_pin(&pin);
    }
    state.fabric.pins = cx.store.fabric_pins().unwrap_or_default();
}

fn save_to_servers(state: &mut AppState, cx: &Ctx, item_id: &str, target: SqlTarget) {
    let Some(mut profile) = ephemeral_profile(state, cx, item_id, &target) else { return };
    profile.id = ProfileId::new();
    // the new profile gets its own copy of the refresh token so it connects silently too
    if let Some(slot) = state.fabric.slot {
        if let Ok(Some(rt)) = cx.secrets.get(&cobalt_auth::CredentialResolver::refresh_token_ref(&slot)) {
            let _ = cx.secrets.set(&cobalt_auth::CredentialResolver::refresh_token_ref(&profile.id), &rt);
        }
    }
    ops::open_connection_dialog_from(state, cx, profile, true, None);
    state.sidebar_view = SidebarView::Servers;
}
