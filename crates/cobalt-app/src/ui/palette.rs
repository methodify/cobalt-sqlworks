//! Command palette: fuzzy search over commands and connections.

use crate::commands::{Command, Keymap, COMMANDS};
use crate::state::AppState;
use crate::ui::theme::Theme;
use crate::ui::widgets::key_chip;
use cobalt_core::{ObjectRef, ProfileId};
use egui::{Key, RichText, Ui, Vec2};

#[derive(Clone, Debug)]
pub enum PaletteItem {
    Command(Command),
    Connect(ProfileId),
    Open(ProfileId, String),
    /// A table/view/procedure known to the catalog cache (Go to Object).
    Object(ProfileId, ObjectRef),
}

/// Every object the app currently knows about: catalogs loaded for completion on connected tabs,
/// plus whatever the Servers tree has expanded. De-duplicated per (connection, database, object).
fn known_objects(state: &AppState) -> Vec<(ProfileId, ObjectRef)> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    let mut push = |pid: ProfileId, o: &ObjectRef| {
        let key = (pid, o.database.to_ascii_lowercase(), o.schema.to_ascii_lowercase(), o.name.to_ascii_lowercase());
        if seen.insert(key) {
            out.push((pid, o.clone()));
        }
    };
    for t in &state.tabs {
        if let (Some(p), Some(cat)) = (&t.profile, &t.catalog) {
            for o in &cat.objects {
                push(p.id, o);
            }
        }
    }
    for (pid, node) in &state.library.servers {
        for dbn in node.db_nodes.values() {
            if let Some(objs) = dbn.objects.get() {
                for o in objs {
                    push(*pid, o);
                }
            }
            if let Some(cat) = dbn.catalog.get() {
                for o in &cat.objects {
                    push(*pid, o);
                }
            }
        }
    }
    out
}

fn fuzzy_score(query: &str, text: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let q: Vec<char> = query.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let mut score = 0i32;
    let mut qi = 0;
    let mut last = usize::MAX;
    for (i, c) in t.iter().enumerate() {
        if qi < q.len() && *c == q[qi] {
            score += if last != usize::MAX && i == last + 1 { 5 } else if i == 0 || !t[i - 1].is_alphanumeric() { 4 } else { 1 };
            last = i;
            qi += 1;
        }
    }
    if qi == q.len() {
        if text.to_lowercase().contains(&query.to_lowercase()) {
            score += 20;
        }
        if text.to_lowercase().starts_with(&query.to_lowercase()) {
            score += 30;
        }
        Some(score)
    } else {
        None
    }
}

pub fn show(ui: &mut Ui, state: &mut AppState, theme: &Theme, keymap: &Keymap) -> Option<PaletteItem> {
    if !state.palette_open {
        return None;
    }
    let mut chosen = None;
    let mut close = false;
    let query = state.palette_query.clone();
    // `#name` = Go to Object: search the catalog cache instead of commands
    let object_mode = query.starts_with('#');
    let mut items: Vec<(i32, PaletteItem, String, String)> = Vec::new();
    if object_mode {
        let q = query[1..].trim().to_string();
        for (pid, o) in known_objects(state) {
            let text = format!("{}.{}", o.schema, o.name);
            if let Some(s) = fuzzy_score(&q, &text) {
                let who = state.library.profile(pid).map(|p| p.display_name()).unwrap_or_default();
                items.push((s, PaletteItem::Object(pid, o.clone()), text, format!("{} · {} · {who}", o.kind.label(), o.database)));
            }
        }
    } else {
        for c in COMMANDS {
            let text = format!("{}: {}", c.category.label(), c.label);
            if let Some(s) = fuzzy_score(&query, &text) {
                items.push((s, PaletteItem::Command(c.cmd), text, keymap.shortcut_text(ui.ctx(), c.cmd)));
            }
        }
        for p in &state.library.profiles {
            let text = format!("Connect: {}", p.display_name());
            if let Some(s) = fuzzy_score(&query, &text) {
                items.push((s - 1, PaletteItem::Connect(p.id), text, p.server.clone()));
            }
        }
    }
    items.sort_by(|a, b| b.0.cmp(&a.0).then(a.2.cmp(&b.2)));
    items.truncate(40);
    if state.palette_selected >= items.len() {
        state.palette_selected = 0;
    }
    ui.input_mut(|i| {
        if i.consume_key(egui::Modifiers::NONE, Key::ArrowDown) && !items.is_empty() {
            state.palette_selected = (state.palette_selected + 1) % items.len();
        }
        if i.consume_key(egui::Modifiers::NONE, Key::ArrowUp) && !items.is_empty() {
            state.palette_selected = (state.palette_selected + items.len() - 1) % items.len();
        }
        if i.consume_key(egui::Modifiers::NONE, Key::Enter) {
            if let Some(it) = items.get(state.palette_selected) {
                chosen = Some(it.1.clone());
            }
            close = true;
        }
        if i.consume_key(egui::Modifiers::NONE, Key::Escape) {
            close = true;
        }
    });
    let screen = ui.ctx().content_rect();
    let width = 640.0f32.min(screen.width() - 40.0);
    egui::Area::new(egui::Id::new("palette")).order(egui::Order::Foreground).fixed_pos(egui::pos2(screen.center().x - width / 2.0, screen.top() + 80.0)).show(ui.ctx(), |ui| {
        egui::Frame::popup(ui.style()).fill(theme.bg_panel).stroke(egui::Stroke::new(1.0, theme.border_strong)).inner_margin(8.0).shadow(egui::Shadow { offset: [0, 6], blur: 24, spread: 0, color: egui::Color32::from_black_alpha(90) }).show(ui, |ui| {
            ui.set_width(width);
            let r = ui.add(egui::TextEdit::singleline(&mut state.palette_query).hint_text("Type a command or connection name… (# to find a table, view or procedure)").desired_width(f32::INFINITY).font(egui::FontId::proportional(16.0)).id(egui::Id::new("palette-input")));
            r.request_focus();
            if r.changed() {
                state.palette_selected = 0;
            }
            ui.add_space(6.0);
            egui::ScrollArea::vertical().max_height(400.0).auto_shrink([false, true]).show(ui, |ui| {
                for (i, (_, item, text, detail)) in items.iter().enumerate() {
                    let selected = i == state.palette_selected;
                    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 28.0), egui::Sense::click());
                    if selected {
                        ui.painter().rect_filled(rect, 4.0, theme.bg_selection);
                        ui.scroll_to_rect(rect, None);
                    } else if resp.hovered() {
                        ui.painter().rect_filled(rect, 4.0, theme.bg_hover);
                    }
                    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(Vec2::new(8.0, 0.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
                    child.label(RichText::new(text).size(14.0).color(theme.text));
                    child.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if matches!(item, PaletteItem::Command(_)) {
                            key_chip(ui, theme, detail);
                        } else {
                            ui.label(RichText::new(detail).size(11.0).color(theme.text_faint));
                        }
                    });
                    if resp.clicked() {
                        chosen = Some(item.clone());
                        close = true;
                    }
                }
                if items.is_empty() {
                    ui.label(RichText::new(if object_mode { "No matching objects. Connect a tab or expand a database in Servers so its objects are known." } else { "No matches" }).color(theme.text_faint));
                }
            });
        });
    });
    // click outside closes
    let palette_rect = egui::Rect::from_min_size(egui::pos2(screen.center().x - width / 2.0, screen.top() + 80.0), Vec2::new(width + 16.0, 480.0));
    if ui.input(|i| i.pointer.any_click()) {
        if let Some(p) = ui.input(|i| i.pointer.interact_pos()) {
            if !palette_rect.contains(p) {
                close = true;
            }
        }
    }
    if close {
        state.palette_open = false;
        state.palette_query.clear();
    }
    chosen
}
