//! `omp-app --bench <flight.toml>`: flies a fixed path and reports frame times, tile
//! latencies, memory and bytes read as JSON. This is how the phase 1 exit criteria are
//! measured (phase1-plan.md).
//!
//! A flight is a list of steps. Each step moves to a view, either at once (a jump,
//! `seconds = 0`) or linearly in Mercator and zoom over `seconds`; then, if `wait` is set
//! (the default for jumps), waits until the view is fully detailed; then holds for `hold`
//! seconds. Steps run on wall time, so a slow machine draws fewer frames of the same path.
//! After the last step the app is left alone without redrawing: [`SETTLE`] for the last
//! tiles and frames, then [`IDLE`] to measure its idle CPU use.
//!
//! A step with `search` types it into the search box one character at a time, after the
//! move: each keystroke waits for its suggestions (or [`SEARCH_TIMEOUT`]) and at least
//! `keystroke_ms` after the previous one, and its latency is the time from the keystroke to
//! the frame that shows them (search level 0). After the last keystroke it waits for the
//! deeper levels (2.7b) and records when each was shown and, with `expect`, when a result
//! of that name first was. Then it presses Enter, jumps to the first result as picking it
//! does, and waits for that view to be detailed. With `OMP_BENCH_SHOTS=<dir>`, the window
//! is saved there as a PNG with the suggestions open and again at the result.
//!
//! A step with `here = true` asks "what's here" (2.8) at the view's centre after the move,
//! as the right-click menu does, and records the time from the request to the frame that
//! shows the panel, and what the panel shows (saved as a PNG too, with `OMP_BENCH_SHOTS`).
//!
//! A flight with `route = "lat,lon;lat,lon…"` asks for the route through those stops when
//! the pack opens (phase 3 step 3.6), as the right-click menu does, and shows it for the
//! whole flight, so frame times include drawing it; the time to the frame showing it is
//! recorded, and with `OMP_BENCH_SHOTS` that frame is saved. With `depart`, the route is
//! timed from then (phase 4 step 4.5), so the page shows its schedule.
//!
//! A step with `route = "lat,lon;lat,lon…"` replaces the stops with those after the move
//! (phase 3 step 3.9), and records the time from the request to the answer reaching the UI,
//! with its directions, and what the route is. The route stays shown for the steps after.
//!
//! The side panel (phase 3 step 3.8) is open throughout, at its default width, and shows
//! what a user's actions would show: the route page once the route is asked for, the search
//! page while typing and for "what's here". With `OMP_BENCH_SHOTS`, each page is saved once
//! the flight is over.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use omp_core::geo;
use serde::{Deserialize, Serialize};

use crate::camera::{Camera, Vec2};
use crate::here::HereTaken;
use crate::loader::Done;
use crate::search::{BenchKey, Taken};
use crate::side::Page;

/// Longest wait for a view to become fully detailed.
const DETAIL_TIMEOUT: Duration = Duration::from_secs(20);

/// After the flight, before idle CPU is measured.
pub const SETTLE: Duration = Duration::from_secs(2);
/// How long idle CPU is measured.
pub const IDLE: Duration = Duration::from_secs(10);
/// Longest wait for a search answer.
const SEARCH_TIMEOUT: Duration = Duration::from_secs(10);
/// Longest wait for a step's printout, and how long after it the memory is read again.
const PRINT_TIMEOUT: Duration = Duration::from_secs(120);
const PRINT_SETTLE: Duration = Duration::from_secs(3);
/// Longest wait for a route.
const ROUTE_TIMEOUT: Duration = Duration::from_secs(120);
/// Default time between keystrokes.
const KEYSTROKE_MS: f64 = 150.0;
/// With `OMP_BENCH_SHOTS`, the least time from the last keystroke to Enter.
const SHOT_WAIT: Duration = Duration::from_secs(1);

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Flight {
    pub name: String,
    /// Window size in points.
    #[serde(default = "default_window")]
    pub window: [f32; 2],
    /// Stops to route through, shown for the whole flight: `lat,lon;lat,lon…`.
    pub route: Option<String>,
    /// Open every leg's routes once a route is shown, as a user would to see them (phase 4
    /// step 4.3): they are looked for, drawn, and their time recorded.
    #[serde(default)]
    pub alternatives: bool,
    /// Time the route from this local date and time at its start, `2026-07-15T08:00`, with
    /// the schedule's default settings (phase 4 step 4.5).
    pub depart: Option<String>,
    /// Pushpins spread over the pack for the whole flight, the same each run (phase 4 step
    /// 4.7, criterion 5: 10,000 on the Ohio flight).
    pub pins: Option<usize>,
    #[serde(rename = "step")]
    pub steps: Vec<Step>,
}

fn default_window() -> [f32; 2] {
    [1280.0, 800.0]
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub label: Option<String>,
    /// Target view; a missing value keeps the current one.
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub zoom: Option<f64>,
    /// 0 jumps.
    #[serde(default)]
    pub seconds: f64,
    /// Wait until fully detailed after moving. Default: true for jumps, false for flights.
    pub wait: Option<bool>,
    #[serde(default)]
    pub hold: f64,
    /// Typed into the search box after the move, then Enter (see the module notes).
    pub search: Option<String>,
    /// Least time from one keystroke to the next.
    pub keystroke_ms: Option<f64>,
    /// Text of the name of the result the search should find.
    pub expect: Option<String>,
    /// Ask "what's here" at the view's centre after the move (not with `search`).
    #[serde(default)]
    pub here: bool,
    /// Stops to route through after the move, `lat,lon;lat,lon…`, replacing the route
    /// shown (not with `search` or `here`).
    pub route: Option<String>,
    /// After the route is shown, press *Optimize stop order* with the ends free and record
    /// its time (phase 4 step 4.11). Needs `route`.
    #[serde(default)]
    pub optimize: bool,
    /// After the route (and its optimizing) is shown, open every leg's routes and record
    /// the time to see them (phase 4 step 4.11). Needs `route`.
    #[serde(default)]
    pub alternatives: bool,
    /// After the move (and the route), open the print page on `map` or `directions`, wait
    /// for its preview, save a PDF at `print_dpi` (default 300) as *Save as PDF* does, and
    /// record the time and the memory it took (phase 4 step 4.11, criterion 7).
    pub print: Option<String>,
    pub print_dpi: Option<f64>,
}

impl Flight {
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let f: Self = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        if f.steps.is_empty() {
            return Err(format!("{}: the flight has no steps", path.display()));
        }
        if let Some(d) = &f.depart
            && d.split_once('T').is_none_or(|(date, time)| {
                date.parse::<jiff::civil::Date>().is_err()
                    || crate::routing::parse_time(time).is_none()
            })
        {
            return Err(format!(
                "{}: `depart` wants a date and time, as 2026-07-15T08:00",
                f.name
            ));
        }
        if f.route_stops().is_err() {
            return Err(format!(
                "{}: `route` wants two stops or more, as lat,lon;lat,lon",
                path.display()
            ));
        }
        if f.steps.iter().any(|s| s.here && s.search.is_some()) {
            return Err(format!(
                "{}: a step has both `search` and `here`",
                path.display()
            ));
        }
        for s in &f.steps {
            if let Some(k) = &s.print
                && !matches!(k.as_str(), "map" | "directions")
            {
                return Err(format!(
                    "{}: `print` wants map or directions",
                    path.display()
                ));
            }
            if (s.optimize || s.alternatives) && s.route.is_none() {
                return Err(format!(
                    "{}: a step has `optimize` or `alternatives` without `route`",
                    path.display()
                ));
            }
            let Some(r) = &s.route else { continue };
            if s.here || s.search.is_some() {
                return Err(format!(
                    "{}: a step has `route` with `search` or `here`",
                    path.display()
                ));
            }
            if parse_stops(r).is_err() {
                return Err(format!(
                    "{}: a step's `route` wants two stops or more, as lat,lon;lat,lon",
                    path.display()
                ));
            }
        }
        Ok(f)
    }
}

impl Flight {
    /// `route`'s stops as world points; empty without one.
    pub fn route_stops(&self) -> Result<Vec<Vec2>, ()> {
        self.route.as_deref().map_or(Ok(Vec::new()), parse_stops)
    }
}

/// `lat,lon;lat,lon…` as world points; two at least.
fn parse_stops(r: &str) -> Result<Vec<Vec2>, ()> {
    let stops: Vec<Vec2> = r
        .split(';')
        .map(|s| {
            let (lat, lon) = s.split_once(',').ok_or(())?;
            let n = |v: &str| v.trim().parse::<f64>().map_err(|_| ());
            Ok(geo::lonlat_to_unit(n(lon)?, n(lat)?))
        })
        .collect::<Result<_, ()>>()?;
    if stops.len() < 2 {
        return Err(());
    }
    Ok(stops)
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    Move,
    Wait,
    /// Typing the step's search.
    Type,
    /// Waiting for the step's "what's here".
    Here,
    /// Waiting for the step's route.
    Route,
    /// Printing the step's page to a PDF.
    Print,
    Hold,
}

/// What the app measured in one frame.
#[derive(Clone, Copy, Debug)]
pub struct FrameSample {
    /// Since the previous frame started.
    pub interval: Duration,
    /// Time spent in the app's own frame code.
    pub cpu: Duration,
    /// Every visible tile at the view's tile zoom is loaded.
    pub detailed: bool,
    pub cache_bytes: usize,
    /// Label placement, part of `cpu`.
    pub labels: Duration,
    pub bytes_read: u64,
}

#[derive(Default)]
struct StepLog {
    intervals: Vec<f64>,
    cpu: Vec<f64>,
    incomplete_frames: usize,
    detail_ms: Option<f64>,
    timed_out: bool,
    tiles: usize,
    bytes_read_start: u64,
    rss_end: Option<u64>,
    search: SearchLog,
    here: HereLog,
    route: RouteLog,
    print: PrintLog,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum PrintStage {
    #[default]
    Start,
    /// The print page is open and its preview is being drawn.
    Preview,
    /// The preview is shown; the PDF is to be started.
    Ready,
    /// The PDF is being made.
    Output,
    /// The PDF is made and the page closed.
    Settled,
}

#[derive(Default)]
struct PrintLog {
    stage: PrintStage,
    started: Option<Instant>,
    output_at: Option<Instant>,
    closed: Option<Instant>,
    rss_before: Option<u64>,
    rss_preview: Option<u64>,
    rss_after: Option<u64>,
    preview_ms: Option<f64>,
    output_ms: Option<f64>,
    pdf_bytes: Option<u64>,
    dpi: f64,
    peak: Option<(
        std::sync::Arc<std::sync::atomic::AtomicU64>,
        std::sync::Arc<std::sync::atomic::AtomicBool>,
    )>,
    rss_peak: Option<u64>,
    path: Option<PathBuf>,
    error: Option<String>,
}

#[derive(Default)]
struct RouteLog {
    asked: Option<Instant>,
    done: bool,
    shown: Option<RouteReport>,
    error: Option<String>,
    timed_out: bool,
    /// When *Optimize stop order* was pressed, and what it came to.
    optimizing: Option<Instant>,
    optimized: Option<OptimizeReport>,
    /// The step's alternatives were asked for, and shown.
    alt_asked: bool,
    alt_done: bool,
}

#[derive(Default)]
struct HereLog {
    asked: Option<Instant>,
    done: bool,
    latency_ms: Option<f64>,
    find_ms: Option<f64>,
    answer: Option<HereAnswerLog>,
    error: Option<String>,
    timed_out: bool,
    skipped: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct HereAnswerLog {
    /// "123 Main Street, Springfield, IL 62701", with " (approximate)".
    pub address: Option<String>,
    pub address_m: Option<f64>,
    pub street: Option<String>,
    pub nearby: Vec<String>,
    pub areas: Vec<String>,
}

#[derive(Default)]
struct SearchLog {
    /// Characters typed so far.
    typed: usize,
    /// The last keystroke (or Enter), and whether its answer is still awaited.
    key_at: Option<Instant>,
    awaiting: bool,
    entered: bool,
    latency: Vec<f64>,
    find: Vec<f64>,
    enter_ms: Option<f64>,
    top: Option<String>,
    /// The first result's view, to jump to after Enter.
    jump: Option<(Vec2, f64)>,
    pick_detail_ms: Option<f64>,
    /// After the last keystroke: when each level was shown, whether the last was, and when
    /// (and at which level) the expected result first was.
    levels: Vec<f64>,
    deep_done: bool,
    expected: Option<(f64, usize)>,
    timed_out: bool,
    /// The pack has no search.
    skipped: bool,
}

#[derive(Default)]
struct TileLog {
    latency: Vec<f64>,
    read: Vec<f64>,
    decode: Vec<f64>,
    prepare: Vec<f64>,
    loaded: usize,
    absent: usize,
    errors: usize,
}

pub struct Bench {
    pub flight: Flight,
    flight_path: PathBuf,
    out: PathBuf,
    started: Instant,
    step: usize,
    phase: Phase,
    phase_start: Instant,
    from: (Vec2, f64),
    logs: Vec<StepLog>,
    tiles: TileLog,
    intervals: Vec<f64>,
    cpu: Vec<f64>,
    labels: Vec<f64>,
    /// Every keystroke's latency, and the first one's.
    keystrokes: Vec<f64>,
    /// Every "what's here" latency.
    heres: Vec<f64>,
    /// The flight's route, once shown.
    route: Option<RouteReport>,
    first_keystroke_ms: Option<f64>,
    startup_ms: Option<f64>,
    cache_peak: usize,
    bytes_read_start: u64,
    finished: bool,
    /// After the flight: its report, and when it ended.
    idle: Option<(Report, Instant)>,
    /// When idle CPU began to be measured, with the CPU time then, and the frames since.
    measure: Option<(Instant, Option<Duration>, usize)>,
    done: bool,
    /// `OMP_BENCH_SHOTS`, and a screenshot to take there.
    shots: Option<PathBuf>,
    shot: Option<String>,
    /// With `shots`, after the flight: the side panel page shown for its screenshot, since
    /// when, and whether the screenshot was asked for.
    page: Option<(usize, Instant, bool)>,
}

/// Where the bench is, after a frame.
pub enum Poll {
    Flying,
    /// Idling; wake again after this long.
    Idle(Duration),
    /// The finished report, given once.
    Done(Box<Report>),
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

impl Bench {
    /// `started` is when the process started, for the start-up time.
    pub fn new(flight: Flight, flight_path: PathBuf, out: PathBuf, started: Instant) -> Self {
        Self {
            logs: flight.steps.iter().map(|_| StepLog::default()).collect(),
            flight,
            flight_path,
            out,
            started,
            step: 0,
            phase: Phase::Move,
            phase_start: Instant::now(),
            from: ([0.5, 0.5], 0.0),
            tiles: TileLog::default(),
            intervals: Vec::new(),
            cpu: Vec::new(),
            labels: Vec::new(),
            keystrokes: Vec::new(),
            heres: Vec::new(),
            route: None,
            first_keystroke_ms: None,
            startup_ms: None,
            cache_peak: 0,
            bytes_read_start: 0,
            finished: false,
            idle: None,
            measure: None,
            done: false,
            shots: std::env::var_os("OMP_BENCH_SHOTS").map(PathBuf::from),
            shot: None,
            page: None,
        }
    }

    pub fn out(&self) -> &Path {
        &self.out
    }

    /// Puts the camera at the first step's view.
    pub fn start(&mut self, cam: &mut Camera, bytes_read: u64) {
        self.bytes_read_start = bytes_read;
        self.begin_step(0, cam, Instant::now(), bytes_read);
    }

    fn target(&self, i: usize, cam: &Camera) -> (Vec2, f64) {
        let s = &self.flight.steps[i];
        let [lon, lat] = cam.lonlat();
        let c = geo::lonlat_to_unit(s.lon.unwrap_or(lon), s.lat.unwrap_or(lat));
        (c, s.zoom.unwrap_or(cam.zoom))
    }

    fn begin_step(&mut self, i: usize, cam: &mut Camera, now: Instant, bytes_read: u64) {
        self.step = i;
        self.phase = Phase::Move;
        self.phase_start = now;
        self.from = (cam.center, cam.zoom);
        self.logs[i].bytes_read_start = bytes_read;
        if self.flight.steps[i].seconds <= 0.0 {
            let (c, z) = self.target(i, cam);
            cam.center = c;
            cam.zoom = z;
            cam.clamp();
        }
    }

    pub fn tile<T>(&mut self, d: &Done<T>) {
        if let Some(log) = self.logs.get_mut(self.step) {
            log.tiles += 1;
        }
        let t = &mut self.tiles;
        match &d.result {
            Ok(Some(_)) => t.loaded += 1,
            Ok(None) => t.absent += 1,
            Err(_) => t.errors += 1,
        }
        t.latency.push(ms(d.latency));
        t.read.push(ms(d.timing.read));
        t.decode.push(ms(d.timing.decode));
        if matches!(d.result, Ok(Some(_))) {
            t.prepare.push(ms(d.prepare));
        }
    }

    /// The search step's next keystroke, when it is time for it. `available` is false for
    /// a pack without search, whose search steps are skipped.
    pub fn search_key(&mut self, available: bool) -> Option<BenchKey> {
        if self.phase != Phase::Type {
            return None;
        }
        let step = &self.flight.steps[self.step];
        let text = step.search.as_deref()?;
        let gap = Duration::from_secs_f64(step.keystroke_ms.unwrap_or(KEYSTROKE_MS) / 1e3);
        let log = &mut self.logs[self.step].search;
        if !available {
            log.skipped = true;
            log.entered = true;
            self.shot("no-search");
            return None;
        }
        let now = Instant::now();
        if log.awaiting {
            if log.key_at.is_some_and(|t| now - t < SEARCH_TIMEOUT) {
                return None;
            }
            log.timed_out = true;
            log.awaiting = false;
        }
        if log.entered {
            return None;
        }
        let n = text.chars().count();
        if log.typed < n {
            if log.key_at.is_some_and(|t| now - t < gap) {
                return None;
            }
            log.typed += 1;
            log.key_at = Some(now);
            log.awaiting = true;
            return Some(BenchKey::Type(text.chars().take(log.typed).collect()));
        }
        // The deeper levels of the last keystroke run first.
        if !log.deep_done {
            if log.key_at.is_some_and(|t| now - t < SEARCH_TIMEOUT) {
                return None;
            }
            log.timed_out = true;
            log.deep_done = true;
        }
        // A screenshot of the suggestions is saved a few frames after it is asked for.
        if self.shots.is_some() && log.key_at.is_some_and(|t| now - t < SHOT_WAIT) {
            return None;
        }
        log.entered = true;
        log.key_at = Some(now);
        log.awaiting = true;
        Some(BenchKey::Enter)
    }

    /// An answer the search box took; `view` is where its first result would fly to.
    pub fn search_answer(&mut self, t: &Taken, view: Option<(Vec2, f64)>) {
        if self.phase != Phase::Type {
            return;
        }
        let step = &self.flight.steps[self.step];
        let log = &mut self.logs[self.step].search;
        let last = log.typed == step.search.as_ref().map_or(0, |q| q.chars().count());
        let latency = ms(t.latency);
        if !t.submit && last && !log.entered && (t.level == 0) == log.awaiting {
            log.levels.push(latency);
            log.deep_done = !t.more;
            let expect = step.expect.as_deref().map(str::to_lowercase);
            if let Some(e) = expect
                && log.expected.is_none()
                && t.names.iter().any(|n| n.to_lowercase().contains(&e))
            {
                log.expected = Some((latency, t.level));
            }
        }
        if !log.awaiting || t.level > 0 && !t.submit {
            return;
        }
        log.awaiting = false;
        if t.submit {
            log.enter_ms = Some(latency);
            log.top = t.first.as_ref().map(|h| match h.context.as_str() {
                "" => h.name.clone(),
                c => format!("{}, {c}", h.name),
            });
            log.jump = view;
        } else {
            log.latency.push(latency);
            log.find.push(ms(t.find));
            self.keystrokes.push(latency);
            self.first_keystroke_ms.get_or_insert(latency);
            if last {
                self.shot("suggestions");
            }
        }
    }

    /// Whether to ask "what's here" at the view's centre now: once, in a `here` step after
    /// its move. `available` is false for a pack without search, whose steps are skipped.
    pub fn here_ask(&mut self, available: bool) -> bool {
        if self.phase != Phase::Here {
            return false;
        }
        let log = &mut self.logs[self.step].here;
        if log.asked.is_some() || log.done {
            return false;
        }
        if !available {
            log.skipped = true;
            log.done = true;
            return false;
        }
        log.asked = Some(Instant::now());
        true
    }

    /// A "what's here" answer the panel took.
    pub fn here_answer(&mut self, t: &HereTaken) {
        if self.phase != Phase::Here {
            return;
        }
        let log = &mut self.logs[self.step].here;
        if log.asked.is_none() || log.done {
            return;
        }
        log.done = true;
        let latency = ms(t.latency);
        log.latency_ms = Some(latency);
        log.find_ms = Some(ms(t.find));
        self.heres.push(latency);
        match &t.shown {
            Ok(s) => {
                log.answer = Some(HereAnswerLog {
                    address: s.address.as_ref().map(|a| {
                        format!(
                            "{}, {}{}",
                            a.label,
                            s.address_place,
                            if a.approx { " (approximate)" } else { "" }
                        )
                    }),
                    address_m: s.address.as_ref().map(|a| (a.dist_m * 10.0).round() / 10.0),
                    street: s.street.as_ref().map(|(n, t, _)| match t.as_str() {
                        "" => n.clone(),
                        t => format!("{n}, {t}"),
                    }),
                    nearby: s.nearby.iter().map(|(e, _)| e.name.clone()).collect(),
                    areas: s.areas.clone(),
                });
            }
            Err(e) => log.error = Some(e.clone()),
        }
    }

    /// The stops to route through now: once, in a `route` step after its move.
    pub fn route_ask(&mut self) -> Option<Vec<Vec2>> {
        if self.phase != Phase::Route {
            return None;
        }
        let log = &mut self.logs[self.step].route;
        if log.asked.is_some() {
            return None;
        }
        log.asked = Some(Instant::now());
        // Checked when the flight was loaded.
        parse_stops(self.flight.steps[self.step].route.as_deref()?).ok()
    }

    /// The answer to the latest route request reached the UI and is drawn this frame: the
    /// flight's route if it is the first, else the `route` step's.
    pub fn route_answer(&mut self, r: Result<RouteReport, String>) {
        let log = &mut self.logs[self.step].route;
        if self.phase == Phase::Route && log.asked.is_some() && !log.done {
            log.done = true;
            match r {
                Ok(r) => log.shown = Some(r),
                Err(e) => log.error = Some(e),
            }
            self.shot("route");
        } else if self.route.is_none()
            && let Ok(r) = r
        {
            self.route = Some(r);
            self.shot("route");
        }
    }

    /// The print page is to be opened now, on this kind of page: once per step.
    pub fn print_open(&mut self) -> Option<crate::printui::Kind> {
        if self.phase != Phase::Print || self.logs[self.step].print.stage != PrintStage::Start {
            return None;
        }
        let kind = match self.flight.steps[self.step].print.as_deref()? {
            "directions" => crate::printui::Kind::Directions,
            _ => crate::printui::Kind::Map,
        };
        let p = &mut self.logs[self.step].print;
        p.stage = PrintStage::Preview;
        p.started = Some(Instant::now());
        p.rss_before = crate::memory::usage().map(|m| m.rss);
        Some(kind)
    }

    /// The preview is being waited for.
    pub fn print_previewing(&self) -> bool {
        self.phase == Phase::Print && self.logs[self.step].print.stage == PrintStage::Preview
    }

    /// The preview is drawn this frame, or could not be.
    pub fn print_previewed(&mut self, r: Result<(), String>) {
        let p = &mut self.logs[self.step].print;
        p.preview_ms = p.started.map(|t| t.elapsed().as_secs_f64() * 1e3);
        p.rss_preview = crate::memory::usage().map(|m| m.rss);
        match r {
            Ok(()) => p.stage = PrintStage::Ready,
            Err(e) => p.error = Some(e),
        }
    }

    /// The PDF is to be started now: where, and at which resolution.
    pub fn print_output(&mut self) -> Option<(PathBuf, f64)> {
        use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
        if self.phase != Phase::Print || self.logs[self.step].print.stage != PrintStage::Ready {
            return None;
        }
        let dpi = self.flight.steps[self.step].print_dpi.unwrap_or(300.0);
        let path = std::env::temp_dir().join(format!("omp-bench-print-{}.pdf", std::process::id()));
        let p = &mut self.logs[self.step].print;
        p.stage = PrintStage::Output;
        p.dpi = dpi;
        p.output_at = Some(Instant::now());
        p.path = Some(path.clone());
        // The process's highest RSS while the PDF is made, polled from a thread.
        let (peak, stop) = (
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicBool::new(false)),
        );
        let (t_peak, t_stop) = (peak.clone(), stop.clone());
        std::thread::spawn(move || {
            while !t_stop.load(Ordering::Relaxed) {
                if let Some(m) = crate::memory::usage() {
                    t_peak.fetch_max(m.rss, Ordering::Relaxed);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        p.peak = Some((peak, stop));
        Some((path, dpi))
    }

    /// The PDF is waited for.
    pub fn print_running(&self) -> bool {
        self.phase == Phase::Print && self.logs[self.step].print.stage == PrintStage::Output
    }

    /// The PDF is written, or failed. The page is to be closed.
    pub fn print_done(&mut self, r: Result<(), String>) {
        use std::sync::atomic::Ordering;
        let p = &mut self.logs[self.step].print;
        p.output_ms = p.output_at.map(|t| t.elapsed().as_secs_f64() * 1e3);
        if let Some((peak, stop)) = p.peak.take() {
            stop.store(true, Ordering::Relaxed);
            p.rss_peak = Some(peak.load(Ordering::Relaxed));
        }
        p.pdf_bytes = p
            .path
            .take()
            .and_then(|f| {
                std::fs::metadata(&f)
                    .ok()
                    .inspect(|_| drop(std::fs::remove_file(&f)))
            })
            .map(|m| m.len());
        p.stage = PrintStage::Settled;
        p.closed = Some(Instant::now());
        if let Err(e) = r {
            p.error = Some(e);
        }
    }

    /// Whether the step's route is shown and *Optimize stop order* is to be pressed now:
    /// once, after the route.
    pub fn optimize_ask(&mut self) -> bool {
        let want = self.flight.steps[self.step].optimize;
        let log = &mut self.logs[self.step].route;
        let go = self.phase == Phase::Route && want && log.done && log.optimizing.is_none();
        if go {
            log.optimizing = Some(Instant::now());
        }
        go
    }

    /// An optimize request is waiting for its answer.
    pub fn optimize_pending(&self) -> bool {
        let log = &self.logs[self.step].route;
        self.phase == Phase::Route && log.optimizing.is_some() && log.optimized.is_none()
    }

    /// The reordered route is drawn this frame, or why it could not be.
    pub fn optimize_answer(&mut self, r: Result<OptimizeReport, &str>) {
        let log = &mut self.logs[self.step].route;
        match r {
            Ok(mut r) => {
                r.latency_ms = log
                    .optimizing
                    .map_or(0.0, |t| t.elapsed().as_secs_f64() * 1e3);
                log.optimized = Some(r);
            }
            Err(why) => log.error = Some(format!("optimize: {why}")),
        }
        self.shot("optimized");
    }

    /// Whether the step's alternatives are to be asked for now: once, after the route and
    /// its optimizing.
    pub fn alternatives_ask(&mut self) -> bool {
        let want = self.flight.steps[self.step].alternatives;
        let log = &mut self.logs[self.step].route;
        let ready = log.done
            && (log.optimizing.is_none() || log.optimized.is_some() || log.error.is_some());
        let go = self.phase == Phase::Route && want && ready && !log.alt_asked;
        if go {
            log.alt_asked = true;
        }
        go
    }

    /// The alternatives to the latest route are drawn this frame: `n` of them, `ms` after
    /// its legs were opened. Recorded for the flight's route and a `route` step's.
    pub fn alternatives_shown(&mut self, n: usize, ms: f64) {
        let log = &mut self.logs[self.step].route;
        if log.alt_asked {
            log.alt_done = true;
        }
        let r = match (self.phase == Phase::Route && log.done, &mut log.shown) {
            (true, Some(r)) => r,
            _ => match &mut self.route {
                Some(r) if r.alternatives.is_none() => r,
                _ => return,
            },
        };
        r.alternatives = Some(n);
        r.alternatives_ms = Some(ms);
    }

    /// Where to save a screenshot of this frame, if one is due.
    pub fn take_shot(&mut self) -> Option<PathBuf> {
        let name = self.shot.take()?;
        Some(self.shots.as_ref()?.join(format!("{name}.png")))
    }

    fn shot(&mut self, what: &str) {
        if self.shots.is_some() {
            self.shot = Some(format!("{}-{:02}-{what}", self.flight.name, self.step + 1));
        }
    }

    /// Records a frame and moves the camera for the next one. Returns the report once the
    /// flight is over.
    pub fn end_frame(
        &mut self,
        f: FrameSample,
        cam: &mut Camera,
        ctx: &ReportContext,
    ) -> Option<Report> {
        if self.finished {
            return None;
        }
        let now = Instant::now();
        self.intervals.push(ms(f.interval));
        self.cpu.push(ms(f.cpu));
        self.labels.push(ms(f.labels));
        self.cache_peak = self.cache_peak.max(f.cache_bytes);
        if f.detailed && self.startup_ms.is_none() {
            self.startup_ms = Some(ms(self.started.elapsed()));
        }
        let step = self.flight.steps[self.step].clone();
        let log = &mut self.logs[self.step];
        log.intervals.push(ms(f.interval));
        log.cpu.push(ms(f.cpu));
        log.incomplete_frames += usize::from(!f.detailed);
        let t = now.duration_since(self.phase_start).as_secs_f64();
        if self.phase == Phase::Move {
            if step.seconds > 0.0 {
                let (c, z) = self.target(self.step, cam);
                let k = (t / step.seconds).min(1.0);
                let lerp = |a: f64, b: f64| a + (b - a) * k;
                cam.center = [lerp(self.from.0[0], c[0]), lerp(self.from.0[1], c[1])];
                cam.zoom = lerp(self.from.1, z);
                cam.clamp();
                if k < 1.0 {
                    return None;
                }
            }
            self.phase = if step.wait.unwrap_or(step.seconds <= 0.0) {
                Phase::Wait
            } else if step.search.is_some() {
                Phase::Type
            } else if step.here {
                Phase::Here
            } else if step.route.is_some() {
                Phase::Route
            } else if step.print.is_some() {
                Phase::Print
            } else {
                Phase::Hold
            };
            self.phase_start = now;
            return None;
        }
        if self.phase == Phase::Wait {
            let picked = log.search.entered;
            if f.detailed {
                if picked {
                    log.search.pick_detail_ms = Some(t * 1e3);
                    self.shot("picked");
                } else {
                    log.detail_ms = Some(t * 1e3);
                }
            } else if t < DETAIL_TIMEOUT.as_secs_f64() {
                return None;
            } else {
                log.timed_out = true;
            }
            self.phase = if step.search.is_some() && !picked {
                Phase::Type
            } else if step.here {
                Phase::Here
            } else if step.route.is_some() {
                Phase::Route
            } else if step.print.is_some() {
                Phase::Print
            } else {
                Phase::Hold
            };
            self.phase_start = now;
            return None;
        }
        if self.phase == Phase::Type {
            let s = &mut log.search;
            if !s.entered || s.awaiting {
                return None;
            }
            self.phase = match s.jump.take() {
                Some((c, z)) => {
                    cam.center = c;
                    cam.zoom = z;
                    cam.clamp();
                    Phase::Wait
                }
                None => Phase::Hold,
            };
            self.phase_start = now;
            return None;
        }
        if self.phase == Phase::Here {
            let h = &mut log.here;
            if !h.done {
                if h.asked.is_none_or(|a| now - a < SEARCH_TIMEOUT) {
                    return None;
                }
                h.timed_out = true;
                h.done = true;
            }
            // A screenshot of the panel, once it has faded in.
            if self.shots.is_some() {
                if t < SHOT_WAIT.as_secs_f64() {
                    return None;
                }
                self.shot("here");
            }
            self.phase = Phase::Hold;
            self.phase_start = now;
            return None;
        }
        if self.phase == Phase::Route {
            let r = &mut log.route;
            let finished = r.done
                && (!step.optimize || r.optimized.is_some() || r.error.is_some())
                && (!step.alternatives || r.alt_done);
            if !finished {
                if r.asked.is_none_or(|a| now - a < ROUTE_TIMEOUT) {
                    return None;
                }
                r.timed_out = true;
                r.done = true;
            }
            self.phase = if step.print.is_some() {
                Phase::Print
            } else {
                Phase::Hold
            };
            self.phase_start = now;
            return None;
        }
        if self.phase == Phase::Print {
            let p = &mut log.print;
            let done = p.stage == PrintStage::Settled
                && p.closed.is_some_and(|c| now - c >= PRINT_SETTLE)
                || p.error.is_some();
            if !done {
                if p.started.is_none_or(|a| now - a < PRINT_TIMEOUT) {
                    return None;
                }
                p.error = Some("timed out".into());
            }
            p.rss_after = crate::memory::usage().map(|m| m.rss);
            self.phase = Phase::Hold;
            self.phase_start = now;
            return None;
        }
        if t < step.hold {
            return None;
        }
        log.rss_end = crate::memory::usage().map(|m| m.rss);
        if self.step + 1 < self.flight.steps.len() {
            self.begin_step(self.step + 1, cam, now, f.bytes_read);
            return None;
        }
        self.finished = true;
        Some(self.report(f.bytes_read, ctx))
    }

    /// Starts the idle period with the flight's report.
    pub fn start_idle(&mut self, report: Report) {
        self.idle = Some((report, Instant::now()));
        if self.shots.is_some() {
            self.page = Some((0, Instant::now(), false));
        }
    }

    /// With `OMP_BENCH_SHOTS`, once the flight is over: each page of the side panel in turn,
    /// saved once it has been shown for a moment, before the app is left idle.
    pub fn page_shot(&mut self) -> Option<Page> {
        let (i, since, asked) = self.page?;
        let page = *Page::ALL.get(i)?;
        if since.elapsed() >= SHOT_WAIT && !asked {
            self.shot(&format!("page-{}", page.name().to_lowercase()));
            self.page = Some((i, since, true));
        } else if asked {
            // The screenshot is of the frame after the one that asks for it.
            self.page = Some((i + 1, Instant::now(), false));
            // The idle period starts after the last.
            if i + 1 == Page::ALL.len()
                && let Some((_, end)) = &mut self.idle
            {
                *end = Instant::now();
            }
        }
        Some(page)
    }

    /// Called once a frame.
    pub fn poll(&mut self) -> Poll {
        if self.page.is_some_and(|(i, ..)| i < Page::ALL.len()) {
            return Poll::Flying;
        }
        let Some((_, end)) = &self.idle else {
            return if self.done {
                Poll::Idle(IDLE)
            } else {
                Poll::Flying
            };
        };
        let left = |since: Instant, span: Duration| {
            span.checked_sub(since.elapsed()).filter(|d| !d.is_zero())
        };
        let Some((start, _, frames)) = &mut self.measure else {
            if let Some(wait) = left(*end, SETTLE) {
                return Poll::Idle(wait);
            }
            self.measure = Some((Instant::now(), crate::memory::cpu_time(), 0));
            return Poll::Idle(IDLE);
        };
        *frames += 1;
        if let Some(wait) = left(*start, IDLE) {
            return Poll::Idle(wait);
        }
        let (mut report, _) = self.idle.take().unwrap();
        let (start, cpu, frames) = self.measure.take().unwrap();
        report.idle_cpu_percent = cpu.zip(crate::memory::cpu_time()).map(|(a, b)| {
            let p = (b - a).as_secs_f64() / start.elapsed().as_secs_f64() * 100.0;
            (p * 100.0).round() / 100.0
        });
        // Not counting this last frame, which only ends the measurement.
        report.idle_frames = frames - 1;
        self.done = true;
        Poll::Done(Box::new(report))
    }

    fn report(&mut self, bytes_read: u64, ctx: &ReportContext) -> Report {
        let mem = crate::memory::usage();
        // Each step's bytes run to the next step's start; the last one's to the end.
        let ends: Vec<u64> = (1..=self.logs.len())
            .map(|i| self.logs.get(i).map_or(bytes_read, |l| l.bytes_read_start))
            .collect();
        let steps = self
            .flight
            .steps
            .iter()
            .zip(&mut self.logs)
            .enumerate()
            .map(|(i, (s, l))| StepReport {
                label: s.label.clone().unwrap_or_else(|| format!("step {}", i + 1)),
                kind: if s.seconds > 0.0 { "fly" } else { "jump" },
                seconds: s.seconds,
                frames: l.intervals.len(),
                incomplete_frames: l.incomplete_frames,
                interval_ms: Summary::of(&mut l.intervals),
                cpu_ms: Summary::of(&mut l.cpu),
                detail_ms: l.detail_ms,
                timed_out: l.timed_out,
                tiles: l.tiles,
                bytes_read: ends[i].saturating_sub(l.bytes_read_start),
                rss_end: l.rss_end,
                search: s.search.clone().map(|query| SearchStepReport {
                    query,
                    keystrokes: l.search.latency.len(),
                    latency_ms: Summary::of(&mut l.search.latency),
                    find_ms: Summary::of(&mut l.search.find),
                    enter_ms: l.search.enter_ms,
                    top: l.search.top.clone(),
                    pick_detail_ms: l.search.pick_detail_ms,
                    levels_ms: l.search.levels.clone(),
                    expect: s.expect.clone(),
                    expected_ms: l.search.expected.map(|e| e.0),
                    expected_level: l.search.expected.map(|e| e.1),
                    timed_out: l.search.timed_out,
                    skipped: l.search.skipped,
                }),
                here: s.here.then(|| HereStepReport {
                    latency_ms: l.here.latency_ms,
                    find_ms: l.here.find_ms,
                    answer: l.here.answer.clone(),
                    error: l.here.error.clone(),
                    timed_out: l.here.timed_out,
                    skipped: l.here.skipped,
                }),
                print: s.print.as_ref().map(|k| PrintReport {
                    page: k.clone(),
                    dpi: l.print.dpi,
                    preview_ms: l.print.preview_ms,
                    output_ms: l.print.output_ms,
                    pdf_bytes: l.print.pdf_bytes,
                    rss_before: l.print.rss_before,
                    rss_preview: l.print.rss_preview,
                    rss_peak: l.print.rss_peak,
                    rss_after: l.print.rss_after,
                    error: l.print.error.clone(),
                }),
                route: s.route.as_ref().map(|_| RouteStepReport {
                    shown: l.route.shown.clone(),
                    optimized: l.route.optimized.clone(),
                    error: l.route.error.clone(),
                    timed_out: l.route.timed_out,
                }),
            })
            .collect::<Vec<_>>();
        Report {
            flight: self.flight.name.clone(),
            flight_file: self.flight_path.display().to_string(),
            pack: ctx.pack.clone(),
            app_version: env!("CARGO_PKG_VERSION"),
            adapter: ctx.adapter.clone(),
            window: self.flight.window,
            pixels_per_point: ctx.pixels_per_point,
            startup_ms: self.startup_ms,
            frames: self.intervals.len(),
            interval_ms: Summary::of(&mut self.intervals),
            cpu_ms: Summary::of(&mut self.cpu),
            labels_ms: Summary::of(&mut self.labels),
            tiles: TilesReport {
                loaded: self.tiles.loaded,
                absent: self.tiles.absent,
                errors: self.tiles.errors,
                latency_ms: Summary::of(&mut self.tiles.latency),
                read_ms: Summary::of(&mut self.tiles.read),
                decode_ms: Summary::of(&mut self.tiles.decode),
                prepare_ms: Summary::of(&mut self.tiles.prepare),
            },
            bytes_read: bytes_read - self.bytes_read_start,
            rss_end: mem.map(|m| m.rss),
            rss_peak: mem.map(|m| m.peak),
            idle_cpu_percent: None,
            idle_frames: 0,
            gpu_allocated: None,
            gpu_reserved: None,
            tile_slabs_peak: None,
            cache_budget: ctx.cache_budget,
            cache_peak: self.cache_peak,
            search: ctx
                .search
                .filter(|_| !self.keystrokes.is_empty())
                .map(|c| SearchReport {
                    open_ms: c.open_ms,
                    first_keystroke_ms: self.first_keystroke_ms,
                    latency_ms: Summary::of(&mut self.keystrokes),
                    heap_bytes: c.heap_bytes,
                    bytes_read: c.bytes_read,
                }),
            here_ms: (!self.heres.is_empty()).then(|| Summary::of(&mut self.heres)),
            route: self.route.clone(),
            steps,
        }
    }
}

/// What the report needs from the app.
pub struct ReportContext {
    pub pack: String,
    pub adapter: String,
    pub pixels_per_point: f32,
    pub cache_budget: usize,
    /// The search readers, when the pack has search.
    pub search: Option<SearchContext>,
}

#[derive(Clone, Copy, Debug)]
pub struct SearchContext {
    pub open_ms: Option<f64>,
    pub heap_bytes: usize,
    pub bytes_read: u64,
}

#[derive(Debug, Default, Serialize)]
pub struct Summary {
    pub count: usize,
    pub mean: f64,
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
    pub max: f64,
}

impl Summary {
    pub fn of(v: &mut [f64]) -> Self {
        if v.is_empty() {
            return Self::default();
        }
        v.sort_by(f64::total_cmp);
        let q = |p: f64| v[((v.len() - 1) as f64 * p).round() as usize];
        let round = |x: f64| (x * 1000.0).round() / 1000.0;
        Self {
            count: v.len(),
            mean: round(v.iter().sum::<f64>() / v.len() as f64),
            p50: round(q(0.5)),
            p95: round(q(0.95)),
            p99: round(q(0.99)),
            max: round(v[v.len() - 1]),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub flight: String,
    pub flight_file: String,
    pub pack: String,
    pub app_version: &'static str,
    /// The wgpu adapter and backend.
    pub adapter: String,
    pub window: [f32; 2],
    pub pixels_per_point: f32,
    /// From process start to the first frame with every visible tile loaded.
    pub startup_ms: Option<f64>,
    pub frames: usize,
    /// Time between frame starts; vsync is off during the bench.
    pub interval_ms: Summary,
    /// Time in the app's frame code.
    pub cpu_ms: Summary,
    /// Label placement, part of `cpu_ms`.
    pub labels_ms: Summary,
    pub tiles: TilesReport,
    pub bytes_read: u64,
    pub rss_end: Option<u64>,
    pub rss_peak: Option<u64>,
    /// CPU use over the idle period after the flight, as a percentage of one core.
    pub idle_cpu_percent: Option<f64>,
    /// Frames drawn while idle CPU was measured; nothing asks for one, so 0 is expected.
    pub idle_frames: usize,
    /// From wgpu's allocator at the end, where the backend has one (DX12, Vulkan): bytes in
    /// live allocations, and in the memory blocks they are cut from.
    pub gpu_allocated: Option<u64>,
    pub gpu_reserved: Option<u64>,
    /// Most bytes in the slabs that hold tile geometry (pool.rs).
    pub tile_slabs_peak: Option<u64>,
    pub cache_budget: usize,
    pub cache_peak: usize,
    /// Over every search step, when there are any and the pack has search.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search: Option<SearchReport>,
    /// Every "what's here" step's latency, from the request to the frame showing it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub here_ms: Option<Summary>,
    /// The flight's route.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route: Option<RouteReport>,
    pub steps: Vec<StepReport>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RouteReport {
    pub stops: usize,
    /// From the request, when the pack opened, to the frame showing the route; this waits
    /// for the routing readers to open.
    pub latency_ms: f64,
    /// Time in `trip`, and making the directions after it; parts of the latency.
    pub trip_ms: f64,
    pub directions_ms: f64,
    /// Steps of the directions.
    pub steps: usize,
    pub km: f64,
    pub minutes: f64,
    pub points: usize,
    /// The legs' alternatives found, and the time from the request to the frame showing
    /// them (phase 4 step 4.3).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alternatives: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alternatives_ms: Option<f64>,
}

impl RouteReport {
    fn line(&self) -> String {
        let alts = match (self.alternatives, self.alternatives_ms) {
            (Some(n), Some(ms)) => format!("; {n} alternatives shown after {ms:.0} ms"),
            _ => String::new(),
        };
        format!(
            "{} stops, {:.1} km, {:.0} min, {} points, {} steps; shown after {:.0} ms (trip {:.1} ms, directions {:.1} ms){alts}",
            self.stops,
            self.km,
            self.minutes,
            self.points,
            self.steps,
            self.latency_ms,
            self.trip_ms,
            self.directions_ms
        )
    }
}

#[derive(Debug, Serialize)]
pub struct SearchReport {
    /// From the pack opening to the search readers being ready.
    pub open_ms: Option<f64>,
    /// The first keystroke's latency, which may wait for the readers to open.
    pub first_keystroke_ms: Option<f64>,
    /// Every keystroke's latency, from the keystroke to the frame showing its suggestions.
    pub latency_ms: Summary,
    /// What the search readers hold at the end, and have read.
    pub heap_bytes: usize,
    pub bytes_read: u64,
}

#[derive(Debug, Serialize)]
pub struct SearchStepReport {
    pub query: String,
    /// Keystrokes answered (the query's length, unless one timed out).
    pub keystrokes: usize,
    pub latency_ms: Summary,
    /// Time in `find`, part of the latency.
    pub find_ms: Summary,
    /// Enter: from the key to the results.
    pub enter_ms: Option<f64>,
    /// The first result, as "name, context".
    pub top: Option<String>,
    /// After jumping to the first result, until that view was fully detailed.
    pub pick_detail_ms: Option<f64>,
    /// From the last keystroke to each search level's suggestions being shown.
    pub levels_ms: Vec<f64>,
    /// The step's expected name, and when (and at which level) a suggestion of that name
    /// was first shown after the last keystroke.
    pub expect: Option<String>,
    pub expected_ms: Option<f64>,
    pub expected_level: Option<usize>,
    pub timed_out: bool,
    /// The pack has no search.
    pub skipped: bool,
}

#[derive(Debug, Serialize)]
pub struct RouteStepReport {
    pub shown: Option<RouteReport>,
    /// *Optimize stop order* on the route shown (step 4.11).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub optimized: Option<OptimizeReport>,
    pub error: Option<String>,
    pub timed_out: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct OptimizeReport {
    /// From pressing the button to the frame showing the reordered route.
    pub latency_ms: f64,
    /// Time in the worker's matrix and order, part of the latency.
    pub order_ms: f64,
    /// The reordered route.
    pub km: f64,
    pub minutes: f64,
}

#[derive(Debug, Serialize)]
pub struct HereStepReport {
    /// From the request to the frame showing the panel.
    pub latency_ms: Option<f64>,
    /// Time in `whats_here`, part of the latency.
    pub find_ms: Option<f64>,
    pub answer: Option<HereAnswerLog>,
    pub error: Option<String>,
    pub timed_out: bool,
    /// The pack has no search.
    pub skipped: bool,
}

#[derive(Debug, Serialize)]
pub struct TilesReport {
    pub loaded: usize,
    pub absent: usize,
    pub errors: usize,
    /// From the first request to the decoded tile being ready.
    pub latency_ms: Summary,
    /// Index lookup and record read.
    pub read_ms: Summary,
    pub decode_ms: Summary,
    /// Tessellation and upload to the GPU, for tiles that have data.
    pub prepare_ms: Summary,
}

#[derive(Debug, Serialize)]
pub struct StepReport {
    pub label: String,
    pub kind: &'static str,
    pub seconds: f64,
    pub frames: usize,
    /// Frames that drew a fallback or nothing for some visible tile.
    pub incomplete_frames: usize,
    pub interval_ms: Summary,
    pub cpu_ms: Summary,
    /// After the move, until the view was fully detailed (steps that wait).
    pub detail_ms: Option<f64>,
    pub timed_out: bool,
    pub tiles: usize,
    pub bytes_read: u64,
    /// RSS when the step ended.
    pub rss_end: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search: Option<SearchStepReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub here: Option<HereStepReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route: Option<RouteStepReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub print: Option<PrintReport>,
}

/// A printout to PDF in the running app (step 4.11): memory is the process's.
#[derive(Debug, Serialize)]
pub struct PrintReport {
    pub page: String,
    pub dpi: f64,
    /// From opening the print page to its preview shown.
    pub preview_ms: Option<f64>,
    /// From pressing *Save as PDF* to the file written.
    pub output_ms: Option<f64>,
    pub pdf_bytes: Option<u64>,
    /// RSS before the page opened, with the preview shown, at its highest during the
    /// printout, and a few seconds after it.
    pub rss_before: Option<u64>,
    pub rss_preview: Option<u64>,
    pub rss_peak: Option<u64>,
    pub rss_after: Option<u64>,
    pub error: Option<String>,
}

impl Report {
    /// A few lines for the terminal.
    pub fn summary(&self) -> String {
        let mib = |b: u64| b as f64 / (1024.0 * 1024.0);
        let mut s = format!(
            "{}: {} frames, interval p50 {:.2} / p95 {:.2} / max {:.1} ms, cpu p95 {:.2} ms, labels p95 {:.2} ms\n\
             start-up {} ms; tiles {} loaded + {} absent, {} errors; latency p50 {:.2} / p95 {:.2} ms, read p95 {:.2} ms, decode p95 {:.2} ms, prepare p95 {:.2} ms\n\
             {:.1} MiB read; RSS peak {} MiB; GPU allocator {} MiB; tile slabs peak {} MiB; cache peak {:.1} MiB of {:.0}; idle CPU {}% ({} frames)\n",
            self.flight,
            self.frames,
            self.interval_ms.p50,
            self.interval_ms.p95,
            self.interval_ms.max,
            self.cpu_ms.p95,
            self.labels_ms.p95,
            self.startup_ms.map_or("-".into(), |v| format!("{v:.0}")),
            self.tiles.loaded,
            self.tiles.absent,
            self.tiles.errors,
            self.tiles.latency_ms.p50,
            self.tiles.latency_ms.p95,
            self.tiles.read_ms.p95,
            self.tiles.decode_ms.p95,
            self.tiles.prepare_ms.p95,
            mib(self.bytes_read),
            self.rss_peak
                .map_or("-".into(), |v| format!("{:.0}", mib(v))),
            match (self.gpu_allocated, self.gpu_reserved) {
                (Some(a), Some(r)) => format!("{:.0} of {:.0}", mib(a), mib(r)),
                _ => "-".into(),
            },
            self.tile_slabs_peak
                .map_or("-".into(), |v| format!("{:.0}", mib(v))),
            mib(self.cache_peak as u64),
            mib(self.cache_budget as u64),
            self.idle_cpu_percent
                .map_or("-".into(), |v| format!("{v:.2}")),
            self.idle_frames,
        );
        if let Some(sr) = &self.search {
            s += &format!(
                "search: opened in {} ms, first keystroke {} ms; {} keystrokes, latency p50 {:.1} / p95 {:.1} / max {:.1} ms; readers hold {:.1} MiB, {:.1} MiB read\n",
                sr.open_ms.map_or("-".into(), |v| format!("{v:.0}")),
                sr.first_keystroke_ms
                    .map_or("-".into(), |v| format!("{v:.1}")),
                sr.latency_ms.count,
                sr.latency_ms.p50,
                sr.latency_ms.p95,
                sr.latency_ms.max,
                mib(sr.heap_bytes as u64),
                mib(sr.bytes_read),
            );
        }
        if let Some(r) = &self.route {
            s += &format!("route: {}\n", r.line());
        }
        if let Some(h) = &self.here_ms {
            s += &format!(
                "what's here: {} asked, latency p50 {:.1} / p95 {:.1} / max {:.1} ms\n",
                h.count, h.p50, h.p95, h.max
            );
        }
        for st in &self.steps {
            let detail = match (st.detail_ms, st.timed_out) {
                (Some(d), _) => format!(", detailed in {d:.0} ms"),
                (None, true) => ", timed out".into(),
                _ => String::new(),
            };
            let rss = st
                .rss_end
                .map_or(String::new(), |v| format!(", RSS {:.0} MiB", mib(v)));
            s += &format!(
                "  {:<28} {:>5} frames, p95 {:>6.2} ms, {:>4} tiles{detail}{rss}\n",
                st.label, st.frames, st.interval_ms.p95, st.tiles
            );
            if let Some(q) = &st.search {
                s += &match (q.skipped, &q.top) {
                    (true, _) => "    search skipped: the pack has no search\n".to_string(),
                    (false, top) => format!(
                        "    \"{}\": {} keystrokes, p95 {:.1} / max {:.1} ms (find p95 {:.1}), levels at {} ms{}, Enter {} ms → {}{}\n",
                        q.query,
                        q.keystrokes,
                        q.latency_ms.p95,
                        q.latency_ms.max,
                        q.find_ms.p95,
                        q.levels_ms
                            .iter()
                            .map(|v| format!("{v:.0}"))
                            .collect::<Vec<_>>()
                            .join(" / "),
                        match (&q.expect, q.expected_ms, q.expected_level) {
                            (Some(_), Some(v), Some(l)) =>
                                format!(", expected at {v:.0} ms (level {l})"),
                            (Some(_), _, _) => ", expected not found".into(),
                            _ => String::new(),
                        },
                        q.enter_ms.map_or("-".into(), |v| format!("{v:.1}")),
                        top.as_deref().unwrap_or("nothing"),
                        if q.timed_out { " (timed out)" } else { "" },
                    ),
                };
            }
            if let Some(r) = &st.route {
                s += &match (&r.shown, &r.error) {
                    (Some(shown), _) => format!("    route: {}\n", shown.line()),
                    (None, Some(e)) => format!("    route: {e}\n"),
                    (None, None) => "    route: timed out\n".to_string(),
                };
                if let Some(o) = &r.optimized {
                    s += &format!(
                        "    optimized: {:.1} km, {:.0} min; shown after {:.0} ms (order {:.1} ms)\n",
                        o.km, o.minutes, o.latency_ms, o.order_ms
                    );
                } else if let (Some(e), Some(_)) = (&r.error, &r.shown) {
                    s += &format!("    {e}\n");
                }
            }
            if let Some(p) = &st.print {
                let m = |v: Option<u64>| v.map_or("-".into(), |v| format!("{:.0}", mib(v)));
                s += &match &p.error {
                    Some(e) => format!("    print {}: {e}\n", p.page),
                    None => format!(
                        "    print {} at {:.0} dpi: preview {} ms, PDF {} ms ({} KiB); RSS MiB before {}, preview {}, peak {}, after {}\n",
                        p.page,
                        p.dpi,
                        p.preview_ms.map_or("-".into(), |v| format!("{v:.0}")),
                        p.output_ms.map_or("-".into(), |v| format!("{v:.0}")),
                        p.pdf_bytes.map_or(0, |b| b / 1024),
                        m(p.rss_before),
                        m(p.rss_preview),
                        m(p.rss_peak),
                        m(p.rss_after),
                    ),
                };
            }
            if let Some(h) = &st.here {
                s += &match (h.skipped, &h.answer) {
                    (true, _) => "    what's here skipped: the pack has no search\n".to_string(),
                    (false, Some(a)) => format!(
                        "    what's here: {} ms (whats_here {} ms) → {}; street {}; nearby {}; in {}\n",
                        h.latency_ms.map_or("-".into(), |v| format!("{v:.1}")),
                        h.find_ms.map_or("-".into(), |v| format!("{v:.1}")),
                        a.address.as_deref().unwrap_or("no address"),
                        a.street.as_deref().unwrap_or("none"),
                        a.nearby
                            .iter()
                            .take(3)
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(" / "),
                        a.areas.join(", "),
                    ),
                    (false, None) => format!(
                        "    what's here: {}\n",
                        h.error.as_deref().unwrap_or(if h.timed_out {
                            "timed out"
                        } else {
                            "no answer"
                        })
                    ),
                };
            }
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_quantiles() {
        let mut v: Vec<f64> = (1..=100).map(f64::from).rev().collect();
        let s = Summary::of(&mut v);
        assert_eq!((s.count, s.p50, s.p95, s.max), (100, 51.0, 95.0, 100.0));
        assert_eq!(Summary::of(&mut []).count, 0);
    }

    #[test]
    fn flights_in_repo_parse() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("bench");
        let mut n = 0;
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.extension().is_some_and(|x| x == "toml") {
                Flight::load(&p).unwrap();
                n += 1;
            }
        }
        assert!(n >= 2);
    }

    fn sample(detailed: bool) -> FrameSample {
        FrameSample {
            interval: Duration::from_millis(5),
            cpu: Duration::from_millis(1),
            detailed,
            cache_bytes: 10,
            labels: Duration::ZERO,
            bytes_read: 100,
        }
    }

    #[test]
    fn jump_waits_for_detail_then_flies() {
        let flight: Flight = toml::from_str(
            r#"
            name = "t"
            [[step]]
            lat = 40.0
            lon = -83.0
            zoom = 8
            [[step]]
            zoom = 9
            seconds = 0.01
            "#,
        )
        .unwrap();
        let mut b = Bench::new(flight, "t.toml".into(), "t.json".into(), Instant::now());
        let mut cam = Camera::new([0.0, 0.0, 1.0, 1.0]);
        b.start(&mut cam, 0);
        assert_eq!(cam.zoom, 8.0);
        let ctx = ReportContext {
            pack: "p".into(),
            adapter: "a".into(),
            pixels_per_point: 1.0,
            cache_budget: 100,
            search: None,
        };
        // Move → wait; two frames not detailed, then detailed → hold → next step.
        for d in [false, false, false, true, true] {
            assert!(b.end_frame(sample(d), &mut cam, &ctx).is_none());
        }
        assert_eq!(b.step, 1);
        std::thread::sleep(Duration::from_millis(20));
        assert!(b.end_frame(sample(true), &mut cam, &ctx).is_none());
        assert_eq!(cam.zoom, 9.0);
        let r = b.end_frame(sample(true), &mut cam, &ctx).unwrap();
        assert!(r.startup_ms.is_some());
        assert_eq!(r.steps[0].incomplete_frames, 3);
        assert!(r.steps[0].detail_ms.is_some() && !r.steps[0].timed_out);
        assert_eq!(r.frames, 7);
        assert!(b.end_frame(sample(true), &mut cam, &ctx).is_none());
        serde_json::to_string(&r).unwrap();
    }

    #[test]
    fn search_step_types_then_jumps_to_the_result() {
        let flight: Flight = toml::from_str(
            r#"
            name = "t"
            [[step]]
            lat = 40.0
            lon = -83.0
            zoom = 8
            search = "Ohi"
            expect = "ohio"
            keystroke_ms = 0
            "#,
        )
        .unwrap();
        let mut b = Bench::new(flight, "t.toml".into(), "t.json".into(), Instant::now());
        let mut cam = Camera::new([0.0, 0.0, 1.0, 1.0]);
        b.start(&mut cam, 0);
        let ctx = ReportContext {
            pack: "p".into(),
            adapter: "a".into(),
            pixels_per_point: 1.0,
            cache_budget: 100,
            search: Some(SearchContext {
                open_ms: Some(5.0),
                heap_bytes: 1,
                bytes_read: 2,
            }),
        };
        // Move, then wait for detail, then type.
        assert!(b.search_key(true).is_none());
        b.end_frame(sample(true), &mut cam, &ctx);
        b.end_frame(sample(true), &mut cam, &ctx);
        assert_eq!(b.phase, Phase::Type);
        let taken = |submit, level, more| Taken {
            submit,
            latency: Duration::from_millis(12 + 100 * level as u64),
            find: Duration::from_millis(9),
            level,
            more,
            hits: 1,
            first: None,
            names: vec!["Ohio".into()],
        };
        let mut typed = Vec::new();
        for _ in 0..3 {
            let Some(BenchKey::Type(t)) = b.search_key(true) else {
                panic!("expected a keystroke")
            };
            // Nothing more until it is answered.
            assert!(b.search_key(true).is_none());
            b.search_answer(&taken(false, 0, true), None);
            assert!(b.end_frame(sample(true), &mut cam, &ctx).is_none());
            typed.push(t);
        }
        assert_eq!(typed, ["O", "Oh", "Ohi"]);
        // Enter waits for the deeper levels of the last keystroke.
        assert!(b.search_key(true).is_none());
        b.search_answer(&taken(false, 1, true), None);
        assert!(b.search_key(true).is_none());
        b.search_answer(&taken(false, 2, false), None);
        assert_eq!(b.search_key(true), Some(BenchKey::Enter));
        b.search_answer(&taken(true, 3, false), Some(([0.25, 0.375], 11.0)));
        b.end_frame(sample(true), &mut cam, &ctx);
        assert_eq!(
            (cam.center, cam.zoom, b.phase),
            ([0.25, 0.375], 11.0, Phase::Wait)
        );
        b.end_frame(sample(true), &mut cam, &ctx);
        let r = b.end_frame(sample(true), &mut cam, &ctx).unwrap();
        let q = r.steps[0].search.as_ref().unwrap();
        assert_eq!((q.keystrokes, q.enter_ms), (3, Some(312.0)));
        assert_eq!(q.levels_ms, [12.0, 112.0, 212.0]);
        assert_eq!((q.expected_ms, q.expected_level), (Some(12.0), Some(0)));
        assert!(q.pick_detail_ms.is_some() && !q.skipped);
        let sr = r.search.as_ref().unwrap();
        assert_eq!(
            (sr.latency_ms.count, sr.first_keystroke_ms),
            (3, Some(12.0))
        );
        serde_json::to_string(&r).unwrap();
    }
}
