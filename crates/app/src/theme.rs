//! The look of the app's own UI, apart from the map's style: colours (search, "what's here",
//! the scale bar), egui's style as the app sets it, and buttons with drawn symbols.

use eframe::egui::{self, Color32, CornerRadius, Stroke, Vec2};

/// Main text, and the scale bar and credits over the map.
pub const INK: Color32 = Color32::from_gray(40);
/// Secondary text: kinds, places, distances.
pub const WEAK: Color32 = Color32::from_gray(105);
/// Marks a result or position that is only approximate.
pub const APPROX: Color32 = Color32::from_rgb(176, 96, 0);

/// Height of the search box and the buttons beside it.
pub const HEADER_H: f32 = 26.0;

/// The shadow of the boxes over the map: close under them, not egui's popup shadow, which
/// sits 6 by 10 points away.
pub const MAP_SHADOW: egui::Shadow = egui::Shadow {
    offset: [0, 1],
    blur: 4,
    spread: 0,
    color: Color32::from_black_alpha(40),
};

/// Corner radius of the widgets, and of windows, menus and the boxes over the map.
const WIDGET_RADIUS: u8 = 3;
const WINDOW_RADIUS: u8 = 4;
/// The outline of a button or text box at rest, a little darker than a button's fill, so
/// both stand out from the panel.
const OUTLINE: Color32 = Color32::from_gray(185);

/// The app's look, over egui's light theme: smaller corner radii, outlined buttons and text
/// boxes, and smaller window titles.
pub fn apply(ctx: &egui::Context) {
    // The map is light, so its controls and dialogs are too, whatever the system theme.
    ctx.set_theme(egui::Theme::Light);
    ctx.all_styles_mut(|s| {
        // egui's 18 points look heavy on small dialogs.
        s.text_styles
            .insert(egui::TextStyle::Heading, egui::FontId::proportional(15.0));
        let v = &mut s.visuals;
        let w = &mut v.widgets;
        for wv in [
            &mut w.noninteractive,
            &mut w.inactive,
            &mut w.hovered,
            &mut w.active,
            &mut w.open,
        ] {
            wv.corner_radius = CornerRadius::same(WIDGET_RADIUS);
        }
        w.inactive.bg_stroke = Stroke::new(1.0, OUTLINE);
        v.window_corner_radius = CornerRadius::same(WINDOW_RADIUS);
        v.menu_corner_radius = CornerRadius::same(WINDOW_RADIUS);
    });
}

/// A symbol drawn on a button, centred, rather than typed: the font's ×, « and » are small
/// and sit off centre.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Glyph {
    Close,
    /// « and »: hide and show the side panel.
    Hide,
    Show,
}

/// A square button `side` points wide with `glyph` on it.
pub fn glyph_button(ui: &mut egui::Ui, glyph: Glyph, side: f32) -> egui::Response {
    let r = ui.add(egui::Button::new("").min_size(Vec2::splat(side)));
    let stroke = Stroke::new(1.5, ui.style().interact(&r).fg_stroke.color);
    let c = r.rect.center();
    let p = ui.painter();
    // The symbol's half size: about a fifth of the button, at least 3 points.
    let h = (side * 0.2).max(3.0);
    match glyph {
        Glyph::Close => {
            p.line_segment([c + Vec2::new(-h, -h), c + Vec2::new(h, h)], stroke);
            p.line_segment([c + Vec2::new(-h, h), c + Vec2::new(h, -h)], stroke);
        }
        Glyph::Hide | Glyph::Show => {
            // Two chevrons, pointing left to hide and right to show.
            let s = if glyph == Glyph::Hide { 1.0 } else { -1.0 };
            for dx in [-0.6 * h, 0.6 * h] {
                let tip = c + Vec2::new(dx - s * 0.5 * h, 0.0);
                let back = Vec2::new(s * h, h);
                p.line_segment([tip, tip + back], stroke);
                p.line_segment([tip, tip + Vec2::new(back.x, -back.y)], stroke);
            }
        }
    }
    r
}
