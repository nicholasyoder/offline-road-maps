//! The Settings window (phase 5 step 5.3): display, map movement, units and start-up. A
//! value is saved with the rest of the config as soon as it changes ([`App::maybe_save`]).

use std::ops::RangeInclusive;

use eframe::egui;

use super::App;
use crate::config::{Config, Display, Movement, Start, UiTheme};
use crate::dialog::{self, Size};
use crate::render;
use crate::scale::Units;
use crate::theme::{self, ink};

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

    /// Draws the UI in the theme the setting and the system call for.
    pub(super) fn sync_theme(&mut self, ctx: &egui::Context) {
        let dark = theme::wants_dark(self.cfg.display.theme, self.sys_theme.get(ctx));
        if dark != theme::is_dark() {
            theme::apply(ctx, dark);
        }
    }

    /// The Settings window: a window of its own, apart from the map's (egui falls back to
    /// one inside the map's window where the platform cannot make another).
    pub(super) fn settings_window(&mut self, ctx: &egui::Context) {
        let Some(mut state) = self.settings.take() else {
            return;
        };
        let pack_units = self.map.as_ref().map(|m| m.units);
        let running = self.msaa_now;
        // Fits its content, up to what the monitor has room for, and scrolls past that.
        let room = ctx
            .input(|i| i.viewport().monitor_size)
            .map_or(700.0, |m| m.y - 120.0)
            .max(300.0);
        let close = dialog::show(ctx, "settings", "Settings", Size::Fit(476.0), |ui| {
            egui::ScrollArea::vertical()
                .max_height(room)
                .show(ui, |ui| {
                    contents(ui, ctx, &mut state, &mut self.cfg, pack_units, running);
                });
        });
        if !close {
            self.settings = Some(state);
        }
    }
}

/// A group of settings: a framed box across the window, with its title above and a line
/// under it.
fn group(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::group(ui.style())
        .inner_margin(10)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(egui::RichText::new(title).strong().size(15.0).color(ink()));
            ui.separator();
            add(ui);
        });
    ui.add_space(6.0);
}

/// What a setting does, in small type under it.
fn note(ui: &mut egui::Ui, text: &str) {
    ui.add(egui::Label::new(egui::RichText::new(text).weak()).wrap());
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
    group(ui, "Display", |ui| {
        egui::Grid::new("settings-display")
            .num_columns(2)
            .spacing([16.0, 6.0])
            .show(ui, |ui| {
                ui.label("UI size");
                let mut v = state.ui_size.unwrap_or(cfg.display.ui_size);
                let r = percent(ui, &mut v, Display::UI_SIZE);
                if r.dragged() {
                    state.ui_size = Some(v);
                } else if r.changed() || r.drag_stopped() {
                    cfg.display.ui_size = v;
                    state.ui_size = None;
                }
                ui.end_row();

                ui.label("Theme");
                ui.horizontal(|ui| {
                    ui.radio_value(&mut cfg.display.theme, UiTheme::System, "As the system");
                    ui.radio_value(&mut cfg.display.theme, UiTheme::Light, "Light");
                    ui.radio_value(&mut cfg.display.theme, UiTheme::Dark, "Dark");
                });
                ui.end_row();

                ui.label("Label size");
                percent(ui, &mut cfg.display.label_size, Display::LABEL_SIZE);
                ui.end_row();
            });
        if let Some(native) = ctx.native_pixels_per_point() {
            let drawn = native * cfg.display.ui_size as f32;
            note(
                ui,
                &format!(
                    "UI size scales the panel, buttons and text. The system reports a scale of {native:.2}; the app draws at {drawn:.2}."
                ),
            );
        }
        note(
            ui,
            "Theme sets the colours of the panel, windows and buttons. It is apart from the map's style: a dark panel goes with any map.",
        );
        note(
            ui,
            "Label size scales the names, road shields and icons on the map only. A printed map keeps its own sizes.",
        );
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label("Anti-aliasing");
            let mut n = render::samples(cfg.msaa);
            for s in [1, 2, 4] {
                ui.radio_value(
                    &mut n,
                    s,
                    if s == 1 {
                        "Off".into()
                    } else {
                        format!("{s}×")
                    },
                );
            }
            cfg.msaa = (n != render::MSAA).then_some(n);
        });
        note(
            ui,
            "Smooths the jagged, stair-stepped edges of roads, outlines and text on the map. Higher looks smoother but takes more of the graphics card's work: choose Off if the map is slow or looks wrong. Not every graphics card can do 2×.",
        );
        if render::samples(cfg.msaa) != running_msaa {
            ui.label(
                egui::RichText::new("Applies the next time the app starts.")
                    .color(crate::theme::approx()),
            );
        }
    });

    group(ui, "Map movement", |ui| {
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
                ui.checkbox(&mut mv.inertia, "Keep gliding after a drag");
                ui.end_row();
                ui.label("Glide time");
                ui.add_enabled_ui(mv.inertia, |ui| {
                    number(ui, &mut mv.glide, Movement::GLIDE, 0.1, " s");
                });
                ui.end_row();
            });
        note(
            ui,
            "A level is one doubling of the scale. Glide time is how long a glide takes to slow almost to a stop.",
        );
    });

    group(ui, "Units", |ui| {
        let auto = match pack_units {
            Some(u) => format!("As the map ({})", u.name()),
            None => "As the map".into(),
        };
        ui.horizontal_wrapped(|ui| {
            ui.radio_value(&mut cfg.units, None, auto);
            for u in Units::ALL {
                ui.radio_value(&mut cfg.units, Some(u), u.name());
            }
        });
        note(ui, "For the scale bar, the route page and the directions.");
    });

    group(ui, "Start-up", |ui| {
        ui.checkbox(&mut cfg.start.reopen_view, "Reopen the last view");
        ui.checkbox(&mut cfg.start.reopen_pack, "Reopen the last map");
    });

    if ui
        .button("Restore defaults")
        .on_hover_text("Put the settings in this window back as they were at first")
        .clicked()
    {
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
