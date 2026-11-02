//! The buttons and readouts over the map: the button that opens the side panel, the scale
//! bar and the cursor's coordinates, the zoom buttons and the credit line.

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, Vec2};

use super::App;
use crate::here::lonlat_text;
use crate::scale::{self, Units};
use crate::side::Action;
use crate::theme::{Glyph, INK, glyph_button};

impl App {
    /// The buttons and readouts over the map: the button that opens the side panel at the
    /// top left when it is closed; the scale bar and the cursor's coordinates at the bottom
    /// left; the zoom buttons and the credit line at the bottom right.
    pub(super) fn map_controls(&mut self, ctx: &egui::Context, map_rect: Rect) {
        let Some(m) = &mut self.map else { return };
        let units = self.cfg.units.unwrap_or(m.units);
        let panel = egui::Frame::popup(&ctx.global_style())
            .shadow(crate::theme::MAP_SHADOW)
            .inner_margin(egui::Margin::symmetric(6, 4))
            .fill(Color32::from_white_alpha(215));
        let mut action = None;
        if !self.cfg.side_panel.open {
            let side = &mut self.cfg.side_panel;
            egui::Area::new(egui::Id::unique("side-open"))
                .fixed_pos(map_rect.left_top() + Vec2::new(8.0, 8.0))
                .show(ctx, |ui| {
                    let open = glyph_button(ui, Glyph::Show, 30.0)
                        .on_hover_text("Show the panel: search, route, layers  (Ctrl+B)");
                    if open.clicked() {
                        side.open = true;
                    }
                });
        }

        // Bottom left: scale bar and coordinates.
        let mpp = scale::metres_per_point(m.camera.lonlat()[1], m.camera.scale());
        let cursor = self.cursor;
        egui::Area::new(egui::Id::unique("map-scale"))
            .fixed_pos(map_rect.left_bottom() + Vec2::new(8.0, -8.0))
            .pivot(Align2::LEFT_BOTTOM)
            .interactable(false)
            .show(ctx, |ui| {
                panel.show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    scale_bar(ui, mpp, units);
                    let text = cursor.map_or_else(String::new, |[lon, lat]| lonlat_text(lon, lat));
                    ui.add_space(3.0);
                    ui.label(egui::RichText::new(text).size(11.0).color(INK));
                });
            });

        // Bottom right: zoom buttons above the credit line. Only OpenStreetMap's credit stays
        // on the map, as its licence asks; the address sources' credits, hundreds for a large
        // pack, are in About this map, which the line opens.
        let credit = "© OpenStreetMap contributors";
        let credit_rect = egui::Area::new(egui::Id::unique("map-credit"))
            .fixed_pos(map_rect.right_bottom())
            .pivot(Align2::RIGHT_BOTTOM)
            .show(ctx, |ui| {
                egui::Frame::NONE
                    .fill(Color32::from_white_alpha(190))
                    .inner_margin(egui::Margin::symmetric(4, 1))
                    .show(ui, |ui| {
                        let label =
                            egui::Label::new(egui::RichText::new(credit).size(10.5).color(INK))
                                .sense(Sense::click());
                        if ui.add(label).on_hover_text("About this map").clicked() {
                            action = Some(Action::About);
                        }
                    });
            })
            .response
            .rect;
        let center = [m.camera.size[0] / 2.0, m.camera.size[1] / 2.0];
        egui::Area::new(egui::Id::unique("map-zoom"))
            .fixed_pos(Pos2::new(map_rect.right() - 8.0, credit_rect.top() - 8.0))
            .pivot(Align2::RIGHT_BOTTOM)
            .show(ctx, |ui| {
                panel.show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    // The signs are drawn, not typed: a font's + and − sit off centre.
                    let button = |ui: &mut egui::Ui, plus: bool, tip: &str| {
                        let b = egui::Button::new("").min_size(Vec2::splat(32.0));
                        let r = ui.add(b).on_hover_text(tip);
                        let (c, h) = (r.rect.center(), 6.5);
                        let stroke = Stroke::new(2.0, ui.style().interact(&r).fg_stroke.color);
                        let p = ui.painter();
                        p.line_segment([c - Vec2::new(h, 0.0), c + Vec2::new(h, 0.0)], stroke);
                        if plus {
                            p.line_segment([c - Vec2::new(0.0, h), c + Vec2::new(0.0, h)], stroke);
                        }
                        r.clicked()
                    };
                    if button(ui, true, "Zoom in  (+)") {
                        m.motion.zoom(1.0, center);
                    }
                    if button(ui, false, "Zoom out  (\u{2212})") {
                        m.motion.zoom(-1.0, center);
                    }
                });
            });

        self.act(ctx, action);
    }
}

/// One bar per unit, at most 120 points long: miles with its ticks up, kilometres below it
/// with its ticks down.
fn scale_bar(ui: &mut egui::Ui, metres_per_point: f64, units: Units) {
    const MAX: f64 = 120.0;
    let bars: &[(bool, bool)] = match units {
        Units::Miles => &[(true, true)],
        Units::Kilometres => &[(false, true)],
        Units::Both => &[(true, true), (false, false)],
    };
    let stroke = Stroke::new(1.5, INK);
    let font = FontId::proportional(11.0);
    for &(imperial, up) in bars {
        let (len, text) = scale::bar(metres_per_point, MAX, imperial);
        let label = ui
            .painter()
            .layout_no_wrap(text.to_string(), font.clone(), INK);
        let width = 1.0 + len as f32 + 5.0 + label.size().x;
        let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 13.0), Sense::hover());
        let p = ui.painter();
        let (x0, x1) = (rect.left() + 1.0, rect.left() + 1.0 + len as f32);
        let (y, tip) = match up {
            true => (rect.bottom() - 0.75, rect.top() + 3.0),
            false => (rect.top() + 0.75, rect.bottom() - 3.0),
        };
        p.line_segment([Pos2::new(x0, y), Pos2::new(x1, y)], stroke);
        p.line_segment([Pos2::new(x0, y), Pos2::new(x0, tip)], stroke);
        p.line_segment([Pos2::new(x1, y), Pos2::new(x1, tip)], stroke);
        p.text(
            Pos2::new(x1 + 5.0, rect.center().y),
            Align2::LEFT_CENTER,
            text,
            font.clone(),
            INK,
        );
    }
}
