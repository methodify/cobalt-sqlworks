//! The Servers view: groups → profiles → databases → folders → objects → columns/keys/indexes.

use crate::state::{DbNode, Folder, Library, Loadable, SubFolder};
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
}

pub fn show(ui: &mut Ui, lib: &mut Library, theme: &Theme, active_profile: Option<ProfileId>) -> Vec<TreeAction> {
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
                ui.label(RichText::new("or File → Import Azure Data Studio connections").size(11.0).color(theme.text_faint));
            });
        }
        ui.add_space(40.0);
    });
    actions
}

#[allow(clippy::too_many_arguments)]
fn group_node(ui: &mut Ui, lib: &mut Library, theme: &Theme, g: &ServerGroup, groups: &[ServerGroup], profiles: &[ConnectionProfile], depth: usize, filter: &str, active: Option<ProfileId>, actions: &mut Vec<TreeAction>) {
    let expanded = lib.expanded_groups.contains(&g.id) || !filter.is_empty();
    let members: Vec<&ConnectionProfile> = profiles.iter().filter(|p| p.group == Some(g.id)).collect();
    let r = tree_row(ui, theme, TreeRow { depth, expandable: true, expanded, loading: false, icon: icons::FOLDER, icon_color: Some(Theme::color32(g.color)), label: &g.name, detail: Some(&format!("{}", members.len())), selected: false, color_dot: None, id_salt: &g.id.to_string(), kind: "group" });
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
    let detail = node.engine.as_ref().map(|e| e.short_label());
    let icon = if p.looks_like_fabric() { icons::CLOUD } else if p.looks_like_azure() { icons::CLOUD } else { icons::HARD_DRIVES };
    let icon_color = if connected { Some(theme.success) } else { None };
    let expanded = node.expanded;
    let r = tree_row(ui, theme, TreeRow { depth, expandable: true, expanded, loading, icon, icon_color, label: &name, detail: detail.as_deref(), selected: active == Some(p.id), color_dot: color, id_salt: &p.id.to_string(), kind: "server" });
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
        Loadable::NotLoaded | Loadable::Loading(_) => {
            if !connected && !loading {
                // connect pending
            }
        }
        Loadable::Failed(e) => {
            let msg = format!("{} {}", icons::WARNING, e.lines().next().unwrap_or(""));
            tree_row(ui, theme, TreeRow { depth, expandable: false, expanded: false, loading: false, icon: icons::WARNING, icon_color: Some(theme.error), label: &msg, detail: None, selected: false, color_dot: None, id_salt: "err", kind: "error" });
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

#[allow(clippy::too_many_arguments)]
fn database_node(ui: &mut Ui, lib: &mut Library, theme: &Theme, p: &ConnectionProfile, db: &DatabaseInfo, engine: Option<&EngineInfo>, depth: usize, actions: &mut Vec<TreeAction>) {
    let node = lib.servers.entry(p.id).or_default();
    let dbn = node.db_nodes.entry(db.name.clone()).or_default();
    let expanded = dbn.expanded;
    let loading = dbn.objects.is_loading();
    let detail = if db.state != "ONLINE" && !db.state.is_empty() { Some(db.state.as_str()) } else if db.is_read_only { Some("read-only") } else { None };
    let r = tree_row(ui, theme, TreeRow { depth, expandable: true, expanded, loading, icon: icons::DATABASE, icon_color: Some(if db.is_system { theme.text_faint } else { theme.accent }), label: &db.name, detail, selected: false, color_dot: None, id_salt: &db.name, kind: "database" });
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
        if ui.button("Copy name").clicked() {
            actions.push(TreeAction::CopyText(db.name.clone()));
            ui.close();
        }
    });
    if !expanded {
        return;
    }
    if let Loadable::Failed(e) = &dbn.objects {
        let msg = e.lines().next().unwrap_or("").to_string();
        tree_row(ui, theme, TreeRow { depth: depth + 1, expandable: false, expanded: false, loading: false, icon: icons::WARNING, icon_color: Some(theme.error), label: &msg, detail: None, selected: false, color_dot: None, id_salt: "dberr", kind: "error" });
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
                ui.add_space(14.0 * (depth + 1) as f32 + 4.0);
                ui.add(egui::TextEdit::singleline(&mut dbn.filter).hint_text("Filter objects").desired_width(200.0));
            });
        }
    }
    for folder in folders {
        folder_node(ui, dbn, theme, p, db, folder, depth + 1, actions);
    }
}

#[allow(clippy::too_many_arguments)]
fn folder_node(ui: &mut Ui, dbn: &mut DbNode, theme: &Theme, p: &ConnectionProfile, db: &DatabaseInfo, folder: Folder, depth: usize, actions: &mut Vec<TreeAction>) {
    let objects: Vec<ObjectRef> = match &dbn.objects {
        Loadable::Loaded(o) => o.clone(),
        _ => Vec::new(),
    };
    let filter = dbn.filter.trim().to_lowercase();
    let expanded = dbn.expanded_folders.contains(&folder);
    let count = if folder == Folder::Programmability {
        None
    } else if folder == Folder::Schemas {
        Some(objects.iter().map(|o| &o.schema).collect::<std::collections::HashSet<_>>().len())
    } else {
        Some(objects.iter().filter(|o| folder.kinds().contains(&o.kind)).count())
    };
    let detail = count.map(|c| c.to_string());
    let icon = match folder {
        Folder::Programmability => icons::CODE,
        Folder::Schemas => icons::FOLDERS,
        _ => icons::FOLDER_SIMPLE,
    };
    let r = tree_row(ui, theme, TreeRow { depth, expandable: true, expanded, loading: dbn.objects.is_loading(), icon, icon_color: Some(theme.warning), label: folder.label(), detail: detail.as_deref(), selected: false, color_dot: None, id_salt: &format!("{}-{:?}", db.name, folder), kind: "folder" });
    if r.toggle || r.response.clicked() {
        if expanded {
            dbn.expanded_folders.remove(&folder);
        } else {
            dbn.expanded_folders.insert(folder);
        }
    }
    if !expanded {
        return;
    }
    match folder {
        Folder::Programmability => {
            for sub in [Folder::Procedures, Folder::Functions] {
                folder_node(ui, dbn, theme, p, db, sub, depth + 1, actions);
            }
        }
        Folder::Functions => {
            for sub in [Folder::TableFunctions, Folder::ScalarFunctions] {
                folder_node(ui, dbn, theme, p, db, sub, depth + 1, actions);
            }
        }
        Folder::Schemas => {
            let mut schemas: Vec<&String> = objects.iter().map(|o| &o.schema).collect::<std::collections::HashSet<_>>().into_iter().collect();
            schemas.sort();
            for s in schemas {
                if !filter.is_empty() && !s.to_lowercase().contains(&filter) {
                    continue;
                }
                let r = tree_row(ui, theme, TreeRow { depth: depth + 1, expandable: false, expanded: false, loading: false, icon: icons::FOLDER_SIMPLE, icon_color: None, label: s, detail: None, selected: false, color_dot: None, id_salt: s, kind: "schema" });
                r.response.context_menu(|ui| {
                    if ui.button("Copy name").clicked() {
                        actions.push(TreeAction::CopyText(s.clone()));
                        ui.close();
                    }
                });
            }
        }
        _ => {
            let mut items: Vec<&ObjectRef> = objects.iter().filter(|o| folder.kinds().contains(&o.kind)).collect();
            items.sort_by(|a, b| a.schema.cmp(&b.schema).then(a.name.cmp(&b.name)));
            for obj in items {
                if !filter.is_empty() && !obj.name.to_lowercase().contains(&filter) && !obj.schema.to_lowercase().contains(&filter) {
                    continue;
                }
                object_node(ui, dbn, theme, p, obj, depth + 1, actions);
            }
        }
    }
}

fn object_node(ui: &mut Ui, dbn: &mut DbNode, theme: &Theme, p: &ConnectionProfile, obj: &ObjectRef, depth: usize, actions: &mut Vec<TreeAction>) {
    let oid = obj.object_id.unwrap_or(0);
    let expandable = matches!(obj.kind, ObjectKind::Table | ObjectKind::View | ObjectKind::Procedure | ObjectKind::TableFunction | ObjectKind::ScalarFunction | ObjectKind::TableType);
    let expanded = dbn.expanded_objects.contains(&oid);
    let label = obj.qualified();
    let r = tree_row(ui, theme, TreeRow { depth, expandable, expanded, loading: false, icon: icon_for_object(obj.kind), icon_color: None, label: &label, detail: None, selected: false, color_dot: None, id_salt: &format!("{}-{}", oid, obj.name), kind: obj.kind.label() });
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
    r.response.context_menu(|ui| {
        match obj.kind {
            ObjectKind::Table | ObjectKind::View | ObjectKind::Synonym | ObjectKind::TableFunction => {
                if ui.button("Select Top 1000 Rows").clicked() {
                    actions.push(TreeAction::SelectTop { profile: p.id, obj: obj.clone() });
                    ui.close();
                }
            }
            _ => {}
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
        let r = tree_row(ui, theme, TreeRow { depth: depth + 1, expandable: true, expanded: sub_expanded, loading, icon: icons::FOLDER_SIMPLE, icon_color: Some(theme.warning), label, detail: detail.as_deref(), selected: false, color_dot: None, id_salt: &format!("{oid}-{sub:?}"), kind: "folder" });
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
                        let r = tree_row(ui, theme, TreeRow { depth: depth + 2, expandable: false, expanded: false, loading: false, icon, icon_color: if c.in_primary_key { Some(theme.warning) } else { None }, label: &c.name, detail: Some(&detail), selected: false, color_dot: None, id_salt: &format!("{oid}-c-{}", c.name), kind: "column" });
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
                    tree_row(ui, theme, TreeRow { depth: depth + 2, expandable: false, expanded: false, loading: false, icon: icons::WARNING, icon_color: Some(theme.error), label: e, detail: None, selected: false, color_dot: None, id_salt: "colerr", kind: "error" });
                }
            }
            SubFolder::Keys => {
                if let Some(Loadable::Loaded(keys)) = dbn.keys.get(&oid) {
                    for k in keys {
                        let detail = match (&k.kind, &k.references) {
                            (KeyKind::ForeignKey, Some((t, cols))) => format!("→ {}.{} ({})", t.schema, t.name, cols.join(", ")),
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
                        let r = tree_row(ui, theme, TreeRow { depth: depth + 2, expandable: false, expanded: false, loading: false, icon, icon_color: None, label: &k.name, detail: Some(&detail), selected: false, color_dot: None, id_salt: &format!("{oid}-k-{}", k.name), kind: "key" });
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
                        let r = tree_row(ui, theme, TreeRow { depth: depth + 2, expandable: false, expanded: false, loading: false, icon: icons::LIST_MAGNIFYING_GLASS, icon_color: None, label: &i.name, detail: Some(&detail), selected: false, color_dot: None, id_salt: &format!("{oid}-i-{}", i.name), kind: "index" });
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
                        tree_row(ui, theme, TreeRow { depth: depth + 2, expandable: false, expanded: false, loading: false, icon: icons::AT, icon_color: None, label: &prm.name, detail: Some(&detail), selected: false, color_dot: None, id_salt: &format!("{oid}-p-{}", prm.name), kind: "parameter" });
                    }
                }
            }
        }
    }
}
