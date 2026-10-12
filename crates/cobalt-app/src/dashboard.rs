//! Dashboards (product design §5.6): a page of tiles, each a query on a connection shown as a
//! number, a chart or a small grid, refreshed on an interval. A tile owns a hidden `EditorTab`
//! (its own connection and run), so the session's connect / run / event paths serve it
//! unchanged — `AppState::tab_mut` finds tile tabs too. The page is a `.cobalt-dash.json`
//! file; a server's home page is the file remembered under `pref:dashboard:<profile id>`.

use crate::ops::{self, Ctx};
use crate::state::{AppState, ChartView, ConnState, ConnectPurpose, DashboardState, Dialog, EditorTab, RunView, Tile, ToastKind};
use cobalt_core::{ConnectionProfile, DashboardFile, ProfileId, ProfileRef, TileKind, TileSpec, DASHBOARD_EXT};
use std::path::PathBuf;
use std::time::Instant;

pub fn home_key(profile: ProfileId) -> String {
    format!("pref:dashboard:{profile}")
}

/// A new dashboard tab from a file's contents (tiles connect and refresh at once).
pub fn new_tab(state: &mut AppState, cx: &Ctx, file: DashboardFile, path: Option<PathBuf>, home_for: Option<ProfileId>) -> usize {
    let idx = state.new_tab();
    let specs = file.tiles.clone();
    let t = &mut state.tabs[idx];
    t.title = path.as_ref().and_then(|p| p.file_name().map(|n| n.to_string_lossy().trim_end_matches(DASHBOARD_EXT).to_string())).unwrap_or_else(|| file.title.clone());
    t.custom_title = true;
    t.file_path = path.clone();
    t.results_visible = false;
    t.dashboard = Some(Box::new(DashboardState { file: DashboardFile { tiles: Vec::new(), ..file }, tiles: Vec::new(), dirty: path.is_none(), home_for }));
    for spec in specs {
        add_tile(state, cx, idx, spec);
    }
    if let Some(d) = state.tabs[idx].dashboard.as_deref_mut() {
        d.dirty = path.is_none();
    }
    idx
}

/// Open a dashboard file (or focus it when it is already open).
pub fn open_path(state: &mut AppState, cx: &Ctx, path: PathBuf) -> bool {
    if let Some(i) = state.tabs.iter().position(|t| t.is_dashboard() && t.file_path.as_deref() == Some(path.as_path())) {
        state.active_tab = Some(i);
        return true;
    }
    match std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|s| DashboardFile::parse(&s)) {
        Ok(file) => {
            new_tab(state, cx, file, Some(path), None);
            true
        }
        Err(e) => {
            cx.toast(ToastKind::Error, format!("Could not open {}: {e}", path.display()));
            false
        }
    }
}

/// The server's home page: the remembered file when it exists, else a fresh page for it.
pub fn open_home(state: &mut AppState, cx: &Ctx, profile: ProfileId) {
    let remembered: Option<String> = cx.store.get_kv(&home_key(profile)).ok().flatten();
    if let Some(p) = remembered.map(PathBuf::from).filter(|p| p.is_file()) {
        if open_path(state, cx, p) {
            if let Some(i) = state.active_tab {
                if let Some(d) = state.tabs[i].dashboard.as_deref_mut() {
                    d.home_for = Some(profile);
                }
            }
            return;
        }
    }
    let name = state.library.profile(profile).map(|p| p.display_name()).unwrap_or_else(|| "Server".into());
    let file = DashboardFile { title: format!("{name} dashboard"), ..Default::default() };
    new_tab(state, cx, file, None, Some(profile));
    cx.toast(ToastKind::Info, format!("A new dashboard for {name}: add tiles, then Save — it opens from the server's menu from then on."));
}

/// The file as the page is now.
pub fn file_of(d: &DashboardState) -> DashboardFile {
    DashboardFile { tiles: d.tiles.iter().map(|t| t.spec.clone()).collect(), ..d.file.clone() }
}

pub fn save(state: &mut AppState, cx: &Ctx, idx: usize, save_as: bool) {
    let Some(t) = state.tabs.get(idx) else { return };
    let Some(d) = t.dashboard.as_deref() else { return };
    let path = match (&t.file_path, save_as) {
        (Some(p), false) => p.clone(),
        _ => {
            let mut dlg = rfd::FileDialog::new().add_filter("Cobalt dashboard", &["json"]).set_file_name(format!("{}{DASHBOARD_EXT}", sanitize(&d.file.title)));
            if let Some(dir) = cx.settings.export.last_dir.as_ref() {
                dlg = dlg.set_directory(dir);
            }
            match dlg.save_file() {
                Some(p) => p,
                None => return,
            }
        }
    };
    let text = file_of(d).to_json();
    match std::fs::write(&path, text) {
        Ok(()) => {
            let home = d.home_for;
            let t = &mut state.tabs[idx];
            t.file_path = Some(path.clone());
            t.title = path.file_name().map(|n| n.to_string_lossy().trim_end_matches(DASHBOARD_EXT).to_string()).unwrap_or(t.title.clone());
            if let Some(d) = t.dashboard.as_deref_mut() {
                d.dirty = false;
            }
            if let Some(p) = home {
                let _ = cx.store.set_kv(&home_key(p), &path.to_string_lossy().to_string());
            }
            state.flash(format!("Saved {}", path.display()));
        }
        Err(e) => cx.toast(ToastKind::Error, format!("Save failed: {e}")),
    }
}

fn sanitize(title: &str) -> String {
    let s: String = title.chars().map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' { c } else { '_' }).collect();
    let s = s.trim().to_string();
    if s.is_empty() { "dashboard".into() } else { s }
}

/// A profile for a tile: by id, then by name, then by server.
pub fn resolve_profile(state: &AppState, p: &ProfileRef) -> Option<ConnectionProfile> {
    let by_id = p.id.as_deref().and_then(ProfileId::parse).and_then(|id| state.library.profile(id).cloned());
    by_id
        .or_else(|| (!p.name.is_empty()).then(|| state.library.profiles.iter().find(|x| x.display_name().eq_ignore_ascii_case(&p.name)).cloned()).flatten())
        .or_else(|| (!p.server.is_empty()).then(|| state.library.profiles.iter().find(|x| x.server.eq_ignore_ascii_case(&p.server)).cloned()).flatten())
}

pub fn profile_ref(p: &ConnectionProfile) -> ProfileRef {
    ProfileRef { id: Some(p.id.to_string()), name: p.display_name(), server: p.server.clone() }
}

/// Add a tile and connect it; the first refresh follows the connection.
pub fn add_tile(state: &mut AppState, cx: &Ctx, idx: usize, spec: TileSpec) -> Option<usize> {
    let mut tab = EditorTab::new(0);
    tab.title = spec.title.clone();
    tab.custom_title = true;
    tab.text = spec.sql.clone();
    tab.mark_saved();
    let d = state.tabs.get_mut(idx)?.dashboard.as_deref_mut()?;
    d.tiles.push(Tile { spec, tab, last_refresh: None, pending: true, configure: false, error: None });
    d.dirty = true;
    let ti = d.tiles.len() - 1;
    connect_tile(state, cx, idx, ti);
    Some(ti)
}

/// Replace a tile's settings (a new query or connection reconnects and refreshes).
pub fn update_tile(state: &mut AppState, cx: &Ctx, idx: usize, ti: usize, spec: TileSpec) {
    let Some(d) = state.tabs.get_mut(idx).and_then(|t| t.dashboard.as_deref_mut()) else { return };
    let Some(tile) = d.tiles.get_mut(ti) else { return };
    let reconnect = tile.spec.profile != spec.profile || tile.spec.database != spec.database;
    let rerun = reconnect || tile.spec.sql != spec.sql;
    let chart_changed = tile.spec.chart != spec.chart || tile.spec.kind != spec.kind;
    tile.spec = spec;
    tile.tab.title = tile.spec.title.clone();
    tile.tab.text = tile.spec.sql.clone();
    if chart_changed {
        if let Some(v) = tile.tab.run.as_mut().and_then(|r| r.result_sets.first_mut()) {
            v.chart = tile.spec.chart.clone().map(ChartView::new);
        }
    }
    d.dirty = true;
    if reconnect {
        let id = tile.tab.id;
        cx.session.send(crate::session::Command::Disconnect { tab: id });
        tile.tab.conn = ConnState::Disconnected;
        tile.tab.creds = None;
        connect_tile(state, cx, idx, ti);
    } else if rerun {
        refresh_tile(state, cx, idx, ti);
    }
}

pub fn remove_tile(state: &mut AppState, cx: &Ctx, idx: usize, ti: usize) {
    let Some(d) = state.tabs.get_mut(idx).and_then(|t| t.dashboard.as_deref_mut()) else { return };
    if ti >= d.tiles.len() {
        return;
    }
    let tile = d.tiles.remove(ti);
    cx.session.send(crate::session::Command::Disconnect { tab: tile.tab.id });
    d.dirty = true;
}

/// Disconnect every tile (the tab is closing).
pub fn close(state: &mut AppState, cx: &Ctx, idx: usize) {
    if let Some(d) = state.tabs.get(idx).and_then(|t| t.dashboard.as_deref()) {
        for tile in &d.tiles {
            cx.session.send(crate::session::Command::Disconnect { tab: tile.tab.id });
        }
    }
}

fn connect_tile(state: &mut AppState, cx: &Ctx, idx: usize, ti: usize) {
    let (tab_id, pref, database) = {
        let Some(d) = state.tabs.get(idx).and_then(|t| t.dashboard.as_deref()) else { return };
        let Some(tile) = d.tiles.get(ti) else { return };
        (tile.tab.id, tile.spec.profile.clone(), tile.spec.database.clone())
    };
    let Some(profile) = resolve_profile(state, &pref) else {
        if let Some(tile) = state.tabs.get_mut(idx).and_then(|t| t.dashboard.as_deref_mut()).and_then(|d| d.tiles.get_mut(ti)) {
            tile.error = Some(format!("no connection named {:?} ({}) in the library — edit the tile and pick one", pref.name, pref.server));
            tile.pending = false;
        }
        return;
    };
    if let Some(tile) = state.tabs.get_mut(idx).and_then(|t| t.dashboard.as_deref_mut()).and_then(|d| d.tiles.get_mut(ti)) {
        tile.error = None;
        tile.pending = true;
    }
    let creds = state.library.servers.get(&profile.id).and_then(|s| s.creds.clone());
    match creds {
        Some(c) => ops::finish_connect(state, cx, profile, c, ConnectPurpose::Tab { tab: tab_id, database }),
        None => ops::begin_connect(state, cx, profile, ConnectPurpose::Tab { tab: tab_id, database }),
    }
}

/// Refresh one tile now (connecting first when it is not connected).
pub fn refresh_tile(state: &mut AppState, cx: &Ctx, idx: usize, ti: usize) {
    let connected = state.tabs.get(idx).and_then(|t| t.dashboard.as_deref()).and_then(|d| d.tiles.get(ti)).map(|t| t.tab.conn.is_connected());
    match connected {
        Some(true) => run_tile(state, cx, idx, ti),
        Some(false) => connect_tile(state, cx, idx, ti),
        None => {}
    }
}

pub fn refresh_all(state: &mut AppState, cx: &Ctx, idx: usize) {
    let n = state.tabs.get(idx).and_then(|t| t.dashboard.as_deref()).map(|d| d.tiles.len()).unwrap_or(0);
    for ti in 0..n {
        refresh_tile(state, cx, idx, ti);
    }
}

fn run_tile(state: &mut AppState, cx: &Ctx, idx: usize, ti: usize) {
    let Some(d) = state.tabs.get_mut(idx).and_then(|t| t.dashboard.as_deref_mut()) else { return };
    let Some(tile) = d.tiles.get_mut(ti) else { return };
    if tile.is_live() {
        return;
    }
    let run_id = cx.session.new_run();
    let mut view = RunView::new(run_id);
    view.script_hash = crate::state::hash_text(&tile.spec.sql);
    tile.tab.run = Some(view);
    tile.pending = false;
    tile.last_refresh = Some(Instant::now());
    let mut opts = tile.tab.exec.clone().with_defaults(&cx.settings.execution.session);
    if opts.timeout_secs == 0 {
        opts.timeout_secs = cx.settings.execution.command_timeout_secs;
    }
    opts.plan = cobalt_core::PlanMode::None;
    cx.session.send(crate::session::Command::Run { tab: tile.tab.id, run: run_id, script: tile.spec.sql.clone(), opts, start_line: 1, sink: None });
}

/// Each frame: start the runs that waited for a connection, refresh tiles whose interval has
/// passed, give chart tiles their chart once rows exist, and keep the page repainting while
/// anything is on a clock.
pub fn tick(state: &mut AppState, cx: &Ctx) {
    let mut to_run: Vec<(usize, usize)> = Vec::new();
    let mut to_connect: Vec<(usize, usize)> = Vec::new();
    let mut any_clock = false;
    for (i, t) in state.tabs.iter_mut().enumerate() {
        let Some(d) = t.dashboard.as_deref_mut() else { continue };
        for (k, tile) in d.tiles.iter_mut().enumerate() {
            match &tile.tab.conn {
                ConnState::Connected { .. } => {
                    if tile.pending {
                        to_run.push((i, k));
                    }
                }
                ConnState::Failed { error, .. } if tile.pending => {
                    tile.error = Some(error.clone());
                    tile.pending = false;
                }
                _ => {}
            }
            if tile.spec.interval_secs > 0 {
                any_clock = true;
                let due = tile.last_refresh.map(|r| r.elapsed().as_secs() >= tile.spec.interval_secs).unwrap_or(false);
                if due && !tile.is_live() && !tile.pending {
                    if tile.tab.conn.is_connected() {
                        to_run.push((i, k));
                    } else if matches!(tile.tab.conn, ConnState::Disconnected) {
                        to_connect.push((i, k));
                    }
                }
            }
            // a chart tile gets its chart as soon as the first result set exists
            if tile.spec.kind == TileKind::Chart {
                if let Some(v) = tile.tab.run.as_mut().and_then(|r| r.result_sets.first_mut()) {
                    if v.chart.is_none() {
                        let spec = tile.spec.chart.clone().unwrap_or_else(|| cobalt_results::chart::default_spec(&v.rs, &[]));
                        v.chart = Some(ChartView::new(spec));
                    }
                    // the strip's edits are the tile's settings
                    if let Some(c) = v.chart.as_ref() {
                        if tile.spec.chart.as_ref() != Some(&c.spec) {
                            tile.spec.chart = Some(c.spec.clone());
                            d.dirty = true;
                        }
                    }
                }
            }
        }
    }
    for (i, k) in to_run {
        run_tile(state, cx, i, k);
    }
    for (i, k) in to_connect {
        connect_tile(state, cx, i, k);
    }
    if any_clock {
        cx.egui.request_repaint_after(std::time::Duration::from_secs(1));
    }
}

/// A tile spec from a query tab's result set: the tab's text and connection, the chart when
/// the set is charted.
pub fn spec_from_tab(state: &AppState, idx: usize, set: usize, title: String, interval_secs: u64, kind: TileKind) -> Option<TileSpec> {
    let t = state.tabs.get(idx)?;
    let profile = t.profile.as_ref()?;
    let view = t.run.as_ref().and_then(|r| r.result_sets.get(set));
    let chart = view.and_then(|v| v.chart.as_ref()).map(|c| c.spec.clone());
    Some(TileSpec {
        title,
        profile: profile_ref(profile),
        database: t.conn.database().map(str::to_string).or_else(|| profile.database.clone()),
        sql: t.text.clone(),
        kind,
        chart,
        interval_secs,
        ..Default::default()
    })
}

/// What a result set looks like as a tile by default: a chart when charted, a number for a
/// single cell, else a grid.
pub fn suggested_kind(state: &AppState, idx: usize, set: usize) -> TileKind {
    let view = state.tabs.get(idx).and_then(|t| t.run.as_ref()).and_then(|r| r.result_sets.get(set));
    match view {
        Some(v) if v.chart.is_some() => TileKind::Chart,
        Some(v) if v.rs.row_count() == 1 && v.rs.column_count() == 1 => TileKind::Number,
        _ => TileKind::Grid,
    }
}

/// Open the pin dialog for a result set of the tab.
pub fn open_pin_dialog(state: &mut AppState, cx: &Ctx, idx: usize, set: usize) {
    if state.tabs.get(idx).and_then(|t| t.profile.as_ref()).is_none() {
        cx.toast(ToastKind::Warning, "Connect the tab first: a tile needs a connection to run on.");
        return;
    }
    let title = state.tabs[idx].title.trim_end_matches(".sql").to_string();
    let target = state.tabs.iter().position(|t| t.is_dashboard());
    let kind = suggested_kind(state, idx, set);
    state.dialog = Dialog::PinTile { tab_index: idx, set, title, target, interval_secs: 300, kind };
}

/// Pin: a tile on an open dashboard, or on a new one.
pub fn pin(state: &mut AppState, cx: &Ctx, idx: usize, set: usize, title: String, target: Option<usize>, interval_secs: u64, kind: TileKind) {
    let Some(spec) = spec_from_tab(state, idx, set, title, interval_secs, kind) else { return };
    let dash = match target.filter(|i| state.tabs.get(*i).map(|t| t.is_dashboard()).unwrap_or(false)) {
        Some(i) => i,
        None => new_tab(state, cx, DashboardFile::default(), None, None),
    };
    add_tile(state, cx, dash, spec);
    state.active_tab = Some(dash);
}
