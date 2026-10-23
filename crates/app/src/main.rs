//! Placeholder window. Exists so CI proves the wgpu + egui stack builds on
//! Linux and Windows, and to track the stripped binary size.

use eframe::egui;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: egui::ViewportBuilder::default().with_inner_size([800.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Offline Road Maps",
        options,
        Box::new(|_cc| Ok(Box::new(App))),
    )
}

struct App;

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ui, |ui| {
            let rect = ui.max_rect().shrink(40.0);
            ui.painter()
                .rect_filled(rect, 8.0, egui::Color32::from_rgb(40, 110, 180));
        });
    }
}
