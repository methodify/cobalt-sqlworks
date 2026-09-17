//! Cobalt Light / Cobalt Dark: one token set, two palettes, applied to egui `Visuals` and
//! exposed to the editor, grid and plan viewer so every surface agrees.

use cobalt_core::{Color, ThemeChoice};
use egui::{Color32, CornerRadius, Stroke, Visuals};

pub const COBALT: Color32 = Color32::from_rgb(0x1F, 0x6F, 0xEB);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Light,
    Dark,
}

/// Semantic colors beyond what egui's `Visuals` covers.
#[derive(Clone, Debug)]
pub struct Theme {
    pub mode: Mode,
    pub accent: Color32,
    pub bg: Color32,
    pub bg_panel: Color32,
    pub bg_sidebar: Color32,
    pub bg_editor: Color32,
    pub bg_grid: Color32,
    pub bg_grid_alt: Color32,
    pub bg_grid_header: Color32,
    pub bg_selection: Color32,
    pub bg_selection_inactive: Color32,
    pub bg_current_statement: Color32,
    pub bg_hover: Color32,
    pub border: Color32,
    pub border_strong: Color32,
    pub text: Color32,
    pub text_muted: Color32,
    pub text_faint: Color32,
    pub text_on_accent: Color32,
    pub null_text: Color32,
    pub error: Color32,
    pub warning: Color32,
    pub success: Color32,
    pub info: Color32,
    pub link: Color32,
    pub tokens: TokenColors,
    pub plan: PlanColors,
}

/// Editor syntax colors.
#[derive(Clone, Debug)]
pub struct TokenColors {
    pub keyword: Color32,
    pub function: Color32,
    pub type_: Color32,
    pub identifier: Color32,
    pub bracketed: Color32,
    pub string: Color32,
    pub number: Color32,
    pub comment: Color32,
    pub variable: Color32,
    pub temp_table: Color32,
    pub operator: Color32,
    pub punct: Color32,
}

/// Plan-viewer colors by operator category and cost.
#[derive(Clone, Debug)]
pub struct PlanColors {
    pub node_bg: Color32,
    pub node_border: Color32,
    pub node_selected: Color32,
    pub edge: Color32,
    pub cost_low: Color32,
    pub cost_mid: Color32,
    pub cost_high: Color32,
    pub cat_scan: Color32,
    pub cat_seek: Color32,
    pub cat_join: Color32,
    pub cat_aggregate: Color32,
    pub cat_sort: Color32,
    pub cat_dml: Color32,
    pub cat_parallel: Color32,
    pub cat_spool: Color32,
    pub cat_scalar: Color32,
    pub cat_other: Color32,
}

impl Theme {
    pub fn light() -> Self {
        Self {
            mode: Mode::Light,
            accent: COBALT,
            bg: Color32::from_rgb(0xF7, 0xF8, 0xFA),
            bg_panel: Color32::from_rgb(0xFF, 0xFF, 0xFF),
            bg_sidebar: Color32::from_rgb(0xF1, 0xF3, 0xF6),
            bg_editor: Color32::from_rgb(0xFF, 0xFF, 0xFF),
            bg_grid: Color32::from_rgb(0xFF, 0xFF, 0xFF),
            bg_grid_alt: Color32::from_rgb(0xF8, 0xF9, 0xFB),
            bg_grid_header: Color32::from_rgb(0xEE, 0xF1, 0xF5),
            bg_selection: Color32::from_rgb(0xCF, 0xE2, 0xFC),
            bg_selection_inactive: Color32::from_rgb(0xE3, 0xE8, 0xEF),
            bg_current_statement: Color32::from_rgb(0xF2, 0xF6, 0xFD),
            bg_hover: Color32::from_rgb(0xEA, 0xF0, 0xFA),
            border: Color32::from_rgb(0xDD, 0xE2, 0xE9),
            border_strong: Color32::from_rgb(0xBF, 0xC7, 0xD2),
            text: Color32::from_rgb(0x1B, 0x1F, 0x24),
            text_muted: Color32::from_rgb(0x5C, 0x66, 0x74),
            text_faint: Color32::from_rgb(0x9A, 0xA3, 0xAF),
            text_on_accent: Color32::WHITE,
            null_text: Color32::from_rgb(0x9A, 0x8F, 0xC0),
            error: Color32::from_rgb(0xC9, 0x2A, 0x2A),
            warning: Color32::from_rgb(0xB0, 0x7A, 0x0A),
            success: Color32::from_rgb(0x1F, 0x8A, 0x3C),
            info: COBALT,
            link: COBALT,
            tokens: TokenColors {
                keyword: Color32::from_rgb(0x0B, 0x4F, 0xC2),
                function: Color32::from_rgb(0x7A, 0x1F, 0xA2),
                type_: Color32::from_rgb(0x0F, 0x76, 0x8C),
                identifier: Color32::from_rgb(0x1B, 0x1F, 0x24),
                bracketed: Color32::from_rgb(0x2B, 0x4A, 0x6B),
                string: Color32::from_rgb(0xB0, 0x2E, 0x2E),
                number: Color32::from_rgb(0x0D, 0x7A, 0x3E),
                comment: Color32::from_rgb(0x6B, 0x80, 0x5C),
                variable: Color32::from_rgb(0x8A, 0x5A, 0x00),
                temp_table: Color32::from_rgb(0x2B, 0x4A, 0x6B),
                operator: Color32::from_rgb(0x4B, 0x55, 0x63),
                punct: Color32::from_rgb(0x4B, 0x55, 0x63),
            },
            plan: PlanColors {
                node_bg: Color32::from_rgb(0xFF, 0xFF, 0xFF),
                node_border: Color32::from_rgb(0xBF, 0xC7, 0xD2),
                node_selected: COBALT,
                edge: Color32::from_rgb(0x8A, 0x94, 0xA3),
                cost_low: Color32::from_rgb(0xE9, 0xEE, 0xF5),
                cost_mid: Color32::from_rgb(0xFF, 0xD7, 0x8A),
                cost_high: Color32::from_rgb(0xF2, 0x7A, 0x6B),
                cat_scan: Color32::from_rgb(0xD1, 0x5B, 0x3E),
                cat_seek: Color32::from_rgb(0x2E, 0x8B, 0x57),
                cat_join: Color32::from_rgb(0x1F, 0x6F, 0xEB),
                cat_aggregate: Color32::from_rgb(0x8A, 0x4F, 0xD3),
                cat_sort: Color32::from_rgb(0xD2, 0x9A, 0x22),
                cat_dml: Color32::from_rgb(0xC9, 0x2A, 0x2A),
                cat_parallel: Color32::from_rgb(0x16, 0x9C, 0xA8),
                cat_spool: Color32::from_rgb(0x6B, 0x72, 0x80),
                cat_scalar: Color32::from_rgb(0x5C, 0x66, 0x74),
                cat_other: Color32::from_rgb(0x6B, 0x72, 0x80),
            },
        }
    }

    pub fn dark() -> Self {
        Self {
            mode: Mode::Dark,
            accent: Color32::from_rgb(0x4C, 0x8F, 0xF5),
            bg: Color32::from_rgb(0x15, 0x18, 0x1E),
            bg_panel: Color32::from_rgb(0x1B, 0x1F, 0x26),
            bg_sidebar: Color32::from_rgb(0x18, 0x1C, 0x22),
            bg_editor: Color32::from_rgb(0x12, 0x15, 0x1A),
            bg_grid: Color32::from_rgb(0x15, 0x18, 0x1E),
            bg_grid_alt: Color32::from_rgb(0x19, 0x1D, 0x24),
            bg_grid_header: Color32::from_rgb(0x22, 0x27, 0x30),
            bg_selection: Color32::from_rgb(0x1E, 0x3A, 0x66),
            bg_selection_inactive: Color32::from_rgb(0x2A, 0x31, 0x3C),
            bg_current_statement: Color32::from_rgb(0x17, 0x1C, 0x25),
            bg_hover: Color32::from_rgb(0x22, 0x29, 0x34),
            border: Color32::from_rgb(0x2B, 0x31, 0x3B),
            border_strong: Color32::from_rgb(0x3D, 0x45, 0x52),
            text: Color32::from_rgb(0xE6, 0xE9, 0xEE),
            text_muted: Color32::from_rgb(0x9A, 0xA3, 0xB2),
            text_faint: Color32::from_rgb(0x66, 0x6F, 0x7D),
            text_on_accent: Color32::WHITE,
            null_text: Color32::from_rgb(0x9C, 0x8E, 0xD6),
            error: Color32::from_rgb(0xF0, 0x6A, 0x6A),
            warning: Color32::from_rgb(0xE8, 0xB3, 0x4B),
            success: Color32::from_rgb(0x4C, 0xC3, 0x7A),
            info: Color32::from_rgb(0x4C, 0x8F, 0xF5),
            link: Color32::from_rgb(0x6F, 0xA8, 0xFF),
            tokens: TokenColors {
                keyword: Color32::from_rgb(0x6F, 0xA8, 0xFF),
                function: Color32::from_rgb(0xC9, 0x8B, 0xF0),
                type_: Color32::from_rgb(0x4E, 0xC9, 0xB0),
                identifier: Color32::from_rgb(0xE6, 0xE9, 0xEE),
                bracketed: Color32::from_rgb(0xB5, 0xCE, 0xA8),
                string: Color32::from_rgb(0xE8, 0x9A, 0x7A),
                number: Color32::from_rgb(0xB5, 0xE8, 0x9A),
                comment: Color32::from_rgb(0x6A, 0x99, 0x55),
                variable: Color32::from_rgb(0xE8, 0xC9, 0x7A),
                temp_table: Color32::from_rgb(0xB5, 0xCE, 0xA8),
                operator: Color32::from_rgb(0xC0, 0xC6, 0xD0),
                punct: Color32::from_rgb(0xA0, 0xA8, 0xB4),
            },
            plan: PlanColors {
                node_bg: Color32::from_rgb(0x1F, 0x24, 0x2D),
                node_border: Color32::from_rgb(0x3D, 0x45, 0x52),
                node_selected: Color32::from_rgb(0x6F, 0xA8, 0xFF),
                edge: Color32::from_rgb(0x7A, 0x84, 0x93),
                cost_low: Color32::from_rgb(0x22, 0x29, 0x34),
                cost_mid: Color32::from_rgb(0x6B, 0x55, 0x1C),
                cost_high: Color32::from_rgb(0x8A, 0x2F, 0x2B),
                cat_scan: Color32::from_rgb(0xE8, 0x7A, 0x5C),
                cat_seek: Color32::from_rgb(0x4C, 0xC3, 0x7A),
                cat_join: Color32::from_rgb(0x6F, 0xA8, 0xFF),
                cat_aggregate: Color32::from_rgb(0xC9, 0x8B, 0xF0),
                cat_sort: Color32::from_rgb(0xE8, 0xB3, 0x4B),
                cat_dml: Color32::from_rgb(0xF0, 0x6A, 0x6A),
                cat_parallel: Color32::from_rgb(0x4E, 0xC9, 0xB0),
                cat_spool: Color32::from_rgb(0x9A, 0xA3, 0xB2),
                cat_scalar: Color32::from_rgb(0xA0, 0xA8, 0xB4),
                cat_other: Color32::from_rgb(0x9A, 0xA3, 0xB2),
            },
        }
    }

    pub fn for_choice(choice: ThemeChoice, system_dark: bool) -> Self {
        match choice {
            ThemeChoice::Light => Self::light(),
            ThemeChoice::Dark => Self::dark(),
            ThemeChoice::System => {
                if system_dark {
                    Self::dark()
                } else {
                    Self::light()
                }
            }
        }
    }

    pub fn is_dark(&self) -> bool {
        self.mode == Mode::Dark
    }

    /// Convert a core `Color` (group colors) to Color32.
    pub fn color32(c: Color) -> Color32 {
        Color32::from_rgb(c.0, c.1, c.2)
    }

    /// Blend the group color into a subtle background tint.
    pub fn tint(&self, c: Color32, strength: f32) -> Color32 {
        let base = self.bg_panel;
        let f = |a: u8, b: u8| ((a as f32) * (1.0 - strength) + (b as f32) * strength) as u8;
        Color32::from_rgb(f(base.r(), c.r()), f(base.g(), c.g()), f(base.b(), c.b()))
    }

    /// Plan cost gradient 0..1 → color.
    pub fn cost_color(&self, t: f32) -> Color32 {
        let t = t.clamp(0.0, 1.0);
        let lerp = |a: Color32, b: Color32, t: f32| {
            let f = |x: u8, y: u8| ((x as f32) + ((y as f32) - (x as f32)) * t) as u8;
            Color32::from_rgb(f(a.r(), b.r()), f(a.g(), b.g()), f(a.b(), b.b()))
        };
        if t < 0.5 {
            lerp(self.plan.cost_low, self.plan.cost_mid, t * 2.0)
        } else {
            lerp(self.plan.cost_mid, self.plan.cost_high, (t - 0.5) * 2.0)
        }
    }

    /// Build egui `Visuals` from the tokens.
    pub fn visuals(&self) -> Visuals {
        let mut v = if self.is_dark() { Visuals::dark() } else { Visuals::light() };
        let radius = CornerRadius::same(4);
        v.override_text_color = Some(self.text);
        v.panel_fill = self.bg_panel;
        v.window_fill = self.bg_panel;
        v.extreme_bg_color = self.bg_editor;
        v.faint_bg_color = self.bg_grid_alt;
        v.code_bg_color = self.bg_editor;
        v.hyperlink_color = self.link;
        v.error_fg_color = self.error;
        v.warn_fg_color = self.warning;
        v.selection.bg_fill = self.bg_selection;
        v.selection.stroke = Stroke::new(1.0, self.accent);
        v.window_stroke = Stroke::new(1.0, self.border);
        v.window_corner_radius = CornerRadius::same(6);
        v.menu_corner_radius = CornerRadius::same(6);
        v.widgets.noninteractive.bg_fill = self.bg_panel;
        v.widgets.noninteractive.weak_bg_fill = self.bg_panel;
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, self.border);
        v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, self.text_muted);
        v.widgets.noninteractive.corner_radius = radius;
        v.widgets.inactive.bg_fill = self.bg_sidebar;
        v.widgets.inactive.weak_bg_fill = self.bg_sidebar;
        v.widgets.inactive.bg_stroke = Stroke::new(1.0, self.border);
        v.widgets.inactive.fg_stroke = Stroke::new(1.0, self.text);
        v.widgets.inactive.corner_radius = radius;
        v.widgets.hovered.bg_fill = self.bg_hover;
        v.widgets.hovered.weak_bg_fill = self.bg_hover;
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, self.border_strong);
        v.widgets.hovered.fg_stroke = Stroke::new(1.5, self.text);
        v.widgets.hovered.corner_radius = radius;
        v.widgets.active.bg_fill = self.bg_selection;
        v.widgets.active.weak_bg_fill = self.bg_selection;
        v.widgets.active.bg_stroke = Stroke::new(1.0, self.accent);
        v.widgets.active.fg_stroke = Stroke::new(2.0, self.text);
        v.widgets.active.corner_radius = radius;
        v.widgets.open.bg_fill = self.bg_hover;
        v.widgets.open.weak_bg_fill = self.bg_hover;
        v.widgets.open.bg_stroke = Stroke::new(1.0, self.border_strong);
        v.widgets.open.fg_stroke = Stroke::new(1.0, self.text);
        v.widgets.open.corner_radius = radius;
        v.striped = false;
        v.slider_trailing_fill = true;
        v
    }
}
