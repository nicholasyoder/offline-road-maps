use super::*;
use crate::config::Location;
use eframe::egui::Vec2;

fn changed() -> Config {
    Config {
        display: Display {
            ui_size: 0.5,
            label_size: 1.5,
        },
        movement: Movement {
            wheel_zoom: 0.5,
            key_zoom: 0.5,
            key_pan: 1200.0,
            inertia: false,
            glide: 2.0,
        },
        start: Start {
            reopen_view: false,
            reopen_pack: false,
        },
        msaa: Some(1),
        units: Some(Units::Both),
        ..Config::default()
    }
}

/// Draws the window's contents once, as a frame of the app would.
fn draw(cfg: &mut Config, state: &mut Window) {
    let ctx = egui::Context::default();
    let raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            Vec2::new(900.0, 700.0),
        )),
        ..Default::default()
    };
    let mut full = ctx.run_ui(raw, |ui| {
        contents(ui, &ctx.clone(), state, cfg, Some(Units::Miles), 4);
    });
    full.textures_delta.clear();
}

#[test]
fn every_value_survives_a_save() {
    let dir = tempfile::tempdir().unwrap();
    let loc = Location::find(dir.path());
    let mut cfg = changed();
    // Drawing the window does not change a value by itself.
    let before = cfg.clone();
    draw(&mut cfg, &mut Window::default());
    assert_eq!(cfg, before);
    loc.save(&cfg).unwrap();
    assert_eq!(loc.load(), (cfg, None));
}

#[test]
fn restore_defaults_resets_the_windows_values_only() {
    let mut cfg = changed();
    cfg.debug_overlay = true;
    restore_defaults(&mut cfg);
    assert_eq!(
        cfg,
        Config {
            debug_overlay: true,
            ..Config::default()
        }
    );
}
