//! The side panel's own parts (phase 3 step 3.8): its pages and their tabs, the ☰ menu, the
//! trips and layers pages, and the disabled places kept for what phases 4 and 5 add. The
//! panel itself, with the search box and the search and route pages, is drawn by
//! [`crate::app`].

use eframe::egui::{self, Rangef, Rect, Stroke, Vec2};

use crate::layers::Layers;
use crate::scale::Units;
use crate::theme::{INK, WEAK};

/// The panel's width range, in points, and the most of a narrow window it may take.
const WIDTH: [f32; 2] = [300.0, 480.0];
const MAX_SHARE: f32 = 0.45;

/// The pages under the search box, one shown at a time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Page {
    #[default]
    Search,
    Route,
    Trips,
    Layers,
}

impl Page {
    pub const ALL: [Self; 4] = [Self::Search, Self::Route, Self::Trips, Self::Layers];

    pub fn name(self) -> &'static str {
        match self {
            Self::Search => "Search",
            Self::Route => "Route",
            Self::Trips => "Trips",
            Self::Layers => "Layers",
        }
    }
}

/// What the ☰ menu asks for, done once the panel is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Pick,
    Verify,
    About,
}

/// The widths the panel may take in a window `window` points wide.
pub fn width_range(window: f32) -> Rangef {
    let max = (window * MAX_SHARE).min(WIDTH[1]);
    Rangef::new(WIDTH[0].min(max), max)
}

/// A button for something a later phase adds: disabled, saying which phase and what it does.
pub fn later_in(ui: &mut egui::Ui, phase: u8, label: &str, what: &str) {
    ui.add_enabled(false, egui::Button::new(label).small())
        .on_disabled_hover_text(format!("Phase {phase}: {what}"));
}

/// [`later_in`] phase 4, trip planning.
pub fn later(ui: &mut egui::Ui, label: &str, what: &str) {
    later_in(ui, 4, label, what);
}

/// The row of tabs. Returns where each was drawn, for the tests.
pub fn tabs(ui: &mut egui::Ui, page: &mut Page) -> [Rect; 4] {
    let mut rects = [Rect::NOTHING; 4];
    ui.horizontal(|ui| {
        for (i, p) in Page::ALL.into_iter().enumerate() {
            rects[i] = ui.selectable_value(page, p, p.name()).rect;
        }
    });
    rects
}

/// The ☰ menu: the pack, About this map and the debug overlay; phase 5's settings and keys
/// are placeholders.
pub fn menu(ui: &mut egui::Ui, debug_overlay: &mut bool) -> Option<Action> {
    let mut action = None;
    let button = egui::Button::new("").min_size(Vec2::splat(24.0));
    let (r, _) = egui::containers::menu::MenuButton::from_button(button).ui(ui, |ui| {
        if ui.button("Open a pack…  (Ctrl+O)").clicked() {
            action = Some(Action::Pick);
        }
        if ui.button("Verify pack…").clicked() {
            action = Some(Action::Verify);
        }
        if ui.button("About this map…").clicked() {
            action = Some(Action::About);
        }
        ui.separator();
        ui.checkbox(debug_overlay, "Debug overlay  (F3)");
        ui.separator();
        later_in(
            ui,
            5,
            "Settings…",
            "units, map styles and the other settings",
        );
        later_in(ui, 5, "Keyboard shortcuts", "a list of the keys");
    });
    let r = r.on_hover_text("Menu");
    // Drawn, not typed: egui's fonts have no ☰.
    let stroke = Stroke::new(1.6, ui.style().interact(&r).fg_stroke.color);
    let c = r.rect.center();
    for k in -1..=1 {
        let y = c.y + k as f32 * 4.5;
        ui.painter().hline((c.x - 6.0)..=(c.x + 6.0), y, stroke);
    }
    action
}

/// The trips page: phase 4's saved trips and pushpins, and import and export.
pub fn trips_ui(ui: &mut egui::Ui) {
    ui.label(egui::RichText::new("Saved trips").strong().color(INK));
    ui.label(egui::RichText::new("No saved trips.").color(WEAK));
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        later(
            ui,
            "Save the route",
            "keep the route with its stops and options",
        );
        later(ui, "Add a pushpin", "a marked place with a note");
    });
    ui.add_space(8.0);
    ui.label(egui::RichText::new("Import and export").strong().color(INK));
    ui.horizontal_wrapped(|ui| {
        later(ui, "Import…", "trips and pushpins from GPX, CSV or KML");
        later(ui, "Export…", "trips and pushpins as GPX, CSV or KML");
    });
}

/// The layers page: the layer toggles and the scale bar's units. `pack_units` are the units
/// the pack suggests, used while the config sets none.
pub fn layers_ui(
    ui: &mut egui::Ui,
    layers: &mut Layers,
    units: &mut Option<Units>,
    pack_units: Units,
) {
    ui.label(egui::RichText::new("Map").strong().color(INK));
    for (i, (name, on)) in layers.entries().into_iter().enumerate() {
        if i == 5 {
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new("Points of interest")
                    .strong()
                    .color(INK),
            );
        }
        ui.checkbox(on, name);
    }
    ui.add_space(6.0);
    ui.label(egui::RichText::new("Scale bar units").strong().color(INK));
    let auto = format!("As the map ({})", pack_units.name());
    ui.radio_value(units, None, auto);
    for u in Units::ALL {
        ui.radio_value(units, Some(u), u.name());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widths() {
        let r = width_range(1600.0);
        assert_eq!((r.min, r.max), (300.0, 480.0));
        // A narrow window: at most 45% of it.
        let r = width_range(1000.0);
        assert_eq!((r.min, r.max), (300.0, 450.0));
        let r = width_range(600.0);
        assert_eq!((r.min, r.max), (270.0, 270.0));
    }
}
