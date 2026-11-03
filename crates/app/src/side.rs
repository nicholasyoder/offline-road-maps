//! The side panel's own parts (phase 3 step 3.8): its pages and their tabs, the ☰ menu, the
//! Map page (the styles and the layers), and the disabled places kept for what phase 5 adds. The panel itself, with
//! the search box and the search and route pages, is drawn by [`crate::app`]'s `panel`;
//! the Trips page by [`crate::trips`].

use eframe::egui::{self, Rangef, Rect, Vec2};

use crate::layers::Layers;
use crate::styles::Entry;
use crate::theme::{HEADER_H, accent, ink, weak};

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
    Map,
    /// The print page (phase 4 step 4.9): opened by a button or Ctrl+P, not a tab.
    Print,
}

impl Page {
    /// The tabs, Print last.
    pub const TABS: [Self; 5] = [
        Self::Search,
        Self::Route,
        Self::Trips,
        Self::Map,
        Self::Print,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Search => "Search",
            Self::Route => "Route",
            Self::Trips => "Trips",
            Self::Map => "Map",
            Self::Print => "Print",
        }
    }
}

/// What the ☰ menu asks for, done once the panel is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Pick,
    Verify,
    About,
    Print,
    Settings,
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

/// The row of tabs. Returns where each was drawn, for the tests.
pub fn tabs(ui: &mut egui::Ui, page: &mut Page) -> [Rect; 5] {
    let mut rects = [Rect::NOTHING; 5];
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        for (i, p) in Page::TABS.into_iter().enumerate() {
            rects[i] = ui.selectable_value(page, p, p.name()).rect;
        }
    });
    rects
}

/// The ☰ menu: the pack, About this map, the debug overlay and Settings; phase 5's keys are
/// a placeholder.
pub fn menu(ui: &mut egui::Ui, debug_overlay: &mut bool) -> Option<Action> {
    let mut action = None;
    let button = egui::Button::new("").min_size(Vec2::splat(HEADER_H));
    let (r, _) = egui::containers::menu::MenuButton::from_button(button).ui(ui, |ui| {
        if ui.button("Open a pack…  (Ctrl+O)").clicked() {
            action = Some(Action::Pick);
        }
        if ui.button("Verify pack…").clicked() {
            action = Some(Action::Verify);
        }
        if ui.button("Print map…  (Ctrl+P)").clicked() {
            action = Some(Action::Print);
        }
        if ui.button("About this map…").clicked() {
            action = Some(Action::About);
        }
        ui.separator();
        ui.checkbox(debug_overlay, "Debug overlay  (F3)");
        ui.separator();
        if ui.button("Settings…").clicked() {
            action = Some(Action::Settings);
        }
        later_in(ui, 5, "Keyboard shortcuts", "a list of the keys");
    });
    let r = r.on_hover_text("Menu");
    // Drawn, not typed: egui's fonts have no ☰. Bars 2 points thick and 5 apart, on whole
    // pixels, so the gaps come out equal.
    let color = ui.style().interact(&r).fg_stroke.color;
    let ppp = ui.ctx().pixels_per_point();
    let snap = |v: f32| (v * ppp).round() / ppp;
    let c = r.rect.center();
    let (top, thick) = (snap(c.y - 1.0), snap(2.0).max(1.0 / ppp));
    for k in -1..=1 {
        let y = top + snap(5.0) * k as f32;
        let bar = Rect::from_min_max(
            egui::pos2(snap(c.x - 6.0), y),
            egui::pos2(snap(c.x + 6.0), y + thick),
        );
        ui.painter().rect_filled(bar, 0.0, color);
    }
    action
}

/// A style's card: its swatch (land, water, a main road) and name.
const CARD: Vec2 = Vec2::new(86.0, 62.0);

/// The Map tab: the styles as cards, then the layer toggles. Returns the id of the style
/// clicked.
pub fn map_ui(
    ui: &mut egui::Ui,
    layers: &mut Layers,
    styles: &[Entry],
    current: &str,
) -> Option<String> {
    let mut picked = None;
    ui.label(egui::RichText::new("Style").strong().color(ink()));
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::splat(6.0);
        for e in styles {
            if style_card(ui, e, e.id == current) {
                picked = Some(e.id.clone());
            }
        }
    });
    ui.add_space(8.0);
    let (map, pois) = layers.groups();
    ui.label(egui::RichText::new("Layers").strong().color(ink()));
    for (name, on) in map {
        ui.checkbox(on, name);
    }
    ui.add_space(6.0);
    ui.label(
        egui::RichText::new("Points of interest")
            .strong()
            .color(ink()),
    );
    for (name, on) in pois {
        ui.checkbox(on, name);
    }
    picked
}

/// One card; whether it was clicked. A style that cannot be used is greyed out, with why as
/// its tooltip.
fn style_card(ui: &mut egui::Ui, e: &Entry, on: bool) -> bool {
    let usable = e.file.is_ok();
    let sense = match usable {
        true => egui::Sense::click(),
        false => egui::Sense::hover(),
    };
    let (rect, r) = ui.allocate_exact_size(CARD, sense);
    let p = ui.painter();
    let swatch = Rect::from_min_size(rect.min, Vec2::new(CARD.x, 40.0)).shrink(1.0);
    let colour = |c: [u8; 4]| crate::style::color32(c);
    let fade = |c: egui::Color32| if usable { c } else { c.gamma_multiply(0.35) };
    if let Some([land, water, road]) = e.swatch() {
        p.rect_filled(swatch, 3.0, fade(colour(land)));
        // Water in the lower left corner, a road across.
        let w = Rect::from_min_max(
            egui::pos2(swatch.left() + 6.0, swatch.bottom() - 15.0),
            egui::pos2(swatch.left() + 34.0, swatch.bottom() - 5.0),
        );
        p.rect_filled(w, 3.0, fade(colour(water)));
        p.line_segment(
            [
                egui::pos2(swatch.left() + 2.0, swatch.top() + 10.0),
                egui::pos2(swatch.right() - 2.0, swatch.top() + 22.0),
            ],
            egui::Stroke::new(4.0, fade(colour(road))),
        );
    } else {
        p.rect_filled(swatch, 3.0, ui.visuals().faint_bg_color);
        p.text(
            swatch.center(),
            egui::Align2::CENTER_CENTER,
            "!",
            egui::FontId::proportional(18.0),
            weak(),
        );
    }
    let (width, colour) = match (on, r.hovered() && usable) {
        (true, _) => (2.0, accent()),
        (false, true) => (1.5, weak()),
        (false, false) => (1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
    };
    p.rect_stroke(
        swatch.expand(1.0),
        3.0,
        egui::Stroke::new(width, colour),
        egui::StrokeKind::Inside,
    );
    let text = if usable { ink() } else { weak() };
    let name = p.layout(
        e.name.clone(),
        egui::FontId::proportional(12.0),
        text,
        CARD.x,
    );
    let at = egui::pos2(
        rect.center().x - name.size().x.min(CARD.x) / 2.0,
        rect.top() + 44.0,
    );
    p.galley(at, name, text);
    match &e.file {
        Ok(_) => r.clone().on_hover_text(&e.name),
        Err(why) => r.clone().on_hover_text(format!("{}: {why}", e.name)),
    };
    usable && r.clicked()
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
