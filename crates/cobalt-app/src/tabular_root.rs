//! The Servers tree's "Analysis Services" root: saved XMLA endpoints (Azure Analysis Services,
//! SSAS over HTTP, Power BI workspaces over XMLA), their catalogs (models), and the credentials
//! their queries use. Profiles live in the store (`tabular:profiles`), basic passwords in the
//! keyring. Design: `docs/design/semantic_models.md` §3.1 (2), §3.3.

use crate::fabric::FabricEvent;
use crate::ops::Ctx;
use crate::session::TabularAuth;
use crate::state::{AppState, Dialog, Loadable, ToastKind};
use cobalt_core::tabular::{TabularAuthKind, TabularProfile, TabularTarget};
use std::sync::Arc;

const KV_PROFILES: &str = "tabular:profiles";

/// Load the saved profiles once.
pub fn ensure_loaded(state: &mut AppState, cx: &Ctx) {
    if state.tabular.loaded {
        return;
    }
    state.tabular.loaded = true;
    if let Ok(Some(list)) = cx.store.get_kv::<Vec<TabularProfile>>(KV_PROFILES) {
        state.tabular.profiles = list;
    }
}

fn persist(state: &AppState, cx: &Ctx) {
    if let Err(e) = cx.store.set_kv(KV_PROFILES, &state.tabular.profiles) {
        cx.toast(ToastKind::Error, format!("Could not save the Analysis Services profiles: {e}"));
    }
}

/// Add or replace a profile; `password` (basic auth) goes to the keyring when given.
pub fn save_profile(state: &mut AppState, cx: &Ctx, profile: TabularProfile, password: Option<String>) {
    if let Some(pw) = password.filter(|p| !p.is_empty()) {
        let _ = cx.secrets.set(&cobalt_core::SecretRef { key: profile.secret_ref() }, &cobalt_core::Secret::new(pw));
    }
    match state.tabular.profiles.iter_mut().find(|p| p.id == profile.id) {
        Some(p) => *p = profile,
        None => state.tabular.profiles.push(profile),
    }
    state.tabular.expanded = true;
    persist(state, cx);
}

pub fn remove_profile(state: &mut AppState, cx: &Ctx, id: uuid::Uuid) {
    if let Some(p) = state.tabular.profile(id).cloned() {
        let _ = cx.secrets.delete(&cobalt_core::SecretRef { key: p.secret_ref() });
    }
    state.tabular.profiles.retain(|p| p.id != id);
    state.tabular.catalogs.remove(&id);
    persist(state, cx);
}

/// The profile whose endpoint a target belongs to (HTTP by URL, Azure AS by server).
pub fn profile_for_target<'a>(state: &'a AppState, target: &TabularTarget) -> Option<&'a TabularProfile> {
    state.tabular.profiles.iter().find(|p| match (&p.target, target) {
        (TabularTarget::Http { url: a, .. }, TabularTarget::Http { url: b, .. }) => a.eq_ignore_ascii_case(b),
        (TabularTarget::AzureAs { region: r1, server: s1, .. }, TabularTarget::AzureAs { region: r2, server: s2, .. }) => r1.eq_ignore_ascii_case(r2) && s1.eq_ignore_ascii_case(s2),
        (TabularTarget::PowerBi { workspace_id: a, .. }, TabularTarget::PowerBi { workspace_id: b, .. }) => a == b,
        _ => false,
    })
}

/// How a run on `target` signs in: the Fabric panel's Entra sign-in for Power BI and Azure AS,
/// the profile's basic credentials (keyring) or nothing for HTTP endpoints.
pub fn auth_for(state: &AppState, cx: &Ctx, target: &TabularTarget, force_xmla: bool) -> Result<TabularAuth, String> {
    let profile = profile_for_target(state, target);
    let (basic, anonymous) = match profile.map(|p| &p.auth) {
        Some(TabularAuthKind::Basic { user }) => {
            let pw = cx.secrets.get(&cobalt_core::SecretRef { key: profile.map(|p| p.secret_ref()).unwrap_or_default() }).ok().flatten().map(|s| s.expose().to_string()).unwrap_or_default();
            (Some((user.clone(), pw)), false)
        }
        Some(TabularAuthKind::Anonymous) => (None, true),
        _ => (None, false),
    };
    let needs_entra = basic.is_none() && !anonymous;
    if needs_entra && state.fabric.slot.is_none() && !matches!(target, TabularTarget::Http { .. }) {
        return Err(crate::session::tabular::NEED_SIGN_IN.into());
    }
    Ok(TabularAuth { resolver: Arc::clone(cx.resolver), slot: state.fabric.slot, tenant: cx.settings.connections.entra_default_tenant.clone(), basic, anonymous, force_xmla })
}

/// List a profile's catalogs (models) through XMLA.
pub fn load_catalogs(state: &mut AppState, cx: &Ctx, id: uuid::Uuid, force: bool) {
    let Some(profile) = state.tabular.profile(id).cloned() else { return };
    if !force && state.tabular.catalogs.get(&id).map(|l| !l.needs_load()).unwrap_or(false) {
        return;
    }
    let auth = match auth_for(state, cx, &profile.target, true) {
        Ok(a) => a,
        Err(e) => {
            state.tabular.catalogs.insert(id, Loadable::Failed(e));
            return;
        }
    };
    state.tabular.catalogs.insert(id, Loadable::Loading);
    let tx = cx.fabric_tx.clone();
    let egui = cx.egui.clone();
    let target = profile.target.clone();
    cx.session.spawn(async move {
        let result = match crate::session::tabular::connect(&auth, &target).await {
            Ok(t) => t.list_catalogs().await.map_err(|e| match e.hint() {
                Some(h) => format!("{e}\n{h}"),
                None => e.to_string(),
            }),
            Err(e) => Err(e),
        };
        let _ = tx.send(FabricEvent::TabularCatalogs { profile: id, result });
        egui.request_repaint();
    });
}

/// "Test" in the profile dialog: connect and list the catalogs.
pub fn test_profile(state: &mut AppState, cx: &Ctx, profile: TabularProfile, password: Option<String>) {
    if let Some(pw) = password.filter(|p| !p.is_empty()) {
        let _ = cx.secrets.set(&cobalt_core::SecretRef { key: profile.secret_ref() }, &cobalt_core::Secret::new(pw));
    }
    // the dialog's draft is not saved yet: build the auth from it directly
    let (basic, anonymous) = match &profile.auth {
        TabularAuthKind::Basic { user } => (Some((user.clone(), cx.secrets.get(&cobalt_core::SecretRef { key: profile.secret_ref() }).ok().flatten().map(|s| s.expose().to_string()).unwrap_or_default())), false),
        TabularAuthKind::Anonymous => (None, true),
        TabularAuthKind::Entra => (None, false),
    };
    if basic.is_none() && !anonymous && state.fabric.slot.is_none() {
        if let Dialog::TabularProfile { test, .. } = &mut state.dialog {
            *test = Some(Loadable::Failed("Sign in on the Fabric panel first (Entra).".into()));
        }
        return;
    }
    let auth = TabularAuth { resolver: Arc::clone(cx.resolver), slot: state.fabric.slot, tenant: cx.settings.connections.entra_default_tenant.clone(), basic, anonymous, force_xmla: true };
    if let Dialog::TabularProfile { test, .. } = &mut state.dialog {
        *test = Some(Loadable::Loading);
    }
    let tx = cx.fabric_tx.clone();
    let egui = cx.egui.clone();
    let target = profile.target.clone();
    let id = profile.id;
    cx.session.spawn(async move {
        let started = std::time::Instant::now();
        let result = match crate::session::tabular::connect(&auth, &target).await {
            Ok(t) => match t.list_catalogs().await {
                Ok(c) => Ok(format!("Connected over {} in {} ms: {} model{} ({})", t.name(), started.elapsed().as_millis(), c.len(), if c.len() == 1 { "" } else { "s" }, c.iter().take(5).cloned().collect::<Vec<_>>().join(", "))),
                Err(e) => Err(match e.hint() {
                    Some(h) => format!("{e}\n{h}"),
                    None => e.to_string(),
                }),
            },
            Err(e) => Err(e),
        };
        let _ = tx.send(FabricEvent::TabularTest { profile: id, result });
        egui.request_repaint();
    });
}

/// A DAX tab on one model of a profile.
pub fn query_tab(state: &mut AppState, cx: &Ctx, id: uuid::Uuid, model: &str) -> Option<usize> {
    let p = state.tabular.profile(id)?.clone();
    let target = p.target_for(model);
    let idx = crate::tabularq::new_tab(state, cx, target.clone(), None);
    if let Some(tt) = state.tabs[idx].tabular.as_mut() {
        tt.profile = Some(id);
        tt.force_xmla = true;
    }
    crate::tabularq::note_recent(cx, &target, Some(id));
    Some(idx)
}

/// An Explore tab on one model of a profile.
pub fn explore_tab(state: &mut AppState, cx: &Ctx, id: uuid::Uuid, model: &str) -> Option<usize> {
    let p = state.tabular.profile(id)?.clone();
    let target = p.target_for(model);
    let idx = crate::exploreq::new_tab(state, cx, target.clone(), None, None);
    if let Some(tt) = state.tabs[idx].tabular.as_mut() {
        tt.profile = Some(id);
        tt.force_xmla = true;
    }
    crate::tabularq::note_recent(cx, &target, Some(id));
    Some(idx)
}

/// Open the add / edit dialog.
pub fn edit_dialog(state: &mut AppState, id: Option<uuid::Uuid>) {
    let (draft, editing) = match id.and_then(|i| state.tabular.profile(i).cloned()) {
        Some(p) => (p, true),
        None => (TabularProfile::new("", TabularTarget::AzureAs { region: "westus".into(), server: String::new(), model: String::new() }, TabularAuthKind::Entra), false),
    };
    state.dialog = Dialog::TabularProfile { draft: Box::new(draft), password: String::new(), editing, test: None };
}
