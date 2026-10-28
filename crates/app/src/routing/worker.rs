//! The routing worker: one thread over [`omp_core::route`], with no egui.
//!
//! It opens the pack's routing readers when the pack opens, then reads the top of the
//! hierarchy on a thread of its own, so the system's file cache holds it before the first
//! long route (step 3.3: about a second from a USB 2 stick, which saves most of a cold
//! route's reads). A new request replaces the one waiting and stops the one running within
//! milliseconds ([`route::Routing::trip_until`]); the UI drops answers to anything but its
//! latest request, as search does. The worker also finds where each point of the line
//! would be dropped ([`Line`]), so the UI thread only filters it, and makes the directions
//! (step 3.7) with the pack's street table, which a newer request stops too. A request may ask for the stops' quickest
//! order first (phase 4 step 4.1): the trip is then routed in that order, and stopped the
//! same way. Once the route is sent, the worker looks for each leg's alternatives (step
//! 4.3) with their own directions, and sends them after it; a newer request stops that too.

use std::io;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use omp_core::geo::P;
use omp_core::pack::Pack;
use omp_core::route::{self, AltParams, Ends, Leg, Names, Road, Step, Stop, Trip, Turns};
use omp_core::street::Streets;

use super::line::Line;

/// Where the worker is.
#[derive(Clone)]
pub enum Status {
    Opening,
    Ready {
        reader: Arc<route::Routing>,
        open: Duration,
    },
    /// No routing sections; the pack's minor version.
    Missing(u16),
    Failed(String),
}

impl Status {
    /// Requests can be made: routing is ready or soon will be.
    pub fn usable(&self) -> bool {
        matches!(self, Status::Opening | Status::Ready { .. })
    }

    /// Why routing cannot be used, for a disabled menu item's tip.
    pub fn why_not(&self) -> Option<String> {
        match self {
            Status::Missing(minor) if *minor < route::MIN_MINOR => Some(format!(
                "This map was built before routing was added (format 0.{minor}). Build it \
                 again with a newer omp-builder to plan routes."
            )),
            Status::Missing(_) => Some("This map was built without routing.".into()),
            Status::Failed(e) => Some(format!("Routing could not be opened: {e}")),
            _ => None,
        }
    }
}

/// A route, as sent to the worker.
#[derive(Clone, Debug)]
pub struct Request {
    pub id: u64,
    pub stops: Vec<Stop>,
    pub avoid: u8,
    pub asked: Instant,
    /// Reorder the stops for the quickest trip first, keeping these ends.
    pub optimize: Option<Ends>,
}

/// A trip and what the UI shows of it.
pub struct Routed {
    pub trip: Trip,
    pub line: Line,
    pub steps: Vec<Step>,
    /// Per leg, the runs of its pieces on restricted roads.
    pub restricted: Vec<Vec<Range<usize>>>,
    /// Per stop, the name of the road it snapped to, or empty.
    pub roads: Vec<String>,
}

impl Routed {
    /// Directions, restricted runs and road names for `trip` through `stops`, with its
    /// line; and per leg the road of each piece, for its alternatives. `None` once `stop`
    /// returns true.
    pub fn with_roads(
        reader: &route::Routing,
        streets: Option<&Streets>,
        stops: &[Stop],
        trip: Trip,
        line: Line,
        stop: &dyn Fn() -> bool,
    ) -> io::Result<Option<(Self, Vec<Vec<Road>>)>> {
        let names = streets.map(|s| s as &dyn Names);
        let Some((steps, pieces)) = reader.directions_roads_until(&trip, stops, names, stop)?
        else {
            return Ok(None);
        };
        let restricted = trip
            .legs
            .iter()
            .map(|l| l.as_ref().map_or(Vec::new(), Leg::restricted))
            .collect();
        let roads = trip
            .snaps
            .iter()
            .map(|s| {
                s.as_ref()
                    .map_or(Ok(String::new()), |s| road(reader, names, s.segment))
            })
            .collect::<io::Result<_>>()?;
        let routed = Self {
            trip,
            line,
            steps,
            restricted,
            roads,
        };
        Ok(Some((routed, pieces)))
    }
}

/// The name of segment `s`'s road: its street's, else its label's.
fn road(reader: &route::Routing, names: Option<&dyn Names>, s: u32) -> io::Result<String> {
    let (street, label) = reader.with_segment(s, |seg| (seg.street, seg.label))?;
    if let (Some(id), Some(n)) = (street, names) {
        return Ok(n.street_road(id)?.text());
    }
    match label {
        Some(l) => Ok(reader.label(l)?.name),
        None => Ok(String::new()),
    }
}

/// One of a leg's routes, as the route page lists it and the map draws it.
#[derive(Clone)]
pub struct Route {
    pub leg: Leg,
    pub turns: Turns,
    /// Where it passes its via node (for a saved trip, step 4.7); `None` for the quickest.
    #[allow(dead_code)]
    pub via: Option<P>,
    /// The road most of it takes that the leg's other routes do not, by its main ref
    /// ("I-40") or its name, or empty.
    pub road: String,
    /// The leg's directions along it, `at_m` and `at_s` from the leg's start.
    pub steps: Vec<Step>,
    /// The runs of its pieces on restricted roads.
    pub restricted: Vec<Range<usize>>,
    pub line: Line,
    /// Where its tag goes on the map: the middle of its longest stretch on none of the
    /// leg's other routes.
    pub tag: Option<P>,
}

/// A leg's routes: the quickest first, then the alternatives, quickest first.
#[derive(Clone)]
pub struct Choices {
    pub routes: Vec<Route>,
    /// The one ranked best with its turns' cost (phase 4 decision 3).
    pub best: usize,
}

/// The alternatives to a route sent before, per leg; `None` for a leg with no route.
pub struct Alternatives {
    pub id: u64,
    pub legs: Result<Vec<Option<Choices>>, String>,
    /// Time looking for them, and making their directions and lines.
    pub search: Duration,
    pub directions: Duration,
}

/// What the worker sends: a route, then its alternatives. A few a request: not worth
/// boxing the route.
#[allow(clippy::large_enum_variant)]
pub enum Reply {
    Route(Answer),
    Alternatives(Alternatives),
}

impl From<Answer> for Reply {
    fn from(a: Answer) -> Self {
        Reply::Route(a)
    }
}

pub struct Answer {
    /// What was asked; for an optimize request, with its stops in the new order (and back
    /// to the first for a round trip), as the trip was routed.
    pub request: Request,
    pub routed: Result<Routed, String>,
    /// For an optimize request: the new order, as indices of the stops asked, or why
    /// there is none (the trip is then not routed).
    pub order: Option<Result<Vec<usize>, String>>,
    /// Time in the matrix and the order.
    pub optimize: Duration,
    /// Time in `trip`, in the directions, and in finding the line's tolerances.
    pub trip: Duration,
    pub directions: Duration,
    pub line: Duration,
}

struct Slot {
    request: Option<Request>,
    quit: bool,
    status: Status,
    /// Bytes of the hierarchy's top read and how long it took, once done.
    top: Option<(u64, Duration)>,
}

struct Shared {
    slot: Mutex<Slot>,
    ready: Condvar,
    /// Counts the requests made (and the quit): a trip stops once it is not its request's.
    generation: AtomicU64,
}

pub struct Worker {
    shared: Arc<Shared>,
    rx: Receiver<Reply>,
}

impl Worker {
    /// Opens the pack's routing readers on a new thread, then answers requests. `wake` is
    /// called when the readers are open and after each answer.
    pub fn start(first_chunk: PathBuf, wake: impl Fn() + Send + Sync + 'static) -> Self {
        let shared = Arc::new(Shared {
            slot: Mutex::new(Slot {
                request: None,
                quit: false,
                status: Status::Opening,
                top: None,
            }),
            ready: Condvar::new(),
            generation: AtomicU64::new(0),
        });
        let (tx, rx) = channel();
        let s = shared.clone();
        std::thread::Builder::new()
            .name("routing".into())
            .spawn(move || work(&s, first_chunk, &tx, Arc::new(wake)))
            .expect("spawn routing worker");
        Self { shared, rx }
    }

    /// A worker with no thread: the test takes the requests and sends the answers.
    #[cfg(test)]
    pub fn stub() -> (Self, Sender<Reply>) {
        let shared = Arc::new(Shared {
            slot: Mutex::new(Slot {
                request: None,
                quit: false,
                status: Status::Opening,
                top: None,
            }),
            ready: Condvar::new(),
            generation: AtomicU64::new(0),
        });
        let (tx, rx) = channel();
        (Self { shared, rx }, tx)
    }

    #[cfg(test)]
    pub fn take_request(&self) -> Option<Request> {
        self.shared.slot.lock().unwrap().request.take()
    }

    /// Replaces the request waiting, if any, and stops the one running.
    pub fn ask(&self, r: Request) {
        let mut slot = self.shared.slot.lock().unwrap();
        self.shared.generation.fetch_add(1, Ordering::Relaxed);
        slot.request = Some(r);
        drop(slot);
        self.shared.ready.notify_one();
    }

    /// Stops the request running, and drops the one waiting.
    pub fn cancel(&self) {
        let mut slot = self.shared.slot.lock().unwrap();
        self.shared.generation.fetch_add(1, Ordering::Relaxed);
        slot.request = None;
    }

    pub fn status(&self) -> Status {
        self.shared.slot.lock().unwrap().status.clone()
    }

    pub fn top(&self) -> Option<(u64, Duration)> {
        self.shared.slot.lock().unwrap().top
    }

    pub fn poll(&self) -> impl Iterator<Item = Reply> + '_ {
        self.rx.try_iter()
    }
}

impl Drop for Worker {
    /// Stops the worker without waiting for it: opening from a slow stick may take a while,
    /// and the thread holds its own handle on the pack.
    fn drop(&mut self) {
        self.shared.slot.lock().unwrap().quit = true;
        self.shared.generation.fetch_add(1, Ordering::Relaxed);
        self.shared.ready.notify_all();
    }
}

/// Opens the readers, starts reading the top, then serves.
fn work(
    shared: &Arc<Shared>,
    first_chunk: PathBuf,
    tx: &Sender<Reply>,
    wake: Arc<dyn Fn() + Send + Sync>,
) {
    let t0 = Instant::now();
    let opened = Pack::open(&first_chunk).map(Arc::new).and_then(|p| {
        let minor = p.minor;
        let streets = Streets::open(p.clone())?;
        route::Routing::open(p).map(|r| (r, streets, minor))
    });
    let (reader, streets) = match opened {
        Ok((Some(r), streets, _)) => (Arc::new(r), streets),
        Ok((None, _, minor)) => {
            shared.slot.lock().unwrap().status = Status::Missing(minor);
            wake();
            return;
        }
        Err(e) => {
            shared.slot.lock().unwrap().status = Status::Failed(e.to_string());
            wake();
            return;
        }
    };
    shared.slot.lock().unwrap().status = Status::Ready {
        reader: reader.clone(),
        open: t0.elapsed(),
    };
    wake();
    let (r, s, w) = (reader.clone(), shared.clone(), wake.clone());
    std::thread::Builder::new()
        .name("routing top".into())
        .spawn(move || {
            let t = Instant::now();
            // A failed read only loses the head start; routes report their own errors.
            if let Ok(bytes) = r.warm_top() {
                s.slot.lock().unwrap().top = Some((bytes, t.elapsed()));
                w();
            }
        })
        .expect("spawn routing top");
    serve(shared, &reader, streets.as_ref(), tx, &*wake);
}

/// Answers requests until the worker is dropped.
fn serve(
    shared: &Arc<Shared>,
    reader: &route::Routing,
    streets: Option<&Streets>,
    tx: &Sender<Reply>,
    wake: &dyn Fn(),
) {
    loop {
        let (mut request, generation) = {
            let mut slot = shared.slot.lock().unwrap();
            loop {
                if slot.quit {
                    return;
                }
                if let Some(r) = slot.request.take() {
                    break (r, shared.generation.load(Ordering::Relaxed));
                }
                slot = shared.ready.wait(slot).unwrap();
            }
        };
        let stop = || shared.generation.load(Ordering::Relaxed) != generation;
        let t = Instant::now();
        let mut order = None;
        if let Some(ends) = request.optimize {
            match reader.optimize_until(&request.stops, request.avoid, ends, &stop) {
                Ok(None) => continue,
                Ok(Some(o)) => {
                    let mut stops: Vec<Stop> = o.order.iter().map(|&i| request.stops[i]).collect();
                    if ends.round_trip {
                        stops.push(stops[0]);
                    }
                    request.stops = stops;
                    order = Some(Ok(o.order));
                }
                Err(e) => {
                    let e = e.to_string();
                    let answer = Answer {
                        request,
                        routed: Err(e.clone()),
                        order: Some(Err(e)),
                        optimize: t.elapsed(),
                        trip: Duration::ZERO,
                        directions: Duration::ZERO,
                        line: Duration::ZERO,
                    };
                    if tx.send(answer.into()).is_err() {
                        return;
                    }
                    wake();
                    continue;
                }
            }
        }
        let optimize = t.elapsed();
        let t = Instant::now();
        let trip = match reader.trip_until(&request.stops, request.avoid, &stop) {
            // Stopped by a newer request, which is waiting.
            Ok(None) => continue,
            Ok(Some(trip)) => Ok(trip),
            Err(e) => Err(e.to_string()),
        };
        let trip_time = t.elapsed();
        let t = Instant::now();
        let trip = trip.map(|trip| {
            let line = Line::of_trip(&trip);
            (trip, line)
        });
        let line = t.elapsed();
        let t = Instant::now();
        let routed = trip.and_then(|(trip, line)| {
            Routed::with_roads(reader, streets, &request.stops, trip, line, &stop)
                .map_err(|e| e.to_string())
        });
        let directions = t.elapsed();
        // What the alternatives start from, kept before the route goes.
        let (routed, base) = match routed {
            // Stopped by a newer request, which is waiting.
            Ok(None) => continue,
            Ok(Some((r, roads))) => {
                let any = r.trip.legs.iter().any(Option::is_some);
                let base = any.then(|| {
                    let (trip, steps) = (r.trip.clone(), r.steps.clone());
                    (trip, steps, r.restricted.clone(), roads)
                });
                (Ok(r), base)
            }
            Err(e) => (Err(e), None),
        };
        let (id, stops) = (request.id, request.stops.clone());
        let answer = Answer {
            request,
            routed,
            order,
            optimize,
            trip: trip_time,
            directions,
            line,
        };
        if tx.send(answer.into()).is_err() {
            return;
        }
        wake();
        let Some((trip, steps, restricted, roads)) = base else {
            continue;
        };
        let names = streets.map(|s| s as &dyn Names);
        let mut times = [Duration::ZERO; 2];
        let quickest = Quickest {
            trip: &trip,
            steps: &steps,
            restricted: &restricted,
            roads: &roads,
        };
        let legs = match choices(reader, names, &stops, &quickest, &stop, &mut times) {
            Ok(None) => continue,
            Ok(Some(legs)) => Ok(legs),
            Err(e) => Err(e.to_string()),
        };
        let alts = Alternatives {
            id,
            legs,
            search: times[0],
            directions: times[1],
        };
        if tx.send(Reply::Alternatives(alts)).is_err() {
            return;
        }
        wake();
    }
}

/// A trip as routed, with what [`Routed::with_roads`] made of it.
pub struct Quickest<'a> {
    pub trip: &'a Trip,
    pub steps: &'a [Step],
    pub restricted: &'a [Vec<Range<usize>>],
    /// Per leg, the road of each piece.
    pub roads: &'a [Vec<Road>],
}

/// Each routed leg's routes: its quickest (with the trip's own directions) and its
/// alternatives, each with its directions, line and tag. `None` once `stop` returns true.
/// `times` gets the time in the searches and in the rest.
pub fn choices(
    reader: &route::Routing,
    names: Option<&dyn Names>,
    stops: &[Stop],
    quickest: &Quickest,
    stop: &dyn Fn() -> bool,
    times: &mut [Duration; 2],
) -> io::Result<Option<Vec<Option<Choices>>>> {
    let Quickest {
        trip,
        steps,
        restricted,
        roads,
    } = *quickest;
    let params = AltParams::default();
    let mut out = Vec::with_capacity(trip.legs.len());
    for (li, leg) in trip.legs.iter().enumerate() {
        let (Some(leg), Some(a), Some(b)) = (leg, &trip.snaps[li], &trip.snaps[li + 1]) else {
            out.push(None);
            continue;
        };
        let t = Instant::now();
        let Some(alts) = reader.alternatives_until(a, b, leg, trip.avoid, &params, stop)? else {
            return Ok(None);
        };
        times[0] += t.elapsed();
        let t = Instant::now();
        // Quickest first, then by time; the best keeps its place.
        let mut order: Vec<usize> = (0..alts.routes.len()).collect();
        order[1..].sort_by(|&i, &j| {
            let (x, y) = (&alts.routes[i].leg, &alts.routes[j].leg);
            x.time_s.total_cmp(&y.time_s)
        });
        let legs: Vec<&Leg> = order.iter().map(|&k| &alts.routes[k].leg).collect();
        let mut routes = Vec::with_capacity(order.len());
        for (n, &k) in order.iter().enumerate() {
            let x = &alts.routes[k];
            let others: Vec<&Leg> = (0..legs.len())
                .filter(|&j| j != n)
                .map(|j| legs[j])
                .collect();
            let (mut own, runs, pieces) = match k {
                0 => (
                    steps.iter().filter(|s| s.leg == li).cloned().collect(),
                    restricted[li].clone(),
                    roads[li].clone(),
                ),
                _ => {
                    // The trip with this leg only, so its steps are numbered as the trip's.
                    let one = Trip {
                        snaps: trip.snaps.clone(),
                        legs: (0..trip.legs.len())
                            .map(|j| (j == li).then(|| x.leg.clone()))
                            .collect(),
                        avoid: trip.avoid,
                    };
                    let found = reader.directions_roads_until(&one, stops, names, stop)?;
                    let Some((s, mut r)) = found else {
                        return Ok(None);
                    };
                    (s, x.leg.restricted(), std::mem::take(&mut r[li]))
                }
            };
            if let Some(f) = own.first().map(|s| (s.at_m, s.at_s)) {
                for s in &mut own {
                    s.at_m -= f.0;
                    s.at_s -= f.1;
                }
            }
            // From the roads its directions found: walking a long leg again would decode
            // its geometry again, as it does not stay in the segment cache.
            let road =
                route::via_road_of(&x.leg, &pieces, &others).map_or(String::new(), |r| r.short());
            routes.push(Route {
                turns: x.turns,
                via: x.via,
                road,
                steps: own,
                restricted: runs,
                line: Line::new([x.leg.points.as_slice()]),
                tag: tag_point(&x.leg, &others),
                leg: x.leg.clone(),
            });
            if stop() {
                return Ok(None);
            }
        }
        let best = order.iter().position(|&k| k == alts.picked).unwrap_or(0);
        times[1] += t.elapsed();
        out.push(Some(Choices { routes, best }));
    }
    Ok(Some(out))
}

/// The middle, by metres, of the longest run of `leg`'s pieces on segments none of
/// `others` drives.
pub fn tag_point(leg: &Leg, others: &[&Leg]) -> Option<P> {
    let theirs: std::collections::HashSet<u32> = others
        .iter()
        .flat_map(|l| l.pieces.iter().map(|p| p.dir >> 1))
        .collect();
    let (mut best, mut run): ((f64, Range<usize>), (f64, usize)) = ((0.0, 0..0), (0.0, 0));
    for (i, p) in leg.pieces.iter().enumerate() {
        if theirs.contains(&(p.dir >> 1)) {
            run = (0.0, i + 1);
            continue;
        }
        run.0 += p.metres;
        if run.0 > best.0 {
            best = (run.0, run.1..i + 1);
        }
    }
    if best.1.is_empty() {
        return None;
    }
    let half = best.0 / 2.0;
    let mut m = 0.0;
    for p in &leg.pieces[best.1.clone()] {
        if m + p.metres >= half {
            let mid = (p.points.start + p.points.end - 1) / 2;
            return leg.points.get(mid).copied();
        }
        m += p.metres;
    }
    None
}
