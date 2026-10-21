//! egui_agent integration: semantic verbs so an agent can drive Cobalt end to end.
//!
//! Verbs (all take/return JSON):
//! - `state` → tabs, connections, run states
//! - `add_profile {name?, server, user, password, database?, trust_server_certificate?}` → profile id (SQL login; the password is stored in the OS keychain)
//! - `connect {profile: <name or id>, database?}` → opens a new tab connected to that profile
//! - `open_query {text, connect?: <profile name>}` → new tab with text
//! - `spark_query {workspace?, lakehouse?, write_mode?, text?}` → a Spark SQL query tab (plain local Spark without a workspace)
//! - `dax_query {workspace, model, text?, run?}` → a DAX query tab on a semantic model (the Fabric panel's sign-in; `fabric_state` lists models once a workspace is expanded)
//! - `model {action: tree|refresh|functions, workspace, model}` → the model's catalog (tables, columns, measures, relationships) as JSON
//! - `explore {action: new|state|add|remove|agg|sort|top_n|subtotals|filter|remove_filter|run|dax|save|open|matrix, workspace?, model?, profile?, field?: "Table[Column]"|"[Measure]", well?: rows|columns|values|filters, agg?, index?, desc?, n?, on?, op?, values?, lo?, hi?, text?, path?}` → the Explore pivot builder (the active Explore tab unless `new`/`open`)
//! - `tabular_profile {action: add|list|remove|catalogs|test, id?, name?, kind?: asazure|http|powerbi, region?, server?, url?, workspace?, workspace_id?, model?, auth?: entra|basic|anonymous, user?, password?}` → the Servers tree's Analysis Services endpoints (XMLA); `dax_query {profile, model}` and `explore {action: new, profile, model}` open tabs on them
//! - `set_query {text}` (active tab), `run {mode?: all|current|selection|estimated_plan}`, `cancel`
//! - `break_connection` → the active tab treats its connection as dead at the next run (exercises idle reconnect)
//! - `wait_run {timeout_ms?}` → blocks the agent until the active tab's run finishes (polls via the app loop)
//! - `results {set?, offset?, limit?}` → rows of the active tab as JSON, `messages`
//! - `export {format, path, set?}`; `plan` → summary JSON; `copy {kind}`; `close_tab`; `command {id}` (any palette command id)
//! - `run_to_export {format, path | lakehouse, name, schema?, delta_mode?}` → runs the active tab's script straight into the target
//! - `library {action: export|import, path}` → connection library as JSON (no secrets)
//! - `import {path, table, schema?, existing?, delimiter?, header?, types?, exclude?, destination?: "file"}` → Import Data on the active tab; `import_start`, `import_state`
//! - `results_to_table {table, schema?, existing?, set?, target?: tab index, selection_only?}` → Save results as table through the target tab's connection
//! - `grid {action: totals|profile|find|find_state|chart|chart_state, kind?, set?, text?, regex?, case?, word?, x?, y?, agg?, top_n?, bins?, stacked?, horizontal?, off?}`; `files_root {path}`; `plan_view {text?, metric?}`
//! - `import_to {format, path | lakehouse, name, schema?, delta_mode?}` → with the Import dialog open in file mode, write the file to that export target (any format, local or OneLake)
//! - `pointer {action: click|rclick|dblclick|tripleclick|drag|dbldrag|tripledrag|move, x, y, x2?, y2?, shift?, ctrl?, alt?}` → real mouse input in screenshot pixels
//! - `paste {text}` → a paste event (bypasses the OS clipboard); `state` tabs carry `cursors: [[anchor, head]…]`
//! - `notebook {action: new|open|save|cells|set_cell|add_cell|delete_cell|move_cell|set_kind|select|run|cancel|clear_outputs|export|md_edit, …}` → notebook tabs; `state` tabs carry `kind` and `cells`
//! - `runtime {action: status|install|smoke|cancel|remove|refresh|libraries}` → the Spark runtime manager (Settings → Spark runtime), status JSON incl. job progress and log tail
//! - `settings {set: {"spark.idle_minutes": 1, ...}}` → dotted-path settings patch, applied and saved
//! - `lakehouse_pane {action: show|state|select|expand|pull|remove_local|mount|discard|refresh|insert, path?, lakehouse?, table?, code?}` → the Lakehouse sidebar
//! - `kernel {action: status|start|stop|restart|interrupt|log}` → the local Spark session notebooks run PySpark cells on; `notebook {action: set_kernel, kernel: connection|spark}`

use crate::app::CobaltApp;
use crate::commands::{Command, COMMANDS};
use crate::ops::{self, Ctx};
use crate::state::{ConnState, Loadable, RunMode, RunViewState, ToastKind};
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
                let cells = t.notebook.as_deref().map(|nb| nb.cells.iter().enumerate().map(|(ci, c)| {
                    let cell = &nb.nb.cells[ci];
                    json!({
                        "index": ci,
                        "id": c.id,
                        "kind": cell.kind.as_str(),
                        "language": nb.nb.cell_language(cell).label(),
                        "source": cell.source,
                        "execution_count": cell.execution_count,
                        "selected": nb.selected == ci,
                        "cell_kernel": if nb.kernel == crate::state::NotebookKernel::Spark { "spark" } else { "connection" },
                        "md_editing": c.md_editing,
                        "cached": c.cached,
                        "queued": nb.is_queued(&c.id),
                        "parameters": cell.is_parameters(),
                        "pip_packages": c.pip_packages,
                        "charts": c.run.as_ref().map(|r| r.result_sets.iter().enumerate().filter_map(|(k, v)| v.chart.as_ref().map(|ch| json!({"set": k, "kind": ch.spec.kind.key(), "x": ch.spec.x, "y": ch.spec.y, "kept": ch.png.is_some()}))).collect::<Vec<_>>()),
                        "saved_charts": cell.chart_specs().len(),
                        "run": c.run.as_ref().map(|r| json!({
                            "state": format!("{:?}", r.state).to_lowercase(),
                            "result_sets": r.result_sets.iter().filter(|s| !s.is_plan).map(|s| json!({"rows": s.rs.row_count(), "visible_rows": s.rs.visible_count(), "columns": s.rs.columns.iter().map(|c| c.name.clone()).collect::<Vec<_>>()})).collect::<Vec<_>>(),
                            "messages": r.messages.iter().filter(|m| !m.is_batch_header).map(|m| json!({"text": m.text, "error": m.is_error})).collect::<Vec<_>>(),
                        })),
                        "extra_outputs": c.extra_outputs.len(),
                    })
                }).collect::<Vec<_>>());
                json!({
                    "index": i,
                    "id": t.id.to_string(),
                    "title": t.title,
                    "kind": if t.is_notebook() { "notebook" } else if t.is_dashboard() { "dashboard" } else if t.spark.is_some() { "spark" } else if t.is_explore() { "explore" } else if t.is_tabular() { "dax" } else { "query" },
                    "explore": t.explore.as_deref().map(|e| json!({
                        "spec": e.spec,
                        "summary": crate::exploreq::describe(&e.spec),
                        "dax": e.dax.as_ref().map(|q| q.text.clone()),
                        "dax_error": e.dax_error,
                        "dirty": e.dirty,
                        "auto_run": e.auto_run,
                        "matrix": e.matrix,
                        "pivot": e.pivot.as_ref().map(|p| json!({"rows": p.row_keys.len(), "cols": p.col_keys.len(), "values": p.value_names, "row_key_names": p.row_key_names, "truncated": p.truncated, "error": p.error,
                            "first_rows": p.row_keys.iter().take(5).enumerate().map(|(r, k)| json!({"key": k.labels, "subtotal": k.subtotal_level, "cells": (0..p.column_count()).map(|c| p.cell(r, c / p.value_names.len().max(1), c % p.value_names.len().max(1)).map(|x| x.text.to_string())).collect::<Vec<_>>()})).collect::<Vec<_>>(),
                            "col_keys": p.col_keys.iter().take(12).map(|k| k.labels.clone()).collect::<Vec<_>>()})),
                        "filter_editor": e.filter_editor.as_ref().map(|f| json!({"table": f.table, "column": f.column, "op": f.op})),
                    })),
                    "tabular": t.tabular.as_ref().map(|tt| json!({"target": tt.target, "model": tt.target.model(), "host": tt.target.host_label(), "last_limit": tt.last_limit, "force_xmla": tt.force_xmla, "transport_setting": self.settings.semantic.transport.clone(), "transport": tt.transport, "profile": tt.profile.map(|p| p.to_string()), "catalog": match self.state.model_catalogs.get(&tt.model_key()) { Some(crate::state::Loadable::Loaded(c)) => json!({"tables": c.tables.len(), "measures": c.measure_count(), "columns": c.column_count(), "expressions_hidden": c.expressions_hidden}), Some(crate::state::Loadable::Loading) => json!("loading"), Some(crate::state::Loadable::Failed(e)) => json!({"error": e}), _ => Value::Null }})),
                    "spark": t.spark.as_ref().map(|s| json!({"workspace_id": s.binding.as_ref().map(|b| b.workspace_id.clone()), "lakehouse_id": s.binding.as_ref().and_then(|b| b.lakehouse_id.clone()), "write_mode": s.binding.as_ref().map(|b| b.write_mode.clone()), "workspace_name": s.workspace_name, "lakehouse_name": s.lakehouse_name, "pending_run": t.pending_run.map(|m| format!("{m:?}"))})),
                    "file_path": t.file_path.as_ref().map(|p| p.to_string_lossy().to_string()),
                    "cells": cells,
                    "selected_cell": t.notebook.as_deref().map(|nb| nb.selected),
                    "kernel": t.notebook.as_deref().map(|nb| if nb.kernel == crate::state::NotebookKernel::Spark { "spark" } else { "connection" }),
                    "lakehouse": t.notebook.as_deref().and_then(|nb| nb.fabric.as_ref()).map(|b| json!({"workspace_id": b.workspace_id, "lakehouse_id": b.lakehouse_id, "write_mode": b.write_mode, "preload": b.preload})),
                    "fabric_item": t.fabric_item.as_ref().map(|fi| json!({"id": fi.item.id, "name": fi.item.display_name, "workspace_id": fi.item.workspace_id, "saving": fi.saving})),
                    "active": self.state.active_tab == Some(i),
                    "dirty": t.is_dirty(),
                    "profile": t.profile.as_ref().map(|p| p.display_name()),
                    "connection": match &t.conn {
                        ConnState::Disconnected => json!("disconnected"),
                        ConnState::Connecting => json!("connecting"),
                        ConnState::Connected { engine, spid, database } => json!({"engine": engine.short_label(), "version": engine.version, "spid": spid, "database": database}),
                        ConnState::Failed { error, hint } => json!({"failed": error, "hint": hint}),
                    },
                    "databases": match &t.databases {
                        Loadable::Loaded(d) => json!(d.len()),
                        Loadable::Loading => json!("loading"),
                        Loadable::Failed(e) => json!({"failed": e}),
                        Loadable::NotLoaded => json!(null),
                    },
                    "text": t.text,
                    "cursor": t.editor.cursor,
                    "selection_summary": crate::ui::shell::cached_summary(t),
                    "cursors": t.editor.cursors.sels.iter().map(|s| json!([s.anchor, s.head])).collect::<Vec<_>>(),
                    "primary": t.editor.cursors.primary,
                    "actual_plan": t.actual_plan,
                    "catalog_objects": t.catalog.as_ref().map(|c| c.objects.len()),
                    "catalog_columns": t.catalog.as_ref().map(|c| c.columns.values().map(|v| v.len()).sum::<usize>()),
                    "text_plan_sections": t.run.as_ref().and_then(|r| r.text_plan.as_ref()).map(|p| p.iter().map(|(t, _)| t.clone()).collect::<Vec<_>>()),
                    "capped": t.run.as_ref().map(|r| r.capped),
                    "run": run,
                })
            })
            .collect();
        json!({
            "tabs": tabs,
            "active_tab": self.state.active_tab,
            "profiles": self.state.library.profiles.iter().map(|p| json!({"id": p.id.to_string(), "name": p.display_name(), "server": p.server, "auth": p.auth.label(), "database": p.database})).collect::<Vec<_>>(),
            "groups": self.state.library.groups.iter().map(|g| json!({"id": g.id.to_string(), "name": g.name})).collect::<Vec<_>>(),
            "dialog": if self.state.dialog.is_open() { dialog_name(&self.state.dialog).to_string() } else { "none".into() },
            "kernel": kernel_json(&self.state.kernel),
            "toasts": self.state.recent_toasts.iter().cloned().collect::<Vec<_>>(),
            "sidebar": format!("{:?}", self.state.sidebar_view),
            "font_packs": crate::fonts::requested(),
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
        ConfirmFabricSave { .. } => "confirm_fabric_save",
        Connection(_) => "connection",
        Password { .. } => "password",
        AuthWaiting { .. } => "auth_waiting",
        Group { .. } => "group",
        ConfirmClose { .. } => "confirm_close",
        ConfirmCloseMany { .. } => "confirm_close_many",
        RunWithParameters { .. } => "run_with_parameters",
        TileEditor { .. } => "tile_editor",
        TabularProfile { .. } => "tabular_profile",
        PinTile { .. } => "pin_tile",
        ConfirmDeleteProfile { .. } => "confirm_delete_profile",
        ConfirmDeleteGroup { .. } => "confirm_delete_group",
        ConfirmWrite { .. } => "confirm_write",
        Export(_) => "export",
        Import(_) => "import",
        ChangeConnection { .. } => "change_connection",
        ExecOptions { .. } => "exec_options",
        Rename { .. } => "rename",
        SparkLakehouse { .. } => "spark_lakehouse",
        ChooseModel { .. } => "choose_model",
        AdsImport { .. } => "ads_import",
        UpdateAvailable { .. } => "update_available",
    }
}

fn kernel_json(k: &crate::kernel::KernelUi) -> Value {
    use crate::kernel::KernelState as K;
    let (state, since, info) = match &k.state {
        K::Stopped => ("stopped", None, None),
        K::Starting { since } => ("starting", Some(since.elapsed().as_secs()), None),
        K::Ready { info, since } => ("ready", Some(since.elapsed().as_secs()), Some(info.clone())),
        K::Failed(_) => ("failed", None, None),
    };
    json!({
        "state": state,
        "error": if let K::Failed(e) = &k.state { Some(e.clone()) } else { None },
        "uptime_s": since,
        "busy": k.busy.as_ref().map(|(t, c)| json!({"tab": t.to_string(), "cell": c})),
        "waiting": k.waiting.len(),
        "profile": k.profile,
        "engine": k.engine.key(),
        "engine_version": k.engine_version,
        "info": info,
        "log_tail": k.log.iter().rev().take(20).cloned().collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>(),
        "fabric": k.fabric.as_ref().map(|f| json!({"workspace_id": f.workspace_id, "workspace": f.workspace_name, "lakehouses": f.lakehouses, "default_lakehouse": f.default_lakehouse, "write_mode": f.write_mode})),
        "pending_fabric_start": k.pending_fabric_start.is_some(),
        "control": k.control.is_some(),
        "native_arrow": k.native_arrow,
        "features": k.features.iter().cloned().collect::<Vec<_>>(),
        "contexts": k.contexts.len(),
        "idle_s": k.idle().as_secs(),
        "last_call": k.last_call,
        "interrupting": k.interrupting.map(|t| t.elapsed().as_secs()),
        "last_interrupt": k.last_interrupt.as_ref().map(|(s, r)| json!({"secs": s, "result": r})),
        "protocol_version": if let K::Ready { info, .. } = &k.state { info.get("protocol_version").cloned() } else { None },
        "token_requests": k.token_requests(),
        "token_error": k.token_error(),
        "catalog": k.sail_catalog.as_ref().map(|c| json!({"url": c.url, "requests": c.requests.load(std::sync::atomic::Ordering::Relaxed), "upstream_calls": c.upstream_calls.load(std::sync::atomic::Ordering::Relaxed), "last_error": c.last_error.lock().clone(), "lakehouses": c.lakehouses().iter().map(|l| l.name.clone()).collect::<Vec<_>>()})),
    })
}

/// Apply the agent's chart arguments to a result set (`off` removes the chart) and answer with
/// its state. Shared by `grid {action: chart}` and `notebook {action: chart}`.
fn apply_chart_args(v: &mut crate::state::ResultSetView, args: Option<&Value>, fmt: &cobalt_results::CellFormatter) -> Result<Value, String> {
    let flag = |k: &str| args.and_then(|a| a.get(k)).and_then(|b| b.as_bool());
    if flag("off") == Some(true) {
        v.chart = None;
        return Ok(json!({"on": false}));
    }
    let rs = v.rs.clone();
    let c = v.chart.get_or_insert_with(|| crate::state::ChartView::new(cobalt_results::chart::default_spec(&rs, &[])));
    if let Some(k) = arg_str(args, "kind") {
        match cobalt_core::ChartKind::parse(&k) {
            Some(k) => c.spec.kind = k,
            None => return Err(format!("unknown chart kind {k:?} (bar, histogram, line, area, scatter, pie)")),
        }
    }
    if let Some(x) = args.and_then(|a| a.get("x")) {
        c.spec.x = match x {
            Value::Null => None,
            Value::String(s) if s.is_empty() || s == "row" || s == "row number" => None,
            Value::String(s) => Some(s.clone()),
            other => Some(other.to_string()),
        };
    }
    if let Some(y) = args.and_then(|a| a.get("y")) {
        c.spec.y = match y {
            Value::Array(a) => a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
            Value::String(s) => vec![s.clone()],
            Value::Null => Vec::new(),
            _ => c.spec.y.clone(),
        };
    }
    if let Some(a) = arg_str(args, "agg") {
        match cobalt_core::Agg::parse(&a) {
            Some(a) => c.spec.agg = a,
            None => return Err(format!("unknown aggregation {a:?} (none, sum, count, avg, min, max, count_distinct)")),
        }
    }
    if let Some(t) = args.and_then(|a| a.get("top_n")) {
        c.spec.top_n = t.as_u64().map(|n| n as usize);
    }
    if let Some(t) = args.and_then(|a| a.get("bins")) {
        c.spec.bins = t.as_u64().map(|n| n as usize);
    }
    if let Some(b) = flag("stacked") {
        c.spec.stacked = b;
    }
    if let Some(b) = flag("horizontal") {
        c.spec.horizontal = b;
    }
    if c.spec.y.is_empty() && c.spec.agg != cobalt_core::Agg::Count {
        c.spec.agg = cobalt_core::Agg::Count;
    }
    crate::ui::results::chart::ensure_data(v, fmt);
    Ok(chart_state_json(v))
}

/// A result set's chart for the agent: the spec and a summary of the data (first 50 points).
fn chart_state_json(v: &crate::state::ResultSetView) -> Value {
    match &v.chart {
        None => json!({"on": false}),
        Some(c) => {
            let data = c.data.as_ref().map(|d| {
                json!({
                    "points": d.points(), "total_rows": d.total_rows, "distinct_x": d.distinct_x, "has_other": d.has_other, "error": d.error,
                    "x_kind": format!("{:?}", d.x_kind).to_lowercase(), "x_name": d.x_name,
                    "x": (0..d.points().min(50)).map(|i| d.x_label(i)).collect::<Vec<_>>(),
                    "series": d.series.iter().map(|s| json!({"name": s.name, "values": s.values.iter().take(50).map(|v| if v.is_nan() { Value::Null } else { json!(v) }).collect::<Vec<_>>(), "len": s.values.len()})).collect::<Vec<_>>(),
                    "build_ms": c.build_ms,
                })
            });
            json!({"on": true, "spec": serde_json::to_value(&c.spec).unwrap_or(Value::Null), "data": data, "rect": c.rect.map(|r| [r.min.x, r.min.y, r.max.x, r.max.y])})
        }
    }
}

fn arg_str(args: Option<&Value>, key: &str) -> Option<String> {
    args.and_then(|a| a.get(key)).and_then(|v| v.as_str()).map(str::to_string)
}
impl CobaltApp {
    /// A saved Analysis Services profile by id or name.
    fn find_tabular_profile(&self, key: &str) -> Option<uuid::Uuid> {
        self.state.tabular.profiles.iter().find(|p| p.id.to_string() == key || p.name.eq_ignore_ascii_case(key)).map(|p| p.id)
    }
}

fn arg_bool(args: Option<&Value>, key: &str) -> Option<bool> {
    args.and_then(|a| a.get(key)).and_then(|v| v.as_bool())
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
            "viewport" => {
                // {w, h} in logical points: resize the window (perf experiments)
                let num = |k: &str| args.and_then(|a| a.get(k)).and_then(|v| v.as_f64());
                let (Some(w), Some(h)) = (num("w"), num("h")) else { return ActionResult::BadArgs("w and h are required".into()) };
                egui.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(w as f32, h as f32)));
                egui.request_repaint();
                ActionResult::ok()
            }
            "spin" => {
                // {ms}: repaint continuously for that long, then read `perf` for the real frame rate
                let ms = args.and_then(|a| a.get("ms")).and_then(|v| v.as_u64()).unwrap_or(3000);
                self.spin_until = Some(std::time::Instant::now() + std::time::Duration::from_millis(ms));
                egui.request_repaint();
                ActionResult::ok()
            }
            "perf" => {
                // last 2-second window: frames per second, mean / max frame CPU ms, adapter in use
                let (fps, mean_ms, max_ms) = self.perf.last;
                ActionResult::with(&json!({
                    "fps": fps,
                    "mean_frame_ms": mean_ms,
                    "max_frame_ms": max_ms,
                    "wall_ms": self.perf.last_wall_ms,
                    "total_frames": self.perf.total_frames,
                    "adapter": crate::gpu::adapter_label(),
                    "software": crate::gpu::is_software(),
                }))
            }
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
                self.with_ctx(egui, ops::load_library);
                ActionResult::with(&json!({"profile_id": id}))
            }
            "connect" => {
                let Some(key) = arg_str(args, "profile") else { return ActionResult::BadArgs("profile is required".into()) };
                let Some(p) = self.find_profile(&key) else { return ActionResult::BadArgs("profile not found".into()) };
                let db = arg_str(args, "database");
                let idx = self.with_ctx(egui, |s, cx| ops::new_query_tab(s, cx, Some(p.id), db, None, false));
                ActionResult::with(&json!({"tab": idx}))
            }
            "spark_query" => {
                let text = arg_str(args, "text");
                let binding = match arg_str(args, "workspace") {
                    None => None,
                    Some(ws_arg) => {
                        let ws_id = self.state.fabric.workspaces.get().and_then(|v| v.iter().find(|w| w.id == ws_arg || w.display_name.eq_ignore_ascii_case(&ws_arg)).map(|w| w.id.clone()));
                        let Some(ws_id) = ws_id else { return ActionResult::BadArgs("unknown workspace (load the Fabric panel first)".into()) };
                        let lh_id = match arg_str(args, "lakehouse") {
                            Some(l) => match self.state.fabric.lakehouses(&ws_id).and_then(|v| v.into_iter().find(|(n, id)| *id == l || n.eq_ignore_ascii_case(&l)).map(|(_, id)| id)) {
                                Some(id) => Some(id),
                                None => return ActionResult::BadArgs("unknown lakehouse (workspace items not loaded?)".into()),
                            },
                            None => None,
                        };
                        Some(crate::state::NotebookFabric { workspace_id: ws_id, lakehouse_id: lh_id, write_mode: arg_str(args, "write_mode").unwrap_or_else(|| "sandbox".into()), preload: false })
                    }
                };
                let idx = self.with_ctx(egui, |s, cx| crate::sparkq::new_tab(s, cx, binding, None, text));
                ActionResult::with(&json!({"tab": idx, "title": self.state.tabs[idx].title}))
            }
            "explore" => {
                use cobalt_core::explore::{FieldRef, FilterOp, FilterSpec, SortBy, SortSpec};
                let action = arg_str(args, "action").unwrap_or_else(|| "state".into());
                let parse_field = |s: &str| -> Option<FieldRef> {
                    let s = s.trim();
                    if let Some(m) = s.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
                        return Some(FieldRef::Measure { name: m.to_string() });
                    }
                    let (t, c) = s.split_once('[')?;
                    Some(FieldRef::Column { table: t.trim_matches('\'').to_string(), column: c.trim_end_matches(']').to_string() })
                };
                if action == "new" {
                    if let Some(pid) = arg_str(args, "profile").and_then(|p| self.find_tabular_profile(&p)) {
                        let Some(model) = arg_str(args, "model") else { return ActionResult::BadArgs("model is required".into()) };
                        let idx = self.with_ctx(egui, |s, cx| crate::tabular_root::explore_tab(s, cx, pid, &model));
                        return match idx {
                            Some(i) => ActionResult::with(&json!({"tab": i, "title": self.state.tabs[i].title})),
                            None => ActionResult::BadArgs("unknown profile".into()),
                        };
                    }
                    let Some(ws_arg) = arg_str(args, "workspace") else { return ActionResult::BadArgs("workspace is required".into()) };
                    let Some(ws_id) = self.state.fabric.workspaces.get().and_then(|v| v.iter().find(|w| w.id == ws_arg || w.display_name.eq_ignore_ascii_case(&ws_arg)).map(|w| w.id.clone())) else {
                        return ActionResult::BadArgs("unknown workspace (load the Fabric panel first)".into());
                    };
                    let Some(m_arg) = arg_str(args, "model") else { return ActionResult::BadArgs("model is required".into()) };
                    let item = self.state.fabric.semantic_models.get(&ws_id).and_then(|l| l.get()).and_then(|v| v.iter().find(|i| i.id == m_arg || i.display_name.eq_ignore_ascii_case(&m_arg)).cloned());
                    let Some(item) = item else { return ActionResult::BadArgs("unknown semantic model (expand the workspace first)".into()) };
                    let target = self.state.fabric.model_target(&item);
                    let idx = self.with_ctx(egui, |s, cx| crate::exploreq::new_tab(s, cx, target, None, None));
                    return ActionResult::with(&json!({"tab": idx, "title": self.state.tabs[idx].title}));
                }
                if action == "open" {
                    let Some(path) = arg_str(args, "path") else { return ActionResult::BadArgs("path is required".into()) };
                    let idx = self.with_ctx(egui, |s, cx| crate::exploreq::open_path(s, cx, std::path::Path::new(&path)));
                    return match idx {
                        Some(i) => ActionResult::with(&json!({"tab": i, "title": self.state.tabs[i].title})),
                        None => ActionResult::BadArgs("could not open the file".into()),
                    };
                }
                let Some(idx) = self.state.active_tab.filter(|&i| self.state.tabs.get(i).map(|t| t.is_explore()).unwrap_or(false)) else {
                    return ActionResult::BadArgs("the active tab is not an Explore tab".into());
                };
                match action.as_str() {
                    "state" => {}
                    "add" => {
                        let Some(field) = arg_str(args, "field").and_then(|s| parse_field(&s)) else { return ActionResult::BadArgs("field is required: Table[Column] or [Measure]".into()) };
                        let well = match arg_str(args, "well").as_deref().unwrap_or("rows") {
                            "columns" => crate::exploreq::Well::Columns,
                            "values" => crate::exploreq::Well::Values,
                            "filters" => crate::exploreq::Well::Filters,
                            _ => crate::exploreq::Well::Rows,
                        };
                        self.with_ctx(egui, |s, cx| crate::exploreq::add_field(s, cx, idx, field, well));
                    }
                    // drop: what a drag-and-drop does — `field` lands in `well` before `before` (a field of that well), out of `from`
                    "drop" => {
                        let Some(field) = arg_str(args, "field").and_then(|s| parse_field(&s)) else { return ActionResult::BadArgs("field is required: Table[Column] or [Measure]".into()) };
                        let well_of = |s: Option<String>| -> Option<crate::exploreq::Well> {
                            match s.as_deref()? {
                                "rows" => Some(crate::exploreq::Well::Rows),
                                "columns" => Some(crate::exploreq::Well::Columns),
                                "values" => Some(crate::exploreq::Well::Values),
                                "filters" => Some(crate::exploreq::Well::Filters),
                                _ => None,
                            }
                        };
                        let to = well_of(arg_str(args, "well")).unwrap_or(crate::exploreq::Well::Rows);
                        let from = well_of(arg_str(args, "from"));
                        let before = arg_str(args, "before").and_then(|s| parse_field(&s));
                        self.with_ctx(egui, |s, cx| crate::exploreq::drop_field(s, cx, idx, field, from, to, before));
                    }
                    "remove" => {
                        let Some(field) = arg_str(args, "field").and_then(|s| parse_field(&s)) else { return ActionResult::BadArgs("field is required".into()) };
                        self.with_ctx(egui, |s, cx| crate::exploreq::remove_field(s, cx, idx, &field));
                    }
                    "agg" => {
                        let i = arg_usize(args, "index").unwrap_or(0);
                        let Some(agg) = arg_str(args, "agg").and_then(|a| cobalt_core::chart::Agg::parse(&a)) else { return ActionResult::BadArgs("agg is required (sum|count|avg|min|max|count_distinct)".into()) };
                        self.with_ctx(egui, |s, cx| crate::exploreq::set_agg(s, cx, idx, i, agg));
                    }
                    "sort" => {
                        let desc = arg_bool(args, "desc").unwrap_or(true);
                        let by = match (arg_str(args, "field"), arg_usize(args, "index")) {
                            (Some(f), _) => parse_field(&f).map(|field| SortBy::Field { field }),
                            (None, Some(i)) => Some(SortBy::Value { index: i }),
                            _ => None,
                        };
                        self.with_ctx(egui, |s, cx| crate::exploreq::set_sort(s, cx, idx, by.map(|by| SortSpec { by, desc })));
                    }
                    "top_n" => {
                        let n = args.and_then(|a| a.get("n")).and_then(|v| v.as_u64());
                        self.with_ctx(egui, |s, cx| crate::exploreq::set_top_n(s, cx, idx, n));
                    }
                    "subtotals" => {
                        let on = arg_bool(args, "on").unwrap_or(true);
                        self.with_ctx(egui, |s, cx| crate::exploreq::set_subtotals(s, cx, idx, on));
                    }
                    "filter" => {
                        let Some(FieldRef::Column { table, column }) = arg_str(args, "field").and_then(|s| parse_field(&s)) else { return ActionResult::BadArgs("field is required: Table[Column]".into()) };
                        let values: Vec<String> = args.and_then(|a| a.get("values")).and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string).or_else(|| x.as_f64().map(|n| n.to_string()))).collect()).unwrap_or_default();
                        let op = match arg_str(args, "op").as_deref().unwrap_or("in") {
                            "not_in" => FilterOp::NotIn { values },
                            "between" => FilterOp::Between { lo: arg_str(args, "lo"), hi: arg_str(args, "hi") },
                            "contains" => FilterOp::Contains { text: arg_str(args, "text").unwrap_or_default() },
                            "starts_with" => FilterOp::StartsWith { text: arg_str(args, "text").unwrap_or_default() },
                            "is_blank" => FilterOp::IsBlank,
                            "not_blank" => FilterOp::NotBlank,
                            "last_days" => FilterOp::RelativeDate { unit: cobalt_core::explore::DateUnit::Days, last: args.and_then(|a| a.get("n")).and_then(|v| v.as_u64()).unwrap_or(30) as u32 },
                            _ => FilterOp::In { values },
                        };
                        if let Some(ex) = self.state.tabs[idx].explore.as_deref_mut() {
                            ex.spec.filters.retain(|f| !(f.table == table && f.column == column));
                            ex.spec.filters.push(FilterSpec { table, column, op });
                        }
                        self.with_ctx(egui, |s, cx| crate::exploreq::changed(s, cx, idx));
                    }
                    "remove_filter" => {
                        let i = arg_usize(args, "index").unwrap_or(0);
                        self.with_ctx(egui, |s, cx| crate::exploreq::remove_filter(s, cx, idx, i));
                    }
                    "run" => self.with_ctx(egui, |s, cx| crate::exploreq::run(s, cx, idx)),
                    "dax" => {
                        self.with_ctx(egui, |s, _| crate::exploreq::regenerate(s, idx));
                    }
                    "matrix" => {
                        if let Some(ex) = self.state.tabs[idx].explore.as_deref_mut() {
                            ex.matrix = arg_bool(args, "on").unwrap_or(true);
                        }
                    }
                    "save" => {
                        if let Some(path) = arg_str(args, "path") {
                            self.state.tabs[idx].file_path = Some(std::path::PathBuf::from(path));
                        }
                        let ok = self.with_ctx(egui, |s, cx| crate::exploreq::save(s, cx, idx, false));
                        if !ok {
                            return ActionResult::BadArgs("save failed".into());
                        }
                    }
                    other => return ActionResult::BadArgs(format!("unknown explore action {other}")),
                }
                let t = &self.state.tabs[idx];
                let e = t.explore.as_deref();
                ActionResult::with(&json!({"tab": idx, "title": t.title, "summary": e.map(|e| crate::exploreq::describe(&e.spec)), "dax": e.and_then(|e| e.dax.as_ref().map(|q| q.text.clone())), "dax_error": e.and_then(|e| e.dax_error.clone()), "run": t.run.as_ref().map(|r| format!("{:?}", r.state).to_lowercase()), "file_path": t.file_path.as_ref().map(|p| p.to_string_lossy().to_string())}))
            }
            "dax_query" | "model" => {
                if let Some(pid) = arg_str(args, "profile").and_then(|p| self.find_tabular_profile(&p)) {
                    let Some(model) = arg_str(args, "model") else { return ActionResult::BadArgs("model is required".into()) };
                    if action.name() == "dax_query" {
                        let text = arg_str(args, "text");
                        let run = arg_bool(args, "run").unwrap_or(false);
                        let idx = self.with_ctx(egui, |s, cx| {
                            let i = crate::tabular_root::query_tab(s, cx, pid, &model);
                            if let (Some(i), Some(t)) = (i, text) {
                                s.tabs[i].text = t;
                                s.tabs[i].mark_saved();
                            }
                            if let (Some(i), true) = (i, run) {
                                ops::run(s, cx, i, crate::state::RunMode::All);
                            }
                            i
                        });
                        return match idx {
                            Some(i) => ActionResult::with(&json!({"tab": i, "title": self.state.tabs[i].title})),
                            None => ActionResult::BadArgs("unknown profile".into()),
                        };
                    }
                    let Some(target) = self.state.tabular.profile(pid).map(|p| p.target_for(&model)) else { return ActionResult::BadArgs("unknown profile".into()) };
                    match arg_str(args, "action").as_deref().unwrap_or("tree") {
                        "refresh" => {
                            self.with_ctx(egui, |s, cx| crate::tabularq::load_catalog(s, cx, &target, true));
                            return ActionResult::ok();
                        }
                        _ => {
                            self.with_ctx(egui, |s, cx| crate::tabularq::ensure_catalog(s, cx, &target));
                            return match self.state.model_catalogs.get(&target.key()) {
                                Some(crate::state::Loadable::Loaded(c)) => ActionResult::with(&json!({"status": "loaded", "key": target.key(), "catalog": c.as_ref()})),
                                Some(crate::state::Loadable::Failed(e)) => ActionResult::with(&json!({"status": "failed", "error": e})),
                                _ => ActionResult::with(&json!({"status": "loading"})),
                            };
                        }
                    }
                }
                let Some(ws_arg) = arg_str(args, "workspace") else { return ActionResult::BadArgs("workspace is required".into()) };
                let Some(ws_id) = self.state.fabric.workspaces.get().and_then(|v| v.iter().find(|w| w.id == ws_arg || w.display_name.eq_ignore_ascii_case(&ws_arg)).map(|w| w.id.clone())) else {
                    return ActionResult::BadArgs("unknown workspace (load the Fabric panel first)".into());
                };
                let Some(m_arg) = arg_str(args, "model") else { return ActionResult::BadArgs("model is required".into()) };
                let item = self.state.fabric.semantic_models.get(&ws_id).and_then(|l| l.get()).and_then(|v| v.iter().find(|i| i.id == m_arg || i.display_name.eq_ignore_ascii_case(&m_arg)).cloned());
                let Some(item) = item else { return ActionResult::BadArgs("unknown semantic model (expand the workspace first)".into()) };
                let target = self.state.fabric.model_target(&item);
                if action.name() == "dax_query" {
                    let text = arg_str(args, "text");
                    let run = arg_bool(args, "run").unwrap_or(false);
                    let idx = self.with_ctx(egui, |s, cx| {
                        let i = crate::tabularq::new_tab(s, cx, target, text);
                        if run {
                            ops::run(s, cx, i, crate::state::RunMode::All);
                        }
                        i
                    });
                    return ActionResult::with(&json!({"tab": idx, "title": self.state.tabs[idx].title}));
                }
                match arg_str(args, "action").as_deref().unwrap_or("tree") {
                    "refresh" => {
                        self.with_ctx(egui, |s, cx| crate::tabularq::load_catalog(s, cx, &target, true));
                        ActionResult::ok()
                    }
                    "functions" => {
                        let f = self.state.dax_functions.get(&target.key()).map(|f| f.iter().map(|x| json!({"name": x.name, "parameters": x.parameters})).collect::<Vec<_>>()).unwrap_or_default();
                        ActionResult::with(&json!({"count": f.len(), "functions": f}))
                    }
                    _ => {
                        self.with_ctx(egui, |s, cx| crate::tabularq::ensure_catalog(s, cx, &target));
                        match self.state.model_catalogs.get(&target.key()) {
                            Some(crate::state::Loadable::Loaded(c)) => ActionResult::with(&json!({"status": "loaded", "key": target.key(), "catalog": c.as_ref()})),
                            Some(crate::state::Loadable::Failed(e)) => ActionResult::with(&json!({"status": "failed", "error": e})),
                            _ => ActionResult::with(&json!({"status": "loading"})),
                        }
                    }
                }
            }
            "tabular_profile" => {
                use cobalt_core::tabular::{TabularAuthKind, TabularProfile, TabularTarget};
                let action = arg_str(args, "action").unwrap_or_else(|| "list".into());
                match action.as_str() {
                    "list" => {
                        self.with_ctx(egui, crate::tabular_root::ensure_loaded);
                        let list: Vec<Value> = self.state.tabular.profiles.iter().map(|p| json!({"id": p.id.to_string(), "name": p.name, "kind": p.kind_label(), "target": p.target, "auth": p.auth, "catalogs": self.state.tabular.catalogs.get(&p.id).map(|l| match l { crate::state::Loadable::Loaded(c) => json!(c), crate::state::Loadable::Loading => json!("loading"), crate::state::Loadable::Failed(e) => json!({"error": e}), _ => Value::Null })})).collect();
                        ActionResult::with(&json!({"profiles": list}))
                    }
                    "add" => {
                        let name = arg_str(args, "name").unwrap_or_else(|| "Analysis Services".into());
                        let model = arg_str(args, "model").unwrap_or_default();
                        let target = match arg_str(args, "kind").as_deref().unwrap_or("asazure") {
                            "http" => TabularTarget::Http { url: arg_str(args, "url").unwrap_or_default(), model },
                            "powerbi" => TabularTarget::PowerBi { workspace_id: arg_str(args, "workspace_id").unwrap_or_default(), workspace_name: arg_str(args, "workspace").unwrap_or_default(), dataset_id: String::new(), model_name: model },
                            _ => TabularTarget::AzureAs { region: arg_str(args, "region").unwrap_or_else(|| "westus".into()), server: arg_str(args, "server").unwrap_or_default(), model },
                        };
                        let auth = match arg_str(args, "auth").as_deref().unwrap_or("entra") {
                            "basic" => TabularAuthKind::Basic { user: arg_str(args, "user").unwrap_or_default() },
                            "anonymous" => TabularAuthKind::Anonymous,
                            _ => TabularAuthKind::Entra,
                        };
                        let profile = TabularProfile::new(name, target, auth);
                        let id = profile.id;
                        let pw = arg_str(args, "password");
                        self.with_ctx(egui, |s, cx| crate::tabular_root::save_profile(s, cx, profile, pw));
                        ActionResult::with(&json!({"id": id.to_string()}))
                    }
                    "remove" => {
                        let Some(pid) = arg_str(args, "id").or_else(|| arg_str(args, "name")).and_then(|p| self.find_tabular_profile(&p)) else { return ActionResult::BadArgs("unknown profile".into()) };
                        self.with_ctx(egui, |s, cx| crate::tabular_root::remove_profile(s, cx, pid));
                        ActionResult::ok()
                    }
                    "catalogs" => {
                        let Some(pid) = arg_str(args, "id").or_else(|| arg_str(args, "name")).and_then(|p| self.find_tabular_profile(&p)) else { return ActionResult::BadArgs("unknown profile".into()) };
                        self.state.tabular.expanded = true;
                        self.state.tabular.expanded_profiles.insert(pid);
                        let force = arg_bool(args, "refresh").unwrap_or(false);
                        self.with_ctx(egui, |s, cx| crate::tabular_root::load_catalogs(s, cx, pid, force));
                        match self.state.tabular.catalogs.get(&pid) {
                            Some(crate::state::Loadable::Loaded(c)) => ActionResult::with(&json!({"status": "loaded", "catalogs": c})),
                            Some(crate::state::Loadable::Failed(e)) => ActionResult::with(&json!({"status": "failed", "error": e})),
                            _ => ActionResult::with(&json!({"status": "loading"})),
                        }
                    }
                    "test" => {
                        let Some(pid) = arg_str(args, "id").or_else(|| arg_str(args, "name")).and_then(|p| self.find_tabular_profile(&p)) else { return ActionResult::BadArgs("unknown profile".into()) };
                        let Some(profile) = self.state.tabular.profile(pid).cloned() else { return ActionResult::BadArgs("unknown profile".into()) };
                        self.state.dialog = crate::state::Dialog::TabularProfile { draft: Box::new(profile.clone()), password: String::new(), editing: true, test: None };
                        self.with_ctx(egui, |s, cx| crate::tabular_root::test_profile(s, cx, profile, None));
                        ActionResult::with(&json!({"status": "testing", "note": "read the dialog's outcome with `state` (dialog) or `tabular_profile list`"}))
                    }
                    other => ActionResult::BadArgs(format!("unknown tabular_profile action {other}")),
                }
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
            "break_connection" => {
                // the active tab's session treats its connection as dead at the next command
                if let Some(i) = self.state.active_tab {
                    let tab = self.state.tabs[i].id;
                    self.with_ctx(egui, |_, cx| cx.session.send(crate::session::Command::SimulateLost { tab }));
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
            "export" | "run_to_export" | "import_to" => {
                let run_to_file = action.name() == "run_to_export";
                // import_to: the open Import dialog's file goes to the target instead of a table
                let importing = action.name() == "import_to";
                let Some(i) = self.state.active_tab else { return ActionResult::BadArgs("no active tab".into()) };
                if importing && !matches!(self.state.dialog, crate::state::Dialog::Import(_)) {
                    return ActionResult::BadArgs("open the Import dialog first (import {path, destination: \"file\"})".into());
                }
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
                    if importing {
                        ops::open_import_export(s, cx);
                    } else {
                        ops::open_export_dialog(s, cx, i, set, false);
                    }
                    if let crate::state::Dialog::Export(d) = &mut s.dialog {
                        d.format_index = fi;
                        if !path.is_empty() {
                            d.path = path;
                        }
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
                        if run_to_file {
                            d.run_mode = Some(RunMode::All);
                        }
                    }
                    if importing {
                        ops::start_import_export(s, cx);
                    } else if run_to_file {
                        ops::start_run_export(s, cx);
                    } else {
                        ops::start_export(s, cx);
                    }
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
                self.state.focus = crate::state::Focus::Results;
                let Some(t) = self.state.active_mut() else { return ActionResult::BadArgs("no active tab".into()) };
                let Some(r) = t.run.as_mut() else { return ActionResult::BadArgs("no run".into()) };
                if r.result_sets.iter().filter(|s| !s.is_plan).count() <= set {
                    return ActionResult::BadArgs("no such result set".into());
                }
                for s in r.result_sets.iter_mut() {
                    s.grid.focused = false;
                }
                let run_id = r.id;
                let tab_id = t.id;
                let v = r.result_sets.iter_mut().filter(|s| !s.is_plan).nth(set).unwrap();
                v.grid.focused = true;
                // like a click: the grid takes egui's keyboard focus away from the editor
                let set_index = r.result_sets.iter().position(|s| s.grid.focused).unwrap_or(set);
                egui.memory_mut(|m| m.request_focus(egui::Id::new(("grid", tab_id, run_id, set_index)).with("kb-focus")));
                let v = r.result_sets.iter_mut().filter(|s| !s.is_plan).nth(set).unwrap();
                v.grid.anchor = Some((row, col));
                // optional row2/col2 select a rectangle
                let row2 = arg_usize(args, "row2").unwrap_or(row);
                let col2 = arg_usize(args, "col2").unwrap_or(col);
                v.grid.selection = crate::state::Selection::Cells { r0: row.min(row2), c0: col.min(col2), r1: row.max(row2), c1: col.max(col2) };
                v.grid.scroll_to = Some((row, col));
                ActionResult::ok()
            }
            "open_path" => {
                let Some(path) = arg_str(args, "path") else { return ActionResult::BadArgs("path is required".into()) };
                self.with_ctx(egui, |s, cx| ops::open_file(s, cx, Some(std::path::PathBuf::from(path))));
                ActionResult::with(&json!({"tab": self.state.active_tab}))
            }
            // drop: what dropping files on the window does — {paths: [..], classify?: true (report the plan only)}
            "drop" => {
                let paths: Vec<std::path::PathBuf> = args.and_then(|a| a.get("paths")).and_then(|v| v.as_array()).map(|v| v.iter().filter_map(|p| p.as_str()).map(std::path::PathBuf::from).collect()).unwrap_or_default();
                if paths.is_empty() {
                    return ActionResult::BadArgs("paths is required".into());
                }
                let plan: Vec<Value> = paths.iter().map(|p| json!({"path": p, "action": format!("{:?}", crate::drop::classify(p))})).collect();
                let only_plan = args.and_then(|a| a.get("classify")).and_then(Value::as_bool).unwrap_or(false);
                // hover: true shows the overlay for these paths (until a drop or {hover: false})
                if let Some(h) = args.and_then(|a| a.get("hover")).and_then(Value::as_bool) {
                    self.state.simulated_hover = if h { paths.clone() } else { Vec::new() };
                    egui.request_repaint();
                    return ActionResult::with(&json!({"plan": plan, "hovering": h}));
                }
                self.state.simulated_hover.clear();
                let opened = if only_plan { 0 } else { self.with_ctx(egui, |s, cx| crate::drop::drop_files(s, cx, &paths)) };
                ActionResult::with(&json!({"plan": plan, "opened": opened, "tab": self.state.active_tab, "dialog": dialog_name(&self.state.dialog)}))
            }
            "complete" => {
                let Some(t) = self.state.active_mut() else { return ActionResult::BadArgs("no active tab".into()) };
                if let Some(text) = arg_str(args, "append") {
                    t.text.push_str(&text);
                }
                let cursor = t.text.len();
                t.editor.cursor = t.text.chars().count();
                t.editor.pending_edit = Some(crate::state::PendingEdit::SetCursor(t.editor.cursor));
                crate::ui::editor::open_completion(&mut t.host(), cursor, egui::pos2(460.0, 120.0), true);
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
            "pointer" => {
                // {action: click|rclick|dblclick|drag|move|scroll, x, y, x2?, y2?, dy?, dx?, shift?, ctrl?, alt?} in screenshot pixels.
                // Each step lands in its own frame via raw_input_hook, so egui treats it like a real mouse.
                let act = arg_str(args, "action").unwrap_or_else(|| "click".into());
                let num = |k: &str| args.and_then(|a| a.get(k)).and_then(|v| v.as_f64());
                let flag = |k: &str| args.and_then(|a| a.get(k)).and_then(|v| v.as_bool()).unwrap_or(false);
                let (Some(x), Some(y)) = (num("x"), num("y")) else { return ActionResult::BadArgs("x and y are required".into()) };
                let ppp = egui.pixels_per_point();
                let at = |x: f64, y: f64| egui::pos2(x as f32 / ppp, y as f32 / ppp);
                let modifiers = egui::Modifiers { alt: flag("alt"), ctrl: flag("ctrl"), shift: flag("shift"), mac_cmd: false, command: flag("ctrl") };
                let button = if act == "rclick" { egui::PointerButton::Secondary } else { egui::PointerButton::Primary };
                let press = |pos: egui::Pos2, pressed: bool| egui::Event::PointerButton { pos, button, pressed, modifiers };
                let mut steps: Vec<Vec<egui::Event>> = vec![vec![egui::Event::PointerMoved(at(x, y))]];
                match act.as_str() {
                    "move" => {}
                    // down / up: half a gesture, so a drag can pause mid-way (a screenshot of the drag state)
                    "down" => steps.push(vec![press(at(x, y), true)]),
                    "up" => steps.push(vec![press(at(x, y), false)]),
                    // scroll: a mouse wheel at (x, y); dy in points, positive scrolls the content down
                    "scroll" => {
                        let dy = args.and_then(|a| a.get("dy")).and_then(|v| v.as_f64()).unwrap_or(300.0) as f32;
                        let dx = args.and_then(|a| a.get("dx")).and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
                        steps.push(vec![egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: egui::vec2(-dx, -dy), modifiers, phase: egui::TouchPhase::Move }]);
                    }
                    "click" | "rclick" => {
                        steps.push(vec![press(at(x, y), true)]);
                        steps.push(vec![press(at(x, y), false)]);
                    }
                    "dblclick" | "tripleclick" => {
                        for _ in 0..(if act == "dblclick" { 2 } else { 3 }) {
                            steps.push(vec![press(at(x, y), true)]);
                            steps.push(vec![press(at(x, y), false)]);
                        }
                    }
                    // drag: press, move, release; dbldrag / tripledrag: click(s) first, then press
                    // and drag from the same point (word / line selection extended by dragging)
                    "drag" | "dbldrag" | "tripledrag" => {
                        let (Some(x2), Some(y2)) = (num("x2"), num("y2")) else { return ActionResult::BadArgs("x2 and y2 are required for drag".into()) };
                        let pre = match act.as_str() {
                            "dbldrag" => 1,
                            "tripledrag" => 2,
                            _ => 0,
                        };
                        for _ in 0..pre {
                            steps.push(vec![press(at(x, y), true)]);
                            steps.push(vec![press(at(x, y), false)]);
                        }
                        steps.push(vec![press(at(x, y), true)]);
                        let n = 8;
                        for i in 1..=n {
                            let t = i as f64 / n as f64;
                            steps.push(vec![egui::Event::PointerMoved(at(x + (x2 - x) * t, y + (y2 - y) * t))]);
                        }
                        steps.push(vec![press(at(x2, y2), false)]);
                    }
                    _ => return ActionResult::BadArgs("unknown pointer action".into()),
                }
                // modifiers must hold for every step of the gesture (moves included)
                if modifiers != egui::Modifiers::default() {
                    for s in &mut steps {
                        s.insert(0, egui::Event::ModifiersChanged(modifiers));
                    }
                }
                self.state.injected_pointer.extend(steps);
                egui.request_repaint();
                ActionResult::ok()
            }
            "import" => {
                // {path, table, schema?, existing?: bool, delimiter?, header?: bool, types?: {col: "sql type"}, exclude?: [col]}
                let Some(i) = self.state.active_tab else { return ActionResult::BadArgs("no active tab".into()) };
                let Some(path) = arg_str(args, "path") else { return ActionResult::BadArgs("path is required".into()) };
                let table = arg_str(args, "table");
                let schema = arg_str(args, "schema");
                let existing = args.and_then(|a| a.get("existing")).and_then(|v| v.as_bool()).unwrap_or(false);
                let delimiter = arg_str(args, "delimiter");
                let header = args.and_then(|a| a.get("header")).and_then(|v| v.as_bool());
                let types: Vec<(String, String)> = args.and_then(|a| a.get("types")).and_then(|v| v.as_object()).map(|m| m.iter().filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string()))).collect()).unwrap_or_default();
                let exclude: Vec<String> = args.and_then(|a| a.get("exclude")).and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default();
                let to_file = arg_str(args, "destination").map(|d| d == "file").unwrap_or(false);
                let start_now = !existing && !to_file; // existing mode needs the table's columns first; file mode waits for import_to
                self.with_ctx(egui, |s, cx| {
                    ops::open_import_dialog(s, cx, i, Some(std::path::PathBuf::from(&path)));
                    if let crate::state::Dialog::Import(d) = &mut s.dialog {
                        if let Some(t) = table { d.table_name = t; }
                        if let Some(sc) = schema { d.schema_name = sc; }
                        let mut re = false;
                        if let Some(dl) = delimiter { d.delimiter = dl; re = true; }
                        if let Some(h) = header { d.has_header = h; re = true; }
                        if re { ops::inspect_import(d); }
                        for (k, v) in &types {
                            if let Some(c) = d.columns.iter_mut().find(|c| c.name.eq_ignore_ascii_case(k)) { c.sql_type = v.clone(); }
                        }
                        for k in &exclude {
                            if let Some(c) = d.columns.iter_mut().find(|c| c.name.eq_ignore_ascii_case(k)) { c.include = false; }
                        }
                        d.existing = existing;
                        if to_file {
                            d.destination = 1;
                        }
                    }
                    if existing {
                        ops::import_request_existing_columns(s, cx);
                    } else if start_now {
                        ops::start_import(s, cx);
                    }
                });
                ActionResult::ok()
            }
            "dashboard" => {
                // {action: new|open|save|state|add_tile|update_tile|remove_tile|refresh|pin|home, path?, tile?, title?, sql?, profile?, database?, kind?, interval_secs?, size?, set?, target?, chart?}
                let Some(action) = arg_str(args, "action") else { return ActionResult::BadArgs("action is required".into()) };
                let dash_idx = || self.state.active_tab.filter(|i| self.state.tabs.get(*i).map(|t| t.is_dashboard()).unwrap_or(false));
                match action.as_str() {
                    "new" => {
                        let mut file = cobalt_core::DashboardFile::default();
                        if let Some(t) = arg_str(args, "title") {
                            file.title = t;
                        }
                        let i = self.with_ctx(egui, |s, cx| crate::dashboard::new_tab(s, cx, file, None, None));
                        ActionResult::with(&json!({"tab": i}))
                    }
                    "open" => {
                        let Some(path) = arg_str(args, "path") else { return ActionResult::BadArgs("path is required".into()) };
                        let ok = self.with_ctx(egui, |s, cx| crate::dashboard::open_path(s, cx, std::path::PathBuf::from(path)));
                        ActionResult::with(&json!({"opened": ok, "tab": self.state.active_tab}))
                    }
                    "home" => {
                        let Some(key) = arg_str(args, "profile") else { return ActionResult::BadArgs("profile is required".into()) };
                        let Some(p) = self.find_profile(&key) else { return ActionResult::BadArgs("profile not found".into()) };
                        let id = p.id;
                        self.with_ctx(egui, |s, cx| crate::dashboard::open_home(s, cx, id));
                        ActionResult::with(&json!({"tab": self.state.active_tab}))
                    }
                    "pin" => {
                        // the active query tab's result set `set` onto the dashboard `target` (tab index; omit = new)
                        let Some(i) = self.state.active_tab else { return ActionResult::BadArgs("no active tab".into()) };
                        let set = arg_usize(args, "set").unwrap_or(0);
                        let title = arg_str(args, "title").unwrap_or_else(|| self.state.tabs[i].title.clone());
                        let target = arg_usize(args, "target");
                        let interval = args.and_then(|a| a.get("interval_secs")).and_then(|v| v.as_u64()).unwrap_or(300);
                        let kind = arg_str(args, "kind").and_then(|k| cobalt_core::TileKind::parse(&k)).unwrap_or_else(|| crate::dashboard::suggested_kind(&self.state, i, set));
                        self.with_ctx(egui, |s, cx| crate::dashboard::pin(s, cx, i, set, title, target, interval, kind));
                        ActionResult::with(&json!({"tab": self.state.active_tab}))
                    }
                    _ => {
                        let Some(i) = dash_idx() else { return ActionResult::BadArgs("the active tab is not a dashboard".into()) };
                        match action.as_str() {
                            "save" => {
                                if let Some(p) = arg_str(args, "path") {
                                    self.state.tabs[i].file_path = Some(std::path::PathBuf::from(p));
                                }
                                self.with_ctx(egui, |s, cx| crate::dashboard::save(s, cx, i, false));
                                ActionResult::with(&json!({"file_path": self.state.tabs[i].file_path, "dirty": self.state.tabs[i].is_dirty()}))
                            }
                            "add_tile" | "update_tile" => {
                                let d = self.state.tabs[i].dashboard.as_deref().unwrap();
                                let existing = arg_usize(args, "tile").and_then(|k| d.tiles.get(k).map(|t| t.spec.clone()));
                                let mut spec = existing.clone().unwrap_or_default();
                                if let Some(t) = arg_str(args, "title") { spec.title = t; }
                                if let Some(s) = arg_str(args, "sql") { spec.sql = s; }
                                if let Some(db) = arg_str(args, "database") { spec.database = Some(db); }
                                if let Some(k) = arg_str(args, "kind") { spec.kind = cobalt_core::TileKind::parse(&k).unwrap_or(spec.kind); }
                                if let Some(sz) = arg_str(args, "size") { spec.size = cobalt_core::TileSize::parse(&sz).unwrap_or(spec.size); }
                                if let Some(v) = args.and_then(|a| a.get("interval_secs")).and_then(|v| v.as_u64()) { spec.interval_secs = v; }
                                if let Some(c) = args.and_then(|a| a.get("chart")) { spec.chart = serde_json::from_value(c.clone()).ok(); }
                                if let Some(key) = arg_str(args, "profile") {
                                    let Some(p) = self.find_profile(&key) else { return ActionResult::BadArgs("profile not found".into()) };
                                    spec.profile = crate::dashboard::profile_ref(&p);
                                    if spec.database.is_none() { spec.database = p.database.clone(); }
                                }
                                if spec.sql.trim().is_empty() { return ActionResult::BadArgs("sql is required".into()); }
                                match (action.as_str(), arg_usize(args, "tile")) {
                                    ("update_tile", Some(k)) => { self.with_ctx(egui, |s, cx| crate::dashboard::update_tile(s, cx, i, k, spec)); ActionResult::with(&json!({"tile": k})) }
                                    _ => { let k = self.with_ctx(egui, |s, cx| crate::dashboard::add_tile(s, cx, i, spec)); ActionResult::with(&json!({"tile": k})) }
                                }
                            }
                            "remove_tile" => {
                                let Some(k) = arg_usize(args, "tile") else { return ActionResult::BadArgs("tile is required".into()) };
                                self.with_ctx(egui, |s, cx| crate::dashboard::remove_tile(s, cx, i, k));
                                ActionResult::ok()
                            }
                            "refresh" => {
                                match arg_usize(args, "tile") {
                                    Some(k) => self.with_ctx(egui, |s, cx| crate::dashboard::refresh_tile(s, cx, i, k)),
                                    None => self.with_ctx(egui, |s, cx| crate::dashboard::refresh_all(s, cx, i)),
                                }
                                ActionResult::ok()
                            }
                            "state" => {
                                let t = &self.state.tabs[i];
                                let d = t.dashboard.as_deref().unwrap();
                                let tiles: Vec<Value> = d.tiles.iter().map(|tile| {
                                    let run = tile.tab.run.as_ref();
                                    let first = run.and_then(|r| r.result_sets.first());
                                    json!({
                                        "title": tile.spec.title, "kind": tile.spec.kind.key(), "size": tile.spec.size.key(), "interval_secs": tile.spec.interval_secs,
                                        "profile": tile.spec.profile, "database": tile.spec.database, "sql": tile.spec.sql,
                                        "connection": match &tile.tab.conn { crate::state::ConnState::Connected { database, .. } => json!({"connected": database}), crate::state::ConnState::Connecting => json!("connecting"), crate::state::ConnState::Failed { error, .. } => json!({"failed": error}), crate::state::ConnState::Disconnected => json!("disconnected") },
                                        "run": run.map(|r| format!("{:?}", r.state).to_lowercase()), "rows": first.map(|v| v.rs.row_count()), "columns": first.map(|v| v.rs.columns.iter().map(|c| c.name.clone()).collect::<Vec<_>>()),
                                        "value": first.filter(|v| v.rs.visible_count() > 0).map(|v| v.rs.cell_text(0, 0, &self.state.formatter).to_string()),
                                        "chart": first.and_then(|v| v.chart.as_ref()).map(|c| serde_json::to_value(&c.spec).unwrap_or(Value::Null)),
                                        "age_secs": tile.age_secs(), "pending": tile.pending, "error": tile.error,
                                        "messages": run.map(|r| r.messages.iter().filter(|m| m.is_error).map(|m| m.text.clone()).collect::<Vec<_>>()).unwrap_or_default(),
                                    })
                                }).collect();
                                ActionResult::with(&json!({"title": d.file.title, "columns": d.file.columns, "file_path": t.file_path, "dirty": d.dirty, "home_for": d.home_for.map(|p| p.to_string()), "tiles": tiles}))
                            }
                            other => ActionResult::BadArgs(format!("unknown dashboard action {other:?}")),
                        }
                    }
                }
            }
            "grid" => {
                // {action: totals|profile|find, kind?: Sum|Avg|Min|Max|Count|Distinct|None, set?, text?, regex?, case?, word?}
                let Some(i) = self.state.active_tab else { return ActionResult::BadArgs("no active tab".into()) };
                let set = arg_usize(args, "set").unwrap_or(0);
                let act = arg_str(args, "action").unwrap_or_default();
                let flag = |k: &str| args.and_then(|a| a.get(k)).and_then(|v| v.as_bool());
                match act.as_str() {
                    "totals" => {
                        let kind = arg_str(args, "kind").and_then(|k| crate::state::TotalKind::ALL.iter().copied().find(|t| t.label().eq_ignore_ascii_case(&k)));
                        self.with_ctx(egui, |s, cx| ops::results_action(s, cx, i, crate::ui::results::ResultsAction::SetTotals { set, kind }));
                        let v = self.state.tabs.get(i).and_then(|t| t.run.as_ref()).and_then(|r| r.result_sets.get(set)).and_then(|v| v.grid.totals.as_ref()).map(|t| json!({"kind": t.kind.label(), "values": t.values}));
                        ActionResult::with(&json!({"totals": v}))
                    }
                    "profile" => {
                        self.with_ctx(egui, |s, cx| ops::results_action(s, cx, i, crate::ui::results::ResultsAction::Profile { set }));
                        let v = self.state.tabs.get(i).and_then(|t| t.run.as_ref()).and_then(|r| r.result_sets.get(set)).and_then(|v| v.profile.as_ref()).map(|p| json!({
                            "rows_scanned": p.rows_scanned,
                            "columns": p.columns.iter().map(|c| json!({"name": c.name, "type": c.type_label, "count": c.summary.count, "nulls": c.summary.nulls, "distinct": c.summary.distinct, "min": c.summary.min, "max": c.summary.max, "avg": c.summary.avg, "top": c.top.iter().map(|(v, n)| json!([v.to_string(), n])).collect::<Vec<_>>(), "histogram": c.histogram})).collect::<Vec<_>>(),
                        }));
                        ActionResult::with(&json!({"profile": v}))
                    }
                    "find" => {
                        // open (or update) the find bar with the given text and toggles, report matches
                        let Some(v) = self.state.tabs.get_mut(i).and_then(|t| t.run.as_mut()).and_then(|r| r.result_sets.get_mut(set)) else { return ActionResult::BadArgs("no such result set".into()) };
                        let f = v.grid.find.get_or_insert_with(Default::default);
                        if let Some(t) = arg_str(args, "text") { f.text = t; }
                        if let Some(b) = flag("regex") { f.use_regex = b; }
                        if let Some(b) = flag("case") { f.case_sensitive = b; }
                        if let Some(b) = flag("word") { f.whole_word = b; }
                        f.generation = u64::MAX; // force a recompute on the next frame
                        egui.request_repaint();
                        ActionResult::ok()
                    }
                    "chart" => {
                        // {set?, kind?, x?, y?: [..], agg?, top_n?, bins?, stacked?, horizontal?, off?}: chart the set (the grid's chart toggle), then its state
                        let fmt = self.state.formatter.clone();
                        let Some(v) = self.state.tabs.get_mut(i).and_then(|t| t.run.as_mut()).and_then(|r| r.result_sets.get_mut(set)) else { return ActionResult::BadArgs("no such result set".into()) };
                        match apply_chart_args(v, args, &fmt) {
                            Ok(state) => {
                                egui.request_repaint();
                                ActionResult::with(&state)
                            }
                            Err(e) => ActionResult::BadArgs(e),
                        }
                    }
                    "chart_state" => {
                        let v = self.state.tabs.get(i).and_then(|t| t.run.as_ref()).and_then(|r| r.result_sets.get(set)).map(chart_state_json);
                        ActionResult::with(&json!({"chart": v}))
                    }
                    "find_state" => {
                        // matches: the first 1,000 (row, col); count: all of them so far; complete: every cell was looked at
                        let v = self.state.tabs.get(i).and_then(|t| t.run.as_ref()).and_then(|r| r.result_sets.get(set)).and_then(|v| v.grid.find.as_ref()).map(|f| json!({"text": f.text, "matches": f.matches.iter().take(1000).collect::<Vec<_>>(), "count": f.matches.len(), "current": f.current, "error": f.error, "complete": f.complete(), "scanned_rows": f.scanned_rows, "total_rows": f.total_rows}));
                        ActionResult::with(&json!({"find": v}))
                    }
                    _ => ActionResult::BadArgs("action must be totals, profile, find or find_state".into()),
                }
            }
            "files_root" => {
                // {path}: point the Files sidebar at a folder (what Open folder… does, without the OS dialog)
                let Some(path) = arg_str(args, "path") else { return ActionResult::BadArgs("path is required".into()) };
                self.state.files.root = Some(std::path::PathBuf::from(path));
                self.state.files.cache.clear();
                self.state.files.expanded.clear();
                self.state.sidebar_visible = true;
                self.state.sidebar_view = crate::state::SidebarView::Files;
                egui.request_repaint();
                ActionResult::ok()
            }
            "plan_view" => {
                // {text?: bool, metric?: 0..4} on the active tab's shown plan
                let Some(t) = self.state.active_mut() else { return ActionResult::BadArgs("no active tab".into()) };
                let Some(pv) = t.run.as_mut().and_then(|r| r.plans.first_mut()) else { return ActionResult::BadArgs("no plan".into()) };
                if let Some(b) = args.and_then(|a| a.get("text")).and_then(|v| v.as_bool()) { pv.text_view = b; }
                if let Some(m) = arg_usize(args, "metric") { pv.metric = m; }
                egui.request_repaint();
                ActionResult::ok()
            }
            "results_to_table" => {
                // {table, schema?, existing?: bool, set?, target?: tab index, selection_only?: bool}: Save results as table
                let Some(i) = self.state.active_tab else { return ActionResult::BadArgs("no active tab".into()) };
                let set = arg_usize(args, "set").unwrap_or(0);
                let table = arg_str(args, "table");
                let schema = arg_str(args, "schema");
                let existing = args.and_then(|a| a.get("existing")).and_then(|v| v.as_bool()).unwrap_or(false);
                let target = arg_usize(args, "target");
                let selection_only = args.and_then(|a| a.get("selection_only")).and_then(|v| v.as_bool()).unwrap_or(false);
                self.with_ctx(egui, |s, cx| {
                    ops::open_results_to_table(s, cx, i, set, selection_only);
                    if let crate::state::Dialog::Import(d) = &mut s.dialog {
                        if let Some(t) = table { d.table_name = t; }
                        if let Some(sc) = schema { d.schema_name = sc; }
                        if let Some(t) = target { d.tab_index = t; }
                        d.existing = existing;
                    }
                    if existing {
                        ops::import_request_existing_columns(s, cx);
                    } else {
                        ops::start_import(s, cx);
                    }
                });
                ActionResult::ok()
            }
            "import_start" => {
                self.with_ctx(egui, ops::start_import);
                ActionResult::ok()
            }
            "import_state" => {
                let v = match &self.state.dialog {
                    crate::state::Dialog::Export(d) if d.import.is_some() => json!({
                        "open": true, "export": true, "running": d.running, "path": d.path,
                        "rows_done": self.state.export_progress.as_ref().map(|p| p.lock().0).unwrap_or(0),
                        "result": d.result.as_ref().map(|r| match r { Ok(m) => json!({"ok": m}), Err(e) => json!({"error": e}) }),
                    }),
                    crate::state::Dialog::Import(d) => json!({
                        "open": true, "running": d.running, "rows_done": d.rows_done, "path": d.path, "table": format!("{}.{}", d.schema_name, d.table_name), "existing": d.existing, "destination": if d.destination == 0 { "table" } else { "file" },
                        "columns": d.columns.iter().map(|c| json!({"name": c.name, "sql_type": c.sql_type, "nullable": c.nullable, "include": c.include, "source": c.source})).collect::<Vec<_>>(),
                        "row_estimate": d.inspection.as_ref().and_then(|i| i.row_estimate), "format": d.inspection.as_ref().map(|i| i.format.label()),
                        "existing_columns": match &d.existing_columns { crate::state::Loadable::Loaded(c) => c.len() as i64, crate::state::Loadable::Loading => -1, _ => 0 },
                        "result": d.result.as_ref().map(|r| match r { Ok(m) => json!({"ok": m}), Err(e) => json!({"error": e}) }), "inspect_error": d.inspect_error,
                    }),
                    _ => json!({"open": false}),
                };
                ActionResult::with(&v)
            }
            "library" => {
                // {action: export|import, path}
                let act = arg_str(args, "action").unwrap_or_default();
                let Some(path) = arg_str(args, "path") else { return ActionResult::BadArgs("path is required".into()) };
                let p = std::path::PathBuf::from(path);
                match act.as_str() {
                    "export" => self.with_ctx(egui, |s, cx| ops::export_connections(s, cx, &p)),
                    "import" => self.with_ctx(egui, |s, cx| ops::import_connections(s, cx, &p)),
                    _ => return ActionResult::BadArgs("action must be export or import".into()),
                }
                ActionResult::ok()
            }
            "type_text" => {
                let text = arg_str(args, "text").unwrap_or_default();
                self.state.injected_events.push(egui::Event::Text(text));
                egui.request_repaint();
                ActionResult::ok()
            }
            "paste" => {
                // {text}: a paste event with this text (the OS clipboard is not involved)
                let text = arg_str(args, "text").unwrap_or_default();
                self.state.injected_events.push(egui::Event::Paste(text));
                egui.request_repaint();
                ActionResult::ok()
            }
            "fabric_state" => {
                let f = &self.state.fabric;
                let ws: Vec<Value> = f.workspaces.get().map(|w| w.iter().map(|w| json!({"id": w.id, "name": w.display_name, "kind": format!("{:?}", w.kind), "expanded": f.expanded.contains(&w.id),
                    "semantic_models": f.semantic_models.get(&w.id).and_then(|l| l.get()).map(|v| v.iter().map(|i| json!({"id": i.id, "name": i.display_name})).collect::<Vec<_>>()),
                    "items": f.items.get(&w.id).and_then(|l| l.get()).map(|v| v.iter().map(|i| json!({"id": i.id, "name": i.display_name, "kind": format!("{:?}", i.kind), "pinned": f.is_pinned(&i.id),
                        "target": f.details.get(&i.id).and_then(|d| d.get()).map(|t| json!({"server": t.server, "database": t.database, "provisioning": t.provisioning}))})).collect::<Vec<_>>())})).collect()).unwrap_or_default();
                ActionResult::with(&json!({
                    "status": format!("{:?}", f.status()),
                    "account": f.account.as_ref().map(|a| a.username.clone()),
                    "slot": f.slot.map(|s| s.to_string()),
                    "workspaces": ws,
                    "pins": f.pins.iter().map(|p| json!({"item_id": p.item_id, "name": p.display_name, "workspace": p.workspace_name})).collect::<Vec<_>>(),
                    "recent": f.recent.iter().map(|p| json!({"item_id": p.item_id, "name": p.display_name, "workspace": p.workspace_name})).collect::<Vec<_>>(),
                    "capacities": f.capacities.values().map(|c| json!({"id": c.id, "name": c.display_name, "sku": c.sku, "region": c.region})).collect::<Vec<_>>(),
                    "expanded_items": f.expanded_items.iter().cloned().collect::<Vec<_>>(),
                }))
            }
            "fabric" => {
                // {action: sign_in|sign_out|grant|refresh|expand|open|pin|save|copy|portal|explore|export_here, workspace?: name, item?: name}
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
                    "grant" => FA::GrantPermissions,
                    "expand" => match arg_str(args, "workspace").and_then(|n| find_ws(&n)) { Some(id) => FA::ToggleWorkspace(id), None => return ActionResult::BadArgs("no such workspace".into()) },
                    "open" | "pin" | "save" | "copy" | "portal" | "explore" | "export_here" | "spark_query" => {
                        let Some(id) = arg_str(args, "item").and_then(|n| find_item(&n)) else { return ActionResult::BadArgs("no such item (expand its workspace first)".into()) };
                        match act.as_str() {
                            "open" => FA::Open { item_id: id },
                            "pin" => FA::TogglePin { item_id: id },
                            "save" => FA::SaveToServers { item_id: id },
                            "copy" => FA::CopyConnectionString { item_id: id },
                            "explore" => FA::ToggleItem { item_id: id },
                            "export_here" => FA::ExportHere { item_id: id },
                            "spark_query" => FA::SparkQuery { item_id: id },
                            _ => FA::OpenInPortal { item_id: id },
                        }
                    }
                    _ => return ActionResult::BadArgs("unknown fabric action".into()),
                };
                self.with_ctx(egui, |s, cx| crate::fabric::action(s, cx, action));
                ActionResult::with(&json!({"status": format!("{:?}", self.state.fabric.status()), "consent_needed": self.state.fabric.consent_needed(), "missing": self.state.fabric.missing_scopes, "dialog": dialog_name(&self.state.dialog)}))
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
            "focus" => {
                // which widget holds keyboard focus, and whether it is the active editor
                let focused = egui.memory(|m| m.focused()).map(|id| format!("{id:?}"));
                let editor = self.state.active_tab.and_then(|i| self.state.tabs.get(i)).map(|t| format!("{:?}", egui::Id::new(("cobalt-editor", t.id))));
                ActionResult::with(&json!({"focused": focused, "editor": editor, "is_editor": focused.is_some() && focused == editor}))
            }
            "dismiss_dialog" => {
                self.state.dialog = crate::state::Dialog::None;
                ActionResult::ok()
            }
            "notebook" => {
                use cobalt_notebook::{CellKind, CellLanguage};
                let Some(action) = arg_str(args, "action") else { return ActionResult::BadArgs("action is required".into()) };
                let idx = self.state.active_tab;
                let is_nb = idx.map(|i| self.state.tabs[i].is_notebook()).unwrap_or(false);
                match action.as_str() {
                    "new" => {
                        let lang = match arg_str(args, "language").as_deref() {
                            Some("pyspark") | Some("python") => CellLanguage::Python,
                            _ => CellLanguage::Sql,
                        };
                        let i = self.with_ctx(egui, |s, cx| crate::notebook::new_tab(s, cx, lang));
                        ActionResult::with(&json!({"tab": i}))
                    }
                    "open" => {
                        let Some(path) = arg_str(args, "path") else { return ActionResult::BadArgs("path is required".into()) };
                        let ok = self.with_ctx(egui, |s, cx| crate::notebook::open_path(s, cx, std::path::PathBuf::from(path)));
                        ActionResult::with(&json!({"opened": ok, "tab": self.state.active_tab}))
                    }
                    "open_fabric" => {
                        // {item: id or display name, copy?}
                        let Some(item) = arg_str(args, "item") else { return ActionResult::BadArgs("item is required".into()) };
                        let copy = args.and_then(|a| a.get("copy")).and_then(|v| v.as_bool()).unwrap_or(false);
                        let id = self.state.fabric.notebooks.values().filter_map(|l| l.get()).flatten().find(|n| n.id == item || n.display_name.eq_ignore_ascii_case(&item)).map(|n| n.id.clone());
                        let Some(id) = id else { return ActionResult::BadArgs("unknown notebook (expand the workspace in the Fabric panel first)".into()) };
                        self.with_ctx(egui, |s, cx| crate::fabric::open_notebook(s, cx, &id, copy));
                        ActionResult::ok()
                    }
                    _ if !is_nb => ActionResult::Rejected("the active tab is not a notebook".into()),
                    "save" => {
                        let i = idx.unwrap();
                        if let Some(p) = arg_str(args, "path") {
                            self.state.tabs[i].file_path = Some(std::path::PathBuf::from(p));
                        }
                        self.with_ctx(egui, |s, cx| crate::notebook::save(s, cx, i, false));
                        ActionResult::with(&json!({"dirty": self.state.tabs[i].is_dirty(), "file_path": self.state.tabs[i].file_path}))
                    }
                    "cells" => ActionResult::with(&self.state_json()["tabs"][idx.unwrap()]["cells"]),
                    "set_lakehouse" => {
                        // {workspace?, lakehouse?, write_mode?}: workspace/lakehouse by id or name; omit workspace to unbind
                        let i = idx.unwrap();
                        let ws_arg = arg_str(args, "workspace");
                        let Some(ws_arg) = ws_arg else {
                            crate::notebook::set_fabric(&mut self.state, i, None);
                            return ActionResult::ok();
                        };
                        let ws_id = self.state.fabric.workspaces.get().and_then(|v| v.iter().find(|w| w.id == ws_arg || w.display_name.eq_ignore_ascii_case(&ws_arg)).map(|w| w.id.clone()));
                        let Some(ws_id) = ws_id else { return ActionResult::BadArgs("unknown workspace (load the Fabric panel first)".into()) };
                        let lh_id = match arg_str(args, "lakehouse") {
                            Some(l) => match self.state.fabric.lakehouses(&ws_id).and_then(|v| v.into_iter().find(|(n, id)| *id == l || n.eq_ignore_ascii_case(&l)).map(|(_, id)| id)) {
                                Some(id) => Some(id),
                                None => return ActionResult::BadArgs("unknown lakehouse (workspace items not loaded?)".into()),
                            },
                            None => None,
                        };
                        let current_mode = self.state.tabs[i].notebook.as_deref().and_then(|nb| nb.fabric.as_ref().map(|f| f.write_mode.clone())).unwrap_or_else(|| "sandbox".into());
                        let write_mode = arg_str(args, "write_mode").unwrap_or(current_mode);
                        let preload = args.and_then(|a| a.get("preload")).and_then(|v| v.as_bool()).unwrap_or(false);
                        // the lakehouse's remembered policy: preload (none|last|all) and keep_clones
                        if let Some(id) = lh_id.clone() {
                            let policy_arg = args.and_then(|a| a.get("preload_policy")).and_then(|v| v.as_str()).map(str::to_string);
                            let preload_given = args.and_then(|a| a.get("preload")).is_some();
                            let keep = args.and_then(|a| a.get("keep_clones")).and_then(|v| v.as_bool());
                            self.with_ctx(egui, |_, cx| {
                                let mut p = crate::notebook::lakehouse_policy(cx, &id);
                                let mut touched = false;
                                if let Some(v) = policy_arg {
                                    p.preload = v;
                                    touched = true;
                                } else if preload {
                                    p.preload = "all".into();
                                    touched = true;
                                } else if preload_given {
                                    p.preload = String::new();
                                    touched = true;
                                }
                                if let Some(k) = keep {
                                    p.keep_clones = k;
                                    touched = true;
                                }
                                if touched {
                                    crate::notebook::set_lakehouse_policy(cx, &id, &p);
                                }
                            });
                        }
                        crate::notebook::set_fabric(&mut self.state, i, Some(crate::state::NotebookFabric { workspace_id: ws_id, lakehouse_id: lh_id, write_mode, preload }));
                        ActionResult::ok()
                    }
                    "save_fabric" => {
                        let i = idx.unwrap();
                        if self.state.tabs[i].fabric_item.is_none() {
                            return ActionResult::Rejected("this notebook is not bound to a Fabric item".into());
                        }
                        self.with_ctx(egui, |s, cx| crate::fabric::save_notebook(s, cx, i));
                        ActionResult::ok()
                    }
                    "set_kernel" => {
                        let k = match arg_str(args, "kernel").as_deref() {
                            Some("spark") => crate::state::NotebookKernel::Spark,
                            _ => crate::state::NotebookKernel::Connection,
                        };
                        crate::notebook::set_kernel(&mut self.state, idx.unwrap(), k);
                        ActionResult::ok()
                    }
                    "set_cell" => {
                        let (Some(ci), Some(text)) = (arg_usize(args, "index"), arg_str(args, "text")) else { return ActionResult::BadArgs("index and text are required".into()) };
                        let nb = self.state.tabs[idx.unwrap()].notebook.as_deref_mut().unwrap();
                        let Some(cell) = nb.nb.cells.get_mut(ci) else { return ActionResult::BadArgs("no such cell".into()) };
                        cell.source = text;
                        nb.cells[ci].editor.cursors.clamp(cell.source.chars().count());
                        nb.dirty = true;
                        egui.request_repaint();
                        ActionResult::ok()
                    }
                    "add_cell" => {
                        let i = idx.unwrap();
                        let kind = match arg_str(args, "kind").as_deref() {
                            Some("markdown") => CellKind::Markdown,
                            _ => CellKind::Code,
                        };
                        let n = self.state.tabs[i].notebook.as_deref().map(|nb| nb.cells.len()).unwrap_or(0);
                        let at = arg_usize(args, "at").unwrap_or(n);
                        let r = crate::notebook::insert_cell(&mut self.state, i, at, kind, false);
                        if let Some(text) = arg_str(args, "text") {
                            if let (Some(at), Some(nb)) = (r, self.state.tabs[i].notebook.as_deref_mut()) {
                                nb.nb.cells[at].source = text;
                            }
                        }
                        egui.request_repaint();
                        ActionResult::with(&json!({"index": r}))
                    }
                    "delete_cell" => {
                        let Some(ci) = arg_usize(args, "index") else { return ActionResult::BadArgs("index is required".into()) };
                        crate::notebook::delete_cell(&mut self.state, idx.unwrap(), ci);
                        ActionResult::ok()
                    }
                    "move_cell" => {
                        let Some(ci) = arg_usize(args, "index") else { return ActionResult::BadArgs("index is required".into()) };
                        let delta = args.and_then(|a| a.get("delta")).and_then(|v| v.as_i64()).unwrap_or(1) as isize;
                        crate::notebook::move_cell(&mut self.state, idx.unwrap(), ci, delta);
                        ActionResult::ok()
                    }
                    "set_kind" => {
                        let Some(ci) = arg_usize(args, "index") else { return ActionResult::BadArgs("index is required".into()) };
                        let kind = match arg_str(args, "kind").as_deref() {
                            Some("markdown") => CellKind::Markdown,
                            _ => CellKind::Code,
                        };
                        crate::notebook::set_kind(&mut self.state, idx.unwrap(), ci, kind);
                        ActionResult::ok()
                    }
                    "set_parameters" => {
                        // {index, on?: true}: the parameters cell (Fabric's `parameters` tag)
                        let Some(ci) = arg_usize(args, "index") else { return ActionResult::BadArgs("index is required".into()) };
                        let on = args.and_then(|a| a.get("on")).and_then(|v| v.as_bool()).unwrap_or(true);
                        crate::notebook::set_parameters(&mut self.state, idx.unwrap(), ci, on);
                        ActionResult::with(&json!({"parameters": crate::notebook::parameters_of(&self.state, idx.unwrap()).map(|(i, a)| json!({"index": i, "assignments": a}))}))
                    }
                    "parameters" => ActionResult::with(&json!({"parameters": crate::notebook::parameters_of(&self.state, idx.unwrap()).map(|(i, a)| json!({"index": i, "assignments": a}))})),
                    "run_with_parameters" => {
                        // {params: {name: "value expression", ...}}: every code cell, the overrides appended to the parameters cell for this run
                        let params: Vec<(String, String)> = args.and_then(|a| a.get("params")).and_then(|v| v.as_object()).map(|m| m.iter().map(|(k, v)| (k.clone(), match v { Value::String(s) => s.clone(), other => other.to_string() })).collect()).unwrap_or_default();
                        let i = idx.unwrap();
                        self.with_ctx(egui, |s, cx| crate::notebook::run_with_parameters(s, cx, i, params));
                        ActionResult::ok()
                    }
                    "chart" => {
                        // {index (cell), set?, kind?, x?, y?, agg?, top_n?, bins?, stacked?, horizontal?, off?, keep?}: a cell result's chart; `keep` captures the picture into the outputs
                        let Some(ci) = arg_usize(args, "index") else { return ActionResult::BadArgs("index is required".into()) };
                        let set = arg_usize(args, "set").unwrap_or(0);
                        let fmt = self.state.formatter.clone();
                        let i = idx.unwrap();
                        let Some(v) = self.state.tabs[i].notebook.as_deref_mut().and_then(|nb| nb.cells.get_mut(ci)).and_then(|c| c.run.as_mut()).and_then(|r| r.result_sets.get_mut(set)) else { return ActionResult::BadArgs("no such cell result set".into()) };
                        let state = match apply_chart_args(v, args, &fmt) {
                            Ok(s) => s,
                            Err(e) => return ActionResult::BadArgs(e),
                        };
                        if args.and_then(|a| a.get("keep")).and_then(|b| b.as_bool()) == Some(true) {
                            let cell_id = self.state.tabs[i].notebook.as_deref().map(|nb| nb.cells[ci].id.clone()).unwrap_or_default();
                            self.with_ctx(egui, |s, cx| crate::notebook::results_action(s, cx, i, ci, crate::ui::results::ResultsAction::CaptureChart { set, purpose: crate::state::CapturePurpose::Cell { cell_id } }));
                        }
                        egui.request_repaint();
                        ActionResult::with(&state)
                    }
                    "add_pip_packages" => {
                        let Some(ci) = arg_usize(args, "index") else { return ActionResult::BadArgs("index is required".into()) };
                        let i = idx.unwrap();
                        self.with_ctx(egui, |s, cx| crate::notebook::add_pip_packages(s, cx, i, ci));
                        ActionResult::ok()
                    }
                    "select" => {
                        let Some(ci) = arg_usize(args, "index") else { return ActionResult::BadArgs("index is required".into()) };
                        let nb = self.state.tabs[idx.unwrap()].notebook.as_deref_mut().unwrap();
                        if ci >= nb.cells.len() {
                            return ActionResult::BadArgs("no such cell".into());
                        }
                        nb.selected = ci;
                        if args.and_then(|a| a.get("focus")).and_then(|v| v.as_bool()).unwrap_or(false) {
                            nb.cells[ci].editor.request_focus = true;
                            if nb.nb.cells[ci].kind == CellKind::Markdown {
                                nb.cells[ci].md_editing = true;
                            }
                        }
                        egui.request_repaint();
                        ActionResult::ok()
                    }
                    "md_edit" => {
                        let Some(ci) = arg_usize(args, "index") else { return ActionResult::BadArgs("index is required".into()) };
                        let editing = args.and_then(|a| a.get("editing")).and_then(|v| v.as_bool()).unwrap_or(true);
                        let nb = self.state.tabs[idx.unwrap()].notebook.as_deref_mut().unwrap();
                        if let Some(c) = nb.cells.get_mut(ci) {
                            c.md_editing = editing;
                        }
                        egui.request_repaint();
                        ActionResult::ok()
                    }
                    "run" => {
                        let i = idx.unwrap();
                        let n = self.state.tabs[i].notebook.as_deref().map(|nb| nb.cells.len()).unwrap_or(0);
                        let cells: Vec<usize> = match args.and_then(|a| a.get("cells")) {
                            Some(Value::Array(a)) => a.iter().filter_map(|v| v.as_u64()).map(|v| v as usize).collect(),
                            Some(Value::String(s)) if s == "all" => (0..n).collect(),
                            _ => match arg_usize(args, "index") {
                                Some(ci) => vec![ci],
                                None => (0..n).collect(),
                            },
                        };
                        self.with_ctx(egui, |s, cx| crate::notebook::run_cells(s, cx, i, cells));
                        ActionResult::ok()
                    }
                    "cancel" => {
                        let i = idx.unwrap();
                        self.with_ctx(egui, |s, cx| crate::notebook::cancel(s, cx, i));
                        ActionResult::ok()
                    }
                    "dequeue" => {
                        let Some(ci) = arg_usize(args, "index") else { return ActionResult::BadArgs("index is required".into()) };
                        crate::notebook::dequeue(&mut self.state, idx.unwrap(), ci);
                        ActionResult::ok()
                    }
                    "run_selection" => {
                        // {index?, selection?: [start_char, end_char]} — runs the cell's selected span (or sets one first)
                        let i = idx.unwrap();
                        let ci = arg_usize(args, "index").or_else(|| self.state.tabs[i].notebook.as_deref().map(|nb| nb.selected)).unwrap_or(0);
                        if let Some(sel) = args.and_then(|a| a.get("selection")).and_then(|v| v.as_array()) {
                            if let (Some(a), Some(b)) = (sel.first().and_then(|v| v.as_u64()), sel.get(1).and_then(|v| v.as_u64())) {
                                if let Some(cs) = self.state.tabs[i].notebook.as_deref_mut().and_then(|nb| nb.cells.get_mut(ci)) {
                                    cs.editor.cursors.set_single(crate::ui::editor::core::Sel::range(a as usize, b as usize));
                                }
                            }
                        }
                        self.with_ctx(egui, |s, cx| crate::notebook::run_selection(s, cx, i, ci));
                        ActionResult::ok()
                    }
                    "clear_outputs" => {
                        crate::notebook::clear_outputs(&mut self.state, idx.unwrap(), arg_usize(args, "index"));
                        ActionResult::ok()
                    }
                    "export" => {
                        let (Some(kind), Some(path)) = (arg_str(args, "kind"), arg_str(args, "path")) else { return ActionResult::BadArgs("kind and path are required".into()) };
                        let kind = if kind == "html" { crate::notebook::ExportKind::Html } else { crate::notebook::ExportKind::Markdown };
                        let i = idx.unwrap();
                        self.with_ctx(egui, |s, cx| crate::notebook::export(s, cx, i, kind, Some(std::path::PathBuf::from(path))));
                        ActionResult::ok()
                    }
                    other => ActionResult::BadArgs(format!("unknown notebook action {other}")),
                }
            }
            "fabric_notebooks" => {
                // {workspace?: id or name} → notebooks listed so far (loads the workspace when needed)
                let ws = arg_str(args, "workspace");
                let ws_id = ws.as_ref().and_then(|w| self.state.fabric.workspaces.get().and_then(|v| v.iter().find(|x| x.id == *w || x.display_name.eq_ignore_ascii_case(w)).map(|x| x.id.clone())));
                if let Some(id) = &ws_id {
                    if !self.state.fabric.items.contains_key(id) {
                        let id2 = id.clone();
                        self.with_ctx(egui, |s, cx| crate::fabric::load_items(s, cx, &id2));
                    }
                }
                let list: Vec<Value> = self.state.fabric.notebooks.iter().filter(|(k, _)| ws_id.as_ref().map(|w| *k == w).unwrap_or(true)).flat_map(|(ws, l)| l.get().into_iter().flatten().map(move |n| json!({"id": n.id, "name": n.display_name, "workspace_id": ws}))).collect();
                let loading: Vec<&String> = self.state.fabric.notebooks.iter().filter(|(_, l)| l.is_loading()).map(|(k, _)| k).collect();
                ActionResult::with(&json!({"notebooks": list, "loading": loading, "fabric_status": format!("{:?}", self.state.fabric.status()), "workspaces": self.state.fabric.workspaces.get().map(|v| v.iter().map(|w| json!({"id": w.id, "name": w.display_name})).collect::<Vec<_>>())}))
            }
            "fabric_scopes" => {
                // the scopes granted on the cached Fabric API token (diagnostics)
                let Some(slot) = self.state.fabric.slot else { return ActionResult::Rejected("not signed in to Fabric".into()) };
                let tenant = self.settings.connections.entra_default_tenant.clone().filter(|s| !s.trim().is_empty());
                let resolver = self.resolver.clone();
                let r = self.session.handle().block_on(async move { resolver.resource_token_silent(slot, cobalt_auth::provider::FABRIC_API_RESOURCE, tenant.as_deref()).await });
                match r {
                    Ok(Some(ts)) => ActionResult::with(&json!({"scopes": ts.access.scope, "missing": cobalt_auth::provider::missing_fabric_scopes(&ts.access.scope), "expires_at": ts.access.expires_at.to_rfc3339(), "account": ts.account.username, "consent_needed": self.state.fabric.consent_needed(), "admin_consent_url": self.state.fabric.admin_consent_url})),
                    Ok(None) => ActionResult::with(&json!({"scopes": null})),
                    Err(e) => ActionResult::Rejected(e.to_string()),
                }
            }
            "shadows" => {
                // {action: status|discard|discard_written|restore, table?}
                let action = arg_str(args, "action").unwrap_or_else(|| "status".into());
                match action.as_str() {
                    "status" => crate::notebook::refresh_shadows(&mut self.state),
                    "peek" => {}
                    "preload_status" => crate::notebook::shadows_action(&mut self.state, "preload_status", json!({})),
                    "discard_table" => {
                        let Some(t) = arg_str(args, "table") else { return ActionResult::BadArgs("table is required".into()) };
                        crate::notebook::shadows_action(&mut self.state, "discard_shadow", json!({"table": t}));
                    }
                    "discard" => crate::notebook::shadows_action(&mut self.state, "discard_shadow", json!({})),
                    "discard_written" => crate::notebook::shadows_action(&mut self.state, "discard_shadow", json!({"only": "written"})),
                    "restore" => {
                        let Some(t) = arg_str(args, "table") else { return ActionResult::BadArgs("table is required".into()) };
                        crate::notebook::shadows_action(&mut self.state, "restore_shadow", json!({"table": t, "version": args.and_then(|a| a.get("version")).and_then(|v| v.as_u64()).unwrap_or(0)}));
                    }
                    other => return ActionResult::BadArgs(format!("unknown shadows action {other}")),
                }
                self.state.shadows.open = true;
                egui.request_repaint();
                let sh = &self.state.shadows;
                ActionResult::with(&json!({"loading": sh.loading, "status": sh.status, "error": sh.error, "note": sh.note}))
            }
            "lakehouse_pane" => {
                // {action: show|state|select|expand|pull|remove_local|mount|discard|refresh|insert, path?, lakehouse?, table?, code?}
                use crate::ui::lakehouse::LakehouseAction as A;
                let action = arg_str(args, "action").unwrap_or_else(|| "state".into());
                let path = arg_str(args, "path").unwrap_or_default();
                let act: Option<A> = match action.as_str() {
                    "show" => {
                        self.state.sidebar_visible = true;
                        self.state.sidebar_view = crate::state::SidebarView::Lakehouse;
                        None
                    }
                    "state" => None,
                    "select" => self.state.fabric.lakehouses(&self.state.lakehouse_pane.selected.as_ref().map(|(w, _, _)| w.clone()).unwrap_or_default()).and_then(|v| v.into_iter().find(|(n, id)| *id == path || n.eq_ignore_ascii_case(&path)).map(|(_, id)| A::Select(id))),
                    "expand" => Some(A::ExpandFiles(path)),
                    "pull" => Some(A::Pull(path)),
                    "remove_local" => Some(A::RemoveLocal(path)),
                    "mount" => Some(A::Mount(arg_str(args, "lakehouse").unwrap_or_default(), arg_str(args, "table").unwrap_or_default())),
                    "discard" => Some(A::Discard(arg_str(args, "table").unwrap_or_default())),
                    "refresh" => Some(A::Refresh),
                    "insert" => Some(A::InsertCell(arg_str(args, "code").unwrap_or_default())),
                    other => return ActionResult::BadArgs(format!("unknown lakehouse_pane action {other}")),
                };
                self.with_ctx(egui, |state, cx| {
                    crate::notebook::lakehouse_pane_shown(state, cx);
                    if let Some(a) = act {
                        crate::notebook::lakehouse_action(state, cx, a);
                    }
                });
                egui.request_repaint();
                let p = &self.state.lakehouse_pane;
                ActionResult::with(&json!({
                    "selected": p.selected.as_ref().map(|(w, n, i)| json!({"workspace_id": w, "lakehouse": n, "lakehouse_id": i})),
                    "tables": p.tables.as_ref().map(|r| match r { Ok(v) => json!(v.iter().map(|t| t.rel_path()).collect::<Vec<_>>()), Err(e) => json!({"error": e}) }),
                    "tables_loading": p.tables_loading,
                    "files": p.files.iter().map(|(k, v)| (k.clone(), match v { Ok(es) => json!(es.iter().map(|e| json!({"name": e.name, "dir": e.is_dir, "size": e.size})).collect::<Vec<_>>()), Err(e) => json!({"error": e}) })).collect::<serde_json::Map<_, _>>(),
                    "files_loading": p.files_loading.iter().cloned().collect::<Vec<_>>(),
                    "expanded": p.expanded.iter().cloned().collect::<Vec<_>>(),
                    "mirror": p.mirror,
                    "note": p.note,
                }))
            }
            "settings" => {
                // {set: {"spark.idle_minutes": 1, "spark.lifecycle": "idle", ...}} — dotted paths into the settings document
                let Some(set) = args.and_then(|a| a.get("set")).and_then(|v| v.as_object()).cloned() else { return ActionResult::BadArgs("set is required".into()) };
                let mut doc = serde_json::to_value(&self.settings).unwrap_or(Value::Null);
                for (path, value) in set {
                    let (parent, leaf) = match path.rsplit_once('.') {
                        Some((p, l)) => (format!("/{}", p.replace('.', "/")), l.to_string()),
                        None => (String::new(), path.clone()),
                    };
                    let target = if parent.is_empty() { Some(&mut doc) } else { doc.pointer_mut(&parent) };
                    match target.and_then(|t| t.as_object_mut()) {
                        Some(o) => {
                            o.insert(leaf, value.clone());
                        }
                        None => return ActionResult::BadArgs(format!("settings: no section {parent}")),
                    }
                }
                match serde_json::from_value::<cobalt_core::Settings>(doc) {
                    Ok(new) => self.apply_settings(egui, new),
                    Err(e) => return ActionResult::BadArgs(format!("settings: {e}")),
                }
                ActionResult::with(&json!({"spark": {"engine": self.settings.spark.engine, "lifecycle": self.settings.spark.lifecycle, "idle_minutes": self.settings.spark.idle_minutes, "early_start": self.settings.spark.early_start}}))
            }
            "kernel" => {
                let action = arg_str(args, "action").unwrap_or_else(|| "status".into());
                match action.as_str() {
                    "status" => {}
                    "start" => {
                        if crate::kernel::start(&mut self.state.kernel, &self.settings, &self.paths, egui, None).is_err() {
                            return ActionResult::Rejected("runtime not provisioned".into());
                        }
                    }
                    "stop" => crate::kernel::stop(&mut self.state.kernel),
                    "restart" => self.run_command(egui, Command::KernelRestart),
                    "interrupt" => {
                        let a = crate::kernel::interrupt(&mut self.state.kernel);
                        self.state.recent_toasts.push_back(format!("interrupt: {a:?}"));
                    }
                    "log" => self.state.kernel.log_open = !self.state.kernel.log_open,
                    // any worker method on the data socket; the reply lands in kernel.last_call
                    "call" => {
                        let Some(method) = arg_str(args, "method") else { return ActionResult::BadArgs("method is required".into()) };
                        let params = args.and_then(|a| a.get("params")).cloned().unwrap_or(json!({}));
                        self.state.kernel.last_call = None;
                        if !crate::kernel::call(&mut self.state.kernel, "agent-call", &method, params) {
                            return ActionResult::Rejected("the Spark session is not ready".into());
                        }
                    }
                    "full_log" => return ActionResult::with(&json!({"log": self.state.kernel.log.iter().cloned().collect::<Vec<_>>()})),
                    other => return ActionResult::BadArgs(format!("unknown kernel action {other}")),
                }
                egui.request_repaint();
                {
                    let mut v = kernel_json(&self.state.kernel);
                    v["preload"] = self.state.shadows.preload.clone().unwrap_or(Value::Null);
                    ActionResult::with(&v)
                }
            }
            "runtime" => {
                use crate::runtime::RuntimeAction;
                let action = arg_str(args, "action").unwrap_or_else(|| "status".into());
                let a = match action.as_str() {
                    "status" => None,
                    "install" => Some(RuntimeAction::Install),
                    "smoke" => Some(RuntimeAction::SmokeTest),
                    "cancel" => Some(RuntimeAction::Cancel),
                    "remove" => Some(RuntimeAction::Remove),
                    "refresh" => Some(RuntimeAction::Refresh),
                    "libraries" => Some(RuntimeAction::InstallLibraries),
                    other => return ActionResult::BadArgs(format!("unknown runtime action {other}")),
                };
                if let Some(a) = a {
                    if let Some(err) = crate::runtime::action(&mut self.state.runtime, &self.settings, &self.paths, egui, a) {
                        return ActionResult::Rejected(err);
                    }
                } else if self.state.runtime.status.is_none() {
                    crate::runtime::refresh_status(&mut self.state.runtime, &self.settings, &self.paths, egui);
                }
                let r = &self.state.runtime;
                let comp = |c: &cobalt_runtime::ComponentState| match c {
                    cobalt_runtime::ComponentState::Managed { detail } => json!({"state": "managed", "detail": detail}),
                    cobalt_runtime::ComponentState::Adopted { detail } => json!({"state": "adopted", "detail": detail}),
                    cobalt_runtime::ComponentState::Missing { reason } => json!({"state": "missing", "detail": reason}),
                };
                ActionResult::with(&json!({
                    "dir": crate::runtime::dirs(&self.settings, &self.paths).root.to_string_lossy(),
                    "profile": self.settings.spark.profile,
                    "engine": self.settings.spark.engine,
                    "pending": r.status_pending,
                    "status": r.status.as_ref().map(|s| json!({
                        "engine": s.engine.key(), "package_version": s.package_version,
                        "roster": s.roster.as_ref().map(|r| json!({"profile": r.profile, "installed": r.installed, "total": r.total, "missing": r.missing, "mismatched": r.mismatched, "variants": r.variants, "failed": r.failed, "complete": r.complete()})),
                        "ready": s.is_ready(), "warm": s.warm, "spark_version": s.spark_version,
                        "uv": comp(&s.uv), "python": comp(&s.python), "env": comp(&s.env), "jdk": comp(&s.jdk),
                        "jdk_candidates": s.jdk_candidates.iter().map(|c| c.label()).collect::<Vec<_>>(),
                        "disk_bytes": s.disk_bytes, "mirror_bytes": s.mirror_bytes, "last_error": s.last_error,
                        "health": s.health.as_ref().map(|h| json!({"ok": h.ok, "problems": h.problems, "warnings": h.warnings, "profile": h.profile, "protocol_version": h.protocol_version})),
                    })),
                    "job": r.job.as_ref().map(|j| json!({"kind": format!("{:?}", j.kind), "step": j.step.as_ref().map(|(s, l)| format!("{s:?}: {l}")), "bytes": j.bytes, "elapsed_s": j.started.elapsed().as_secs(), "log_tail": j.log.iter().rev().take(15).cloned().collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>()})),
                    "last_result": r.last_result.as_ref().map(|x| match x { Ok(m) => json!({"ok": m}), Err(e) => json!({"error": e}) }),
                    "last_smoke": r.last_smoke,
                }))
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
