use egui::{Color32, RichText};

/// M0 placeholder application state. Replaced by the real shell in M2.
pub struct CobaltApp {
    dark: bool,
    frames: u64,
}

impl CobaltApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut fonts = egui::FontDefinitions::default();
        egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
        cc.egui_ctx.set_fonts(fonts);
        Self { dark: true, frames: 0 }
    }
}

impl eframe::App for CobaltApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.frames += 1;
        ui.heading(RichText::new(format!("{}  Cobalt SQL Works", egui_phosphor::regular::DATABASE)).color(Color32::from_rgb(0x1F, 0x6F, 0xEB)));
        ui.label("M0 skeleton — eframe 0.36 + egui_agent 0.3.0 + arrow 58 + tiberius-ng + deltalake all linked.");
        ui.separator();
        #[cfg(feature = "agent")]
        {
            egui_agent::widgets::checkbox(ui, "theme.dark", "Dark theme", &mut self.dark);
            egui_agent::widgets::expose(ui.ctx(), "frames", self.frames);
        }
        #[cfg(not(feature = "agent"))]
        {
            ui.checkbox(&mut self.dark, "Dark theme");
        }
        ui.ctx().set_visuals(if self.dark { egui::Visuals::dark() } else { egui::Visuals::light() });
    }
}

#[cfg(feature = "agent")]
impl egui_agent::AgentApp for CobaltApp {}
