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
//! same way. The worker keeps the last route it sent, and finds the alternatives to its
//! legs when asked (step 4.3: when a leg's routes are opened in the route page), with
//! their own directions, sending each leg's as it is done; a newer request stops that too.
//! It also reads the pack's time zones (step 4.5), which the schedule uses on the UI
//! thread.

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
use omp_core::tz::Zones;

use super::line::Line;

/// Where the worker is.
#[derive(Clone)]
pub enum Status {
    Opening,
    Ready {
        reader: Arc<route::Routing>,
        open: Duration,
        /// The pack's time zones (format 0.6), for the schedule (phase 4 step 4.5).
        zones: Option<Arc<Zones>>,
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

/// The alternatives to one leg of a route sent before; `None` for a leg with no route.
pub struct Alternatives {
    /// The route's request.
    pub id: u64,
    pub leg: usize,
    pub choices: Result<Option<Choices>, String>,
    /// Time looking for them, and making their directions and lines.
    pub search: Duration,
    pub directions: Duration,
}

/// What the worker is asked to do.
#[derive(Clone, Debug)]
pub enum Job {
    Route(Request),
    /// The alternatives to these legs of route `id`, the last sent.
    Alternatives {
        id: u64,
        legs: Vec<usize>,
    },
}

/// What the worker sends: routes, and alternatives to their legs. A few a request: not
/// worth boxing the route.
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
    request: Option<Job>,
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

    /// The route request waiting, taken; an alternatives job is left.
    #[cfg(test)]
    pub fn take_request(&self) -> Option<Request> {
        let mut slot = self.shared.slot.lock().unwrap();
        match slot.request.take() {
            Some(Job::Route(r)) => Some(r),
            other => {
                slot.request = other;
                None
            }
        }
    }

    /// The alternatives job waiting, taken: the route's request and the legs.
    #[cfg(test)]
    pub fn take_alternatives(&self) -> Option<(u64, Vec<usize>)> {
        let mut slot = self.shared.slot.lock().unwrap();
        match slot.request.take() {
            Some(Job::Alternatives { id, legs }) => Some((id, legs)),
            other => {
                slot.request = other;
                None
            }
        }
    }

    /// Replaces the job waiting, if any, and stops the one running.
    pub fn ask(&self, job: Job) {
        let mut slot = self.shared.slot.lock().unwrap();
        self.shared.generation.fetch_add(1, Ordering::Relaxed);
        slot.request = Some(job);
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
        let zones = Zones::open(&p)?;
        route::Routing::open(p).map(|r| (r, streets, zones, minor))
    });
    let (reader, streets, zones) = match opened {
        Ok((Some(r), streets, zones, _)) => (Arc::new(r), streets, zones.map(Arc::new)),
        Ok((None, _, _, minor)) => {
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
        zones,
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

/// The last route sent, kept for its legs' alternatives.
struct Kept {
    id: u64,
    stops: Vec<Stop>,
    trip: Trip,
    steps: Vec<Step>,
    restricted: Vec<Vec<Range<usize>>>,
    roads: Vec<Vec<Road>>,
}

/// Answers requests until the worker is dropped.
fn serve(
    shared: &Arc<Shared>,
    reader: &route::Routing,
    streets: Option<&Streets>,
    tx: &Sender<Reply>,
    wake: &dyn Fn(),
) {
    let mut kept: Option<Kept> = None;
    loop {
        let (job, generation) = {
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
        let sent = match job {
            Job::Route(request) => route(reader, streets, request, &stop, &mut kept, tx),
            Job::Alternatives { id, legs } => match kept.as_ref().filter(|k| k.id == id) {
                Some(k) => alternatives(reader, streets, k, &legs, &stop, tx, wake),
                // Not the last route: the UI has a newer one coming.
                None => Ok(()),
            },
        };
        if sent.is_err() {
            return;
        }
        wake();
    }
}

/// Routes `request` and sends it; keeps it in `kept`. `Err` once the UI is gone.
fn route(
    reader: &route::Routing,
    streets: Option<&Streets>,
    mut request: Request,
    stop: &dyn Fn() -> bool,
    kept: &mut Option<Kept>,
    tx: &Sender<Reply>,
) -> Result<(), ()> {
    let t = Instant::now();
    let mut order = None;
    if let Some(ends) = request.optimize {
        match reader.optimize_until(&request.stops, request.avoid, ends, stop) {
            Ok(None) => return Ok(()),
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
                return tx.send(answer.into()).map_err(drop);
            }
        }
    }
    let optimize = t.elapsed();
    let t = Instant::now();
    let trip = match reader.trip_until(&request.stops, request.avoid, stop) {
        // Stopped by a newer request, which is waiting.
        Ok(None) => return Ok(()),
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
        Routed::with_roads(reader, streets, &request.stops, trip, line, stop)
            .map_err(|e| e.to_string())
    });
    let directions = t.elapsed();
    let routed = match routed {
        // Stopped by a newer request, which is waiting.
        Ok(None) => return Ok(()),
        Ok(Some((r, roads))) => {
            *kept = Some(Kept {
                id: request.id,
                stops: request.stops.clone(),
                trip: r.trip.clone(),
                steps: r.steps.clone(),
                restricted: r.restricted.clone(),
                roads,
            });
            Ok(r)
        }
        Err(e) => Err(e),
    };
    let answer = Answer {
        request,
        routed,
        order,
        optimize,
        trip: trip_time,
        directions,
        line,
    };
    tx.send(answer.into()).map_err(drop)
}

/// Finds the alternatives to `legs` of the route `k`, sending each leg's as it is done.
/// `Err` once the UI is gone.
fn alternatives(
    reader: &route::Routing,
    streets: Option<&Streets>,
    k: &Kept,
    legs: &[usize],
    stop: &dyn Fn() -> bool,
    tx: &Sender<Reply>,
    wake: &dyn Fn(),
) -> Result<(), ()> {
    let names = streets.map(|s| s as &dyn Names);
    let quickest = Quickest {
        trip: &k.trip,
        steps: &k.steps,
        restricted: &k.restricted,
        roads: &k.roads,
    };
    for &leg in legs.iter().filter(|&&l| l < k.trip.legs.len()) {
        let mut times = [Duration::ZERO; 2];
        let choices = match leg_choices(reader, names, &k.stops, &quickest, leg, stop, &mut times) {
            // Stopped: the UI asks again for what it still wants.
            Ok(None) => return Ok(()),
            Ok(Some(c)) => Ok(c),
            Err(e) => Err(e.to_string()),
        };
        let alts = Alternatives {
            id: k.id,
            leg,
            choices,
            search: times[0],
            directions: times[1],
        };
        tx.send(Reply::Alternatives(alts)).map_err(drop)?;
        wake();
    }
    Ok(())
}

/// A trip as routed, with what [`Routed::with_roads`] made of it.
pub struct Quickest<'a> {
    pub trip: &'a Trip,
    pub steps: &'a [Step],
    pub restricted: &'a [Vec<Range<usize>>],
    /// Per leg, the road of each piece.
    pub roads: &'a [Vec<Road>],
}

/// Each routed leg's routes ([`leg_choices`]); `None` once `stop` returns true.
pub fn choices(
    reader: &route::Routing,
    names: Option<&dyn Names>,
    stops: &[Stop],
    quickest: &Quickest,
    stop: &dyn Fn() -> bool,
    times: &mut [Duration; 2],
) -> io::Result<Option<Vec<Option<Choices>>>> {
    let mut out = Vec::with_capacity(quickest.trip.legs.len());
    for li in 0..quickest.trip.legs.len() {
        match leg_choices(reader, names, stops, quickest, li, stop, times)? {
            Some(c) => out.push(c),
            None => return Ok(None),
        }
    }
    Ok(Some(out))
}

/// Leg `li`'s routes: its quickest (with the trip's own directions) and its alternatives,
/// each with its directions, line and tag; `Some(None)` for a leg with no route. `None`
/// once `stop` returns true. `times` gets the time in the search and in the rest.
#[allow(clippy::too_many_arguments)]
pub fn leg_choices(
    reader: &route::Routing,
    names: Option<&dyn Names>,
    stops: &[Stop],
    quickest: &Quickest,
    li: usize,
    stop: &dyn Fn() -> bool,
    times: &mut [Duration; 2],
) -> io::Result<Option<Option<Choices>>> {
    let Quickest {
        trip,
        steps,
        restricted,
        roads,
    } = *quickest;
    let params = AltParams::default();
    let (Some(leg), Some(a), Some(b)) = (&trip.legs[li], &trip.snaps[li], &trip.snaps[li + 1])
    else {
        return Ok(Some(None));
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
        // From the roads its directions found: walking a long leg again would decode its
        // geometry again, as it does not stay in the segment cache.
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
    Ok(Some(Some(Choices { routes, best })))
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
