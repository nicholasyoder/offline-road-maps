//! Routing in the app (phase 3 step 3.6): stops from the map's right-click menu and from
//! search results, the route from the worker, its line drawn by the map renderer, and the
//! stops' markers.
//!
//! - [`worker`]: the thread that opens the routing readers and runs the trips; no egui.
//! - [`line`]: the route's lines, simplified per zoom band, as the renderer draws them.
//! - [`panel`]: the route page of the side panel (steps 3.7 and 3.8): the stops, the avoid
//!   options, the totals, the notices and the directions.
//!
//! Stops are kept in order: the start, the stops added between, and the destination. Each
//! change asks for the route again once there is a start and a destination; the answer to
//! the latest request replaces the route shown, and the view fits it unless it is already
//! in view. *Optimize stop order* (phase 4 step 4.1) asks the worker to reorder the stops
//! first; the answer puts the stops in the new order, says what it saved, and can be
//! undone until the stops change again. Over the route are drawn the parts on restricted roads and the step picked in
//! the directions, and dashed lines join each stop to where the route meets its road.

mod line;
mod panel;
mod worker;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Pos2};
use omp_core::find::Hit;
use omp_core::geo::{self, P};
use omp_core::route::{Ends, FAR_SNAP_M, FERRY, Stop, TOLL};

use crate::camera::{self, Camera, TILE_PX};
use crate::pool::Pool;
use crate::scale::Units;
use crate::search::{self, distance_text};

pub use line::{GpuBand, Line, LineDraw, Look, RouteDraw, RouteSegment, band_for};
use worker::{Request, Worker};
pub use worker::{Routed, Status};

/// The most the view zooms in to fit a route, and the share of the map it fills.
const FIT_MAX_ZOOM: f64 = 17.0;
const FIT_SHARE: f64 = 0.8;
/// Beyond the view, in points, where pieces of the line are still drawn: its halo's width.
pub const DRAW_MARGIN: f64 = 8.0;
/// A stop nearer its road than this gets no dashed line to it, in metres.
const CONNECTOR_M: f64 = 3.0;
/// The farthest a stop may be from a road, as [`omp_core::route`] snaps (its last radius).
const SNAP_MAX_KM: f64 = 50.0;

const START: Color32 = Color32::from_rgb(40, 150, 70);
const VIA: Color32 = Color32::from_rgb(230, 140, 20);
const END: Color32 = Color32::from_rgb(208, 52, 44);

/// A point the route goes through.
#[derive(Clone, Debug, PartialEq)]
pub struct Place {
    pub p: P,
    /// A search result's own street, to snap to first.
    pub street: Option<u32>,
    /// A search result's name; `None` for a point on the map.
    pub name: Option<String>,
}

impl Place {
    pub fn at(w: camera::Vec2) -> Self {
        Self {
            p: geo::from_unit(w),
            street: None,
            name: None,
        }
    }

    pub fn of_hit(h: &Hit) -> Self {
        Self {
            p: h.p,
            street: h.street,
            name: Some(h.name.clone()),
        }
    }

    fn stop(&self) -> Stop {
        Stop {
            p: self.p,
            street: self.street,
        }
    }
}

/// What the "Route from here" items ask for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopAction {
    From,
    To,
    /// A stop before the destination.
    Add,
    Clear,
}

/// What the route menu items need to know, for menus drawn by other parts of the UI.
#[derive(Clone, Debug, Default)]
pub struct MenuState {
    pub enabled: bool,
    /// Why the items are disabled.
    pub why: Option<String>,
    pub has_stops: bool,
}

const ITEMS: [(&str, StopAction); 3] = [
    ("Route from here", StopAction::From),
    ("Route to here", StopAction::To),
    ("Add stop", StopAction::Add),
];

/// The route items of a menu: "Route from here", "Route to here", "Add stop", and "Clear
/// route" once there are stops. Returns the one clicked.
pub fn menu_items(ui: &mut egui::Ui, state: &MenuState) -> Option<StopAction> {
    let mut chosen = None;
    for (label, action) in ITEMS {
        let r = ui.add_enabled(state.enabled, egui::Button::new(label));
        if r.clicked() {
            chosen = Some(action);
        }
        if let Some(why) = &state.why {
            r.on_disabled_hover_text(why.as_str());
        }
    }
    if state.has_stops && ui.button("Clear route").clicked() {
        chosen = Some(StopAction::Clear);
    }
    chosen
}

/// The same items as a row of buttons, for the search page's "what's here" and picked
/// result (step 3.10). Returns the one clicked, and where each was drawn, for the tests.
pub fn stop_buttons(ui: &mut egui::Ui, state: &MenuState) -> (Option<StopAction>, [egui::Rect; 3]) {
    let mut chosen = None;
    let mut rects = [egui::Rect::NOTHING; 3];
    ui.horizontal_wrapped(|ui| {
        for (i, (label, action)) in ITEMS.into_iter().enumerate() {
            let r = ui.add_enabled(state.enabled, egui::Button::new(label).small());
            rects[i] = r.rect;
            if r.clicked() {
                chosen = Some(action);
            }
            if let Some(why) = &state.why {
                r.on_disabled_hover_text(why.as_str());
            }
        }
    });
    (chosen, rects)
}

/// Timings of the last answer, for the overlay.
#[derive(Clone, Copy, Debug)]
pub struct Taken {
    /// From the request to the answer reaching the UI.
    pub latency: Duration,
    /// In the matrix and the order, for an optimize request.
    pub optimize: Duration,
    pub trip: Duration,
    pub directions: Duration,
    pub line: Duration,
    pub points: usize,
}

/// "45 min", "3 h 05 min"; "under a minute".
pub fn duration_text(s: f64) -> String {
    let min = (s / 60.0).round() as u64;
    match min {
        0 => "under a minute".into(),
        m if m < 60 => format!("{m} min"),
        m => format!("{} h {:02} min", m / 60, m % 60),
    }
}

/// The lines drawn over and around the route, from what the route and its stops are.
pub fn overlay_lines(routed: &Routed, stops: &[P], picked: Option<usize>) -> Vec<(Look, Line)> {
    let trip = &routed.trip;
    let connectors: Vec<[P; 2]> = trip
        .snaps
        .iter()
        .zip(stops)
        .filter_map(|(s, &p)| {
            let s = s.as_ref()?;
            (s.dist_m >= CONNECTOR_M).then_some([s.point, p])
        })
        .collect();
    let mut restricted = Vec::new();
    for (leg, runs) in trip.legs.iter().zip(&routed.restricted) {
        let Some(leg) = leg else { continue };
        for r in runs {
            let pts = leg.pieces[r.start].points.start..leg.pieces[r.end - 1].points.end;
            restricted.push(&leg.points[pts]);
        }
    }
    let mut out = vec![
        (
            Look::Connector,
            Line::new(connectors.iter().map(|c| &c[..])),
        ),
        (Look::Restricted, Line::new(restricted)),
    ];
    let step = picked.and_then(|i| routed.steps.get(i));
    if let Some(leg) = step.and_then(|s| trip.legs.get(s.leg)?.as_ref()) {
        let s = step.unwrap();
        let end = s.points.end.min(leg.points.len());
        let start = s.points.start.min(end);
        out.push((Look::Picked, Line::new([&leg.points[start..end]])));
    }
    out
}

/// The stops as they were before *Optimize stop order*, and the route's totals then.
#[derive(Clone, Debug)]
pub struct Before {
    places: Vec<Place>,
    time_s: f64,
    metres: f64,
}

/// What the last *Optimize stop order* did, shown under it.
#[derive(Clone, Debug)]
pub enum Reordered {
    /// The stops' new order saves this much on the route before; it can be undone.
    Saved {
        before: Before,
        time_s: f64,
        metres: f64,
    },
    /// The order was already the quickest.
    Already,
    Failed(String),
}

pub struct RouteUi {
    worker: Worker,
    pub from: Option<Place>,
    pub via: Vec<Place>,
    pub to: Option<Place>,
    /// [`omp_core::route::TOLL`] and [`omp_core::route::FERRY`].
    pub avoid: u8,
    /// The ends *Optimize stop order* keeps.
    pub ends: Ends,
    /// While an optimize request is waiting: the stops before it, and those it sent.
    optimizing: Option<(Before, Vec<Place>)>,
    pub reordered: Option<Reordered>,
    next_id: u64,
    /// The latest request, until its answer comes.
    pub waiting: Option<u64>,
    pub route: Option<Routed>,
    /// The stops the route shown was asked through.
    routed_stops: Vec<P>,
    pub error: Option<String>,
    /// The lines other than the route's own ([`overlay_lines`]).
    overlays: Vec<(Look, Line)>,
    /// Per [`Look`], its line at the band last drawn.
    bands: [Option<Arc<GpuBand>>; 4],
    pub last: Option<Taken>,
    /// Answers to older requests, dropped.
    pub dropped: usize,
    /// A stop was set since the side panel last looked: it shows the route page.
    pub opened: bool,
    /// The step of the directions picked: drawn over the route.
    pub picked: Option<usize>,
    /// The directions' rows, laid out once per route and width.
    rows: Option<panel::Rows>,
    /// What the panel drew last frame, for the tests to click on.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) targets: Vec<(panel::Target, egui::Rect)>,
}

impl RouteUi {
    pub fn start(first_chunk: PathBuf, ctx: &egui::Context) -> Self {
        let ctx = ctx.clone();
        Self::with_worker(Worker::start(first_chunk, move || ctx.request_repaint()))
    }

    /// A route UI on a worker with no thread, which never answers.
    #[cfg(test)]
    pub fn stub() -> Self {
        Self::with_worker(Worker::stub().0)
    }

    fn with_worker(worker: Worker) -> Self {
        Self {
            worker,
            from: None,
            via: Vec::new(),
            to: None,
            avoid: 0,
            ends: Ends {
                keep_first: true,
                keep_last: true,
                round_trip: false,
            },
            optimizing: None,
            reordered: None,
            next_id: 0,
            waiting: None,
            route: None,
            routed_stops: Vec::new(),
            error: None,
            overlays: Vec::new(),
            bands: Default::default(),
            last: None,
            dropped: 0,
            opened: false,
            picked: None,
            rows: None,
            targets: Vec::new(),
        }
    }

    pub fn status(&self) -> Status {
        self.worker.status()
    }

    pub fn menu_state(&self) -> MenuState {
        let status = self.status();
        MenuState {
            enabled: status.usable(),
            why: status.why_not(),
            has_stops: self.has_stops(),
        }
    }

    pub fn has_stops(&self) -> bool {
        self.from.is_some() || self.to.is_some() || !self.via.is_empty()
    }

    /// The stops in order, each with its kind.
    pub fn stops(&self) -> Vec<(StopAction, &Place)> {
        let from = self.from.iter().map(|p| (StopAction::From, p));
        let via = self.via.iter().map(|p| (StopAction::Add, p));
        let to = self.to.iter().map(|p| (StopAction::To, p));
        from.chain(via).chain(to).collect()
    }

    /// Changes the stops and asks for the route again. A stop set opens the route page.
    pub fn apply(&mut self, action: StopAction, place: Place) {
        match action {
            StopAction::From => self.from = Some(place),
            StopAction::To => self.to = Some(place),
            StopAction::Add => self.via.push(place),
            StopAction::Clear => {
                self.from = None;
                self.via.clear();
                self.to = None;
            }
        }
        self.opened = action != StopAction::Clear;
        self.reroute();
    }

    /// Puts `places` back as the stops, the first as the start if `from` and the last as
    /// the destination if `to`.
    fn set_stops(&mut self, places: Vec<Place>, from: bool, to: bool) {
        self.put_stops(places, from, to);
        self.reroute();
    }

    /// [`Self::set_stops`] without asking for the route.
    fn put_stops(&mut self, mut places: Vec<Place>, from: bool, to: bool) {
        self.from = (from && !places.is_empty()).then(|| places.remove(0));
        self.to = if to { places.pop() } else { None };
        self.via = places;
    }

    fn places(&self) -> Vec<Place> {
        self.stops().into_iter().map(|(_, p)| p.clone()).collect()
    }

    /// Moves stop `from` (in [`Self::stops`]' order) to `to`; the first is then the start
    /// and the last the destination.
    pub fn move_stop(&mut self, from: usize, to: usize) {
        let mut places = self.places();
        if from >= places.len() || to >= places.len() || from == to {
            return;
        }
        let p = places.remove(from);
        places.insert(to, p);
        let (f, t) = (self.from.is_some(), self.to.is_some());
        self.set_stops(places, f, t);
    }

    /// Removes stop `i`. The stop next to a removed start or destination takes its place.
    pub fn remove_stop(&mut self, i: usize) {
        let mut places = self.places();
        if i >= places.len() {
            return;
        }
        let n = places.len();
        let via = !self.via.is_empty();
        let from = self.from.is_some() && (i != 0 || via);
        let to = self.to.is_some() && (i != n - 1 || via);
        places.remove(i);
        self.set_stops(places, from, to);
    }

    /// Reverses the stops: the destination becomes the start.
    pub fn reverse(&mut self) {
        let mut places = self.places();
        places.reverse();
        let (f, t) = (self.to.is_some(), self.from.is_some());
        self.set_stops(places, f, t);
    }

    /// Turns an avoid option ([`TOLL`], [`FERRY`]) on or off, and routes again.
    pub fn set_avoid(&mut self, flag: u8, on: bool) {
        let avoid = match on {
            true => self.avoid | flag,
            false => self.avoid & !flag,
        };
        if avoid != self.avoid {
            self.avoid = avoid;
            self.reroute();
        }
    }

    /// The stops *Optimize stop order* sends: all of them, but for a round trip whose
    /// destination is already back at the start, without it.
    fn to_optimize(&self) -> Vec<Place> {
        let mut places = self.places();
        let back = places.len() > 2 && places.first().map(|p| p.p) == places.last().map(|p| p.p);
        if self.ends.round_trip && back {
            places.pop();
        }
        places
    }

    /// The route is shown, with stops that a new order could change.
    pub fn can_optimize(&self) -> bool {
        let places = self.to_optimize();
        let round =
            self.ends.round_trip && places.len() >= 2 && places.len() == self.places().len();
        let shown = self.route.as_ref().is_some_and(|r| r.trip.complete());
        shown && self.waiting.is_none() && (self.ends.free(places.len()) >= 2 || round)
    }

    /// Asks for the route through the stops in their quickest order, keeping [`Self::ends`].
    pub fn optimize(&mut self) {
        let Some(route) = self.route.as_ref().filter(|_| self.can_optimize()) else {
            return;
        };
        let before = Before {
            places: self.places(),
            time_s: route.trip.time_s(),
            metres: route.trip.metres(),
        };
        let places = self.to_optimize();
        self.next_id += 1;
        self.waiting = Some(self.next_id);
        self.reordered = None;
        self.worker.ask(Request {
            id: self.next_id,
            stops: places.iter().map(Place::stop).collect(),
            avoid: self.avoid,
            asked: Instant::now(),
            optimize: Some(self.ends),
        });
        self.optimizing = Some((before, places));
    }

    /// Stops an optimize request; the route shown stays.
    pub fn cancel_optimize(&mut self) {
        if self.optimizing.take().is_some() {
            self.worker.cancel();
            self.waiting = None;
        }
    }

    /// Puts the stops back as they were before the last *Optimize stop order*.
    pub fn undo_optimize(&mut self) {
        if let Some(Reordered::Saved { before, .. }) = self.reordered.take() {
            self.set_stops(before.places, true, true);
        }
    }

    /// Picks a step of the directions (or none), to draw over the route.
    pub fn pick(&mut self, step: Option<usize>) {
        if step == self.picked {
            return;
        }
        self.picked = step;
        self.overlays.retain(|(l, _)| *l != Look::Picked);
        self.bands[Look::Picked as usize] = None;
        if let Some(r) = &self.route {
            let picked = overlay_lines(r, &self.routed_stops, step);
            self.overlays
                .extend(picked.into_iter().filter(|(l, _)| *l == Look::Picked));
        }
    }

    /// The view that shows step `i`, its stretch of road fitted.
    pub fn step_view(&self, i: usize, size: camera::Vec2) -> Option<(camera::Vec2, f64)> {
        let r = self.route.as_ref()?;
        let s = r.steps.get(i)?;
        let leg = r.trip.legs.get(s.leg)?.as_ref()?;
        let pts = leg.points.get(s.points.clone()).unwrap_or_default();
        let one = [s.point];
        let pts = if pts.is_empty() { &one[..] } else { pts };
        let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
        for &p in pts {
            let w = geo::to_unit(p);
            b = [
                b[0].min(w[0]),
                b[1].min(w[1]),
                b[2].max(w[0]),
                b[3].max(w[1]),
            ];
        }
        Some(view_for(b, size))
    }

    /// What the panel says about the route: stops far from a road or with none, legs with
    /// no route, avoid options broken, and stretches on restricted roads.
    pub fn notices(&self, units: Units) -> Vec<String> {
        let Some(r) = &self.route else {
            return Vec::new();
        };
        let trip = &r.trip;
        let n = trip.snaps.len();
        let who = |i: usize| match i {
            0 => "the start".to_string(),
            i if i + 1 == n => "the destination".into(),
            i => format!("stop {}", i + 1),
        };
        let mut out = Vec::new();
        for (i, s) in trip.snaps.iter().enumerate() {
            match s {
                None => out.push(capital(&format!(
                    "{} has no road within {}.",
                    who(i),
                    distance_text(SNAP_MAX_KM * 1000.0, units)
                ))),
                Some(s) if s.dist_m > FAR_SNAP_M => out.push(capital(&format!(
                    "{} is {} from the nearest road.",
                    who(i),
                    distance_text(s.dist_m, units)
                ))),
                _ => {}
            }
        }
        for (i, l) in trip.legs.iter().enumerate() {
            let snapped = trip.snaps[i].is_some() && trip.snaps[i + 1].is_some();
            if l.is_none() && snapped {
                out.push(format!("No road goes from {} to {}.", who(i), who(i + 1)));
            }
        }
        let broken = trip.broken();
        if broken & TOLL != 0 {
            out.push("This route uses a toll road: no route avoids them.".into());
        }
        if broken & FERRY != 0 {
            out.push("This route uses a ferry: no route avoids them.".into());
        }
        let last = trip.legs.len().saturating_sub(1);
        for (li, (leg, runs)) in trip.legs.iter().zip(&r.restricted).enumerate() {
            let Some(leg) = leg else { continue };
            for run in runs {
                let m: f64 = leg.pieces[run.clone()].iter().map(|p| p.metres).sum();
                let d = distance_text(m, units);
                let first = leg.pieces[..run.start].iter().all(|p| p.metres <= 0.0);
                let end = leg.pieces[run.end..].iter().all(|p| p.metres <= 0.0);
                let what = "on a road with restricted access (private or destination only).";
                out.push(match (first, end) {
                    (true, _) if li == 0 => format!("The first {d} is {what}"),
                    (_, true) if li == last => format!("The last {d} is {what}"),
                    (true, _) => format!("{d} after {} is {what}", who(li)),
                    (_, true) => format!("{d} before {} is {what}", who(li + 1)),
                    _ => format!("{d} of the route is {what}"),
                });
            }
        }
        out
    }

    /// Asks for the route through the stops, or drops it when there is no start or no
    /// destination.
    pub fn reroute(&mut self) {
        self.optimizing = None;
        self.reordered = None;
        let (Some(from), Some(to)) = (&self.from, &self.to) else {
            self.worker.cancel();
            self.waiting = None;
            self.route = None;
            self.error = None;
            self.overlays.clear();
            self.bands = Default::default();
            self.picked = None;
            return;
        };
        let stops = std::iter::once(from)
            .chain(&self.via)
            .chain(std::iter::once(to))
            .map(Place::stop)
            .collect();
        self.next_id += 1;
        self.waiting = Some(self.next_id);
        self.worker.ask(Request {
            id: self.next_id,
            stops,
            avoid: self.avoid,
            asked: Instant::now(),
            optimize: None,
        });
    }

    /// Takes the worker's answers to the latest request, dropping the others. Returns the
    /// view to fly to when a new route is not already in view.
    pub fn take(&mut self, cam: &Camera) -> Option<(camera::Vec2, f64)> {
        let mut fit = None;
        let answers: Vec<_> = self.worker.poll().collect();
        for a in answers {
            if self.waiting != Some(a.request.id) {
                self.dropped += 1;
                continue;
            }
            self.waiting = None;
            if let Some(order) = &a.order {
                let Some((before, sent)) = self.optimizing.take() else {
                    continue;
                };
                match order {
                    Ok(order) => self.put_order(before, &sent, order, &a.routed),
                    // The route shown stays.
                    Err(e) => {
                        self.reordered = Some(Reordered::Failed(e.clone()));
                        continue;
                    }
                }
            }
            self.picked = None;
            self.rows = None;
            self.overlays.clear();
            match a.routed {
                Ok(routed) => {
                    let line = &routed.line;
                    self.last = Some(Taken {
                        latency: a.request.asked.elapsed(),
                        optimize: a.optimize,
                        trip: a.trip,
                        directions: a.directions,
                        line: a.line,
                        points: line.points,
                    });
                    self.routed_stops = a.request.stops.iter().map(|s| s.p).collect();
                    self.overlays = overlay_lines(&routed, &self.routed_stops, None);
                    // The stops off the road too, at the ends of their connectors.
                    let bounds =
                        fit_bounds(std::iter::once(line).chain(self.overlays.iter().map(|o| &o.1)));
                    fit = (!line.is_empty() && !in_view(cam, bounds))
                        .then(|| view_for(bounds, cam.size));
                    self.route = Some(routed);
                    self.error = None;
                }
                Err(e) => {
                    self.route = None;
                    self.error = Some(e);
                }
            }
            self.bands = Default::default();
        }
        fit
    }

    /// Puts the stops in the order an optimize request found, and says what it saved.
    fn put_order(
        &mut self,
        before: Before,
        sent: &[Place],
        order: &[usize],
        routed: &Result<Routed, String>,
    ) {
        let mut places: Vec<Place> = order.iter().map(|&i| sent[i].clone()).collect();
        if self.ends.round_trip {
            places.push(places[0].clone());
        }
        let same = places.len() == before.places.len()
            && places.iter().zip(&before.places).all(|(a, b)| a.p == b.p);
        self.reordered = Some(match (same, routed) {
            (true, _) => Reordered::Already,
            (false, Ok(r)) => Reordered::Saved {
                time_s: before.time_s - r.trip.time_s(),
                metres: before.metres - r.trip.metres(),
                before,
            },
            (false, Err(_)) => Reordered::Saved {
                time_s: 0.0,
                metres: 0.0,
                before,
            },
        });
        self.put_stops(places, true, true);
    }

    /// The lines to draw on `cam`'s view, each made again for a new band.
    pub fn layout(
        &mut self,
        pool: &Arc<Pool>,
        cam: &Camera,
        pixels_per_point: f32,
    ) -> Option<RouteDraw> {
        let routed = self.route.as_ref()?;
        let band = band_for(cam.zoom);
        let mut draw = RouteDraw::default();
        for look in Look::ALL {
            let line = match look {
                Look::Route => Some(&routed.line),
                _ => self.overlays.iter().find(|(l, _)| *l == look).map(|o| &o.1),
            };
            let Some(line) = line.filter(|l| !l.is_empty()) else {
                continue;
            };
            let slot = &mut self.bands[look as usize];
            if slot.as_ref().is_none_or(|b| b.band != band) {
                *slot = GpuBand::upload(pool, line.band(band)).map(Arc::new);
            }
            if let Some(b) = slot.clone() {
                draw.lines.push(LineDraw::layout(
                    look,
                    b,
                    cam,
                    pixels_per_point,
                    DRAW_MARGIN,
                ));
            }
        }
        Some(draw)
    }

    /// The stops' markers: numbered pins, green at the start and red at the destination.
    pub fn draw_markers(&self, painter: &egui::Painter, origin: Pos2, cam: &Camera) {
        let stops = self.stops();
        for (i, (kind, place)) in stops.iter().enumerate().rev() {
            let s = cam.world_to_screen(geo::to_unit(place.p));
            let tip = Pos2::new(origin.x + s[0] as f32, origin.y + s[1] as f32);
            let fill = match kind {
                StopAction::From => START,
                StopAction::To => END,
                _ => VIA,
            };
            search::draw_pin(painter, tip, fill, Some(&(i + 1).to_string()));
        }
    }

    /// The overlay's lines: the readers, and the last route.
    pub fn overlay_lines(&self) -> Vec<String> {
        let mib = |b: f64| b / (1024.0 * 1024.0);
        let mut lines = vec![match self.status() {
            Status::Opening => "routing: opening…".into(),
            Status::Ready { reader, open } => {
                let h = &reader.header;
                let r = reader.reads();
                let top = match self.worker.top() {
                    Some((bytes, t)) => format!(
                        ", top {:.1} MiB read in {:.0} ms",
                        mib(bytes as f64),
                        t.as_secs_f64() * 1e3
                    ),
                    None => ", reading the top…".into(),
                };
                format!(
                    "routing: opened in {:.0} ms, {} nodes, {} segments{top}; readers hold {:.1} MiB, {:.1} MiB read, {} stale answers dropped",
                    open.as_secs_f64() * 1e3,
                    h.nodes,
                    h.segments,
                    mib(reader.heap_bytes() as f64),
                    mib((r.nodes + r.segments + r.other) as f64),
                    self.dropped
                )
            }
            Status::Missing(_) | Status::Failed(_) => {
                format!("routing: {}", self.status().why_not().unwrap_or_default())
            }
        }];
        if let (Some(t), Some(s)) = (&self.last, &self.route) {
            let trip = &s.trip;
            let band = self.bands[Look::Route as usize].as_ref();
            lines.push(format!(
                "last route: {} stops, {:.1} km, {:.0} min{}, {}trip {:.1} ms, directions {:.1} ms ({} steps), line {:.1} ms ({} points, band z{} {} segments), shown after {:.1} ms",
                trip.snaps.len(),
                trip.metres() / 1000.0,
                trip.time_s() / 60.0,
                if trip.complete() { "" } else { " (incomplete)" },
                match t.optimize.is_zero() {
                    true => String::new(),
                    false => format!("order {:.1} ms, ", t.optimize.as_secs_f64() * 1e3),
                },
                t.trip.as_secs_f64() * 1e3,
                t.directions.as_secs_f64() * 1e3,
                s.steps.len(),
                t.line.as_secs_f64() * 1e3,
                t.points,
                band.map_or(0, |b| b.band),
                band.map_or(0, |b| b.pieces.last().map_or(0, |p| p.segments.end)),
                t.latency.as_secs_f64() * 1e3
            ));
        }
        if self.waiting.is_some() {
            lines.push("routing…".into());
        }
        if let Some(e) = &self.error {
            lines.push(format!("route failed: {e}"));
        }
        lines
    }
}

/// `s` with its first letter in capitals.
fn capital(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map_or_else(String::new, |f| f.to_uppercase().chain(c).collect())
}

/// The bounds (world) of the lines that have points.
pub fn fit_bounds<'a>(lines: impl IntoIterator<Item = &'a Line>) -> [f64; 4] {
    lines.into_iter().filter(|l| !l.is_empty()).fold(
        [f64::MAX, f64::MAX, f64::MIN, f64::MIN],
        |b, l| {
            let c = l.bounds;
            [
                b[0].min(c[0]),
                b[1].min(c[1]),
                b[2].max(c[2]),
                b[3].max(c[3]),
            ]
        },
    )
}

/// `bounds` (world) are on screen.
fn in_view(cam: &Camera, bounds: [f64; 4]) -> bool {
    let [x0, y0, x1, y1] = cam.visible();
    bounds[0] >= x0 && bounds[1] >= y0 && bounds[2] <= x1 && bounds[3] <= y1
}

/// The view that fits `bounds` (world) into [`FIT_SHARE`] of a map of `size` points.
pub fn view_for(bounds: [f64; 4], size: camera::Vec2) -> (camera::Vec2, f64) {
    let [x0, y0, x1, y1] = bounds;
    let fit =
        |extent: f64, px: f64| (FIT_SHARE * px.max(100.0) / (extent.max(1e-12) * TILE_PX)).log2();
    let zoom = fit(x1 - x0, size[0]).min(fit(y1 - y0, size[1]));
    (
        [(x0 + x1) / 2.0, (y0 + y1) / 2.0],
        zoom.clamp(camera::MIN_ZOOM, FIT_MAX_ZOOM),
    )
}

#[cfg(test)]
mod tests;
