//! The Servers view: groups → profiles → databases → folders → objects → columns/keys/indexes.

use crate::state::{fmt_count, DbNode, Folder, Library, Loadable, SubFolder};
use crate::ui::theme::Theme;
use crate::ui::widgets::{icon_for_object, tree_row, TreeRow};
use cobalt_core::*;
use cobalt_driver::ScriptKind;
use egui::{RichText, Ui};
use egui_phosphor::regular as icons;

#[derive(Debug, Clone)]
pub enum TreeAction {
    NewConnection { group: Option<GroupId> },
    NewGroup,
    EditGroup(GroupId),
    DeleteGroup(GroupId),
    EditProfile(ProfileId),
    DeleteProfile(ProfileId),
    ConnectServer(ProfileId),
    DisconnectServer(ProfileId),
    RefreshServer(ProfileId),
    ExpandDatabase { profile: ProfileId, database: String },
    RefreshDatabase { profile: ProfileId, database: String },
    LoadObjectChildren { profile: ProfileId, obj: ObjectRef, sub: SubFolder },
    NewQuery { profile: ProfileId, database: Option<String> },
    SelectTop { profile: ProfileId, obj: ObjectRef },
    Script { profile: ProfileId, obj: ObjectRef, kind: ScriptKind },
    CopyText(String),
    InsertIntoEditor(String),
    MoveProfile { profile: ProfileId, group: Option<GroupId> },
    ToggleSystemDbs(ProfileId),
    ToggleGroupBySchema(ProfileId),
    /// Row count and size for the Describe hover.
    LoadTableStats { profile: ProfileId, obj: ObjectRef },
    /// Import a flat file into a new or existing table of this database.
    ImportFile { profile: ProfileId, database: String },
    /// A Spark SQL query tab on this lakehouse (None = plain local Spark, no lakehouse).
    SparkQuery(Option<crate::sparkq::SparkEntry>),
    /// A Spark SQL tab whose lakehouse is picked from its toolbar.
    SparkChoose,
    /// Start (true) or stop (false) the local Spark session.
    SparkSession(bool),
}

pub fn show(ui: &mut Ui, lib: &mut Library, theme: &Theme, active_profile: Option<ProfileId>, spark: &crate::sparkq::SparkRoot, spark_expanded: &mut bool, spark_active: bool) -> Vec<TreeAction> {
    let mut actions = Vec::new();
    // toolbar
    egui::Frame::new().inner_margin(egui::Margin::symmetric(6, 4)).show(ui, |ui| {
        ui.horizontal(|ui| {
            crate::ui::widgets::section_title(ui, theme, "Servers");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if crate::ui::widgets::icon_button(ui, icons::PLUS, "New connection (Ctrl+Shift+N)", true).clicked() {
                    actions.push(TreeAction::NewConnection { group: None });
                }
                if crate::ui::widgets::icon_button(ui, icons::FOLDER_PLUS, "New server group", true).clicked() {
                    actions.push(TreeAction::NewGroup);
                }
                if crate::ui::widgets::icon_button(ui, icons::ARROWS_IN_LINE_VERTICAL, "Collapse all", true).clicked() {
                    for s in lib.servers.values_mut() {
                        s.expanded = false;
                    }
                }
            });
        });
        ui.add(egui::TextEdit::singleline(&mut lib.filter).hint_text(format!("{} Filter servers", icons::MAGNIFYING_GLASS)).desired_width(f32::INFINITY));
    });
    ui.separator();
    egui::ScrollArea::both().id_salt("servers-tree").auto_shrink([false, false]).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        let filter = lib.filter.trim().to_lowercase();
        let groups = lib.groups.clone();
        let profiles = lib.profiles.clone();
        spark_root(ui, theme, spark, spark_expanded, spark_active, &filter, &mut actions);
        // ungrouped first, then groups
        let ungrouped: Vec<&ConnectionProfile> = profiles.iter().filter(|p| p.group.is_none() || !groups.iter().any(|g| Some(g.id) == p.group)).collect();
        for p in ungrouped {
            if !filter.is_empty() && !p.display_name().to_lowercase().contains(&filter) && !p.server.to_lowercase().contains(&filter) {
                continue;
            }
            server_node(ui, lib, theme, p, 0, active_profile, &mut actions);
        }
        for g in groups.iter().filter(|g| g.parent.is_none()) {
            group_node(ui, lib, theme, g, &groups, &profiles, 0, &filter, active_profile, &mut actions);
        }
        if profiles.is_empty() && groups.is_empty() {
            ui.add_space(12.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("No connections yet").color(theme.text_muted));
                if ui.button(format!("{} Add a connection", icons::PLUS)).clicked() {
                    actions.push(TreeAction::NewConnection { group: None });
                }
                ui.add_space(6.0);
                ui.label(RichText::new("or File › Import Azure Data Studio connections").size(11.0).color(theme.text_faint));
            });
        }
        ui.add_space(40.0);
    });
    actions
}

/// The Local Spark root: the session's state, then the lakehouses a Spark SQL tab can open on
/// (bound to open tabs and notebooks, pinned, used before), plain local Spark, and a chooser.
fn spark_root(ui: &mut Ui, theme: &Theme, spark: &crate::sparkq::SparkRoot, expanded: &mut bool, active: bool, filter: &str, actions: &mut Vec<TreeAction>) {
    let open = *expanded || !filter.is_empty();
    let icon_color = if spark.ready { Some(theme.success) } else if spark.starting { Some(theme.warning) } else { None };
    let detail = spark.session.strip_prefix("Spark: ").map(str::to_string).unwrap_or_else(|| spark.session.clone());
    let r = tree_row(ui, theme, TreeRow { depth: 0, expandable: true, expanded: open, loading: spark.starting, icon: icons::FIRE, icon_color, label: "Local Spark", detail: Some(&detail), selected: active, color_dot: None, kind: "server" });
    if r.toggle || r.response.clicked() {
        *expanded = !*expanded;
    }
    r.response.context_menu(|ui| {
        if ui.button(format!("{} New Spark SQL query", icons::FIRE)).clicked() {
            actions.push(TreeAction::SparkQuery(None));
            ui.close();
        }
        ui.separator();
        if !spark.ready && !spark.starting {
            if ui.button(format!("{} Start session", icons::PLAY)).clicked() {
                actions.push(TreeAction::SparkSession(true));
                ui.close();
            }
        } else if ui.button(format!("{} Stop session", icons::STOP)).clicked() {
            actions.push(TreeAction::SparkSession(false));
            ui.close();
        }
    });
    if !open {
        return;
    }
    let mut shown = 0;
    for e in &spark.entries {
        if !filter.is_empty() && !e.lakehouse_name.to_lowercase().contains(filter) && !e.workspace_name.to_lowercase().contains(filter) {
            continue;
        }
        shown += 1;
        let detail = if e.open > 0 { format!("{} · {} open", e.workspace_name, e.open) } else { e.workspace_name.clone() };
        let r = tree_row(ui, theme, TreeRow { depth: 1, expandable: false, expanded: false, loading: false, icon: icons::DROP, icon_color: if e.open > 0 { Some(theme.accent) } else { None }, label: &e.lakehouse_name, detail: Some(&detail), selected: false, color_dot: None, kind: "lakehouse" });
        let r = r.response.on_hover_text(format!("{} ({})
Double-click for a Spark SQL query tab on this lakehouse.", e.lakehouse_name, e.workspace_name));
        if r.double_clicked() {
            actions.push(TreeAction::SparkQuery(Some(e.clone())));
        }
        r.context_menu(|ui| {
            if ui.button("New Spark SQL query").clicked() {
                actions.push(TreeAction::SparkQuery(Some(e.clone())));
                ui.close();
            }
        });
    }
    if filter.is_empty() {
        if spark.entries.is_empty() {
            let r = tree_row(ui, theme, TreeRow { depth: 1, expandable: false, expanded: false, loading: false, icon: icons::INFO, icon_color: None, label: if spark.signed_in { "No lakehouses yet" } else { "Sign in on the Fabric panel for lakehouses" }, detail: None, selected: false, color_dot: None, kind: "" });
            r.response.on_hover_text("Lakehouses appear here when a Spark notebook or tab binds one, when they are pinned on the Fabric panel, or after they were used in a Spark SQL tab.");
        }
        let r = tree_row(ui, theme, TreeRow { depth: 1, expandable: false, expanded: false, loading: false, icon: icons::DOTS_THREE, icon_color: None, label: "Choose a lakehouse…", detail: None, selected: false, color_dot: None, kind: "lakehouse" });
        if r.response.clicked() {
            actions.push(TreeAction::SparkChoose);
        }
        let r = tree_row(ui, theme, TreeRow { depth: 1, expandable: false, expanded: false, loading: false, icon: icons::TERMINAL_WINDOW, icon_color: None, label: "Plain local Spark", detail: Some("no lakehouse"), selected: false, color_dot: None, kind: "lakehouse" });
        if r.response.double_clicked() || r.response.clicked() && shown == 0 && spark.entries.is_empty() {
            actions.push(TreeAction::SparkQuery(None));
        }
        r.response.on_hover_text("Double-click for a Spark SQL tab with no lakehouse (temp views, paths, the session catalog).");
    }
    ui.add_space(4.0);
}

fn group_node(ui: &mut Ui, lib: &mut Library, theme: &Theme, g: &ServerGroup, groups: &[ServerGroup], profiles: &[ConnectionProfile], depth: usize, filter: &str, active: Option<ProfileId>, actions: &mut Vec<TreeAction>) {
    let expanded = lib.expanded_groups.contains(&g.id) || !filter.is_empty();
    let members: Vec<&ConnectionProfile> = profiles.iter().filter(|p| p.group == Some(g.id)).collect();
    let r = tree_row(ui, theme, TreeRow { depth, expandable: true, expanded, loading: false, icon: icons::FOLDER, icon_color: Some(Theme::color32(g.color)), label: &g.name, detail: Some(&format!("{}", members.len())), selected: false, color_dot: None, kind: "group" });
    if r.toggle || r.response.clicked() {
        if lib.expanded_groups.contains(&g.id) {
            lib.expanded_groups.remove(&g.id);
        } else {
            lib.expanded_groups.insert(g.id);
        }
    }
    r.response.context_menu(|ui| {
        if ui.button("New connection in this group").clicked() {
            actions.push(TreeAction::NewConnection { group: Some(g.id) });
            ui.close();
        }
        if ui.button("Edit group…").clicked() {
            actions.push(TreeAction::EditGroup(g.id));
            ui.close();
        }
        if ui.button("Delete group").clicked() {
            actions.push(TreeAction::DeleteGroup(g.id));
            ui.close();
        }
    });
    if expanded {
        for child in groups.iter().filter(|c| c.parent == Some(g.id)) {
            group_node(ui, lib, theme, child, groups, profiles, depth + 1, filter, active, actions);
        }
        for p in members {
            if !filter.is_empty() && !p.display_name().to_lowercase().contains(filter) && !p.server.to_lowercase().contains(filter) {
                continue;
            }
            server_node(ui, lib, theme, p, depth + 1, active, actions);
        }
    }
}

fn server_node(ui: &mut Ui, lib: &mut Library, theme: &Theme, p: &ConnectionProfile, depth: usize, active: Option<ProfileId>, actions: &mut Vec<TreeAction>) {
    let color = lib.color_for(p).map(Theme::color32);
    let name = p.display_name();
    let node = lib.servers.entry(p.id).or_default();
    let connected = node.creds.is_some();
    let loading = node.databases.is_loading();
    let detail = {
        let engine = node.engine.as_ref().map(|e| e.short_label());
        match (p.read_only_guard, engine) {
            (true, Some(e)) => Some(format!("{} read-only · {e}", icons::LOCK_SIMPLE)),
            (true, None) => Some(format!("{} read-only", icons::LOCK_SIMPLE)),
            (false, e) => e,
        }
    };
    let icon = if p.looks_like_fabric() || p.looks_like_azure() { icons::CLOUD } else { icons::HARD_DRIVES };
    let icon_color = if connected { Some(theme.success) } else { None };
    let expanded = node.expanded;
    let r = tree_row(ui, theme, TreeRow { depth, expandable: true, expanded, loading, icon, icon_color, label: &name, detail: detail.as_deref(), selected: active == Some(p.id), color_dot: color, kind: "server" });
    if r.toggle || r.response.clicked() {
        if expanded {
            node.expanded = false;
        } else {
            node.expanded = true;
            if !connected {
                actions.push(TreeAction::ConnectServer(p.id));
            } else if node.databases.needs_load() {
                actions.push(TreeAction::RefreshServer(p.id));
            }
        }
    }
    if r.response.double_clicked() {
        actions.push(TreeAction::NewQuery { profile: p.id, database: None });
    }
    r.response.context_menu(|ui| {
        if ui.button(format!("{} New Query", icons::FILE_PLUS)).clicked() {
            actions.push(TreeAction::NewQuery { profile: p.id, database: None });
            ui.close();
        }
        if connected {
            if ui.button("Disconnect").clicked() {
                actions.push(TreeAction::DisconnectServer(p.id));
                ui.close();
            }
            if ui.button("Refresh").clicked() {
                actions.push(TreeAction::RefreshServer(p.id));
                ui.close();
            }
            let label = if node.show_system_dbs { "Hide system databases" } else { "Show system databases" };
            if ui.button(label).clicked() {
                actions.push(TreeAction::ToggleSystemDbs(p.id));
                ui.close();
            }
            if ui.button(format!("{} Group objects by schema", if node.group_by_schema { icons::CHECK_SQUARE } else { icons::SQUARE })).clicked() {
                actions.push(TreeAction::ToggleGroupBySchema(p.id));
                ui.close();
            }
        } else if ui.button("Connect").clicked() {
            actions.push(TreeAction::ConnectServer(p.id));
            ui.close();
        }
        ui.separator();
        if ui.button("Edit connection…").clicked() {
            actions.push(TreeAction::EditProfile(p.id));
            ui.close();
        }
        if ui.button("Copy server name").clicked() {
            actions.push(TreeAction::CopyText(p.server.clone()));
            ui.close();
        }
        ui.menu_button("Move to group", |ui| {
            if ui.button("(no group)").clicked() {
                actions.push(TreeAction::MoveProfile { profile: p.id, group: None });
                ui.close();
            }
            for g in &lib.groups {
                if ui.button(&g.name).clicked() {
                    actions.push(TreeAction::MoveProfile { profile: p.id, group: Some(g.id) });
                    ui.close();
                }
            }
        });
        ui.separator();
        if ui.button(RichText::new("Delete connection").color(theme.error)).clicked() {
            actions.push(TreeAction::DeleteProfile(p.id));
            ui.close();
        }
    });
    if !expanded {
        return;
    }
    profile_children(ui, lib, theme, p, depth + 1, actions);
}

/// The database subtree of a connected profile (shared with the Fabric explorer, which draws its
/// own header row). Pushes nothing when the profile is not connected yet.
pub fn profile_children(ui: &mut Ui, lib: &mut Library, theme: &Theme, p: &ConnectionProfile, depth: usize, actions: &mut Vec<TreeAction>) {
    let node = lib.servers.entry(p.id).or_default();
    let connected = node.creds.is_some();
    let loading = node.databases.is_loading();
    match &node.databases {
        Loadable::NotLoaded | Loadable::Loading => {
            if !connected && !loading {
                // connect pending
            }
        }
        Loadable::Failed(e) => {
            let msg = format!("{} {}", icons::WARNING, e.lines().next().unwrap_or(""));
            tree_row(ui, theme, TreeRow { depth, expandable: false, expanded: false, loading: false, icon: icons::WARNING, icon_color: Some(theme.error), label: &msg, detail: None, selected: false, color_dot: None, kind: "error" });
        }
        Loadable::Loaded(dbs) => {
            let show_sys = node.show_system_dbs;
            let dbs: Vec<DatabaseInfo> = dbs.iter().filter(|d| show_sys || !d.is_system).cloned().collect();
            let engine = node.engine.clone();
            for db in dbs {
                database_node(ui, lib, theme, p, &db, engine.as_ref(), depth, actions);
            }
        }
    }
}

fn database_node(ui: &mut Ui, lib: &mut Library, theme: &Theme, p: &ConnectionProfile, db: &DatabaseInfo, engine: Option<&EngineInfo>, depth: usize, actions: &mut Vec<TreeAction>) {
    let node = lib.servers.entry(p.id).or_default();
    let dbn = node.db_nodes.entry(db.name.clone()).or_default();
    let expanded = dbn.expanded;
    let loading = dbn.objects.is_loading();
    let detail = if db.state != "ONLINE" && !db.state.is_empty() { Some(db.state.as_str()) } else if db.is_read_only { Some("read-only") } else { None };
    let r = tree_row(ui, theme, TreeRow { depth, expandable: true, expanded, loading, icon: icons::DATABASE, icon_color: Some(if db.is_system { theme.text_faint } else { theme.accent }), label: &db.name, detail, selected: false, color_dot: None, kind: "database" });
    if r.toggle || r.response.clicked() {
        dbn.expanded = !expanded;
        if !expanded && dbn.objects.needs_load() {
            actions.push(TreeAction::ExpandDatabase { profile: p.id, database: db.name.clone() });
        }
    }
    if r.response.double_clicked() {
        actions.push(TreeAction::NewQuery { profile: p.id, database: Some(db.name.clone()) });
    }
    r.response.context_menu(|ui| {
        if ui.button(format!("{} New Query", icons::FILE_PLUS)).clicked() {
            actions.push(TreeAction::NewQuery { profile: p.id, database: Some(db.name.clone()) });
            ui.close();
        }
        if ui.button("Refresh").clicked() {
            actions.push(TreeAction::RefreshDatabase { profile: p.id, database: db.name.clone() });
            ui.close();
        }
        if ui.button(format!("{} Import data from file…", icons::UPLOAD_SIMPLE)).clicked() {
            actions.push(TreeAction::ImportFile { profile: p.id, database: db.name.clone() });
            ui.close();
        }
        if ui.button("Copy name").clicked() {
            actions.push(TreeAction::CopyText(db.name.clone()));
            ui.close();
        }
    });
    if !expanded {
        return;
    }
    database_children(ui, lib, theme, p, db, engine, depth + 1, actions);
}

/// The folders under one database (Tables, Views, Programmability, …) rendered at `depth`, without
/// the database row itself. Used by the Servers tree under an expanded database row and by the
/// Fabric panel, where an item *is* a database on the workspace endpoint and gets no extra row.
pub fn database_children(ui: &mut Ui, lib: &mut Library, theme: &Theme, p: &ConnectionProfile, db: &DatabaseInfo, engine: Option<&EngineInfo>, depth: usize, actions: &mut Vec<TreeAction>) {
    let node = lib.servers.entry(p.id).or_default();
    let group_by_schema = node.group_by_schema;
    let dbn = node.db_nodes.entry(db.name.clone()).or_default();
    if let Loadable::Failed(e) = &dbn.objects {
        let msg = e.lines().next().unwrap_or("").to_string();
        tree_row(ui, theme, TreeRow { depth, expandable: false, expanded: false, loading: false, icon: icons::WARNING, icon_color: Some(theme.error), label: &msg, detail: None, selected: false, color_dot: None, kind: "error" });
        return;
    }
    let caps = engine.map(|e| e.capabilities).unwrap_or(EngineKind::SqlServer.capabilities());
    let folders: Vec<Folder> = {
        let mut f = vec![Folder::Tables, Folder::Views];
        if caps.procedures || caps.functions {
            f.push(Folder::Programmability);
        }
        f.push(Folder::Schemas);
        if caps.synonyms {
            f.push(Folder::Synonyms);
        }
        if caps.sequences {
            f.push(Folder::Sequences);
        }
        if caps.table_types {
            f.push(Folder::TableTypes);
        }
        f
    };
    // filter box per database when loaded and big
    if let Loadable::Loaded(objs) = &dbn.objects {
        if objs.len() > 40 {
            ui.horizontal(|ui| {
                ui.add_space(14.0 * depth as f32 + 4.0);
                ui.add(egui::TextEdit::singleline(&mut dbn.filter).hint_text("Filter objects").desired_width(200.0));
            });
        }
    }
    for folder in folders {
        folder_node(ui, dbn, theme, p, folder, depth, group_by_schema, actions);
    }
}

fn folder_node(ui: &mut Ui, dbn: &mut DbNode, theme: &Theme, p: &ConnectionProfile, folder: Folder, depth: usize, group_by_schema: bool, actions: &mut Vec<TreeAction>) {
    let objects: Vec<ObjectRef> = match &dbn.objects {
        Loadable::Loaded(o) => o.clone(),
        _ => Vec::new(),
    };
    let filter = dbn.filter.trim().to_lowercase();
    // this folder's own filter (context menu → Filter…), on top of the database-wide one
    let folder_filter = dbn.folder_filters.get(&folder).map(|f| f.trim().to_lowercase()).unwrap_or_default();
    let matches = |o: &ObjectRef| {
        let n = o.name.to_lowercase();
        let sch = o.schema.to_lowercase();
        (filter.is_empty() || n.contains(&filter) || sch.contains(&filter)) && (folder_filter.is_empty() || n.contains(&folder_filter) || sch.contains(&folder_filter) || format!("{sch}.{n}").contains(&folder_filter))
    };
    let expanded = dbn.expanded_folders.contains(&folder);
    let count = if folder == Folder::Programmability {
        None
    } else if folder == Folder::Schemas {
        Some(objects.iter().map(|o| &o.schema).collect::<std::collections::HashSet<_>>().len())
    } else {
        Some(objects.iter().filter(|o| folder.kinds().contains(&o.kind)).count())
    };
    let shown = if folder_filter.is_empty() || folder == Folder::Programmability || folder == Folder::Schemas { None } else { Some(objects.iter().filter(|o| folder.kinds().contains(&o.kind) && matches(o)).count()) };
    let detail = match (shown, count) {
        (Some(s), Some(c)) => Some(format!("{s} of {c}")),
        (None, Some(c)) => Some(c.to_string()),
        _ => None,
    };
    let icon = match folder {
        Folder::Programmability => icons::CODE,
        Folder::Schemas => icons::FOLDERS,
        _ => icons::FOLDER_SIMPLE,
    };
    let r = tree_row(ui, theme, TreeRow { depth, expandable: true, expanded, loading: dbn.objects.is_loading(), icon, icon_color: Some(theme.warning), label: folder.label(), detail: detail.as_deref(), selected: false, color_dot: None, kind: "folder" });
    if r.toggle || r.response.clicked() {
        if expanded {
            dbn.expanded_folders.remove(&folder);
        } else {
            dbn.expanded_folders.insert(folder);
        }
    }
    let filterable = !matches!(folder, Folder::Programmability | Folder::Schemas);
    if filterable {
        r.response.context_menu(|ui| {
            let open = dbn.filter_open.contains(&folder);
            if ui.button(if open { "Hide filter" } else { "Filter…" }).clicked() {
                if open {
                    dbn.filter_open.remove(&folder);
                    dbn.folder_filters.remove(&folder);
                } else {
                    dbn.filter_open.insert(folder);
                    dbn.expanded_folders.insert(folder);
                }
                ui.close();
            }
        });
    }
    if !expanded {
        return;
    }
    if filterable && dbn.filter_open.contains(&folder) {
        ui.horizontal(|ui| {
            ui.add_space(14.0 * (depth + 1) as f32 + 4.0);
            let text = dbn.folder_filters.entry(folder).or_default();
            let r = ui.add(egui::TextEdit::singleline(text).hint_text(format!("Filter {}", folder.label().to_lowercase())).desired_width(180.0));
            r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, true, format!("filter {}", folder.label().to_lowercase())));
            if ui.small_button(icons::X).on_hover_text("Clear and hide the filter").clicked() {
                dbn.folder_filters.remove(&folder);
                dbn.filter_open.remove(&folder);
            }
        });
    }
    match folder {
        Folder::Programmability => {
            for sub in [Folder::Procedures, Folder::Functions] {
                folder_node(ui, dbn, theme, p, sub, depth + 1, group_by_schema, actions);
            }
        }
        Folder::Functions => {
            for sub in [Folder::TableFunctions, Folder::ScalarFunctions] {
                folder_node(ui, dbn, theme, p, sub, depth + 1, group_by_schema, actions);
            }
        }
        Folder::Schemas => {
            let mut schemas: Vec<&String> = objects.iter().map(|o| &o.schema).collect::<std::collections::HashSet<_>>().into_iter().collect();
            schemas.sort();
            for s in schemas {
                if !filter.is_empty() && !s.to_lowercase().contains(&filter) {
                    continue;
                }
                let r = tree_row(ui, theme, TreeRow { depth: depth + 1, expandable: false, expanded: false, loading: false, icon: icons::FOLDER_SIMPLE, icon_color: None, label: s, detail: None, selected: false, color_dot: None, kind: "schema" });
                r.response.context_menu(|ui| {
                    if ui.button("Copy name").clicked() {
                        actions.push(TreeAction::CopyText(s.clone()));
                        ui.close();
                    }
                });
            }
        }
        _ => {
            let mut items: Vec<&ObjectRef> = objects.iter().filter(|o| folder.kinds().contains(&o.kind) && matches(o)).collect();
            items.sort_by(|a, b| a.schema.cmp(&b.schema).then(a.name.cmp(&b.name)));
            if group_by_schema {
                // one expandable row per schema; a filter opens every schema it matches into
                let mut schemas: Vec<String> = items.iter().map(|o| o.schema.clone()).collect();
                schemas.dedup();
                for schema in schemas {
                    let key = (folder, schema.clone());
                    let forced = !filter.is_empty() || !folder_filter.is_empty();
                    let open = forced || dbn.expanded_schemas.contains(&key);
                    let n = items.iter().filter(|o| o.schema == schema).count().to_string();
                    let r = tree_row(ui, theme, TreeRow { depth: depth + 1, expandable: true, expanded: open, loading: false, icon: icons::FOLDER_SIMPLE, icon_color: None, label: &schema, detail: Some(&n), selected: false, color_dot: None, kind: "schema" });
                    if (r.toggle || r.response.clicked()) && !forced {
                        if open {
                            dbn.expanded_schemas.remove(&key);
                        } else {
                            dbn.expanded_schemas.insert(key.clone());
                        }
                    }
                    if open {
                        for obj in items.iter().filter(|o| o.schema == schema) {
                            object_node(ui, dbn, theme, p, obj, depth + 2, actions);
                        }
                    }
                }
            } else {
                for obj in items {
                    object_node(ui, dbn, theme, p, obj, depth + 1, actions);
                }
            }
        }
    }
}

/// The hover card for a table-like object: name, kind, and its columns (name · type · nullability).
fn describe_ui(ui: &mut Ui, theme: &Theme, obj: &ObjectRef, cols: Option<&Loadable<Vec<ColumnInfo>>>, stats: Option<&Loadable<TableStats>>) {
    ui.set_max_width(460.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new(obj.qualified()).strong());
        ui.label(RichText::new(obj.kind.label()).small().color(theme.text_muted));
        if let Some(Loadable::Loaded(s)) = stats {
            ui.label(RichText::new(format!("{} row{} · {}", fmt_count(s.rows), if s.rows == 1 { "" } else { "s" }, humansize::format_size(s.reserved_bytes, humansize::DECIMAL))).small().color(theme.text_faint));
        } else if matches!(stats, Some(Loadable::Loading)) {
            ui.label(RichText::new("counting…").small().color(theme.text_faint));
        }
    });
    match cols {
        Some(Loadable::Loaded(cols)) => {
            ui.label(RichText::new(format!("{} column{}", cols.len(), if cols.len() == 1 { "" } else { "s" })).small().color(theme.text_faint));
            egui::Grid::new(("describe", &obj.database, &obj.schema, &obj.name)).num_columns(3).spacing([12.0, 2.0]).show(ui, |ui| {
                for c in cols.iter().take(60) {
                    let mut name = RichText::new(&c.name).monospace();
                    if c.is_identity {
                        name = name.color(theme.accent);
                    }
                    ui.label(name);
                    ui.label(RichText::new(c.sql_type.to_string()).monospace().color(theme.text_muted));
                    ui.label(RichText::new(if c.nullable { "null" } else { "not null" }).small().color(theme.text_faint));
                    ui.end_row();
                }
            });
            if cols.len() > 60 {
                ui.label(RichText::new(format!("… and {} more (expand Columns)", cols.len() - 60)).small().color(theme.text_faint));
            }
        }
        Some(Loadable::Failed(e)) => {
            ui.colored_label(theme.error, e.lines().next().unwrap_or(""));
        }
        _ => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("Loading columns…").small().color(theme.text_muted));
            });
        }
    }
    ui.label(RichText::new("Double-click: SELECT TOP 1000 · drag into the editor").small().color(theme.text_faint));
}

fn object_node(ui: &mut Ui, dbn: &mut DbNode, theme: &Theme, p: &ConnectionProfile, obj: &ObjectRef, depth: usize, actions: &mut Vec<TreeAction>) {
    let oid = obj.object_id.unwrap_or(0);
    let expandable = matches!(obj.kind, ObjectKind::Table | ObjectKind::View | ObjectKind::Procedure | ObjectKind::TableFunction | ObjectKind::ScalarFunction | ObjectKind::TableType);
    let expanded = dbn.expanded_objects.contains(&oid);
    let label = obj.qualified();
    let r = tree_row(ui, theme, TreeRow { depth, expandable, expanded, loading: false, icon: icon_for_object(obj.kind), icon_color: None, label: &label, detail: None, selected: false, color_dot: None, kind: obj.kind.label() });
    if (r.toggle || r.response.clicked()) && expandable {
        if expanded {
            dbn.expanded_objects.remove(&oid);
        } else {
            dbn.expanded_objects.insert(oid);
        }
    }
    if r.response.double_clicked() {
        match obj.kind {
            ObjectKind::Table | ObjectKind::View | ObjectKind::Synonym => actions.push(TreeAction::SelectTop { profile: p.id, obj: obj.clone() }),
            ObjectKind::Procedure => actions.push(TreeAction::Script { profile: p.id, obj: obj.clone(), kind: ScriptKind::Execute }),
            _ => actions.push(TreeAction::Script { profile: p.id, obj: obj.clone(), kind: ScriptKind::Create }),
        }
    }
    if r.response.drag_started() {
        // drag into editor: keep it simple — copy name to a drag payload
        r.response.dnd_set_drag_payload(obj.bracketed());
    }
    // Describe on hover: columns and types, fetched on first hover and cached on the node.
    if matches!(obj.kind, ObjectKind::Table | ObjectKind::View | ObjectKind::TableType | ObjectKind::TableFunction) && r.response.contains_pointer() {
        if !dbn.columns.contains_key(&oid) {
            actions.push(TreeAction::LoadObjectChildren { profile: p.id, obj: obj.clone(), sub: SubFolder::Columns });
        }
        if obj.kind == ObjectKind::Table && !dbn.stats.contains_key(&oid) {
            actions.push(TreeAction::LoadTableStats { profile: p.id, obj: obj.clone() });
        }
        let stats = dbn.stats.get(&oid);
        let cols = dbn.columns.get(&oid);
        r.response.clone().on_hover_ui(|ui| describe_ui(ui, theme, obj, cols, stats));
    }
    r.response.context_menu(|ui| {
        if matches!(obj.kind, ObjectKind::Table | ObjectKind::View | ObjectKind::Synonym | ObjectKind::TableFunction) && ui.button("Select Top 1000 Rows").clicked() {
            actions.push(TreeAction::SelectTop { profile: p.id, obj: obj.clone() });
            ui.close();
        }
        if ui.button("New Query").clicked() {
            actions.push(TreeAction::NewQuery { profile: p.id, database: Some(obj.database.clone()) });
            ui.close();
        }
        ui.separator();
        ui.menu_button("Script as", |ui| {
            let kinds: &[(ScriptKind, &str)] = match obj.kind {
                ObjectKind::Table => &[(ScriptKind::Create, "CREATE"), (ScriptKind::Drop, "DROP"), (ScriptKind::Select, "SELECT")],
                ObjectKind::View => &[(ScriptKind::Create, "CREATE"), (ScriptKind::Alter, "ALTER"), (ScriptKind::Drop, "DROP"), (ScriptKind::Select, "SELECT")],
                ObjectKind::Procedure => &[(ScriptKind::Create, "CREATE"), (ScriptKind::Alter, "ALTER"), (ScriptKind::Drop, "DROP"), (ScriptKind::Execute, "EXECUTE")],
                ObjectKind::ScalarFunction | ObjectKind::TableFunction | ObjectKind::AggregateFunction => &[(ScriptKind::Create, "CREATE"), (ScriptKind::Alter, "ALTER"), (ScriptKind::Drop, "DROP"), (ScriptKind::Execute, "EXECUTE")],
                _ => &[(ScriptKind::Create, "CREATE"), (ScriptKind::Drop, "DROP")],
            };
            for (k, label) in kinds {
                if ui.button(format!("{label} to new tab")).clicked() {
                    actions.push(TreeAction::Script { profile: p.id, obj: obj.clone(), kind: *k });
                    ui.close();
                }
            }
        });
        ui.separator();
        if ui.button("Copy name").clicked() {
            actions.push(TreeAction::CopyText(obj.bracketed()));
            ui.close();
        }
        if ui.button("Insert name into editor").clicked() {
            actions.push(TreeAction::InsertIntoEditor(obj.bracketed()));
            ui.close();
        }
    });
    if !expanded {
        return;
    }
    let subs: Vec<SubFolder> = match obj.kind {
        ObjectKind::Table => vec![SubFolder::Columns, SubFolder::Keys, SubFolder::Indexes],
        ObjectKind::View | ObjectKind::TableType => vec![SubFolder::Columns],
        ObjectKind::Procedure | ObjectKind::ScalarFunction | ObjectKind::TableFunction => vec![SubFolder::Parameters],
        _ => vec![],
    };
    for sub in subs {
        let key = (oid, sub);
        let sub_expanded = dbn.expanded_subfolders.contains(&key);
        let (label, loading, loaded_count) = match sub {
            SubFolder::Columns => ("Columns", dbn.columns.get(&oid).map(|l| l.is_loading()).unwrap_or(false), dbn.columns.get(&oid).and_then(|l| l.get()).map(|v| v.len())),
            SubFolder::Keys => ("Keys", dbn.keys.get(&oid).map(|l| l.is_loading()).unwrap_or(false), dbn.keys.get(&oid).and_then(|l| l.get()).map(|v| v.len())),
            SubFolder::Indexes => ("Indexes", dbn.indexes.get(&oid).map(|l| l.is_loading()).unwrap_or(false), dbn.indexes.get(&oid).and_then(|l| l.get()).map(|v| v.len())),
            SubFolder::Parameters => ("Parameters", dbn.parameters.get(&oid).map(|l| l.is_loading()).unwrap_or(false), dbn.parameters.get(&oid).and_then(|l| l.get()).map(|v| v.len())),
        };
        let detail = loaded_count.map(|c| c.to_string());
        let r = tree_row(ui, theme, TreeRow { depth: depth + 1, expandable: true, expanded: sub_expanded, loading, icon: icons::FOLDER_SIMPLE, icon_color: Some(theme.warning), label, detail: detail.as_deref(), selected: false, color_dot: None, kind: "folder" });
        if r.toggle || r.response.clicked() {
            if sub_expanded {
                dbn.expanded_subfolders.remove(&key);
            } else {
                dbn.expanded_subfolders.insert(key);
                let needs = match sub {
                    SubFolder::Columns => dbn.columns.get(&oid).map(|l| l.needs_load()).unwrap_or(true),
                    SubFolder::Keys => dbn.keys.get(&oid).map(|l| l.needs_load()).unwrap_or(true),
                    SubFolder::Indexes => dbn.indexes.get(&oid).map(|l| l.needs_load()).unwrap_or(true),
                    SubFolder::Parameters => dbn.parameters.get(&oid).map(|l| l.needs_load()).unwrap_or(true),
                };
                if needs {
                    actions.push(TreeAction::LoadObjectChildren { profile: p.id, obj: obj.clone(), sub });
                }
            }
        }
        if !sub_expanded {
            continue;
        }
        match sub {
            SubFolder::Columns => {
                if let Some(Loadable::Loaded(cols)) = dbn.columns.get(&oid) {
                    for c in cols {
                        let detail = format!("{}{}{}", c.sql_type, if c.nullable { ", null" } else { ", not null" }, if c.is_identity { ", identity" } else if c.is_computed { ", computed" } else { "" });
                        let icon = if c.in_primary_key { icons::KEY } else { icons::COLUMNS };
                        let r = tree_row(ui, theme, TreeRow { depth: depth + 2, expandable: false, expanded: false, loading: false, icon, icon_color: if c.in_primary_key { Some(theme.warning) } else { None }, label: &c.name, detail: Some(&detail), selected: false, color_dot: None, kind: "column" });
                        if r.response.double_clicked() {
                            actions.push(TreeAction::InsertIntoEditor(quote_ident(&c.name)));
                        }
                        r.response.context_menu(|ui| {
                            if ui.button("Copy name").clicked() {
                                actions.push(TreeAction::CopyText(c.name.clone()));
                                ui.close();
                            }
                            if ui.button("Insert into editor").clicked() {
                                actions.push(TreeAction::InsertIntoEditor(quote_ident(&c.name)));
                                ui.close();
                            }
                        });
                    }
                } else if let Some(Loadable::Failed(e)) = dbn.columns.get(&oid) {
                    tree_row(ui, theme, TreeRow { depth: depth + 2, expandable: false, expanded: false, loading: false, icon: icons::WARNING, icon_color: Some(theme.error), label: e, detail: None, selected: false, color_dot: None, kind: "error" });
                }
            }
            SubFolder::Keys => {
                if let Some(Loadable::Loaded(keys)) = dbn.keys.get(&oid) {
                    for k in keys {
                        let detail = match (&k.kind, &k.references) {
                            (KeyKind::ForeignKey, Some((t, cols))) => format!("› {}.{} ({})", t.schema, t.name, cols.join(", ")),
                            (KeyKind::Check, _) | (KeyKind::Default, _) => k.definition.clone().unwrap_or_default(),
                            _ => format!("({})", k.columns.join(", ")),
                        };
                        let icon = match k.kind {
                            KeyKind::PrimaryKey => icons::KEY,
                            KeyKind::ForeignKey => icons::LINK,
                            KeyKind::Unique => icons::FINGERPRINT,
                            KeyKind::Check => icons::CHECK_SQUARE,
                            KeyKind::Default => icons::EQUALS,
                        };
                        let r = tree_row(ui, theme, TreeRow { depth: depth + 2, expandable: false, expanded: false, loading: false, icon, icon_color: None, label: &k.name, detail: Some(&detail), selected: false, color_dot: None, kind: "key" });
                        r.response.context_menu(|ui| {
                            if ui.button("Copy name").clicked() {
                                actions.push(TreeAction::CopyText(k.name.clone()));
                                ui.close();
                            }
                        });
                    }
                }
            }
            SubFolder::Indexes => {
                if let Some(Loadable::Loaded(idx)) = dbn.indexes.get(&oid) {
                    for i in idx {
                        let mut detail = format!("{}{}{} ({})", if i.is_unique { "unique " } else { "" }, if i.is_clustered { "clustered" } else { "nonclustered" }, if i.is_primary_key { " PK" } else { "" }, i.key_columns.iter().map(|(n, d)| if *d { format!("{n} DESC") } else { n.clone() }).collect::<Vec<_>>().join(", "));
                        if !i.included_columns.is_empty() {
                            detail.push_str(&format!(" include ({})", i.included_columns.join(", ")));
                        }
                        let r = tree_row(ui, theme, TreeRow { depth: depth + 2, expandable: false, expanded: false, loading: false, icon: icons::LIST_MAGNIFYING_GLASS, icon_color: None, label: &i.name, detail: Some(&detail), selected: false, color_dot: None, kind: "index" });
                        r.response.context_menu(|ui| {
                            if ui.button("Copy name").clicked() {
                                actions.push(TreeAction::CopyText(i.name.clone()));
                                ui.close();
                            }
                        });
                    }
                }
            }
            SubFolder::Parameters => {
                if let Some(Loadable::Loaded(params)) = dbn.parameters.get(&oid) {
                    for prm in params {
                        let detail = format!("{}{}", prm.sql_type, if prm.is_output { ", output" } else { "" });
                        tree_row(ui, theme, TreeRow { depth: depth + 2, expandable: false, expanded: false, loading: false, icon: icons::AT, icon_color: None, label: &prm.name, detail: Some(&detail), selected: false, color_dot: None, kind: "parameter" });
                    }
                }
            }
        }
    }
}
