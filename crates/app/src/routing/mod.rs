//! Routing in the app (phase 3 step 3.6): stops from the map's right-click menu and from
//! search results, the route from the worker, its line drawn by the map renderer, and the
//! stops' markers.
//!
//! - [`worker`]: the thread that opens the routing readers and runs the trips; no egui.
//! - [`line`]: the route's line, simplified per zoom band, as the renderer draws it.
//!
//! Stops are kept in order: the start, the stops added between, and the destination. Each
//! change asks for the route again once there is a start and a destination; the answer to
//! the latest request replaces the route shown, and the view fits it unless it is already
//! in view. The panel with the stops, the avoid options and the directions is step 3.7.

mod line;
mod worker;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Pos2};
use omp_core::find::Hit;
use omp_core::geo::{self, P};
use omp_core::route::{Stop, Trip};

use crate::camera::{self, Camera, TILE_PX};
use crate::pool::Pool;
use crate::search;

pub use line::{GpuBand, Line, RouteDraw, RouteSegment, band_for};
pub use worker::Status;
use worker::{Request, Worker};

/// The most the view zooms in to fit a route, and the share of the map it fills.
const FIT_MAX_ZOOM: f64 = 17.0;
const FIT_SHARE: f64 = 0.8;
/// Beyond the view, in points, where pieces of the line are still drawn: its halo's width.
const DRAW_MARGIN: f64 = 8.0;

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

/// The route items of a menu: "Route from here", "Route to here", "Add stop", and "Clear
/// route" once there are stops. Returns the one clicked.
pub fn menu_items(ui: &mut egui::Ui, state: &MenuState) -> Option<StopAction> {
    let mut chosen = None;
    let items = [
        ("Route from here", StopAction::From),
        ("Route to here", StopAction::To),
        ("Add stop", StopAction::Add),
    ];
    for (label, action) in items {
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

/// A route taken from the worker.
pub struct Shown {
    pub trip: Trip,
    pub line: Line,
}

/// Timings of the last answer, for the overlay.
#[derive(Clone, Copy, Debug)]
pub struct Taken {
    /// From the request to the answer reaching the UI.
    pub latency: Duration,
    pub trip: Duration,
    pub line: Duration,
    pub points: usize,
}

pub struct RouteUi {
    worker: Worker,
    pub from: Option<Place>,
    pub via: Vec<Place>,
    pub to: Option<Place>,
    /// [`omp_core::route::TOLL`] and [`omp_core::route::FERRY`].
    pub avoid: u8,
    next_id: u64,
    /// The latest request, until its answer comes.
    pub waiting: Option<u64>,
    pub route: Option<Shown>,
    pub error: Option<String>,
    /// The line at the band last drawn.
    band: Option<Arc<GpuBand>>,
    pub last: Option<Taken>,
    /// Answers to older requests, dropped.
    pub dropped: usize,
}

impl RouteUi {
    pub fn start(first_chunk: PathBuf, ctx: &egui::Context) -> Self {
        let ctx = ctx.clone();
        Self::with_worker(Worker::start(first_chunk, move || ctx.request_repaint()))
    }

    fn with_worker(worker: Worker) -> Self {
        Self {
            worker,
            from: None,
            via: Vec::new(),
            to: None,
            avoid: 0,
            next_id: 0,
            waiting: None,
            route: None,
            error: None,
            band: None,
            last: None,
            dropped: 0,
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
            has_stops: self.from.is_some() || self.to.is_some() || !self.via.is_empty(),
        }
    }

    /// The stops in order, each with its kind.
    pub fn stops(&self) -> Vec<(StopAction, &Place)> {
        let from = self.from.iter().map(|p| (StopAction::From, p));
        let via = self.via.iter().map(|p| (StopAction::Add, p));
        let to = self.to.iter().map(|p| (StopAction::To, p));
        from.chain(via).chain(to).collect()
    }

    /// Changes the stops and asks for the route again.
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
        self.reroute();
    }

    /// Asks for the route through the stops, or drops it when there is no start or no
    /// destination.
    pub fn reroute(&mut self) {
        let (Some(from), Some(to)) = (&self.from, &self.to) else {
            self.worker.cancel();
            self.waiting = None;
            self.route = None;
            self.error = None;
            self.band = None;
            return;
        };
        let stop = |p: &Place| Stop {
            p: p.p,
            street: p.street,
        };
        let stops = std::iter::once(from)
            .chain(&self.via)
            .chain(std::iter::once(to))
            .map(stop)
            .collect();
        self.next_id += 1;
        self.waiting = Some(self.next_id);
        self.worker.ask(Request {
            id: self.next_id,
            stops,
            avoid: self.avoid,
            asked: Instant::now(),
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
            match a.routed {
                Ok((trip, line)) => {
                    self.last = Some(Taken {
                        latency: a.request.asked.elapsed(),
                        trip: a.trip,
                        line: a.line,
                        points: line.points,
                    });
                    fit = (!line.is_empty() && !in_view(cam, line.bounds))
                        .then(|| view_for(line.bounds, cam.size));
                    self.route = Some(Shown { trip, line });
                    self.error = None;
                }
                Err(e) => {
                    self.route = None;
                    self.error = Some(e);
                }
            }
            self.band = None;
        }
        fit
    }

    /// The line to draw on `cam`'s view, made again for a new band.
    pub fn layout(
        &mut self,
        pool: &Arc<Pool>,
        cam: &Camera,
        pixels_per_point: f32,
    ) -> Option<RouteDraw> {
        let shown = self.route.as_ref()?;
        let band = band_for(cam.zoom);
        if self.band.as_ref().is_none_or(|b| b.band != band) {
            self.band = GpuBand::upload(pool, shown.line.band(band)).map(Arc::new);
        }
        let b = self.band.clone()?;
        Some(RouteDraw::layout(b, cam, pixels_per_point, DRAW_MARGIN))
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
            lines.push(format!(
                "last route: {} stops, {:.1} km, {:.0} min{}, trip {:.1} ms, line {:.1} ms ({} points, band z{} {} segments), shown after {:.1} ms",
                trip.snaps.len(),
                trip.metres() / 1000.0,
                trip.time_s() / 60.0,
                if trip.complete() { "" } else { " (incomplete)" },
                t.trip.as_secs_f64() * 1e3,
                t.line.as_secs_f64() * 1e3,
                t.points,
                self.band.as_ref().map_or(0, |b| b.band),
                self.band.as_ref().map_or(0, |b| b.pieces.last().map_or(0, |p| p.segments.end)),
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
