//! egui_agent integration: semantic verbs so an agent can drive Cobalt end to end.
//!
//! Verbs (all take/return JSON):
//! - `state` → tabs, connections, run states
//! - `add_profile {name?, server, user, password, database?, trust_server_certificate?}` → profile id (SQL login; the password is stored in the OS keychain)
//! - `connect {profile: <name or id>, database?}` → opens a new tab connected to that profile
//! - `open_query {text, connect?: <profile name>}` → new tab with text
//! - `set_query {text}` (active tab), `run {mode?: all|current|selection|estimated_plan}`, `cancel`
//! - `wait_run {timeout_ms?}` → blocks the agent until the active tab's run finishes (polls via the app loop)
//! - `results {set?, offset?, limit?}` → rows of the active tab as JSON, `messages`
//! - `export {format, path, set?}`; `plan` → summary JSON; `copy {kind}`; `close_tab`; `command {id}` (any palette command id)

use crate::app::CobaltApp;
use crate::commands::{Command, COMMANDS};
use crate::ops::{self, Ctx};
use crate::state::{ConnState, RunMode, RunViewState, ToastKind};
use cobalt_core::*;
use egui_agent::{Action, ActionResult, AgentApp, Snapshot, SnapshotCtx};
use serde_json::{json, Value};
use std::cell::RefCell;

impl CobaltApp {
    fn state_json(&self) -> Value {
        let tabs: Vec<Value> = self
            .state
            .tabs
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let run = t.run.as_ref().map(|r| {
                    json!({
                        "state": format!("{:?}", r.state).to_lowercase(),
                        "result_sets": r.result_sets.iter().filter(|s| !s.is_plan).map(|s| json!({"rows": s.rs.row_count(), "visible_rows": s.rs.visible_count(), "columns": s.rs.columns.iter().map(|c| json!({"name": c.name, "type": c.sql_type.to_string()})).collect::<Vec<_>>(), "state": s.rs.state()})).collect::<Vec<_>>(),
                        "messages": r.messages.iter().map(|m| json!({"text": m.text, "error": m.is_error})).collect::<Vec<_>>(),
                        "plans": r.plans.len(),
                        "elapsed_ms": if r.is_live() { r.started.elapsed().as_millis() } else { r.elapsed.as_millis() },
                        "total_rows": r.total_rows,
                        "paused": r.paused_set,
                    })
                });
                json!({
                    "index": i,
                    "id": t.id.to_string(),
                    "title": t.title,
                    "active": self.state.active_tab == Some(i),
                    "dirty": t.is_dirty(),
                    "profile": t.profile.as_ref().map(|p| p.display_name()),
                    "connection": match &t.conn {
                        ConnState::Disconnected => json!("disconnected"),
                        ConnState::Connecting => json!("connecting"),
                        ConnState::Connected { engine, spid, database } => json!({"engine": engine.short_label(), "version": engine.version, "spid": spid, "database": database}),
                        ConnState::Failed { error, hint } => json!({"failed": error, "hint": hint}),
                    },
                    "text": t.text,
                    "cursor": t.editor.cursor,
                    "actual_plan": t.actual_plan,
                    "run": run,
                })
            })
            .collect();
        json!({
            "tabs": tabs,
            "active_tab": self.state.active_tab,
            "profiles": self.state.library.profiles.iter().map(|p| json!({"id": p.id.to_string(), "name": p.display_name(), "server": p.server, "auth": p.auth.label(), "database": p.database})).collect::<Vec<_>>(),
            "groups": self.state.library.groups.iter().map(|g| json!({"id": g.id.to_string(), "name": g.name})).collect::<Vec<_>>(),
            "dialog": if self.state.dialog.is_open() { format!("{}", dialog_name(&self.state.dialog)) } else { "none".into() },
            "sidebar": format!("{:?}", self.state.sidebar_view),
            "theme": if self.theme.is_dark() { "dark" } else { "light" },
        })
    }

    fn with_ctx<R>(&mut self, egui: &egui::Context, f: impl FnOnce(&mut crate::state::AppState, &Ctx) -> R) -> R {
        let toasts = RefCell::new(Vec::new());
        let r = {
            let cx = Ctx {
                session: &self.session,
                store: &self.store,
                resolver: &self.resolver,
                secrets: &self.secrets,
                settings: &self.settings,
                paths: &self.paths,
                auth_tx: &self.auth_tx_ref(),
                export_tx: &self.export_tx_ref(),
                fabric_tx: &self.fabric_tx_ref(),
                egui,
                toasts: &toasts,
            };
            f(&mut self.state, &cx)
        };
        for (k, m) in toasts.into_inner() {
            let t = match k {
                ToastKind::Error => self.toasts.error(m),
                ToastKind::Warning => self.toasts.warning(m),
                ToastKind::Success => self.toasts.success(m),
                ToastKind::Info => self.toasts.info(m),
            };
            t.closable(true);
        }
        r
    }

    fn find_profile(&self, key: &str) -> Option<ConnectionProfile> {
        self.state.library.profiles.iter().find(|p| p.id.to_string() == key || p.display_name().eq_ignore_ascii_case(key) || p.server.eq_ignore_ascii_case(key)).cloned()
    }
}

fn dialog_name(d: &crate::state::Dialog) -> &'static str {
    use crate::state::Dialog::*;
    match d {
        None => "none",
        Connection(_) => "connection",
        Password { .. } => "password",
        AuthWaiting { .. } => "auth_waiting",
        Group { .. } => "group",
        ConfirmClose { .. } => "confirm_close",
        ConfirmDeleteProfile { .. } => "confirm_delete_profile",
        ConfirmDeleteGroup { .. } => "confirm_delete_group",
        ConfirmWrite { .. } => "confirm_write",
        Export(_) => "export",
        ChangeConnection { .. } => "change_connection",
        ExecOptions { .. } => "exec_options",
        Rename { .. } => "rename",
        Error { .. } => "error",
        AdsImport { .. } => "ads_import",
        UpdateAvailable { .. } => "update_available",
    }
}

fn arg_str(args: Option<&Value>, key: &str) -> Option<String> {
    args.and_then(|a| a.get(key)).and_then(|v| v.as_str()).map(str::to_string)
}
fn arg_usize(args: Option<&Value>, key: &str) -> Option<usize> {
    args.and_then(|a| a.get(key)).and_then(|v| v.as_u64()).map(|v| v as usize)
}

impl AgentApp for CobaltApp {
    fn snapshot(&self, cx: &SnapshotCtx<'_>) -> Snapshot {
        Snapshot::new().nodes(cx.registry_nodes()).nodes(cx.accesskit_nodes()).nodes(cx.event_nodes()).with_data(&self.state_json())
    }

    fn dispatch(&mut self, action: &Action, egui: &egui::Context) -> ActionResult {
        let args = action.args();
        match action.name() {
            "state" => ActionResult::with(&self.state_json()),
            "add_profile" => {
                let Some(server) = arg_str(args, "server") else { return ActionResult::BadArgs("server is required".into()) };
                let user = arg_str(args, "user").unwrap_or_default();
                let password = arg_str(args, "password").unwrap_or_default();
                let auth = match arg_str(args, "auth").as_deref() {
                    Some("entra") | Some("entra_interactive") => AuthMethod::EntraInteractive { tenant: arg_str(args, "tenant"), account_hint: arg_str(args, "account_hint") },
                    Some("device_code") => AuthMethod::EntraDeviceCode { tenant: arg_str(args, "tenant") },
                    Some("azure_cli") => AuthMethod::AzureCli { tenant: arg_str(args, "tenant") },
                    Some("windows") => AuthMethod::WindowsIntegrated,
                    _ => AuthMethod::SqlLogin { user: user.clone(), password: None },
                };
                let mut p = ConnectionProfile::new(server, auth);
                p.name = arg_str(args, "name");
                p.database = arg_str(args, "database");
                p.options.trust_server_certificate = args.and_then(|a| a.get("trust_server_certificate")).and_then(|v| v.as_bool()).unwrap_or(true);
                if !password.is_empty() && matches!(p.auth, AuthMethod::SqlLogin { .. }) {
                    let r = SecretRef::for_profile(&p.id, "password");
                    if self.secrets.set(&r, &Secret::new(password)).is_ok() {
                        p.auth = AuthMethod::SqlLogin { user, password: Some(r) };
                    }
                }
                if let Err(e) = self.store.upsert_profile(&p) {
                    return ActionResult::Rejected(format!("save failed: {e}"));
                }
                let id = p.id.to_string();
                self.with_ctx(egui, |s, cx| ops::load_library(s, cx));
                ActionResult::with(&json!({"profile_id": id}))
            }
            "connect" => {
                let Some(key) = arg_str(args, "profile") else { return ActionResult::BadArgs("profile is required".into()) };
                let Some(p) = self.find_profile(&key) else { return ActionResult::BadArgs("profile not found".into()) };
                let db = arg_str(args, "database");
                let idx = self.with_ctx(egui, |s, cx| ops::new_query_tab(s, cx, Some(p.id), db, None, false));
                ActionResult::with(&json!({"tab": idx}))
            }
            "open_query" => {
                let text = arg_str(args, "text").unwrap_or_default();
                let profile = arg_str(args, "connect").and_then(|k| self.find_profile(&k));
                let db = arg_str(args, "database");
                let idx = self.with_ctx(egui, |s, cx| {
                    let i = ops::new_query_tab(s, cx, profile.as_ref().map(|p| p.id), db, Some(("Agent query".into(), text)), false);
                    s.tabs[i].custom_title = false;
                    s.tabs[i].title = format!("SQLQuery_{}", s.tabs[i].untitled_index);
                    i
                });
                ActionResult::with(&json!({"tab": idx}))
            }
            "set_query" => {
                let text = arg_str(args, "text").unwrap_or_default();
                match self.state.active_mut() {
                    Some(t) => {
                        t.editor.pending_edit = Some(crate::state::PendingEdit::SetText { text, cursor: 0 });
                        ActionResult::ok()
                    }
                    None => ActionResult::BadArgs("no active tab".into()),
                }
            }
            "activate_tab" => {
                let i = arg_usize(args, "tab").unwrap_or(0);
                if i < self.state.tabs.len() {
                    self.state.active_tab = Some(i);
                    ActionResult::ok()
                } else {
                    ActionResult::BadArgs("no such tab".into())
                }
            }
            "run" => {
                let mode = match arg_str(args, "mode").as_deref() {
                    Some("current") => RunMode::Current,
                    Some("selection") => RunMode::Selection,
                    Some("estimated_plan") | Some("plan") => RunMode::EstimatedPlan,
                    _ => RunMode::All,
                };
                let Some(i) = self.state.active_tab else { return ActionResult::BadArgs("no active tab".into()) };
                if args.and_then(|a| a.get("actual_plan")).and_then(|v| v.as_bool()).unwrap_or(false) {
                    self.state.tabs[i].actual_plan = true;
                }
                self.with_ctx(egui, |s, cx| ops::run(s, cx, i, mode));
                ActionResult::ok()
            }
            "cancel" => {
                if let Some(i) = self.state.active_tab {
                    self.with_ctx(egui, |s, cx| ops::cancel(s, cx, i));
                }
                ActionResult::ok()
            }
            "fetch_more" => {
                if let Some(i) = self.state.active_tab {
                    let rows = args.and_then(|a| a.get("rows")).and_then(|v| v.as_u64());
                    self.with_ctx(egui, |s, cx| ops::fetch_more(s, cx, i, rows));
                }
                ActionResult::ok()
            }
            "results" => {
                let Some(t) = self.state.active() else { return ActionResult::BadArgs("no active tab".into()) };
                let Some(r) = &t.run else { return ActionResult::BadArgs("no run".into()) };
                let set = arg_usize(args, "set").unwrap_or(0);
                let data_sets: Vec<&crate::state::ResultSetView> = r.result_sets.iter().filter(|s| !s.is_plan).collect();
                let Some(v) = data_sets.get(set) else { return ActionResult::BadArgs("no such result set".into()) };
                let offset = arg_usize(args, "offset").unwrap_or(0);
                let limit = arg_usize(args, "limit").unwrap_or(50).min(5000);
                let rs = &v.rs;
                let rows: Vec<Value> = (offset..(offset + limit).min(rs.visible_count())).map(|row| Value::Array((0..rs.column_count()).map(|c| rs.cell_value(row, c).to_json()).collect())).collect();
                ActionResult::with(&json!({
                    "columns": rs.columns.iter().map(|c| json!({"name": c.name, "type": c.sql_type.to_string()})).collect::<Vec<_>>(),
                    "rows": rows,
                    "total_rows": rs.row_count(),
                    "visible_rows": rs.visible_count(),
                    "state": rs.state(),
                }))
            }
            "messages" => {
                let Some(t) = self.state.active() else { return ActionResult::BadArgs("no active tab".into()) };
                let msgs: Vec<Value> = t.run.as_ref().map(|r| r.messages.iter().map(|m| json!({"text": m.text, "error": m.is_error, "line": m.line})).collect()).unwrap_or_default();
                ActionResult::with(&json!(msgs))
            }
            "plan" => {
                let Some(t) = self.state.active() else { return ActionResult::BadArgs("no active tab".into()) };
                let Some(r) = &t.run else { return ActionResult::BadArgs("no run".into()) };
                let plans: Vec<Value> = r
                    .plans
                    .iter()
                    .map(|p| match cobalt_plan::parse(&p.xml) {
                        Ok(plan) => cobalt_plan::plan_to_json(&plan),
                        Err(e) => json!({"error": e.to_string()}),
                    })
                    .collect();
                ActionResult::with(&json!(plans))
            }
            "export" => {
                let Some(i) = self.state.active_tab else { return ActionResult::BadArgs("no active tab".into()) };
                let format = arg_str(args, "format").unwrap_or_else(|| "csv".into());
                // OneLake: {lakehouse: <name or id>, name: <table or file name>} instead of path
                let lakehouse = arg_str(args, "lakehouse");
                let onelake_name = arg_str(args, "name").unwrap_or_default();
                let path = match (arg_str(args, "path"), &lakehouse) {
                    (Some(p), _) => p,
                    (None, Some(_)) => String::new(),
                    (None, None) => return ActionResult::BadArgs("path (or lakehouse + name) is required".into()),
                };
                let lakehouse_id = lakehouse.as_ref().map(|n| {
                    self.state.fabric.items.values().filter_map(|l| l.get()).flatten().find(|i| matches!(i.kind, cobalt_fabric::SqlItemKind::Lakehouse) && (i.display_name.eq_ignore_ascii_case(n) || i.id == *n)).map(|i| i.id.clone())
                });
                if let Some(None) = lakehouse_id {
                    return ActionResult::BadArgs("no such lakehouse (expand its workspace in the Fabric panel first)".into());
                }
                let set = arg_usize(args, "set").unwrap_or(0);
                let fi = ops::FORMAT_LABELS.iter().position(|(_, e)| *e == format).unwrap_or(0);
                self.with_ctx(egui, |s, cx| {
                    ops::open_export_dialog(s, cx, i, set, false);
                    if let crate::state::Dialog::Export(d) = &mut s.dialog {
                        d.format_index = fi;
                        d.path = path;
                        if let Some(Some(id)) = lakehouse_id {
                            d.destination = 1;
                            d.onelake_item = Some(id);
                            d.onelake_name = onelake_name;
                            d.onelake_schema = arg_str(args, "schema").unwrap_or_default();
                        }
                        if let Some(m) = arg_str(args, "delta_mode") {
                            d.delta_mode = match m.as_str() {
                                "overwrite" => 1,
                                "append" => 2,
                                _ => 0,
                            };
                        }
                    }
                    ops::start_export(s, cx);
                });
                ActionResult::ok()
            }
            "copy" => {
                let Some(i) = self.state.active_tab else { return ActionResult::BadArgs("no active tab".into()) };
                let kind = match arg_str(args, "kind").as_deref() {
                    Some("headers") => crate::copy::CopyKind::TsvWithHeaders,
                    Some("markdown") => crate::copy::CopyKind::Markdown,
                    Some("json") => crate::copy::CopyKind::Json,
                    Some("csv") => crate::copy::CopyKind::Csv,
                    Some("insert") => crate::copy::CopyKind::Insert,
                    Some("in_list") => crate::copy::CopyKind::InList,
                    _ => crate::copy::CopyKind::Tsv,
                };
                self.with_ctx(egui, |s, cx| ops::copy_cells(s, cx, i, 0, kind));
                ActionResult::ok()
            }
            "close_tab" => {
                if let Some(i) = self.state.active_tab {
                    self.with_ctx(egui, |s, cx| ops::close_tab(s, cx, i, true));
                }
                ActionResult::ok()
            }
            "command" => {
                let Some(id) = arg_str(args, "id") else { return ActionResult::BadArgs("id is required".into()) };
                let Some(c) = COMMANDS.iter().find(|c| c.id == id).map(|c| c.cmd) else { return ActionResult::BadArgs("unknown command id".into()) };
                self.run_command(egui, c);
                ActionResult::ok()
            }
            "select_cell" => {
                let row = arg_usize(args, "row").unwrap_or(0);
                let col = arg_usize(args, "col").unwrap_or(0);
                let set = arg_usize(args, "set").unwrap_or(0);
                let Some(t) = self.state.active_mut() else { return ActionResult::BadArgs("no active tab".into()) };
                let Some(r) = t.run.as_mut() else { return ActionResult::BadArgs("no run".into()) };
                let mut data_sets: Vec<&mut crate::state::ResultSetView> = r.result_sets.iter_mut().filter(|s| !s.is_plan).collect();
                let Some(v) = data_sets.get_mut(set) else { return ActionResult::BadArgs("no such result set".into()) };
                for s in r.result_sets.iter_mut() {
                    s.grid.focused = false;
                }
                let v = r.result_sets.iter_mut().filter(|s| !s.is_plan).nth(set).unwrap();
                v.grid.focused = true;
                v.grid.anchor = Some((row, col));
                v.grid.selection = crate::state::Selection::Cells { r0: row, c0: col, r1: row, c1: col };
                v.grid.scroll_to = Some((row, col));
                ActionResult::ok()
            }
            "open_path" => {
                let Some(path) = arg_str(args, "path") else { return ActionResult::BadArgs("path is required".into()) };
                self.with_ctx(egui, |s, cx| ops::open_file(s, cx, Some(std::path::PathBuf::from(path))));
                ActionResult::with(&json!({"tab": self.state.active_tab}))
            }
            "complete" => {
                let Some(t) = self.state.active_mut() else { return ActionResult::BadArgs("no active tab".into()) };
                if let Some(text) = arg_str(args, "append") {
                    t.text.push_str(&text);
                }
                let cursor = t.text.len();
                t.editor.cursor = t.text.chars().count();
                t.editor.pending_edit = Some(crate::state::PendingEdit::SetCursor(t.editor.cursor));
                crate::ui::editor::open_completion(t, cursor, egui::pos2(460.0, 120.0), true);
                let items: Vec<String> = t.editor.completion.as_ref().map(|c| c.items.iter().map(|i| i.label.clone()).collect()).unwrap_or_default();
                ActionResult::with(&json!({"items": items}))
            }
            "press" => {
                let Some(name) = arg_str(args, "key") else { return ActionResult::BadArgs("key is required".into()) };
                let Some(key) = egui::Key::from_name(&name) else { return ActionResult::BadArgs(format!("unknown key {name}")) };
                let flag = |k: &str| args.and_then(|a| a.get(k)).and_then(|v| v.as_bool()).unwrap_or(false);
                let modifiers = egui::Modifiers { alt: flag("alt"), ctrl: flag("ctrl"), shift: flag("shift"), mac_cmd: false, command: flag("ctrl") };
                self.state.injected_events.push(egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers });
                self.state.injected_events.push(egui::Event::Key { key, physical_key: None, pressed: false, repeat: false, modifiers });
                // the platform turns Ctrl+C/X/V into dedicated events; mirror that for injected keys
                if modifiers.ctrl && !modifiers.shift {
                    match key {
                        egui::Key::C => self.state.injected_events.push(egui::Event::Copy),
                        egui::Key::X => self.state.injected_events.push(egui::Event::Cut),
                        _ => {}
                    }
                }
                egui.request_repaint();
                ActionResult::ok()
            }
            "type_text" => {
                let text = arg_str(args, "text").unwrap_or_default();
                self.state.injected_events.push(egui::Event::Text(text));
                egui.request_repaint();
                ActionResult::ok()
            }
            "fabric_state" => {
                let f = &self.state.fabric;
                let ws: Vec<Value> = f.workspaces.get().map(|w| w.iter().map(|w| json!({"id": w.id, "name": w.display_name, "kind": format!("{:?}", w.kind), "expanded": f.expanded.contains(&w.id),
                    "items": f.items.get(&w.id).and_then(|l| l.get()).map(|v| v.iter().map(|i| json!({"id": i.id, "name": i.display_name, "kind": format!("{:?}", i.kind), "pinned": f.is_pinned(&i.id),
                        "target": f.details.get(&i.id).and_then(|d| d.get()).map(|t| json!({"server": t.server, "database": t.database, "provisioning": t.provisioning}))})).collect::<Vec<_>>())})).collect()).unwrap_or_default();
                ActionResult::with(&json!({
                    "status": format!("{:?}", f.status()),
                    "account": f.account.as_ref().map(|a| a.username.clone()),
                    "slot": f.slot.map(|s| s.to_string()),
                    "workspaces": ws,
                    "pins": f.pins.iter().map(|p| json!({"item_id": p.item_id, "name": p.display_name, "workspace": p.workspace_name})).collect::<Vec<_>>(),
                }))
            }
            "fabric" => {
                // {action: sign_in|sign_out|refresh|expand|open|pin|save|copy|portal, workspace?: name, item?: name}
                use crate::fabric::FabricAction as FA;
                let act = arg_str(args, "action").unwrap_or_default();
                // prefer the parent item over its SQL-endpoint child when names collide
                let find_item = |name: &str| {
                    let all: Vec<_> = self.state.fabric.items.values().filter_map(|l| l.get()).flatten().filter(|i| i.display_name.eq_ignore_ascii_case(name) || i.id == name).collect();
                    all.iter().find(|i| !i.kind.is_child_endpoint()).or(all.first()).map(|i| i.id.clone())
                };
                let find_ws = |name: &str| self.state.fabric.workspaces.get().and_then(|w| w.iter().find(|w| w.display_name.eq_ignore_ascii_case(name) || w.id == name)).map(|w| w.id.clone());
                let action = match act.as_str() {
                    "sign_in" => FA::SignIn,
                    "sign_out" => FA::SignOut,
                    "refresh" => FA::Refresh,
                    "expand" => match arg_str(args, "workspace").and_then(|n| find_ws(&n)) { Some(id) => FA::ToggleWorkspace(id), None => return ActionResult::BadArgs("no such workspace".into()) },
                    "open" | "pin" | "save" | "copy" | "portal" => {
                        let Some(id) = arg_str(args, "item").and_then(|n| find_item(&n)) else { return ActionResult::BadArgs("no such item (expand its workspace first)".into()) };
                        match act.as_str() {
                            "open" => FA::Open { item_id: id },
                            "pin" => FA::TogglePin { item_id: id },
                            "save" => FA::SaveToServers { item_id: id },
                            "copy" => FA::CopyConnectionString { item_id: id },
                            _ => FA::OpenInPortal { item_id: id },
                        }
                    }
                    _ => return ActionResult::BadArgs("unknown fabric action".into()),
                };
                self.with_ctx(egui, |s, cx| crate::fabric::action(s, cx, action));
                ActionResult::ok()
            }
            "focus_editor" => {
                if let Some(t) = self.state.active_mut() {
                    t.editor.request_focus = true;
                    for s in t.run.iter_mut().flat_map(|r| r.result_sets.iter_mut()) {
                        s.grid.focused = false;
                    }
                }
                ActionResult::ok()
            }
            "dismiss_dialog" => {
                self.state.dialog = crate::state::Dialog::None;
                ActionResult::ok()
            }
            "run_state" => {
                let Some(t) = self.state.active() else { return ActionResult::BadArgs("no active tab".into()) };
                let st = t.run.as_ref().map(|r| match r.state {
                    RunViewState::Running => "running",
                    RunViewState::Paused => "paused",
                    RunViewState::Cancelling => "cancelling",
                    RunViewState::Done => "done",
                    RunViewState::Failed => "failed",
                    RunViewState::Cancelled => "cancelled",
                });
                ActionResult::with(&json!({"state": st, "connection": matches!(t.conn, ConnState::Connected{..})}))
            }
            _ => ActionResult::Unhandled,
        }
    }
}

impl CobaltApp {
    fn run_command(&mut self, egui: &egui::Context, c: Command) {
        let toasts = RefCell::new(Vec::new());
        let theme = self.theme.clone();
        {
            let cx = Ctx {
                session: &self.session,
                store: &self.store,
                resolver: &self.resolver,
                secrets: &self.secrets,
                settings: &self.settings,
                paths: &self.paths,
                auth_tx: &self.auth_tx_ref(),
                export_tx: &self.export_tx_ref(),
                fabric_tx: &self.fabric_tx_ref(),
                egui,
                toasts: &toasts,
            };
            let mut f = crate::ui::shell::Frame { state: &mut self.state, cx: &cx, theme: &theme, keymap: &self.keymap };
            crate::ui::shell::dispatch(&mut f, c);
        }
        for (_, m) in toasts.into_inner() {
            self.toasts.info(m);
        }
    }
}
