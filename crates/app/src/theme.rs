//! The look of the app's own UI, apart from the map's style: colours (search, "what's here",
//! the scale bar), egui's style as the app sets it, and buttons with drawn symbols.

use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui::{self, Color32, CornerRadius, Pos2, Sense, Stroke, Vec2};

use crate::config::UiTheme;

/// Whether the UI is drawn dark: set by [`apply`], read by the colours below, so every
/// window of the app agrees with the one egui theme.
static DARK: AtomicBool = AtomicBool::new(false);

fn pick(light: Color32, dark: Color32) -> Color32 {
    if DARK.load(Ordering::Relaxed) {
        dark
    } else {
        light
    }
}

/// Main text, and the scale bar and credits over the map.
pub fn ink() -> Color32 {
    pick(Color32::from_gray(40), Color32::from_gray(225))
}
/// Secondary text: kinds, places, distances.
pub fn weak() -> Color32 {
    pick(Color32::from_gray(105), Color32::from_gray(160))
}
/// Marks a result or position that is only approximate.
pub fn approx() -> Color32 {
    pick(
        Color32::from_rgb(176, 96, 0),
        Color32::from_rgb(235, 160, 60),
    )
}
/// A time in another zone than the trip's departure (phase 4 decision 14).
pub fn accent() -> Color32 {
    pick(
        Color32::from_rgb(0, 112, 140),
        Color32::from_rgb(90, 195, 220),
    )
}
/// An error: a message that something failed.
pub fn error() -> Color32 {
    pick(
        Color32::from_rgb(180, 30, 30),
        Color32::from_rgb(255, 120, 110),
    )
}
/// The fill of the boxes over the map (scale bar, zoom buttons), a little see-through.
pub fn map_box() -> Color32 {
    pick(
        Color32::from_white_alpha(215),
        Color32::from_rgba_unmultiplied(34, 34, 38, 220),
    )
}
/// The fill of the credit line over the map.
pub fn map_credit() -> Color32 {
    pick(
        Color32::from_white_alpha(190),
        Color32::from_rgba_unmultiplied(34, 34, 38, 190),
    )
}
/// A selected row or card: a light blue, or a dark one.
pub fn picked() -> Color32 {
    pick(
        Color32::from_rgb(222, 232, 250),
        Color32::from_rgb(40, 58, 90),
    )
}
/// A quiet note's fill.
pub fn note_fill() -> Color32 {
    pick(
        Color32::from_rgb(243, 244, 250),
        Color32::from_rgb(40, 42, 52),
    )
}
/// A hairline over a fill: the outline of a mark.
pub fn hairline() -> Color32 {
    pick(Color32::from_gray(190), Color32::from_gray(90))
}
/// The fill of a hovered row or button, in every list and in egui's own widgets.
pub fn hover_fill() -> Color32 {
    pick(Color32::from_gray(220), Color32::from_gray(64))
}
/// A wash over a rule between rows: black on light, white on dark.
pub fn hover_wash(alpha: u8) -> Color32 {
    pick(
        Color32::from_black_alpha(alpha),
        Color32::from_white_alpha(alpha),
    )
}
/// The fill of a text box holding something that does not read.
pub fn bad_field() -> Color32 {
    pick(
        Color32::from_rgb(253, 236, 220),
        Color32::from_rgb(88, 52, 36),
    )
}
/// The lighter of two neighbouring greys (an even row, a disabled field).
pub fn soft_fill(strong: bool) -> Color32 {
    match (strong, DARK.load(Ordering::Relaxed)) {
        (true, false) => Color32::from_gray(232),
        (false, false) => Color32::from_gray(244),
        (true, true) => Color32::from_gray(58),
        (false, true) => Color32::from_gray(46),
    }
}

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
fn outline() -> Color32 {
    pick(Color32::from_gray(185), Color32::from_gray(95))
}

/// Whether the UI is dark: the setting, or for "as the system" what the system asks for,
/// light when it does not say. Independent of the map's style.
pub fn wants_dark(setting: UiTheme, system: Option<egui::Theme>) -> bool {
    match setting {
        UiTheme::Light => false,
        UiTheme::Dark => true,
        UiTheme::System => system == Some(egui::Theme::Dark),
    }
}

/// Whether [`apply`] last made the UI dark.
pub fn is_dark() -> bool {
    DARK.load(Ordering::Relaxed)
}

/// The app's look, over egui's light or dark theme: smaller corner radii, outlined buttons
/// and text boxes, and smaller window titles.
pub fn apply(ctx: &egui::Context, dark: bool) {
    DARK.store(dark, Ordering::Relaxed);
    style(ctx, dark);
}

/// The paper's text colours: the printed pages are always drawn light, whatever the UI is.
pub const PAPER_INK: Color32 = Color32::from_gray(40);
pub const PAPER_WEAK: Color32 = Color32::from_gray(105);
pub const PAPER_APPROX: Color32 = Color32::from_rgb(176, 96, 0);

/// [`apply`] for a context of its own (the printed pages), leaving the app's colours be.
pub fn style(ctx: &egui::Context, dark: bool) {
    ctx.set_theme(if dark {
        egui::Theme::Dark
    } else {
        egui::Theme::Light
    });
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
        w.inactive.bg_stroke = Stroke::new(1.0, outline());
        w.hovered.weak_bg_fill = hover_fill();
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
    let color = ui.style().interact(&r).fg_stroke.color;
    draw_glyph(ui.painter(), glyph, r.rect, color);
    r
}

/// A square button `side` points wide with `glyph` on it, for the end of a [`row`]: just
/// the symbol, muted, until the pointer is on the button, which then gets a background (a
/// stronger one while pressed).
pub fn icon_button(ui: &mut egui::Ui, glyph: Glyph, side: f32) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(Vec2::splat(side), Sense::click());
    let color = if r.hovered() || r.is_pointer_button_down_on() {
        let w = &ui.visuals().widgets;
        let fill = if r.is_pointer_button_down_on() {
            w.active.weak_bg_fill
        } else {
            hover_fill()
        };
        ui.painter().rect_filled(rect, WIDGET_RADIUS, fill);
        ink()
    } else {
        weak()
    };
    draw_glyph(ui.painter(), glyph, rect, color);
    r
}

fn draw_glyph(p: &egui::Painter, glyph: Glyph, rect: egui::Rect, color: Color32) {
    let side = rect.width();
    let stroke = Stroke::new(1.5, color);
    let c = rect.center();
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
            // The back sheet's edges that show beyond the front one.
            p.add(egui::Shape::line(
                vec![
                    Pos2::new(back.left(), front.top()),
                    back.left_top(),
                    back.right_top(),
                    back.right_bottom(),
                    Pos2::new(front.right(), back.bottom()),
                ],
                s,
            ));
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
}

/// A list row: `add_contents` lays out its content, and the whole row, across the width and
/// as high as the content, lights up under the pointer and answers `sense` (a click, or a
/// click and a drag). Buttons added inside it, such as [`icon_button`]s, take the pointer
/// first, so the row's click and drag start only where no button is.
pub fn row<R>(
    ui: &mut egui::Ui,
    sense: Sense,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    let fill = ui.painter().add(egui::Shape::Noop);
    let out = ui.scope_builder(egui::UiBuilder::new().sense(sense), |ui| {
        ui.set_width(ui.available_width());
        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(4, 1))
            .show(ui, add_contents)
            .inner
    });
    let lit = ui.is_enabled()
        && sense != Sense::hover()
        && ui.rect_contains_pointer(out.response.rect)
        && !egui::DragAndDrop::has_any_payload(ui.ctx());
    if lit {
        let fill_color = hover_fill();
        ui.painter().set(
            fill,
            egui::Shape::rect_filled(out.response.rect, WIDGET_RADIUS, fill_color),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_setting_beats_the_system_and_unknown_is_light() {
        use egui::Theme::{Dark, Light};
        assert!(wants_dark(UiTheme::System, Some(Dark)));
        assert!(!wants_dark(UiTheme::System, Some(Light)));
        assert!(!wants_dark(UiTheme::System, None));
        assert!(wants_dark(UiTheme::Dark, Some(Light)));
        assert!(!wants_dark(UiTheme::Light, Some(Dark)));
    }

    /// WCAG contrast ratio of two opaque colours.
    fn contrast(a: Color32, b: Color32) -> f32 {
        let lum = |c: Color32| {
            let f = |v: u8| {
                let v = f32::from(v) / 255.0;
                if v <= 0.03928 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * f(c.r()) + 0.7152 * f(c.g()) + 0.0722 * f(c.b())
        };
        let (x, y) = (lum(a), lum(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    #[test]
    fn text_reads_on_the_panel_in_both_themes() {
        for dark in [false, true] {
            let ctx = egui::Context::default();
            apply(&ctx, dark);
            let panel = ctx.global_style().visuals.panel_fill;
            assert!(contrast(ink(), panel) >= 4.5, "ink, dark={dark}");
            for (name, c) in [("weak", weak()), ("approx", approx()), ("accent", accent())] {
                assert!(contrast(c, panel) >= 3.0, "{name}, dark={dark}");
            }
            assert!(contrast(error(), panel) >= 3.0, "error, dark={dark}");
            assert!(contrast(ink(), picked()) >= 4.5, "picked, dark={dark}");
            assert!(contrast(ink(), note_fill()) >= 4.5, "note, dark={dark}");
        }
        apply(&egui::Context::default(), false);
    }
}
