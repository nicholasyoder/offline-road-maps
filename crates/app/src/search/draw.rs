//! Painting: the kind icons, a suggestion or result row, and the marker left at a result.

use std::sync::Arc;

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use omp_core::find::{Hit, HitKind};
use omp_core::geo;
use omp_core::schema::Schema;
use omp_core::search::Kind;

use super::distance_text;
use crate::camera::Camera;
use crate::scale::Units;
use crate::theme::{APPROX, INK, WEAK};

const PIN: Color32 = Color32::from_rgb(208, 52, 44);

/// The kind icons: glyphs from egui's built-in emoji fonts, by kind and class.
pub struct Icons {
    places: Vec<&'static str>,
    pois: Vec<&'static str>,
    labels: Vec<&'static str>,
}

impl Icons {
    pub fn new(schema: &Schema) -> Self {
        let classes = |layer: &str, f: fn(&str) -> &'static str| {
            schema
                .layer(layer)
                .map(|(_, l)| l.classes.iter().map(|c| f(&c.name)).collect())
                .unwrap_or_default()
        };
        Self {
            places: classes("places", |c| match c {
                "country" | "state" => "🗺",
                "ocean" | "sea" | "bay" => "🌊",
                "city" | "town" | "village" => "🏢",
                _ => "🏡",
            }),
            pois: classes("pois", |c| match c {
                "fuel" => "⛽",
                "charging_station" => "🔌",
                "rest_area" | "parking" => "🅿",
                "hotel" => "🏨",
                "restaurant" | "fast_food" => "🍴",
                "hospital" => "🏥",
                "airport" => "✈",
                "campground" => "⛺",
                _ => "📍",
            }),
            labels: classes("labels", |c| match c {
                "bay" => "🌊",
                "river" | "riverbank" | "lake" => "💧",
                _ => "🌲",
            }),
        }
    }

    pub fn of(&self, hit: &Hit) -> &'static str {
        match hit.kind {
            HitKind::Address => "🏠",
            HitKind::Entry(k) => self.of_entry(k, hit.class),
        }
    }

    pub fn of_entry(&self, kind: Kind, class: u16) -> &'static str {
        let class = |v: &[&'static str], or| v.get(usize::from(class)).copied().unwrap_or(or);
        match kind {
            Kind::Street | Kind::AddressStreet => "🚗",
            Kind::Admin => "🗺",
            Kind::Postcode => "📮",
            Kind::Place => class(&self.places, "🏡"),
            Kind::Poi => class(&self.pois, "📍"),
            Kind::Feature => class(&self.labels, "🌲"),
        }
    }
}

/// One row of the suggestions or the results.
pub(super) enum Row {
    Hit(Hit),
    Recent(String),
}

/// A suggestion or result: icon, name, a tag when approximate, the place it is in and the
/// distance from the view centre.
pub(super) fn row(
    ui: &mut egui::Ui,
    row: &Row,
    icons: &Icons,
    cam: &Camera,
    units: Units,
    selected: bool,
) -> egui::Response {
    let width = ui.available_width();
    let (icon, name, tag, context, right) = match row {
        Row::Recent(r) => ("🕒", r.to_string(), None, String::new(), String::new()),
        Row::Hit(h) => {
            let tag = if h.number_missing {
                Some("number not found")
            } else if h.approx {
                Some("approximate")
            } else {
                None
            };
            let mut context = h.context.clone();
            if let Some(pc) = &h.postcode {
                context = if context.is_empty() {
                    pc.clone()
                } else {
                    format!("{context} {pc}")
                };
            }
            if !h.units.is_empty() {
                context += &format!(" · units {}", h.units.join(", "));
            }
            let d = distance_text(geo::distance_m(cam.center, geo::to_unit(h.p)), units);
            (icons.of(h), h.name.clone(), tag, context, d)
        }
    };
    let height = if context.is_empty() { 26.0 } else { 38.0 };
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, height), Sense::click());
    let p = ui.painter_at(rect);
    if selected || resp.hovered() {
        let fill = if selected {
            Color32::from_rgb(218, 230, 248)
        } else {
            Color32::from_gray(238)
        };
        p.rect_filled(rect, 3.0, fill);
    }
    let x = rect.left() + 28.0;
    p.text(
        Pos2::new(rect.left() + 13.0, rect.center().y),
        Align2::CENTER_CENTER,
        icon,
        FontId::proportional(15.0),
        WEAK,
    );
    let right_g = p.layout_no_wrap(right, FontId::proportional(11.5), WEAK);
    let right_w = right_g.size().x;
    let top = rect.top() + if context.is_empty() { 5.0 } else { 3.0 };
    p.galley(
        Pos2::new(rect.right() - 4.0 - right_w, top + 2.0),
        right_g,
        WEAK,
    );
    let name_w = (rect.right() - 12.0 - right_w - x).max(40.0);
    let name_g = one_line(&p, name, FontId::proportional(14.0), INK, name_w);
    let after = x + name_g.size().x + 6.0;
    p.galley(Pos2::new(x, top), name_g, INK);
    if let Some(t) = tag {
        let g = p.layout_no_wrap(t.into(), FontId::proportional(11.0), APPROX);
        if after + g.size().x < rect.right() - 12.0 - right_w {
            p.galley(Pos2::new(after, top + 3.0), g, APPROX);
        }
    }
    if !context.is_empty() {
        let g = one_line(
            &p,
            context,
            FontId::proportional(11.5),
            WEAK,
            rect.right() - 4.0 - x,
        );
        p.galley(Pos2::new(x, top + 18.0), g, WEAK);
    }
    resp
}

/// `text` on one line, cut with "…" to `width`.
fn one_line(
    p: &egui::Painter,
    text: String,
    font: FontId,
    color: Color32,
    width: f32,
) -> Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple_singleline(text, font, color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(width);
    p.layout_job(job)
}

/// The marker of a picked result: a pin at its point, with its name, where it is, and
/// "Approximate" for interpolated and blockface addresses.
pub fn draw_marker(painter: &egui::Painter, origin: Pos2, cam: &Camera, hit: &Hit) {
    let s = cam.world_to_screen(geo::to_unit(hit.p));
    let tip = Pos2::new(origin.x + s[0] as f32, origin.y + s[1] as f32);
    if !painter.clip_rect().expand(300.0).contains(tip) {
        return;
    }
    let head = tip - Vec2::new(0.0, 22.0);
    let r = 8.0;
    let outline = Stroke::new(1.5, Color32::from_rgb(120, 20, 16));
    painter.circle_filled(tip, 2.5, Color32::from_black_alpha(90));
    painter.add(egui::Shape::convex_polygon(
        vec![
            tip,
            head + Vec2::new(r * 0.85, 4.0),
            head + Vec2::new(-r * 0.85, 4.0),
        ],
        PIN,
        Stroke::NONE,
    ));
    painter.circle(head, r, PIN, outline);
    painter.circle_filled(head, 3.0, Color32::WHITE);

    let mut lines = vec![(hit.name.clone(), FontId::proportional(13.5), INK)];
    let mut context = hit.context.clone();
    if let Some(pc) = &hit.postcode {
        context = if context.is_empty() {
            pc.clone()
        } else {
            format!("{context} {pc}")
        };
    }
    if !context.is_empty() {
        lines.push((context, FontId::proportional(11.5), WEAK));
    }
    if hit.number_missing {
        lines.push((
            "Number not found on this street".into(),
            FontId::proportional(11.5),
            APPROX,
        ));
    } else if hit.approx {
        lines.push(("Approximate".into(), FontId::proportional(11.5), APPROX));
    }
    let galleys: Vec<_> = lines
        .into_iter()
        .map(|(t, f, c)| (painter.layout_no_wrap(t, f, c), c))
        .collect();
    let w = galleys.iter().map(|(g, _)| g.size().x).fold(0.0, f32::max);
    let h: f32 = galleys.iter().map(|(g, _)| g.size().y).sum();
    let pad = Vec2::new(6.0, 4.0);
    let at = head + Vec2::new(r + 6.0, -h / 2.0 - pad.y);
    let rect = Rect::from_min_size(at, Vec2::new(w, h) + 2.0 * pad);
    painter.rect(
        rect,
        4.0,
        Color32::from_white_alpha(235),
        Stroke::new(1.0, Color32::from_gray(170)),
        StrokeKind::Inside,
    );
    let mut y = rect.top() + pad.y;
    for (g, c) in galleys {
        let gh = g.size().y;
        painter.galley(Pos2::new(rect.left() + pad.x, y), g, c);
        y += gh;
    }
}
