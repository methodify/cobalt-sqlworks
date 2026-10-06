//! Window chrome: a floating window with a flat, left-aligned title bar and a plain close
//! button, rounded corners and a soft shadow — closer to a native dialog than egui's default
//! collapsible window (centered title, collapse triangle, hard shadow). Every secondary window
//! in the app (Settings, cell viewer, column profile, session log, shadows, About, shortcuts)
//! goes through this so they all look alike.

use crate::ui::theme::Theme;
use egui::{Color32, Id, RichText, Sense, Shadow, Stroke, Ui, Vec2};
use egui_phosphor::regular as icons;

pub const TITLE_H: f32 = 38.0;

pub struct Window<'a> {
    title: String,
    id: Id,
    open: Option<&'a mut bool>,
    default_size: Vec2,
    resizable: bool,
    min_size: Vec2,
}

impl<'a> Window<'a> {
    pub fn new(title: impl Into<String>) -> Self {
        let title = title.into();
        Self { id: Id::new(("cobalt-window", title.as_str())), title, open: None, default_size: Vec2::new(520.0, 400.0), resizable: true, min_size: Vec2::new(280.0, 120.0) }
    }
    pub fn id(mut self, id: Id) -> Self {
        self.id = id;
        self
    }
    /// Closing (the title-bar button) sets this to false; when it is already false the window
    /// is not shown.
    pub fn open(mut self, open: &'a mut bool) -> Self {
        self.open = Some(open);
        self
    }
    pub fn default_size(mut self, size: impl Into<Vec2>) -> Self {
        self.default_size = size.into();
        self
    }
    pub fn default_width(mut self, w: f32) -> Self {
        self.default_size.x = w;
        self
    }
    pub fn default_height(mut self, h: f32) -> Self {
        self.default_size.y = h;
        self
    }
    pub fn resizable(mut self, on: bool) -> Self {
        self.resizable = on;
        self
    }
    /// Accepted for drop-in compatibility with `egui::Window`; these windows never collapse.
    pub fn collapsible(self, _on: bool) -> Self {
        self
    }
    pub fn min_size(mut self, size: impl Into<Vec2>) -> Self {
        self.min_size = size.into();
        self
    }

    /// Show the window. Returns the content closure's value when the window was drawn.
    pub fn show<R>(self, ctx: &egui::Context, theme: &Theme, add: impl FnOnce(&mut Ui) -> R) -> Option<R> {
        if let Some(false) = self.open.as_deref() {
            return None;
        }
        let dark = theme.is_dark();
        let frame = egui::Frame::new()
            .fill(theme.bg_panel)
            .stroke(Stroke::new(1.0, theme.border))
            .corner_radius(10.0)
            .shadow(Shadow { offset: [0, 6], blur: 24, spread: 0, color: Color32::from_black_alpha(if dark { 110 } else { 45 }) })
            .inner_margin(0.0);
        let mut close = false;
        let title = self.title.clone();
        let win = egui::Window::new(&self.title).id(self.id).title_bar(false).collapsible(false).frame(frame).default_size(self.default_size).min_size(self.min_size).resizable(self.resizable).movable(true);
        let inner = win.show(ctx, |ui| {
            // title bar: drag handle, title on the left, a flat close button on the right
            let bar_w = ui.available_width();
            let (bar_rect, _) = ui.allocate_exact_size(Vec2::new(bar_w, TITLE_H), Sense::hover());
            let mut bar = ui.new_child(egui::UiBuilder::new().max_rect(bar_rect).layout(egui::Layout::left_to_right(egui::Align::Center)));
            bar.add_space(16.0);
            bar.label(RichText::new(&title).size(14.0).strong().color(theme.text));
            bar.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(8.0);
                let (r, resp) = ui.allocate_exact_size(Vec2::new(28.0, 28.0), Sense::click());
                resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("close {title}")));
                if resp.hovered() {
                    ui.painter().rect_filled(r, 6.0, theme.bg_hover);
                }
                ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, icons::X, egui::FontId::proportional(14.0), if resp.hovered() { theme.text } else { theme.text_muted });
                if resp.clicked() {
                    close = true;
                }
            });
            ui.painter().line_segment([egui::pos2(bar_rect.left(), bar_rect.bottom()), egui::pos2(bar_rect.right(), bar_rect.bottom())], Stroke::new(1.0, theme.border));
            // content
            egui::Frame::new().inner_margin(egui::Margin { left: 16, right: 16, top: 12, bottom: 14 }).show(ui, |ui| add(ui)).inner
        });
        if close {
            if let Some(o) = self.open {
                *o = false;
            }
        }
        inner.and_then(|ir| ir.inner)
    }
}
