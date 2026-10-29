//! Routing in the app (phase 3 step 3.6): stops from the map's right-click menu and from
//! search results, the route from the worker, its line drawn by the map renderer, and the
//! stops' markers.
//!
//! - [`worker`]: the thread that opens the routing readers and runs the trips; no egui.
//! - [`line`]: the route's lines, simplified per zoom band, as the renderer draws them.
//! - [`panel`]: the route page of the side panel (steps 3.7 and 3.8): the stops, the avoid
//!   options, the totals, the notices and the directions.
//! - [`times`]: the schedule's settings, its times as shown, its rows in the directions,
//!   and the towns its nights are near.
//!
//! Stops are kept in order: the start, the stops added between, and the destination. Each
//! change asks for the route again once there is a start and a destination; the answer to
//! the latest request replaces the route shown, and the view fits it unless it is already
//! in view. *Optimize stop order* (phase 4 step 4.1) asks the worker to reorder the stops
//! first; the answer puts the stops in the new order, says what it saved, and can be
//! undone until the stops change again. Over the route are drawn the parts on restricted
//! roads and the step picked in the directions, and dashed lines join each stop to where
//! the route meets its road.
//!
//! Each leg's alternatives (phase 4 step 4.3) are looked for when the leg's row in the
//! route page is opened (decision 23), and drawn grey under the route with a tag giving the
//! time against the route shown, while it stays open. Clicking a grey line or its tag, or
//! a route in the leg's list, picks it: the leg, its directions and the totals are swapped
//! for its own. Closing the row leaves only the route picked on the map. The choice holds
//! until the stops or avoid options change; a leg left open is looked at again then.
//!
//! With a time set (phase 4 step 4.5, [`times`]), the route's schedule is worked out on the
//! UI thread whenever the route, its settings or a stop's time there change, with the
//! pack's time zones from the worker. Where a day's driving ends along a leg, the map gets
//! a small disc with the next day's number, and *Make overnight stop* puts a stop there
//! that stays the night.

mod line;
pub(crate) mod panel;
mod saved;
mod times;
mod worker;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Pos2};
use omp_core::find::Hit;
use omp_core::geo::{self, P};
use omp_core::route::{Ends, FAR_SNAP_M, FERRY, Stop, TOLL};
use omp_core::schedule::Stay;

use crate::camera::{self, Camera, TILE_PX};
use crate::pool::Pool;
use crate::scale::Units;
use crate::search::{self, distance_text};

pub use line::{GpuBand, Line, LineDraw, Look, RouteDraw, RouteSegment, band_for};
pub use times::{Mode, parse_time};
use times::{Note, Plan, Timed, Towns};
pub use worker::{Choices, Quickest, choices};
use worker::{Job, Reply, Request, Worker};
pub use worker::{Routed, Status};

/// The most the view zooms in to fit a route, and the share of the map it fills.
const FIT_MAX_ZOOM: f64 = 17.0;
const FIT_SHARE: f64 = 0.8;
/// Beyond the view, in points, where pieces of the line are still drawn: its halo's width.
pub const DRAW_MARGIN: f64 = 8.0;
/// How near a click must be to a grey line, or to its tag's anchor, to pick it, in points.
const CLICK_PT: f64 = 8.0;
const TAG_HALF: [f64; 2] = [30.0, 11.0];
const TAG_FILL: Color32 = Color32::from_rgb(98, 104, 114);
/// A stop nearer its road than this gets no dashed line to it, in metres.
const CONNECTOR_M: f64 = 3.0;
/// The farthest a stop may be from a road, as [`omp_core::route`] snaps (its last radius).
const SNAP_MAX_KM: f64 = 50.0;

const START: Color32 = Color32::from_rgb(40, 150, 70);
const VIA: Color32 = Color32::from_rgb(230, 140, 20);
const END: Color32 = Color32::from_rgb(208, 52, 44);
/// Where a day's driving ends.
const NIGHT: Color32 = Color32::from_rgb(52, 62, 120);

/// A point the route goes through.
#[derive(Clone, Debug, PartialEq)]
pub struct Place {
    pub p: P,
    /// A search result's own street, to snap to first.
    pub street: Option<u32>,
    /// That street's name, which a saved trip keeps instead of its pack-specific ID (phase 4
    /// step 4.7): the routing worker finds a street of that name near the stop again.
    pub street_name: Option<String>,
    /// A search result's name; `None` for a point on the map.
    pub name: Option<String>,
    /// Time spent there, for the schedule (phase 4 step 4.5).
    pub stay: Stay,
}

impl Place {
    pub fn at(w: camera::Vec2) -> Self {
        Self {
            p: geo::from_unit(w),
            street: None,
            street_name: None,
            name: None,
            stay: Stay::default(),
        }
    }

    pub fn of_hit(h: &Hit) -> Self {
        Self {
            p: h.p,
            street: h.street,
            street_name: None,
            name: Some(h.name.clone()),
            stay: Stay::default(),
        }
    }

    fn stop(&self) -> Stop {
        Stop {
            p: self.p,
            street: self.street,
        }
    }

    /// Its street's name, for the worker to find when it has no street ID yet.
    fn name_to_find(&self) -> Option<String> {
        self.street.is_none().then(|| self.street_name.clone())?
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

/// Time to a leg's alternatives, for the overlay.
#[derive(Clone, Copy, Debug)]
pub struct AltTaken {
    pub search: Duration,
    pub directions: Duration,
    /// From the leg's row being opened to its alternatives reaching the UI.
    pub latency: Duration,
}

/// What is known of a leg's alternatives.
#[derive(Clone)]
pub enum LegAlts {
    NotAsked,
    /// Asked of the worker, since then.
    Looking(Instant),
    Found(Choices),
    Failed(String),
}

/// "+4 min", "−1 h 02 min", "same time": `s` seconds more or less.
pub fn delta_text(s: f64) -> String {
    match (s / 60.0).round() {
        0.0 => "same time".into(),
        m if m > 0.0 => format!("+{}", duration_text(s)),
        _ => format!("−{}", duration_text(-s)),
    }
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

/// Puts route `chosen[i]` of each leg's `legs[i]` in `r`, for the legs that have them:
/// its leg, directions, restricted runs and line.
pub fn put_choices(r: &mut Routed, legs: &[Option<&Choices>], chosen: &[usize]) {
    let mut steps = Vec::with_capacity(r.steps.len());
    let mut lines = Vec::new();
    let own: Vec<Line> = r
        .trip
        .legs
        .iter()
        .enumerate()
        .map(|(li, l)| match (l, legs.get(li).copied().flatten()) {
            (Some(l), None) => Line::new([l.points.as_slice()]),
            _ => Line::new(std::iter::empty()),
        })
        .collect();
    for (li, own) in own.iter().enumerate() {
        match legs.get(li).copied().flatten() {
            Some(c) => {
                let x = &c.routes[chosen[li]];
                r.trip.legs[li] = Some(x.leg.clone());
                r.restricted[li] = x.restricted.clone();
                steps.extend(x.steps.iter().cloned());
                lines.push(&x.line);
            }
            None => {
                steps.extend(r.steps.iter().filter(|s| s.leg == li).cloned());
                lines.push(own);
            }
        }
    }
    let (mut at_m, mut at_s) = (0.0, 0.0);
    for s in &mut steps {
        s.at_m = at_m;
        s.at_s = at_s;
        at_m += s.metres;
        at_s += s.time_s;
    }
    r.steps = steps;
    r.line = Line::join(lines);
}

/// The line of each leg's routes other than `chosen[i]`.
pub fn grey_line(legs: &[Option<&Choices>], chosen: &[usize]) -> Line {
    let others = legs.iter().zip(chosen).flat_map(|(c, &k)| {
        let routes = c.map_or(&[][..], |c| &c.routes[..]);
        routes
            .iter()
            .enumerate()
            .filter(move |(j, _)| *j != k)
            .map(|(_, x)| &x.line)
    });
    Line::join(others)
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
    /// Whether the *Optimize stop order* window is open.
    pub optimize_open: bool,
    /// Whether *More options* is open.
    pub more_open: bool,
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
    bands: [Option<Arc<GpuBand>>; Look::ALL.len()],
    pub last: Option<Taken>,
    /// Answers to older requests, dropped.
    pub dropped: usize,
    /// A stop was set since the side panel last looked: it shows the route page.
    pub opened: bool,
    /// The step of the directions picked: drawn over the route.
    pub picked: Option<usize>,
    /// The request the route shown answered, whose alternatives are asked for.
    shown: Option<u64>,
    /// Per leg of the route shown: its alternatives, whether its row is open (its grey
    /// lines drawn), and the route shown, in its [`Choices`].
    pub alts: Vec<LegAlts>,
    pub expanded: Vec<bool>,
    pub chosen: Vec<usize>,
    pub alt_taken: Option<AltTaken>,
    /// The directions' rows, laid out once per route and width.
    rows: Option<panel::Rows>,
    /// What the panel drew last frame, for the tests to click on.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) targets: Vec<(panel::Target, egui::Rect)>,
    /// The schedule's settings (phase 4 step 4.5).
    pub plan: Plan,
    /// Whether *Depart at / Arrive by* is open.
    pub times_open: bool,
    /// The stop whose time field is open.
    pub stay_open: Option<usize>,
    /// The route's schedule, and its rows in the directions.
    times: Times,
    notes: Vec<Note>,
    towns: Towns,
    /// The saved trip shown, once saved or opened (phase 4 step 4.7).
    pub named: Option<crate::trips::Named>,
    /// *Save trip* or *Save as…* was clicked: the app saves.
    pub save_asked: Option<crate::trips::SaveAsk>,
    /// Per leg of a trip just opened, the via point of its route saved, until its
    /// alternatives come and the one passing it is picked.
    wanted: Vec<Option<P>>,
    /// Legs whose saved route was not found again.
    lost: Vec<usize>,
}

/// What is known of the route's schedule.
enum Times {
    /// To work out again: the route or the settings changed.
    Stale,
    /// No time set, or no route.
    Off,
    Failed(String),
    Ready(Box<Timed>),
}

impl RouteUi {
    pub fn start(first_chunk: PathBuf, ctx: &egui::Context) -> Self {
        let ctx = ctx.clone();
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || ctx.request_repaint());
        let towns = Towns::new(first_chunk.clone(), wake.clone());
        Self {
            towns,
            ..Self::with_worker(Worker::start(first_chunk, move || wake()))
        }
    }

    /// A route UI on a worker with no thread, which never answers.
    #[cfg(test)]
    pub fn stub() -> Self {
        Self::with_worker(Worker::stub().0)
    }

    /// With no towns for the nights: [`Self::start`] gives them their thread.
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
            optimize_open: false,
            more_open: false,
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
            shown: None,
            alts: Vec::new(),
            expanded: Vec::new(),
            chosen: Vec::new(),
            alt_taken: None,
            rows: None,
            targets: Vec::new(),
            plan: Plan::new(Default::default()),
            times_open: false,
            stay_open: None,
            times: Times::Off,
            notes: Vec::new(),
            towns: Towns::new(PathBuf::new(), Arc::new(|| {})),
            named: None,
            save_asked: None,
            wanted: Vec::new(),
            lost: Vec::new(),
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
                self.named = None;
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
        self.optimize_blocked().is_none()
    }

    /// Why *Optimize* cannot run now, said under its button; `None` when it can.
    pub fn optimize_blocked(&self) -> Option<&'static str> {
        if self.waiting.is_some() {
            return Some("Waiting for the route…");
        }
        if !self.route.as_ref().is_some_and(|r| r.trip.complete()) {
            return Some("Needs a route to every stop.");
        }
        let places = self.to_optimize();
        let round =
            self.ends.round_trip && places.len() >= 2 && places.len() == self.places().len();
        match self.ends.free(places.len()) >= 2 || round {
            true => None,
            false => Some("Already optimal: with these ends kept, the stops have only one order."),
        }
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
        self.worker.ask(Job::Route(Request {
            id: self.next_id,
            stops: places.iter().map(Place::stop).collect(),
            avoid: self.avoid,
            asked: Instant::now(),
            optimize: Some(self.ends),
            names: places.iter().map(Place::name_to_find).collect(),
        }));
        self.optimizing = Some((before, places));
    }

    /// Stops an optimize request; the route shown stays.
    pub fn cancel_optimize(&mut self) {
        if self.optimizing.take().is_some() {
            self.worker.cancel();
            self.waiting = None;
            self.ask_alternatives();
        }
    }

    /// Puts the stops back as they were before the last *Optimize stop order*.
    pub fn undo_optimize(&mut self) {
        if let Some(Reordered::Saved { before, .. }) = self.reordered.take() {
            self.set_stops(before.places, true, true);
        }
    }

    /// Works out the schedule again once the route or its settings changed, and takes the
    /// towns found for its nights.
    pub fn refresh_times(&mut self) {
        let mut renote = self.towns.poll();
        if matches!(self.times, Times::Stale) {
            self.times = self.work_out_times();
            renote = true;
        }
        if renote {
            self.notes = match (&self.times, &self.route) {
                (Times::Ready(t), Some(r)) => times::notes(&r.steps, t, &self.towns),
                _ => Vec::new(),
            };
            let nights: Vec<P> = self
                .notes
                .iter()
                .filter(|n| n.movable)
                .map(|n| n.point)
                .collect();
            self.towns.ask(nights);
            self.rows = None;
        }
    }

    /// The pack's time zones, if it has them.
    fn zones(&self) -> Option<Arc<omp_core::tz::Zones>> {
        match self.status() {
            Status::Ready { zones, .. } => zones,
            _ => None,
        }
    }

    /// The zone at `p`: the pack's, else the computer's.
    pub fn tz_at(&self, p: P) -> jiff::tz::TimeZone {
        let zones = self.zones();
        local(zones.as_deref()).tz(p)
    }

    fn work_out_times(&self) -> Times {
        let Some(r) = &self.route else {
            return Times::Off;
        };
        let zones = self.zones();
        let local = local(zones.as_deref());
        let places = self.places();
        let refs: Vec<&Place> = places.iter().collect();
        match self.plan.settings(&refs, &self.routed_stops, &local) {
            None => Times::Off,
            Some(Err(e)) => Times::Failed(e),
            Some(Ok(st)) => match Timed::new(r, &self.routed_stops, &st, &local) {
                Some(t) => Times::Ready(Box::new(t)),
                None => Times::Failed(match r.trip.complete() {
                    true => "The driving window is too short to make progress.".into(),
                    false => "The trip cannot be timed until every stop has a route.".into(),
                }),
            },
        }
    }

    /// The schedule's settings or a stop's time changed: work it out again.
    pub fn retime(&mut self) {
        self.times = Times::Stale;
    }

    /// The schedule of the route shown, if a time is set and it could be worked out.
    pub fn timed(&self) -> Option<&Timed> {
        match &self.times {
            Times::Ready(t) => Some(t),
            _ => None,
        }
    }

    /// Why the trip has no schedule although a time is set.
    pub fn times_failed(&self) -> Option<&str> {
        match &self.times {
            Times::Failed(e) => Some(e),
            _ => None,
        }
    }

    /// The day that starts after night `k` of the schedule.
    fn night_day(&self, k: usize) -> u32 {
        self.timed()
            .and_then(|t| t.sched.events.get(k))
            .map_or(0, |e| e.day)
    }

    /// Turns night `k` of the schedule into a stop that stays the night, named after the
    /// town it is near.
    pub fn make_overnight(&mut self, k: usize) {
        let Some(e) = self.timed().and_then(|t| t.sched.events.get(k)).cloned() else {
            return;
        };
        let mut places = self.places();
        if e.leg + 1 > places.len() {
            return;
        }
        let name = self.towns.get(e.point).cloned().flatten();
        places.insert(
            e.leg + 1,
            Place {
                p: e.point,
                street: None,
                street_name: None,
                name,
                stay: Stay {
                    s: 0.0,
                    overnight: true,
                },
            },
        );
        let (f, t) = (self.from.is_some(), self.to.is_some());
        self.set_stops(places, f, t);
    }

    /// Sets stop `i`'s time there.
    pub fn set_stay(&mut self, i: usize, stay: Stay) {
        let mut places = self.places();
        let Some(p) = places.get_mut(i) else { return };
        if p.stay == stay {
            return;
        }
        p.stay = stay;
        let (f, t) = (self.from.is_some(), self.to.is_some());
        self.put_stops(places, f, t);
        self.retime();
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
        for &leg in &self.lost {
            out.push(format!(
                "The route saved from {} to {} was not found on this map: the quickest is shown.",
                who(leg),
                who(leg + 1)
            ));
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
        self.wanted.clear();
        self.lost.clear();
        let (Some(from), Some(to)) = (&self.from, &self.to) else {
            self.worker.cancel();
            self.waiting = None;
            self.route = None;
            self.error = None;
            self.times = Times::Stale;
            self.overlays.clear();
            self.bands = Default::default();
            self.picked = None;
            self.shown = None;
            self.alts.clear();
            self.expanded.clear();
            self.chosen.clear();
            return;
        };
        let places: Vec<&Place> = std::iter::once(from)
            .chain(&self.via)
            .chain(std::iter::once(to))
            .collect();
        self.next_id += 1;
        self.waiting = Some(self.next_id);
        self.worker.ask(Job::Route(Request {
            id: self.next_id,
            stops: places.iter().map(|p| p.stop()).collect(),
            avoid: self.avoid,
            asked: Instant::now(),
            optimize: None,
            names: places.iter().map(|p| p.name_to_find()).collect(),
        }));
    }

    /// Takes the worker's answers to the latest request, dropping the others. Returns the
    /// view to fly to when a new route is not already in view.
    pub fn take(&mut self, cam: &Camera) -> Option<(camera::Vec2, f64)> {
        let mut fit = None;
        let replies: Vec<_> = self.worker.poll().collect();
        for reply in replies {
            let a = match reply {
                Reply::Route(a) => a,
                Reply::Alternatives(x) => {
                    let asked = match self.alts.get(x.leg) {
                        Some(LegAlts::Looking(t)) if self.shown == Some(x.id) => *t,
                        _ => {
                            self.dropped += 1;
                            continue;
                        }
                    };
                    self.alt_taken = Some(AltTaken {
                        search: x.search,
                        directions: x.directions,
                        latency: asked.elapsed(),
                    });
                    self.alts[x.leg] = match x.choices {
                        Ok(Some(c)) => LegAlts::Found(c),
                        Ok(None) => LegAlts::Failed("this leg has no route".into()),
                        Err(e) => LegAlts::Failed(e),
                    };
                    self.refresh_grey();
                    self.pick_wanted(x.leg);
                    continue;
                }
            };
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
                    // The route shown stays, and what its legs were looking for.
                    Err(e) => {
                        self.reordered = Some(Reordered::Failed(e.clone()));
                        self.ask_alternatives();
                        continue;
                    }
                }
            }
            self.fill_streets(&a.request.stops, &a.street_names);
            self.picked = None;
            self.rows = None;
            self.times = Times::Stale;
            self.overlays.clear();
            self.alt_taken = None;
            self.shown = None;
            // Rows left open stay open, for as many legs.
            let n = a.routed.as_ref().map_or(0, |r| r.trip.legs.len());
            if self.expanded.len() != n {
                self.expanded = vec![false; n];
            }
            self.alts = vec![LegAlts::NotAsked; n];
            self.chosen = vec![0; n];
            match a.routed {
                Ok(routed) => {
                    let line = &routed.line;
                    self.shown = Some(a.request.id);
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
            for leg in 0..self.expanded.len() {
                // A saved trip's legs with a route picked are looked at at once.
                let wanted = self.wanted.get(leg).is_some_and(Option::is_some);
                if self.expanded[leg] || wanted {
                    self.look_for(leg);
                }
            }
            self.ask_alternatives();
        }
        fit
    }

    /// Takes what the worker found of the stops' streets: the ID of a street found by name,
    /// and the name of a street given by ID, for the stops as they were routed.
    fn fill_streets(&mut self, stops: &[Stop], names: &[Option<String>]) {
        let mut places = self.places();
        if places.len() != stops.len() {
            return;
        }
        for (i, (p, s)) in places.iter_mut().zip(stops).enumerate() {
            if p.p != s.p {
                return;
            }
            if p.street.is_none() {
                p.street = s.street;
            }
            if p.street_name.is_none() {
                p.street_name = names.get(i).cloned().flatten();
            }
        }
        let (f, t) = (self.from.is_some(), self.to.is_some());
        self.put_stops(places, f, t);
    }

    /// The alternatives found to the route shown, over every leg.
    pub fn found(&self) -> usize {
        self.found_legs()
            .iter()
            .flatten()
            .map(|c| c.routes.len() - 1)
            .sum()
    }

    /// Some leg's alternatives are being looked for.
    pub fn looking(&self) -> bool {
        self.alts.iter().any(|a| matches!(a, LegAlts::Looking(_)))
    }

    fn found_legs(&self) -> Vec<Option<&Choices>> {
        found_in(&self.alts)
    }

    /// Opens every leg's row (the bench's flights with `alternatives = true`).
    pub fn expand_all(&mut self) {
        for leg in 0..self.expanded.len() {
            self.expand(leg, true);
        }
    }

    /// Opens or closes leg `leg`'s row: open, its alternatives are looked for if they have
    /// not been, and drawn grey; closed, only the route picked is drawn.
    pub fn expand(&mut self, leg: usize, open: bool) {
        if leg >= self.expanded.len() || self.expanded[leg] == open {
            return;
        }
        self.expanded[leg] = open;
        if open && self.look_for(leg) {
            self.ask_alternatives();
        }
        self.refresh_grey();
    }

    /// Marks leg `leg`'s alternatives as looked for, if it has a route and they were not
    /// asked for yet. Returns whether it did.
    fn look_for(&mut self, leg: usize) -> bool {
        let routed = self
            .route
            .as_ref()
            .is_some_and(|r| r.trip.legs.get(leg).is_some_and(Option::is_some));
        if routed && matches!(self.alts.get(leg), Some(LegAlts::NotAsked)) {
            self.alts[leg] = LegAlts::Looking(Instant::now());
            return true;
        }
        false
    }

    /// Asks the worker for the alternatives of every leg looking for them. Not while a
    /// route is waiting: the job would replace it, and its answer starts again anyway.
    fn ask_alternatives(&mut self) {
        let legs: Vec<usize> = (0..self.alts.len())
            .filter(|&i| matches!(self.alts[i], LegAlts::Looking(_)))
            .collect();
        if let (Some(id), None, false) = (self.shown, self.waiting, legs.is_empty()) {
            self.worker.ask(Job::Alternatives { id, legs });
        }
    }

    /// Shows route `k` of leg `leg`'s alternatives.
    pub fn choose(&mut self, leg: usize, k: usize) {
        let ok = match self.alts.get(leg) {
            Some(LegAlts::Found(c)) => k < c.routes.len(),
            _ => false,
        };
        if ok && self.chosen[leg] != k {
            self.chosen[leg] = k;
            self.show_choices();
        }
    }

    /// Puts the chosen routes in the route shown, and the others of the open legs in grey.
    fn show_choices(&mut self) {
        let legs = found_in(&self.alts);
        let Some(r) = &mut self.route else { return };
        put_choices(r, &legs, &self.chosen);
        self.picked = None;
        self.rows = None;
        self.times = Times::Stale;
        self.overlays = overlay_lines(r, &self.routed_stops, None);
        self.bands = Default::default();
        self.refresh_grey();
    }

    /// The grey line again: the routes not shown of the open legs.
    fn refresh_grey(&mut self) {
        let legs: Vec<Option<&Choices>> = self
            .found_legs()
            .into_iter()
            .zip(&self.expanded)
            .map(|(c, &open)| c.filter(|_| open))
            .collect();
        let grey = grey_line(&legs, &self.chosen);
        self.overlays.retain(|(l, _)| *l != Look::Alternative);
        if self.route.is_some() {
            self.overlays.push((Look::Alternative, grey));
        }
        self.bands[Look::Alternative as usize] = None;
    }

    /// The open legs' routes not shown, each with its leg and place in its [`Choices`].
    fn not_shown(&self) -> impl Iterator<Item = (usize, usize, &worker::Route)> {
        self.alts.iter().enumerate().flat_map(move |(li, a)| {
            let routes = match a {
                LegAlts::Found(c) if self.expanded[li] => &c.routes[..],
                _ => &[],
            };
            let k = self.chosen.get(li).copied().unwrap_or(0);
            routes
                .iter()
                .enumerate()
                .filter(move |(j, _)| *j != k)
                .map(move |(j, x)| (li, j, x))
        })
    }

    /// A tag's text: the time against the route shown on its leg.
    fn tag_text(&self, leg: usize, x: &worker::Route) -> String {
        let shown = self
            .route
            .as_ref()
            .and_then(|r| r.trip.legs.get(leg)?.as_ref());
        delta_text(x.leg.time_s - shown.map_or(x.leg.time_s, |l| l.time_s))
    }

    /// Picks the grey line or tag at `at` (points in the map), if any. Returns whether it
    /// did.
    pub fn click(&mut self, cam: &Camera, at: camera::Vec2) -> bool {
        let w = cam.screen_to_world(at);
        let mut best: Option<(f64, usize, usize)> = None;
        for (li, k, x) in self.not_shown() {
            let on_tag = x.tag.is_some_and(|t| {
                let s = cam.world_to_screen(geo::to_unit(t));
                (s[0] - at[0]).abs() <= TAG_HALF[0] && (s[1] - at[1]).abs() <= TAG_HALF[1]
            });
            let d = match on_tag {
                true => Some(0.0),
                false => x.line.distance_pt(cam, w, CLICK_PT),
            };
            if let Some(d) = d.filter(|&d| best.is_none_or(|b| d < b.0)) {
                best = Some((d, li, k));
            }
        }
        let Some((_, li, k)) = best else { return false };
        self.choose(li, k);
        true
    }

    /// The tags of the routes not shown: the time against the route shown, at a point
    /// where each runs apart from the others.
    fn draw_tags(&self, painter: &egui::Painter, origin: Pos2, cam: &Camera) {
        for (li, _, x) in self.not_shown() {
            let Some(t) = x.tag else { continue };
            let s = cam.world_to_screen(geo::to_unit(t));
            let c = Pos2::new(origin.x + s[0] as f32, origin.y + s[1] as f32);
            if !painter.clip_rect().expand(40.0).contains(c) {
                continue;
            }
            let galley = painter.layout_no_wrap(
                self.tag_text(li, x),
                egui::FontId::proportional(12.0),
                Color32::WHITE,
            );
            let rect = egui::Rect::from_center_size(c, galley.size() + egui::vec2(10.0, 4.0));
            painter.rect(
                rect,
                4.0,
                TAG_FILL,
                egui::Stroke::new(1.0, Color32::WHITE),
                egui::StrokeKind::Outside,
            );
            painter.galley(rect.min + egui::vec2(5.0, 2.0), galley, Color32::WHITE);
        }
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

    /// The alternatives' tags, then the stops' markers: numbered pins, green at the start and red at the destination.
    pub fn draw_markers(&self, painter: &egui::Painter, origin: Pos2, cam: &Camera) {
        self.draw_tags(painter, origin, cam);
        // Where each day's driving ends along a leg: a small disc with the next day's number.
        for n in self.notes.iter().filter(|n| n.movable) {
            let s = cam.world_to_screen(geo::to_unit(n.point));
            let c = Pos2::new(origin.x + s[0] as f32, origin.y + s[1] as f32);
            if !painter.clip_rect().expand(12.0).contains(c) {
                continue;
            }
            let day = match n.kind {
                times::NoteKind::Night(k) => self.night_day(k),
                _ => continue,
            };
            painter.circle(c, 7.5, NIGHT, egui::Stroke::new(1.5, Color32::WHITE));
            painter.text(
                c,
                egui::Align2::CENTER_CENTER,
                day.to_string(),
                egui::FontId::proportional(10.0),
                Color32::WHITE,
            );
        }
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
            Status::Ready {
                reader,
                open,
                zones,
            } => {
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
                let zones = match zones {
                    Some(z) => format!(
                        "; {} time zones, {:.1} MiB",
                        z.zones().len(),
                        mib(z.heap_bytes() as f64)
                    ),
                    None => "; no time zones".into(),
                };
                format!(
                    "routing: opened in {:.0} ms, {} nodes, {} segments{top}; readers hold {:.1} MiB, {:.1} MiB read, {} stale answers dropped{zones}",
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
        if let Some(t) = &self.alt_taken {
            let found = self.found();
            lines.push(format!(
                "alternatives: {found} found; the last leg's search {:.1} ms, directions and lines {:.1} ms, shown {:.1} ms after its row was opened",
                t.search.as_secs_f64() * 1e3,
                t.directions.as_secs_f64() * 1e3,
                t.latency.as_secs_f64() * 1e3
            ));
        }
        if self.waiting.is_some() {
            lines.push("routing…".into());
        } else if self.looking() {
            lines.push("looking for alternatives…".into());
        }
        if let Some(e) = &self.error {
            lines.push(format!("route failed: {e}"));
        }
        lines
    }
}

/// Per leg, its routes once found.
/// Finds zones in `zones`, else takes the computer's.
fn local(zones: Option<&omp_core::tz::Zones>) -> omp_core::schedule::Local<'_> {
    omp_core::schedule::Local {
        zones,
        fallback: jiff::tz::TimeZone::try_system().unwrap_or(jiff::tz::TimeZone::UTC),
    }
}

fn found_in(alts: &[LegAlts]) -> Vec<Option<&Choices>> {
    alts.iter()
        .map(|a| match a {
            LegAlts::Found(c) => Some(c),
            _ => None,
        })
        .collect()
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
