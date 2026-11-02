//! The Settings window (phase 5 step 5.3): display, map movement, units and start-up. A
//! value is saved with the rest of the config as soon as it changes ([`App::maybe_save`]).

use std::ops::RangeInclusive;

use eframe::egui::{self, Align2, Vec2};

use super::App;
use crate::config::{Config, Display, Movement, Start};
use crate::render;
use crate::scale::Units;
use crate::theme::INK;

/// What the window keeps besides the config.
#[derive(Default)]
pub struct Window {
    /// The UI size being dragged: the window changes size with it, so it is applied when the
    /// slider is let go.
    ui_size: Option<f64>,
}

impl App {
    /// Keeps egui's zoom factor and the UI size setting as one: a change of either, the
    /// window's or egui's own Ctrl+plus and Ctrl+minus, is followed by the other.
    pub(super) fn sync_zoom(&mut self, ctx: &egui::Context) {
        let now = ctx.zoom_factor();
        if now != self.zoom_seen {
            self.cfg.display.ui_size =
                f64::from(now).clamp(*Display::UI_SIZE.start(), *Display::UI_SIZE.end());
        }
        let want = self.cfg.display.ui_size as f32;
        if want != now {
            ctx.set_zoom_factor(want);
        }
        self.zoom_seen = want;
    }

    pub(super) fn settings_window(&mut self, ctx: &egui::Context) {
        let Some(mut state) = self.settings.take() else {
            return;
        };
        let pack_units = self.map.as_ref().map(|m| m.units);
        let running = self.msaa_now;
        let mut open = true;
        egui::Window::new("Settings")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                contents(ui, ctx, &mut state, &mut self.cfg, pack_units, running);
            });
        if open {
            self.settings = Some(state);
        }
    }
}

fn heading(ui: &mut egui::Ui, text: &str) {
    ui.add_space(4.0);
    ui.label(egui::RichText::new(text).strong().color(INK));
}

/// A slider showing a factor as a percentage, with the range's ends as its limits.
fn percent(ui: &mut egui::Ui, v: &mut f64, range: RangeInclusive<f64>) -> egui::Response {
    let slider = egui::Slider::new(v, range)
        .step_by(0.05)
        .custom_formatter(|v, _| format!("{:.0}%", v * 100.0))
        .custom_parser(|s| {
            s.trim()
                .trim_end_matches('%')
                .parse::<f64>()
                .ok()
                .map(|p| p / 100.0)
        });
    ui.add(slider)
}

fn number(
    ui: &mut egui::Ui,
    v: &mut f64,
    range: RangeInclusive<f64>,
    step: f64,
    suffix: &str,
) -> egui::Response {
    ui.add(
        egui::Slider::new(v, range)
            .step_by(step)
            .suffix(suffix)
            .max_decimals(2),
    )
}

fn contents(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    state: &mut Window,
    cfg: &mut Config,
    pack_units: Option<Units>,
    running_msaa: u32,
) {
    heading(ui, "Display");
    egui::Grid::new("settings-display")
        .num_columns(2)
        .spacing([16.0, 6.0])
        .show(ui, |ui| {
            ui.label("UI size");
            ui.horizontal(|ui| {
                let mut v = state.ui_size.unwrap_or(cfg.display.ui_size);
                let r = percent(ui, &mut v, Display::UI_SIZE);
                if r.dragged() {
                    state.ui_size = Some(v);
                } else if r.changed() || r.drag_stopped() {
                    cfg.display.ui_size = v;
                    state.ui_size = None;
                }
                if let Some(native) = ctx.native_pixels_per_point() {
                    let drawn = native * cfg.display.ui_size as f32;
                    ui.weak(format!(
                        "the system reports {native:.2} px/pt; drawn at {drawn:.2}"
                    ));
                }
            });
            ui.end_row();

            ui.label("Label size");
            let r = percent(ui, &mut cfg.display.label_size, Display::LABEL_SIZE);
            r.on_hover_text("Names, shields and icons on the map; the printed map keeps its own");
            ui.end_row();

            ui.label("Anti-aliasing");
            ui.horizontal(|ui| {
                let mut n = render::samples(cfg.msaa);
                for s in [1, 2, 4] {
                    ui.radio_value(&mut n, s, format!("{s}×"));
                }
                cfg.msaa = (n != render::MSAA).then_some(n);
                if render::samples(cfg.msaa) != running_msaa {
                    ui.weak("applies after a restart");
                } else if n == 2 {
                    ui.weak("not every GPU can do 2×");
                }
            });
            ui.end_row();
        });

    heading(ui, "Map movement");
    let mv = &mut cfg.movement;
    egui::Grid::new("settings-movement")
        .num_columns(2)
        .spacing([16.0, 6.0])
        .show(ui, |ui| {
            ui.label("Wheel zoom per notch");
            number(
                ui,
                &mut mv.wheel_zoom,
                Movement::WHEEL_ZOOM,
                0.05,
                " levels",
            );
            ui.end_row();
            ui.label("Zoom per key press");
            number(ui, &mut mv.key_zoom, Movement::KEY_ZOOM, 0.25, " levels");
            ui.end_row();
            ui.label("Arrow keys pan");
            number(ui, &mut mv.key_pan, Movement::KEY_PAN, 50.0, " pt/s");
            ui.end_row();
            ui.label("Pan inertia");
            ui.horizontal(|ui| {
                ui.checkbox(&mut mv.inertia, "Glide after a drag");
            });
            ui.end_row();
            ui.label("Glide time");
            ui.add_enabled_ui(mv.inertia, |ui| {
                number(ui, &mut mv.glide, Movement::GLIDE, 0.1, " s")
                    .on_hover_text("How long a glide takes to slow to almost nothing");
            });
            ui.end_row();
        });

    heading(ui, "Units");
    ui.horizontal(|ui| {
        let auto = match pack_units {
            Some(u) => format!("As the map ({})", u.name()),
            None => "As the map".into(),
        };
        ui.radio_value(&mut cfg.units, None, auto);
        for u in Units::ALL {
            ui.radio_value(&mut cfg.units, Some(u), u.name());
        }
    });
    ui.weak("For the scale bar, the route page and the directions.");

    heading(ui, "Start-up");
    ui.checkbox(&mut cfg.start.reopen_view, "Reopen the last view");
    ui.checkbox(&mut cfg.start.reopen_pack, "Reopen the last pack");

    ui.add_space(8.0);
    if ui.button("Restore defaults").clicked() {
        restore_defaults(cfg);
        state.ui_size = None;
    }
}

/// The window's values back to their defaults.
pub fn restore_defaults(cfg: &mut Config) {
    cfg.display = Display::default();
    cfg.movement = Movement::default();
    cfg.start = Start::default();
    cfg.msaa = None;
    cfg.units = None;
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
