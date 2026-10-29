//! The look of the app's own UI, apart from the map's style: colours (search, "what's here",
//! the scale bar), egui's style as the app sets it, and buttons with drawn symbols.

use eframe::egui::{self, Color32, CornerRadius, Pos2, Stroke, Vec2};

/// Main text, and the scale bar and credits over the map.
pub const INK: Color32 = Color32::from_gray(40);
/// Secondary text: kinds, places, distances.
pub const WEAK: Color32 = Color32::from_gray(105);
/// Marks a result or position that is only approximate.
pub const APPROX: Color32 = Color32::from_rgb(176, 96, 0);
/// A time in another zone than the trip's departure (phase 4 decision 14).
pub const ACCENT: Color32 = Color32::from_rgb(0, 112, 140);

/// Height of the search box and the buttons beside it.
pub const HEADER_H: f32 = 26.0;

/// The shadow of the boxes over the map: nearer them than egui's popup shadow, which sits
/// 6 by 10 points away.
pub const MAP_SHADOW: egui::Shadow = egui::Shadow {
    offset: [3, 5],
    blur: 6,
    spread: 0,
    color: Color32::from_black_alpha(32),
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
    /// A pencil, two sheets and a bin: rename, copy and delete a saved item.
    Rename,
    Copy,
    Delete,
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
        Glyph::Rename => {
            // A pencil from the bottom left (its point) to the top right.
            let s = Stroke::new(1.25, stroke.color);
            let d = Vec2::new(1.0, -1.0) / 2f32.sqrt();
            let n = Vec2::new(-d.y, d.x);
            let tip = c + Vec2::new(-1.1 * h, 1.1 * h);
            let (a, b) = (tip + d * 0.9 * h, tip + d * 3.1 * h);
            for k in [-1.0, 1.0] {
                let o = n * (0.42 * h) * k;
                p.line_segment([a + o, b + o], s);
                p.line_segment([tip, a + o], s);
            }
            p.line_segment([b - n * 0.42 * h, b + n * 0.42 * h], s);
        }
        Glyph::Copy => {
            let s = Stroke::new(1.25, stroke.color);
            let side = Vec2::splat(1.35 * h);
            let back = egui::Rect::from_min_size(c + Vec2::new(-0.55 * h, -1.1 * h), side);
            let front = egui::Rect::from_min_size(c + Vec2::new(-h, -0.45 * h), side);
            p.rect_stroke(back, 1.0, s, egui::StrokeKind::Middle);
            p.rect_filled(front, 1.0, ui.style().interact(&r).bg_fill);
            p.rect_stroke(front, 1.0, s, egui::StrokeKind::Middle);
        }
        Glyph::Delete => {
            // A bin: its lid and handle, and the can narrowing to its foot.
            let s = Stroke::new(1.25, stroke.color);
            let top = c.y - 0.75 * h;
            p.line_segment(
                [Pos2::new(c.x - 1.1 * h, top), Pos2::new(c.x + 1.1 * h, top)],
                s,
            );
            p.line_segment(
                [
                    Pos2::new(c.x - 0.35 * h, top - 0.4 * h),
                    Pos2::new(c.x + 0.35 * h, top - 0.4 * h),
                ],
                s,
            );
            let can = vec![
                Pos2::new(c.x - 0.8 * h, top),
                Pos2::new(c.x - 0.6 * h, c.y + 1.1 * h),
                Pos2::new(c.x + 0.6 * h, c.y + 1.1 * h),
                Pos2::new(c.x + 0.8 * h, top),
            ];
            p.add(egui::Shape::line(can, s));
            for dx in [-0.25 * h, 0.25 * h] {
                p.line_segment(
                    [
                        Pos2::new(c.x + dx, top + 0.45 * h),
                        Pos2::new(c.x + dx, c.y + 0.75 * h),
                    ],
                    s,
                );
            }
        }
    }
    r
}
