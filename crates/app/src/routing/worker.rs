//! The routing worker: one thread over [`omp_core::route`], with no egui.
//!
//! It opens the pack's routing readers when the pack opens, then reads the top of the
//! hierarchy on a thread of its own, so the system's file cache holds it before the first
//! long route (step 3.3: about a second from a USB 2 stick, which saves most of a cold
//! route's reads). A new request replaces the one waiting and stops the one running within
//! milliseconds ([`route::Routing::trip_until`]); the UI drops answers to anything but its
//! latest request, as search does. The worker also finds where each point of the line
//! would be dropped ([`Line`]), so the UI thread only filters it, and makes the directions
//! (step 3.7) with the pack's street table.

use std::io;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use omp_core::pack::Pack;
use omp_core::route::{self, Names, Step, Stop, Trip};
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
    /// line.
    pub fn new(
        reader: &route::Routing,
        streets: Option<&Streets>,
        stops: &[Stop],
        trip: Trip,
        line: Line,
    ) -> io::Result<Self> {
        let names = streets.map(|s| s as &dyn Names);
        let steps = reader.directions(&trip, stops, names)?;
        let restricted = trip
            .legs
            .iter()
            .map(|l| l.as_ref().map_or(Ok(Vec::new()), |l| reader.restricted(l)))
            .collect::<io::Result<_>>()?;
        let roads = trip
            .snaps
            .iter()
            .map(|s| {
                s.as_ref()
                    .map_or(Ok(String::new()), |s| road(reader, names, s.segment))
            })
            .collect::<io::Result<_>>()?;
        Ok(Self {
            trip,
            line,
            steps,
            restricted,
            roads,
        })
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

pub struct Answer {
    pub request: Request,
    pub routed: Result<Routed, String>,
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
    rx: Receiver<Answer>,
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
    pub fn stub() -> (Self, Sender<Answer>) {
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

    pub fn poll(&self) -> impl Iterator<Item = Answer> + '_ {
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
    tx: &Sender<Answer>,
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
    tx: &Sender<Answer>,
    wake: &dyn Fn(),
) {
    loop {
        let (request, generation) = {
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
            Routed::new(reader, streets, &request.stops, trip, line).map_err(|e| e.to_string())
        });
        let directions = t.elapsed();
        let answer = Answer {
            request,
            routed,
            trip: trip_time,
            directions,
            line,
        };
        if tx.send(answer).is_err() {
            return;
        }
        wake();
    }
}
