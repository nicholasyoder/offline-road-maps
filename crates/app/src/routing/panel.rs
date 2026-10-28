//! The route page of the side panel (phase 3 steps 3.7 and 3.8): the stops in order (drag a
//! stop's handle to move it, × to remove it, a button to reverse them), the avoid options,
//! the distance and driving time in all and per leg, notices about the route, and the
//! directions. Clicking a step flies to it and draws its stretch over the route; Escape
//! lets go of it. Phase 4's trip planning has its places here, disabled.

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, Vec2};
use omp_core::geo;
use omp_core::route::{FERRY, Kind, Modifier, Step, TOLL};

use super::{END, Place, RouteUi, START, StopAction, VIA, duration_text};
use crate::camera::{self, Camera};
use crate::scale::Units;
use crate::search::distance_text;
use crate::side::later;
use crate::theme::{APPROX, Glyph, INK, WEAK, glyph_button};

const ICON: f32 = 20.0;
/// Where a step's text starts, from the row's left, and the line under it.
const TEXT_X: f32 = ICON + 14.0;
const STEP_FONT: FontId = FontId::proportional(13.5);
const SUB_H: f32 = 16.0;
const PICKED_FILL: Color32 = Color32::from_rgb(222, 232, 250);

/// What a click or a drag in the page landed on, for the tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Clear,
    /// A stop's drag handle, and its row (where a dragged stop is dropped).
    Handle(usize),
    Row(usize),
    Remove(usize),
    Reverse,
    Avoid(u8),
    Step(usize),
}

/// A change asked for in the page, made once it is drawn.
enum Edit {
    Clear,
    Move(usize, usize),
    Remove(usize),
    Reverse,
    Avoid(u8, bool),
    Pick(usize),
}

impl RouteUi {
    /// Escape lets go of the step picked. Returns whether it did.
    pub fn escape(&mut self, ctx: &egui::Context) -> bool {
        if self.picked.is_none() || ctx.egui_wants_keyboard_input() {
            return false;
        }
        let esc = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        if esc {
            self.pick(None);
        }
        esc
    }

    /// Draws the page in `ui`, the directions filling what is left of its height. Returns
    /// the view to fly to when a step is clicked.
    pub fn page_ui(
        &mut self,
        ui: &mut egui::Ui,
        cam: &Camera,
        units: Units,
    ) -> Option<(camera::Vec2, f64)> {
        self.targets.clear();
        let mut edits = Vec::new();
        self.header(ui, &mut edits);
        ui.separator();
        if self.has_stops() {
            self.stop_rows(ui, &mut edits);
        }
        self.options(ui, &mut edits);
        ui.separator();
        self.summary(ui, units);
        if self.route.is_some() {
            ui.horizontal_wrapped(|ui| {
                later(
                    ui,
                    "Alternatives",
                    "Other routes to pick from, drawn grey on the map",
                );
                later(
                    ui,
                    "Save trip",
                    "Keep this route with its stops and options",
                );
                later(
                    ui,
                    "Print or export directions",
                    "As a page to print, or a file",
                );
            });
        }
        let left = ui.available_height();
        self.directions(ui, left.max(80.0), units, &mut edits);
        let mut fly = None;
        for e in edits {
            match e {
                Edit::Clear => self.apply(StopAction::Clear, Place::at([0.0, 0.0])),
                Edit::Move(a, b) => self.move_stop(a, b),
                Edit::Remove(i) => self.remove_stop(i),
                Edit::Reverse => self.reverse(),
                Edit::Avoid(flag, on) => self.set_avoid(flag, on),
                Edit::Pick(i) => {
                    self.pick(Some(i));
                    fly = self.step_view(i, cam.size);
                }
            }
        }
        fly
    }

    fn header(&mut self, ui: &mut egui::Ui, edits: &mut Vec<Edit>) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Route").strong().color(INK));
            if self.waiting.is_some() {
                ui.add(egui::Spinner::new().size(12.0).color(WEAK));
            }
            if self.has_stops() {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let r = ui
                        .small_button("Clear")
                        .on_hover_text("Remove every stop and the route");
                    self.targets.push((Target::Clear, r.rect));
                    if r.clicked() {
                        edits.push(Edit::Clear);
                    }
                });
            }
        });
    }

    /// The stops, each with its handle, numbered pin, name and remove button.
    fn stop_rows(&mut self, ui: &mut egui::Ui, edits: &mut Vec<Edit>) {
        let stops: Vec<(StopAction, String)> = self
            .stops()
            .iter()
            .map(|(k, p)| (*k, p.name.clone().unwrap_or_default()))
            .collect();
        let roads = self.route.as_ref().map(|r| r.roads.clone());
        let places: Vec<_> = self.stops().iter().map(|(_, p)| p.p).collect();
        let n = stops.len();
        let dragging = egui::DragAndDrop::payload::<usize>(ui.ctx()).map(|p| *p);
        for (i, (kind, name)) in stops.iter().enumerate() {
            let row = ui.horizontal(|ui| {
                let (handle_rect, handle) =
                    ui.allocate_exact_size(Vec2::new(12.0, 18.0), Sense::drag());
                handle.dnd_set_drag_payload(i);
                let handle = handle.on_hover_text("Drag to change the order");
                self.targets.push((Target::Handle(i), handle_rect));
                if n > 1 {
                    let c = if handle.hovered() || dragging == Some(i) {
                        INK
                    } else {
                        WEAK
                    };
                    for k in 0..3 {
                        let y = handle_rect.center().y + (k as f32 - 1.0) * 4.0;
                        ui.painter().hline(
                            handle_rect.x_range().shrink(1.5),
                            y,
                            Stroke::new(1.5, c),
                        );
                    }
                }
                let fill = match kind {
                    StopAction::From => START,
                    StopAction::To => END,
                    _ => VIA,
                };
                numbered(ui, fill, i + 1);
                // A search result by its name; a point on the map by the road it is on.
                let road = roads
                    .as_ref()
                    .and_then(|r| r.get(i))
                    .filter(|r| !r.is_empty());
                let ll = geo::unit_to_lonlat(geo::to_unit(places[i]));
                let text = match (name.is_empty(), road) {
                    (false, _) => name.clone(),
                    (true, Some(r)) => format!("Near {r}"),
                    (true, None) => format!("{:.5}, {:.5}", ll[1], ll[0]),
                };
                let what = match kind {
                    StopAction::From => "Start",
                    StopAction::To => "Destination",
                    _ => "Stop",
                };
                let rest = ui.available_width() - 26.0;
                ui.allocate_ui(Vec2::new(rest.max(40.0), 18.0), |ui| {
                    ui.add(egui::Label::new(egui::RichText::new(text).color(INK)).truncate())
                        .on_hover_text(format!("{what}: {:.5}, {:.5}", ll[1], ll[0]));
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let r = glyph_button(ui, Glyph::Close, 18.0).on_hover_text("Remove this stop");
                    self.targets.push((Target::Remove(i), r.rect));
                    if r.clicked() {
                        edits.push(Edit::Remove(i));
                    }
                });
            });
            let row = row.response;
            self.targets.push((Target::Row(i), row.rect));
            if let Some(from) = row.dnd_hover_payload::<usize>()
                && *from != i
            {
                // Where the stop would go: above this row when it comes from below.
                let y = match *from > i {
                    true => row.rect.top(),
                    false => row.rect.bottom(),
                };
                ui.painter()
                    .hline(row.rect.x_range(), y, Stroke::new(2.0, START));
            }
            if let Some(from) = row.dnd_release_payload::<usize>() {
                edits.push(Edit::Move(*from, i));
            }
        }
    }

    fn options(&mut self, ui: &mut egui::Ui, edits: &mut Vec<Edit>) {
        ui.horizontal(|ui| {
            let full = self.from.is_some() && self.to.is_some();
            let r = ui
                .add_enabled(full, egui::Button::new("Reverse").small())
                .on_hover_text("Swap the start and the destination, and reverse the stops between");
            self.targets.push((Target::Reverse, r.rect));
            if r.clicked() {
                edits.push(Edit::Reverse);
            }
            ui.add_space(8.0);
            for (flag, label) in [(TOLL, "Avoid tolls"), (FERRY, "Avoid ferries")] {
                let mut on = self.avoid & flag != 0;
                let r = ui.checkbox(&mut on, label);
                self.targets.push((Target::Avoid(flag), r.rect));
                if r.changed() {
                    edits.push(Edit::Avoid(flag, on));
                }
            }
        });
        ui.horizontal_wrapped(|ui| {
            later(
                ui,
                "Optimize stop order",
                "The shortest order of the stops between, with the first and last fixed or open",
            );
            later(
                ui,
                "Depart at / Arrive by",
                "Times along the route, with overnight and fuel stops",
            );
        });
    }

    /// The totals, per leg when there are stops between, and the notices.
    fn summary(&self, ui: &mut egui::Ui, units: Units) {
        let Some(r) = &self.route else {
            let text = match (&self.error, self.from.is_some() && self.to.is_some()) {
                (None, _) if !self.has_stops() => {
                    "Pick a search result or right-click the map: “Route from here”, “Route to here”."
                        .into()
                }
                (Some(e), _) => format!("Routing failed: {e}"),
                (None, true) => "Routing…".into(),
                (None, false) if self.from.is_none() => {
                    "Pick a search result or right-click the map: “Route from here”.".into()
                }
                (None, false) => "Pick a search result or right-click the map: “Route to here”.".into(),
            };
            ui.label(egui::RichText::new(text).color(WEAK));
            return;
        };
        let trip = &r.trip;
        ui.label(
            egui::RichText::new(format!(
                "{} · {}",
                distance_text(trip.metres(), units),
                duration_text(trip.time_s())
            ))
            .size(16.0)
            .strong()
            .color(INK),
        );
        if trip.legs.len() > 1 {
            for (i, l) in trip.legs.iter().enumerate() {
                let text = match l {
                    Some(l) => format!(
                        "{} to {}: {} · {}",
                        i + 1,
                        i + 2,
                        distance_text(l.metres, units),
                        duration_text(l.time_s)
                    ),
                    None => format!("{} to {}: no route", i + 1, i + 2),
                };
                ui.label(egui::RichText::new(text).size(12.0).color(WEAK));
            }
        }
        for n in self.notices(units) {
            ui.label(egui::RichText::new(format!("⚠ {n}")).color(APPROX));
        }
    }

    /// The steps, only those in view laid out and drawn: a long route has hundreds.
    fn directions(
        &mut self,
        ui: &mut egui::Ui,
        max_height: f32,
        units: Units,
        edits: &mut Vec<Edit>,
    ) {
        let Some(r) = &self.route else { return };
        if r.steps.is_empty() {
            return;
        }
        ui.separator();
        let width = ui.available_width();
        if self
            .rows
            .as_ref()
            .is_none_or(|c| c.width != width || c.units != units)
        {
            self.rows = Some(Rows::new(ui, &r.steps, width, units));
        }
        let rows = self.rows.as_ref().unwrap();
        let total = rows.heights.iter().sum::<f32>();
        let mut targets = Vec::new();
        // Asked for its height, so it fills the rest of the page rather than shrinking to
        // last frame's size.
        egui::ScrollArea::vertical()
            .max_height(max_height)
            .min_scrolled_height(total.min(max_height))
            .auto_shrink([false, true])
            .show(ui, |ui| {
                let (all, _) = ui.allocate_exact_size(Vec2::new(width, total), Sense::hover());
                let clip = ui.clip_rect();
                let mut y = all.top();
                for (i, (s, &h)) in r.steps.iter().zip(&rows.heights).enumerate() {
                    let rect = Rect::from_min_size(Pos2::new(all.left(), y), Vec2::new(width, h));
                    y += h;
                    if rect.bottom() < clip.top() || rect.top() > clip.bottom() {
                        continue;
                    }
                    let picked = self.picked == Some(i);
                    let resp = step_row(ui, rect, i, s, &rows.subs[i], picked);
                    targets.push((Target::Step(i), resp.rect));
                    if resp.clicked() {
                        edits.push(Edit::Pick(i));
                    }
                }
            });
        self.targets.extend(targets);
    }
}

/// The directions' rows as laid out for a width: their heights, and the distance and time
/// under each instruction.
pub(super) struct Rows {
    width: f32,
    units: Units,
    heights: Vec<f32>,
    subs: Vec<Option<String>>,
}

impl Rows {
    fn new(ui: &egui::Ui, steps: &[Step], width: f32, units: Units) -> Self {
        let subs: Vec<Option<String>> = steps
            .iter()
            .map(|s| {
                (s.metres > 0.0).then(|| {
                    format!(
                        "{} · {}",
                        distance_text(s.metres, units),
                        duration_text(s.time_s)
                    )
                })
            })
            .collect();
        let heights = steps
            .iter()
            .zip(&subs)
            .map(|(s, sub)| {
                let g = ui
                    .painter()
                    .layout(s.text(), STEP_FONT, INK, width - TEXT_X);
                let h = g.size().y + if sub.is_some() { SUB_H } else { 0.0 } + 8.0;
                h.max(ICON + 8.0)
            })
            .collect();
        Self {
            width,
            units,
            heights,
            subs,
        }
    }
}

/// A stop's number in a disc of its pin's colour.
fn numbered(ui: &mut egui::Ui, fill: Color32, n: usize) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(18.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 8.5, fill);
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        n.to_string(),
        FontId::proportional(11.0),
        Color32::WHITE,
    );
}

/// A step in `rect`: its icon, instruction, and the distance and time to the next.
fn step_row(
    ui: &mut egui::Ui,
    rect: Rect,
    i: usize,
    s: &Step,
    sub: &Option<String>,
    picked: bool,
) -> egui::Response {
    let resp = ui.interact(rect, ui.scope_id().with(("step", i)), Sense::click());
    let p = ui.painter();
    if picked {
        p.rect_filled(rect, 3.0, PICKED_FILL);
    } else if resp.hovered() {
        p.rect_filled(rect, 3.0, Color32::from_black_alpha(10));
    }
    let icon = Rect::from_min_size(rect.min + Vec2::new(2.0, 4.0), Vec2::splat(ICON));
    draw_icon(p, icon, s);
    let galley = p.layout(s.text(), STEP_FONT, INK, rect.width() - TEXT_X);
    let x = rect.left() + TEXT_X - 4.0;
    let text_h = galley.size().y;
    p.galley(Pos2::new(x, rect.top() + 4.0), galley, INK);
    if let Some(sub) = sub {
        p.text(
            Pos2::new(x, rect.top() + 5.0 + text_h),
            Align2::LEFT_TOP,
            sub,
            FontId::proportional(11.5),
            WEAK,
        );
    }
    p.hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        Stroke::new(0.5, Color32::from_black_alpha(25)),
    );
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A step's icon: an arrow bent by the turn, a ring for roundabouts, a boat for ferries,
/// and the stops' discs.
fn draw_icon(p: &egui::Painter, r: Rect, s: &Step) {
    let c = r.center();
    let ink = Stroke::new(2.2, INK);
    let disc = |fill: Color32, label: String| {
        p.circle_filled(c, 8.5, fill);
        p.text(
            c,
            Align2::CENTER_CENTER,
            label,
            FontId::proportional(10.5),
            Color32::WHITE,
        );
    };
    match s.kind {
        Kind::Depart if s.leg == 0 => disc(START, "1".into()),
        Kind::Depart => disc(VIA, (s.leg + 1).to_string()),
        Kind::Stop => disc(VIA, s.stop.to_string()),
        Kind::Arrive => disc(END, s.stop.to_string()),
        Kind::Ferry | Kind::OffFerry => {
            let hull = vec![
                c + Vec2::new(-8.0, 0.0),
                c + Vec2::new(8.0, 0.0),
                c + Vec2::new(5.0, 5.0),
                c + Vec2::new(-5.0, 5.0),
            ];
            p.add(egui::Shape::convex_polygon(hull, INK, Stroke::NONE));
            p.rect_filled(
                Rect::from_min_size(c + Vec2::new(-4.0, -6.0), Vec2::new(7.0, 5.0)),
                1.0,
                INK,
            );
            let wave: Vec<Pos2> = (0..=16)
                .map(|k| {
                    let x = k as f32 - 8.0;
                    c + Vec2::new(x, 8.5 + (x * 0.8).sin() * 1.2)
                })
                .collect();
            p.add(egui::Shape::line(wave, Stroke::new(1.2, INK)));
        }
        Kind::Roundabout => {
            let ring = c + Vec2::new(0.0, 1.0);
            p.circle_stroke(ring, 4.5, ink);
            p.line_segment(
                [Pos2::new(c.x, r.bottom() - 1.0), ring + Vec2::new(0.0, 4.5)],
                ink,
            );
            arrow(p, ring, s.angle, 4.5, ink);
        }
        Kind::UTurn => {
            let right = s.angle > 0.0;
            let sx = if right { 1.0 } else { -1.0 };
            let x0 = c.x - sx * 3.5;
            let mut pts = vec![Pos2::new(x0, r.bottom() - 1.0), Pos2::new(x0, c.y - 1.0)];
            for k in 1..=8 {
                let a = std::f32::consts::PI * k as f32 / 8.0;
                pts.push(Pos2::new(
                    c.x - sx * 3.5 * a.cos(),
                    c.y - 1.0 - 3.5 * a.sin(),
                ));
            }
            let tip = Pos2::new(c.x + sx * 3.5, c.y + 5.0);
            pts.push(tip);
            p.add(egui::Shape::line(pts, ink));
            head(p, tip, Vec2::new(0.0, 1.0), ink);
        }
        _ => {
            let angle = match (s.kind, s.modifier) {
                (Kind::Continue, Modifier::Straight) => 0.0,
                _ => s.angle,
            };
            p.line_segment(
                [Pos2::new(c.x, r.bottom() - 1.0), c + Vec2::new(0.0, 2.0)],
                ink,
            );
            arrow(p, c + Vec2::new(0.0, 2.0), angle, 0.0, ink);
        }
    }
}

/// An arrow from `from` (or `skip` along it) at `angle` degrees from straight ahead.
fn arrow(p: &egui::Painter, from: Pos2, angle: f64, skip: f32, ink: Stroke) {
    let a = (angle.clamp(-150.0, 150.0) as f32).to_radians();
    let dir = Vec2::new(a.sin(), -a.cos());
    let start = from + dir * skip;
    let tip = from + dir * (skip + 8.0);
    p.line_segment([start, tip], ink);
    head(p, tip, dir, ink);
}

fn head(p: &egui::Painter, tip: Pos2, dir: Vec2, ink: Stroke) {
    let n = Vec2::new(-dir.y, dir.x);
    let back = tip - dir * 4.5;
    p.add(egui::Shape::convex_polygon(
        vec![tip + dir * 1.0, back + n * 4.0, back - n * 4.0],
        ink.color,
        Stroke::NONE,
    ));
}

#[cfg(test)]
#[path = "panel_tests.rs"]
mod tests;
