//! "What's here" (phase2-plan.md 2.8): right-click on the map opens a menu with "What's
//! here?" and the point's coordinates (a click copies them). "What's here?" asks the search
//! worker for the nearest address, the nearest street, the places and POIs within about
//! [`NEARBY_PX`] and the areas holding the point ([`omp_core::search::Search::whats_here`],
//! as `omp-builder here` prints it), and shows them in a panel next to a marker at the
//! point. Escape, the panel's × or a click on the map closes it. Left click stays free for
//! panning; phase 3 adds "Route from here / to here" to the same menu.

use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Id, Pos2, Rect, Stroke, Vec2};
use omp_core::geo::{self, P};
use omp_core::here::Shown;

use crate::camera::{self, Camera};
use crate::scale::{self, Units};
use crate::search::{self, Icons, SearchUi, Status};
use crate::theme::{APPROX, INK, WEAK};

/// Places and POIs this far from the point, in screen points, are "nearby".
pub const NEARBY_PX: f64 = 30.0;
/// … but at least this far and at most this far, in metres.
const NEARBY_M: [f64; 2] = [15.0, 300.0];
/// Nearby places and POIs shown.
const NEARBY_SHOWN: usize = 5;
const PANEL_WIDTH: f32 = 300.0;
const DOT: Color32 = Color32::from_rgb(30, 110, 220);

/// A "what's here" request, as sent to the search worker.
#[derive(Clone, Debug)]
pub struct HereQuery {
    pub id: u64,
    pub p: P,
    /// How far to look for places and POIs, in metres.
    pub entries_m: f64,
    pub asked: Instant,
}

pub struct HereAnswer {
    pub query: HereQuery,
    pub shown: Result<Shown, String>,
    /// Time in `whats_here`.
    pub find: Duration,
}

/// An answer the panel took, for the bench and the overlay.
#[derive(Clone, Debug)]
pub struct HereTaken {
    /// From the request to the answer reaching the UI.
    pub latency: Duration,
    pub find: Duration,
    pub shown: Result<Shown, String>,
}

/// The panel: where it points, and the answer once it is in.
pub struct Panel {
    pub at: camera::Vec2,
    id: u64,
    pub answer: Option<Result<Shown, String>>,
}

#[derive(Default)]
pub struct HereUi {
    next_id: u64,
    /// Where the open menu was opened, in world coordinates.
    menu_at: Option<camera::Vec2>,
    pub panel: Option<Panel>,
    pub last: Option<HereTaken>,
}

impl HereUi {
    /// Asks what is at `at` (world coordinates) and opens the panel there.
    pub fn ask(&mut self, search: &SearchUi, at: camera::Vec2, cam: &Camera) {
        self.next_id += 1;
        let mpp = scale::metres_per_point(geo::unit_to_lonlat(at)[1], cam.scale());
        let q = HereQuery {
            id: self.next_id,
            p: geo::from_unit(at),
            entries_m: (NEARBY_PX * mpp).clamp(NEARBY_M[0], NEARBY_M[1]),
            asked: Instant::now(),
        };
        search.ask_here(q);
        self.panel = Some(Panel {
            at,
            id: self.next_id,
            answer: None,
        });
    }

    /// Takes the worker's answers to the open panel, dropping the others.
    pub fn take(&mut self, search: &SearchUi) -> Vec<HereTaken> {
        let mut out = Vec::new();
        for a in search.here_answers() {
            let Some(panel) = self.panel.as_mut().filter(|p| p.id == a.query.id) else {
                continue;
            };
            panel.answer = Some(a.shown.clone());
            let t = HereTaken {
                latency: a.query.asked.elapsed(),
                find: a.find,
                shown: a.shown,
            };
            self.last = Some(t.clone());
            out.push(t);
        }
        out
    }

    /// The map's right-click menu, and a left click closing the panel. `origin` is the
    /// map's top left on screen.
    pub fn map_input(
        &mut self,
        resp: &egui::Response,
        origin: Pos2,
        cam: &Camera,
        search: &SearchUi,
    ) {
        if resp.clicked() {
            self.panel = None;
        }
        if resp.secondary_clicked()
            && let Some(p) = resp.interact_pointer_pos()
        {
            self.menu_at =
                Some(cam.screen_to_world([f64::from(p.x - origin.x), f64::from(p.y - origin.y)]));
        }
        resp.context_menu(|ui| {
            let Some(at) = self.menu_at else {
                ui.close();
                return;
            };
            let status = search.status();
            let enabled = matches!(status, Status::Opening | Status::Ready);
            let r = ui.add_enabled(enabled, egui::Button::new("What's here?"));
            if r.clicked() {
                self.ask(search, at, cam);
                ui.close();
            }
            match &status {
                Status::Missing(why) => {
                    r.on_disabled_hover_text(why.as_str());
                }
                Status::Failed(e) => {
                    r.on_disabled_hover_text(format!("Search could not be opened: {e}"));
                }
                _ => {}
            }
            ui.separator();
            let [lon, lat] = geo::unit_to_lonlat(at);
            let r = ui
                .button(lonlat_text(lon, lat))
                .on_hover_text(format!("Copy as {}", copy_text(lon, lat)));
            if r.clicked() {
                ui.ctx().copy_text(copy_text(lon, lat));
                ui.close();
            }
        });
    }

    /// Escape closes the panel; true if it did. Called before the search box, so Escape
    /// on the map closes the panel before the search's results and marker.
    pub fn escape(&mut self, ctx: &egui::Context) -> bool {
        if self.panel.is_none() || ctx.egui_wants_keyboard_input() {
            return false;
        }
        let esc = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        if esc {
            self.panel = None;
        }
        esc
    }

    /// The marker: a dot at the point, and a ring at the address found when it is apart.
    pub fn draw_marker(&self, painter: &egui::Painter, origin: Pos2, cam: &Camera) {
        let Some(panel) = &self.panel else { return };
        let screen = |w: camera::Vec2| {
            let s = cam.world_to_screen(w);
            Pos2::new(origin.x + s[0] as f32, origin.y + s[1] as f32)
        };
        let at = screen(panel.at);
        if let Some(Ok(Shown {
            address: Some(a), ..
        })) = &panel.answer
        {
            let p = screen(geo::to_unit(a.p));
            if p.distance(at) > 8.0 {
                let stroke = Stroke::new(1.5, DOT);
                painter.line_segment([at, p], Stroke::new(1.0, DOT.gamma_multiply(0.6)));
                painter.circle(p, 5.0, Color32::from_white_alpha(200), stroke);
            }
        }
        painter.circle_filled(at, 7.5, Color32::WHITE);
        painter.circle_filled(at, 5.5, DOT);
    }

    /// The panel, next to the point and kept inside the map.
    pub fn ui(
        &mut self,
        ctx: &egui::Context,
        map_rect: Rect,
        cam: &Camera,
        units: Units,
        icons: &Icons,
    ) {
        let Some(panel) = &self.panel else { return };
        let s = cam.world_to_screen(panel.at);
        let at = map_rect.min + Vec2::new(s[0] as f32, s[1] as f32);
        if !map_rect.expand(40.0).contains(at) {
            return;
        }
        let mut close = false;
        let frame = egui::Frame::popup(&ctx.global_style())
            .inner_margin(egui::Margin::symmetric(8, 6))
            .fill(Color32::from_white_alpha(240));
        egui::Area::new(Id::unique("whats-here"))
            .fixed_pos(at + Vec2::new(14.0, -14.0))
            .constrain_to(map_rect)
            .show(ctx, |ui| {
                frame.show(ui, |ui| {
                    ui.set_width(PANEL_WIDTH);
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("What's here").small().color(WEAK));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            close = ui.small_button("×").on_hover_text("Close  (Esc)").clicked();
                        });
                    });
                    match &panel.answer {
                        None => {
                            ui.horizontal(|ui| {
                                ui.add(egui::Spinner::new().size(12.0).color(WEAK));
                                ui.label(egui::RichText::new("Looking…").color(WEAK));
                            });
                        }
                        Some(Err(e)) => {
                            ui.label(
                                egui::RichText::new(format!("Could not look: {e}")).color(APPROX),
                            );
                        }
                        Some(Ok(shown)) => shown_ui(ui, shown, units, icons),
                    }
                    ui.add_space(2.0);
                    let [lon, lat] = geo::unit_to_lonlat(panel.at);
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(lonlat_text(lon, lat))
                                .size(11.5)
                                .color(INK),
                        );
                        if ui
                            .small_button("Copy")
                            .on_hover_text(format!("Copy as {}", copy_text(lon, lat)))
                            .clicked()
                        {
                            ui.ctx().copy_text(copy_text(lon, lat));
                        }
                    });
                });
            });
        if close {
            self.panel = None;
        }
    }

    pub fn overlay_line(&self) -> Option<String> {
        let t = self.last.as_ref()?;
        Some(format!(
            "what's here: shown after {:.1} ms, whats_here {:.1} ms",
            t.latency.as_secs_f64() * 1e3,
            t.find.as_secs_f64() * 1e3
        ))
    }
}

/// The answer: the address and its place, the street when it is not the address's, the
/// places and POIs nearby, and the areas.
fn shown_ui(ui: &mut egui::Ui, s: &Shown, units: Units, icons: &Icons) {
    let away = |m: f64| {
        if m < 10.0 {
            String::new()
        } else {
            format!(" · {} away", search::distance_text(m, units))
        }
    };
    let weak = |t: String| egui::RichText::new(t).size(11.5).color(WEAK);
    if let Some(a) = &s.address {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("🏠").color(WEAK));
            ui.label(egui::RichText::new(&a.label).strong().size(14.0).color(INK));
            if a.approx {
                ui.label(egui::RichText::new("approximate").size(11.0).color(APPROX))
                    .on_hover_text(
                        "Interpolated from a range of numbers, or a point per block face",
                    );
            }
        });
        ui.label(weak(format!("{}{}", s.address_place, away(a.dist_m))));
        if !a.units.is_empty() {
            ui.label(weak(format!("Units {}", a.units.join(", "))));
        }
    }
    match &s.street {
        Some((name, town, d))
            if s.address
                .as_ref()
                .is_none_or(|a| !a.label.ends_with(name.as_str())) =>
        {
            let town = if town.is_empty() {
                String::new()
            } else {
                format!(", {town}")
            };
            let what = if s.address.is_some() {
                "Nearest street: "
            } else {
                ""
            };
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("🚗").color(WEAK));
                ui.label(egui::RichText::new(format!("{what}{name}{town}")).color(INK));
            });
            ui.label(weak(away(*d).trim_start_matches(" · ").to_string()));
        }
        None if s.address.is_none() => {
            ui.label(weak(format!(
                "No address or street within {}",
                search::distance_text(omp_core::here::ADDRESS_RADIUS_M, units)
            )));
        }
        _ => {}
    }
    if !s.nearby.is_empty() {
        ui.add_space(3.0);
        for (e, d) in s.nearby.iter().take(NEARBY_SHOWN) {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(icons.of_entry(e.kind, e.class)).color(WEAK));
                ui.label(egui::RichText::new(&e.name).color(INK));
                if *d >= 10.0 {
                    ui.label(weak(search::distance_text(*d, units)));
                }
            });
        }
        if s.nearby.len() > NEARBY_SHOWN {
            ui.label(weak(format!(
                "and {} more nearby",
                s.nearby.len() - NEARBY_SHOWN
            )));
        }
    }
    if !s.areas.is_empty() {
        ui.add_space(3.0);
        ui.add(egui::Label::new(weak(format!("In {}", s.areas.join(", ")))).wrap());
    }
}

/// `40.12345° N, 83.12345° W`.
pub fn lonlat_text(lon: f64, lat: f64) -> String {
    let ns = if lat < 0.0 { 'S' } else { 'N' };
    let ew = if lon < 0.0 { 'W' } else { 'E' };
    format!("{:.5}° {ns}, {:.5}° {ew}", lat.abs(), lon.abs())
}

/// What the copy buttons copy: `40.123450, -83.123450`, as most tools paste it.
fn copy_text(lon: f64, lat: f64) -> String {
    format!("{lat:.6}, {lon:.6}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{Event, PointerButton};
    use omp_core::here::HereAddress;

    fn shown(p: P) -> Shown {
        Shown {
            p,
            address: Some(HereAddress {
                label: "1 Capitol Square".into(),
                p,
                dist_m: 12.0,
                approx: false,
                street: Some(0),
                postcode: Some("43215".into()),
                units: Vec::new(),
            }),
            address_place: "Columbus, OH 43215".into(),
            street: None,
            nearby: Vec::new(),
            areas: vec!["Columbus".into(), "Franklin County".into(), "Ohio".into()],
            chain: Vec::new(),
        }
    }

    #[test]
    fn right_click_asks_and_the_panel_takes_its_answer() {
        let (search, tx) = SearchUi::stub();
        let mut here = HereUi::default();
        let ctx = egui::Context::default();
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 700.0));
        let mut cam = Camera::new([0.2, 0.3, 0.3, 0.4]);
        cam.size = [1000.0, 700.0];
        cam.zoom = 17.0;
        let frame = |here: &mut HereUi, events: Vec<Event>| {
            let raw = egui::RawInput {
                events,
                screen_rect: Some(rect),
                focused: true,
                ..Default::default()
            };
            let mut full = ctx.run_ui(raw, |ui| {
                let resp = ui.allocate_rect(rect, egui::Sense::click_and_drag());
                here.map_input(&resp, rect.min, &cam, &search);
                let ctx = ui.ctx().clone();
                here.escape(&ctx);
                here.ui(&ctx, rect, &cam, Units::Kilometres, search.icons());
            });
            full.textures_delta.clear();
        };
        let at = Pos2::new(300.0, 200.0);
        let esc = Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };

        let button = |button, pressed| Event::PointerButton {
            pos: at,
            button,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };

        // A right click opens the menu for the point under the pointer.
        frame(&mut here, vec![Event::PointerMoved(at)]);
        frame(&mut here, vec![button(PointerButton::Secondary, true)]);
        frame(&mut here, vec![button(PointerButton::Secondary, false)]);
        let w = here.menu_at.unwrap();
        let s = cam.world_to_screen(w);
        assert!((s[0] - 300.0).abs() < 0.5 && (s[1] - 200.0).abs() < 0.5);

        // The menu's own Escape closes it (as picking "What's here?" does).
        frame(&mut here, vec![esc.clone()]);

        // "What's here?" asks the worker, looking about 30 px around for places and POIs.
        here.ask(&search, w, &cam);
        let q = search.take_here().unwrap();
        assert_eq!(q.p, geo::from_unit(w));
        assert!((NEARBY_M[0]..=NEARBY_M[1]).contains(&q.entries_m));
        // A stale answer is dropped; the panel's own is taken.
        here.ask(&search, w, &cam);
        let q2 = search.take_here().unwrap();
        for q in [q, q2] {
            tx.send(HereAnswer {
                shown: Ok(shown(q.p)),
                query: q,
                find: Duration::from_millis(3),
            })
            .unwrap();
        }
        let taken = here.take(&search);
        assert_eq!(taken.len(), 1);
        assert!(matches!(&here.panel.as_ref().unwrap().answer, Some(Ok(s)) if s.areas.len() == 3));
        frame(&mut here, Vec::new());

        // Escape closes the panel; so does a left click on the map.
        frame(&mut here, vec![esc]);
        assert!(here.panel.is_none());
        here.ask(&search, w, &cam);
        frame(&mut here, vec![button(PointerButton::Primary, true)]);
        frame(&mut here, vec![button(PointerButton::Primary, false)]);
        assert!(here.panel.is_none());
    }
}
