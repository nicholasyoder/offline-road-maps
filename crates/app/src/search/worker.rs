//! The search worker: one thread over [`omp_core::find`], with no egui.
//!
//! A new keystroke replaces the query waiting there, and answers to anything but the latest
//! query are dropped ([`super::session`]), as the tile loader drops tiles the view has left.
//! The worker opens the pack's search readers itself when the pack opens, so start-up does
//! not wait for them, and wakes the UI when an answer is ready; nothing polls.
//!
//! Search is progressive (2.7b): a keystroke is answered at level 0 of a
//! [`find::Session`]; when the user pauses for [`PAUSE`], the worker runs the deeper levels
//! one by one and sends each one's answer, until the last, then sleeps. A new query stops a
//! level within milliseconds (a generation number the session checks), so deep work never
//! delays the next keystroke. Enter runs every level at once for the results panel, going
//! on with the suggestions' session when it is for the same text: after a pause, its
//! levels are done and Enter is answered at once. A session's caches are kept until the
//! next query.
//!
//! The worker also answers "what's here" (2.8, [`crate::here`]): a request waiting is
//! answered before the next search level, and stops a level running, which runs again
//! after it.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use omp_core::find::{self, Hit, Request};
use omp_core::geo::{self, P};
use omp_core::here::Shown;
use omp_core::pack::Pack;
use omp_core::search::Search;

use crate::camera;
use crate::here::{HereAnswer, HereQuery};

/// How long typing pauses before the deeper search levels start, so fast typing never
/// starts them.
pub const PAUSE: Duration = Duration::from_millis(150);

/// Where the worker is.
#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    /// Reading the search sections; queries asked meanwhile wait.
    Opening,
    Ready,
    /// The pack has no search, and why.
    Missing(String),
    Failed(String),
}

/// A search, as sent to the worker.
#[derive(Clone, Debug)]
pub struct Query {
    pub id: u64,
    pub text: String,
    /// The view's centre (world coordinates) and zoom, for ranking.
    pub near: camera::Vec2,
    pub zoom: f64,
    pub limit: usize,
    /// Asked by Enter, for the results panel.
    pub submit: bool,
    pub asked: Instant,
}

pub struct Answer {
    pub query: Query,
    pub hits: Result<Vec<Hit>, String>,
    /// Time in `find`: in this level, or in every level for Enter.
    pub find: Duration,
    /// The search level answered.
    pub level: usize,
    /// Deeper levels will follow.
    pub more: bool,
}

/// What the readers hold and have read, for the overlay and the bench.
#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    /// From the pack opening to the readers being ready.
    pub open: Option<Duration>,
    pub heap_bytes: usize,
    pub bytes_read: u64,
    /// What the latest query's session caches hold, and the most any has held.
    pub session_bytes: usize,
    pub session_peak: usize,
}

struct Slot {
    query: Option<Query>,
    here: Option<HereQuery>,
    quit: bool,
    status: Status,
    stats: Stats,
}

struct Shared {
    slot: Mutex<Slot>,
    ready: Condvar,
    /// Counts the queries asked: a level stops once it is not its query's.
    generation: AtomicU64,
    /// A "what's here" request is waiting: a level stops for it.
    here_waiting: AtomicBool,
}

impl Shared {
    fn new(status: Status) -> Self {
        Self {
            slot: Mutex::new(Slot {
                query: None,
                here: None,
                quit: false,
                status,
                stats: Stats::default(),
            }),
            ready: Condvar::new(),
            generation: AtomicU64::new(0),
            here_waiting: AtomicBool::new(false),
        }
    }
}

pub struct Worker {
    shared: Arc<Shared>,
    rx: Receiver<Answer>,
    here_rx: Receiver<HereAnswer>,
}

impl Worker {
    /// Opens the pack's search readers on a new thread, then answers queries. `wake` is
    /// called when the readers are open and after each answer.
    pub fn start(first_chunk: PathBuf, wake: impl Fn() + Send + 'static) -> Self {
        Self::spawn(Status::Opening, move |s, tx, here_tx| {
            work(&s, &first_chunk, &tx, &here_tx, &wake);
        })
    }

    /// A worker thread running `f`, which starts at `status`.
    fn spawn(
        status: Status,
        f: impl FnOnce(Arc<Shared>, Sender<Answer>, Sender<HereAnswer>) + Send + 'static,
    ) -> Self {
        let shared = Arc::new(Shared::new(status));
        let (tx, rx) = channel();
        let (here_tx, here_rx) = channel();
        let s = shared.clone();
        std::thread::Builder::new()
            .name("search".into())
            .spawn(move || f(s, tx, here_tx))
            .expect("spawn search worker");
        Self {
            shared,
            rx,
            here_rx,
        }
    }

    /// A worker with no thread, ready: the test takes the queries and sends the answers.
    #[cfg(test)]
    pub(super) fn stub() -> (Self, Sender<Answer>, Sender<HereAnswer>) {
        let shared = Arc::new(Shared::new(Status::Ready));
        let (tx, rx) = channel();
        let (here_tx, here_rx) = channel();
        (
            Self {
                shared,
                rx,
                here_rx,
            },
            tx,
            here_tx,
        )
    }

    #[cfg(test)]
    pub(super) fn take_query(&self) -> Option<Query> {
        self.shared.slot.lock().unwrap().query.take()
    }

    #[cfg(test)]
    pub(super) fn take_here(&self) -> Option<HereQuery> {
        self.shared.slot.lock().unwrap().here.take()
    }

    /// Replaces the query waiting, if any, and stops the one running.
    pub fn ask(&self, q: Query) {
        let mut slot = self.shared.slot.lock().unwrap();
        self.shared.generation.fetch_add(1, Ordering::Relaxed);
        slot.query = Some(q);
        drop(slot);
        self.shared.ready.notify_one();
    }

    /// Replaces the "what's here" request waiting, if any; a search level running stops
    /// for it.
    pub fn ask_here(&self, q: HereQuery) {
        let mut slot = self.shared.slot.lock().unwrap();
        self.shared.here_waiting.store(true, Ordering::Relaxed);
        slot.here = Some(q);
        drop(slot);
        self.shared.ready.notify_one();
    }

    pub fn status(&self) -> Status {
        self.shared.slot.lock().unwrap().status.clone()
    }

    pub fn stats(&self) -> Stats {
        self.shared.slot.lock().unwrap().stats
    }

    pub fn poll(&self) -> impl Iterator<Item = Answer> + '_ {
        self.rx.try_iter()
    }

    pub fn poll_here(&self) -> impl Iterator<Item = HereAnswer> + '_ {
        self.here_rx.try_iter()
    }
}

impl Drop for Worker {
    /// Stops the worker without waiting for it: a query, or opening a large pack from a
    /// slow stick, may take a while, and the thread holds its own handle on the pack.
    fn drop(&mut self) {
        self.shared.slot.lock().unwrap().quit = true;
        self.shared.ready.notify_all();
    }
}

/// What the worker searches: the pack's [`Search`], or a stand-in in the tests.
trait Engine {
    type Levels<'a>: Levels
    where
        Self: 'a;
    fn start(&self, req: &Request) -> Self::Levels<'_>;
    fn whats_here(&self, p: P, radius_m: f64, entries_m: f64) -> io::Result<Shown>;
    fn heap_bytes(&self) -> usize;
    fn bytes_read(&self) -> u64;
}

/// One query's levels: a [`find::Session`], or a stand-in in the tests.
trait Levels {
    fn stop_when(&mut self, stop: impl Fn() -> bool + 'static);
    fn set_limit(&mut self, limit: usize);
    fn hits(&mut self) -> io::Result<Vec<Hit>>;
    fn level(&mut self, d: usize) -> io::Result<Option<Vec<Hit>>>;
    fn heap_bytes(&self) -> usize;
}

impl Engine for Search {
    type Levels<'a> = find::Session<'a>;
    fn start(&self, req: &Request) -> find::Session<'_> {
        Search::start(self, req)
    }
    fn whats_here(&self, p: P, radius_m: f64, entries_m: f64) -> io::Result<Shown> {
        Search::whats_here(self, p, radius_m, entries_m)
    }
    fn heap_bytes(&self) -> usize {
        Search::heap_bytes(self)
    }
    fn bytes_read(&self) -> u64 {
        Search::bytes_read(self)
    }
}

impl Levels for find::Session<'_> {
    fn stop_when(&mut self, stop: impl Fn() -> bool + 'static) {
        find::Session::stop_when(self, stop);
    }
    fn set_limit(&mut self, limit: usize) {
        find::Session::set_limit(self, limit);
    }
    fn hits(&mut self) -> io::Result<Vec<Hit>> {
        find::Session::hits(self)
    }
    fn level(&mut self, d: usize) -> io::Result<Option<Vec<Hit>>> {
        find::Session::level(self, d)
    }
    fn heap_bytes(&self) -> usize {
        find::Session::heap_bytes(self)
    }
}

/// What the worker does next.
enum Next {
    Here(HereQuery),
    /// A new query, or the next level of the current one; with the generation then.
    Search(Option<Query>, u64),
}

/// A session's stop check: a newer query was asked, or a "what's here" is waiting.
fn stopper(shared: &Arc<Shared>, generation: u64) -> impl Fn() -> bool + 'static {
    let s = shared.clone();
    move || {
        s.generation.load(Ordering::Relaxed) != generation || s.here_waiting.load(Ordering::Relaxed)
    }
}

/// Opens the pack's search readers, then serves.
fn work(
    shared: &Arc<Shared>,
    first_chunk: &Path,
    tx: &Sender<Answer>,
    here_tx: &Sender<HereAnswer>,
    wake: &dyn Fn(),
) {
    let t0 = Instant::now();
    let opened = Pack::open(first_chunk).map(Arc::new).and_then(|p| {
        let minor = p.minor;
        Search::open(p).map(|s| (s, minor))
    });
    let set = |status: Status, stats: Option<Stats>| {
        let mut slot = shared.slot.lock().unwrap();
        slot.status = status;
        if let Some(s) = stats {
            slot.stats = s;
        }
    };
    let search = match opened {
        Ok((Some(s), _)) => {
            let stats = Stats {
                open: Some(t0.elapsed()),
                heap_bytes: s.heap_bytes(),
                bytes_read: s.bytes_read(),
                ..Stats::default()
            };
            set(Status::Ready, Some(stats));
            s
        }
        Ok((None, minor)) => {
            let why = if minor < omp_core::search::MIN_MINOR {
                format!(
                    "This map was built before search was added (format 0.{minor}). Build it \
                     again with a newer omp-builder to search it."
                )
            } else {
                "This map was built without search.".into()
            };
            set(Status::Missing(why), None);
            wake();
            return;
        }
        Err(e) => {
            set(Status::Failed(e.to_string()), None);
            wake();
            return;
        }
    };
    wake();
    serve(shared, &search, tx, here_tx, wake);
}

/// Answers queries and "what's here" until the worker is dropped.
fn serve<E: Engine>(
    shared: &Arc<Shared>,
    search: &E,
    tx: &Sender<Answer>,
    here_tx: &Sender<HereAnswer>,
    wake: &dyn Fn(),
) {
    // The latest query, its session, the next level to run and when it may start.
    let mut current: Option<(Query, E::Levels<'_>, usize, Instant)> = None;
    loop {
        let next = {
            let mut slot = shared.slot.lock().unwrap();
            loop {
                if slot.quit {
                    return;
                }
                if let Some(h) = slot.here.take() {
                    shared.here_waiting.store(false, Ordering::Relaxed);
                    break Next::Here(h);
                }
                if let Some(q) = slot.query.take() {
                    break Next::Search(Some(q), shared.generation.load(Ordering::Relaxed));
                }
                match &current {
                    Some((_, _, next, at)) if *next < find::Session::LEVELS => {
                        let now = Instant::now();
                        if now >= *at {
                            break Next::Search(None, shared.generation.load(Ordering::Relaxed));
                        }
                        slot = shared.ready.wait_timeout(slot, *at - now).unwrap().0;
                    }
                    _ => slot = shared.ready.wait(slot).unwrap(),
                }
            }
        };
        let (new, generation) = match next {
            Next::Here(h) => {
                let t = Instant::now();
                let shown = search
                    .whats_here(h.p, omp_core::here::ADDRESS_RADIUS_M, h.entries_m)
                    .map_err(|e| e.to_string());
                let answer = HereAnswer {
                    query: h,
                    shown,
                    find: t.elapsed(),
                };
                {
                    let mut slot = shared.slot.lock().unwrap();
                    slot.stats.heap_bytes = search.heap_bytes();
                    slot.stats.bytes_read = search.bytes_read();
                }
                if here_tx.send(answer).is_err() {
                    return;
                }
                wake();
                continue;
            }
            Next::Search(q, g) => (q, g),
        };
        // Enter on the text the suggestions are for (same view) goes on with their session:
        // after a pause, its levels are done.
        let same = |c: &Query, q: &Query| c.text == q.text && c.near == q.near && c.zoom == q.zoom;
        if let Some(q) = new.as_ref().filter(|q| q.submit)
            && let Some((c, session, _, _)) = current.as_mut().filter(|c| same(&c.0, q))
        {
            session.set_limit(q.limit);
            session.stop_when(stopper(shared, generation));
            *c = q.clone();
        } else if let Some(q) = new {
            let req = Request {
                text: &q.text,
                near: Some(geo::from_unit(q.near)),
                zoom: q.zoom,
                limit: q.limit,
                // Typed, Enter included: Enter on a word still being typed means what its
                // suggestions showed.
                prefix: true,
            };
            let mut session = search.start(&req);
            session.stop_when(stopper(shared, generation));
            current = Some((q, session, 0, Instant::now()));
        }
        let Some((q, session, next, at)) = current.as_mut() else {
            continue;
        };
        // Enter runs every level at once; a keystroke one level, and the next after a pause.
        let last = if q.submit {
            find::Session::LEVELS - 1
        } else {
            *next
        };
        let t = Instant::now();
        let mut hits = Ok(None);
        if *next > last {
            hits = session.hits().map(Some);
        }
        for d in *next..=last {
            hits = session.level(d);
            if !matches!(hits, Ok(Some(_))) {
                break;
            }
        }
        let find = t.elapsed();
        let hits = match hits {
            // Stopped by a newer query or a "what's here", which is waiting; a level stopped
            // for "what's here" runs again after it.
            Ok(None) => continue,
            Ok(Some(h)) => Ok(h),
            Err(e) => {
                *next = find::Session::LEVELS;
                Err(e.to_string())
            }
        };
        let level = last;
        if hits.is_ok() {
            *next = (*next).max(last + 1);
        }
        *at = Instant::now() + if level == 0 { PAUSE } else { Duration::ZERO };
        let more = *next < find::Session::LEVELS;
        {
            let mut slot = shared.slot.lock().unwrap();
            slot.stats.heap_bytes = search.heap_bytes();
            slot.stats.bytes_read = search.bytes_read();
            slot.stats.session_bytes = session.heap_bytes();
            slot.stats.session_peak = slot.stats.session_peak.max(slot.stats.session_bytes);
        }
        let answer = Answer {
            query: q.clone(),
            hits,
            find,
            level,
            more,
        };
        if tx.send(answer).is_err() {
            return;
        }
        wake();
    }
}

#[cfg(test)]
#[path = "worker_tests.rs"]
mod tests;
