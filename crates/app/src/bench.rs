//! `omp-app --bench <flight.toml>`: flies a fixed path and reports frame times, tile
//! latencies, memory and bytes read as JSON. This is how the phase 1 exit criteria are
//! measured (phase1-plan.md).
//!
//! A flight is a list of steps. Each step moves to a view, either at once (a jump,
//! `seconds = 0`) or linearly in Mercator and zoom over `seconds`; then, if `wait` is set
//! (the default for jumps), waits until the view is fully detailed; then holds for `hold`
//! seconds. Steps run on wall time, so a slow machine draws fewer frames of the same path.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::camera::{Camera, Vec2, lonlat_to_world};
use crate::loader::Done;

/// Longest wait for a view to become fully detailed.
const DETAIL_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Flight {
    pub name: String,
    /// Window size in points.
    #[serde(default = "default_window")]
    pub window: [f32; 2],
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
}

impl Flight {
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let f: Self = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        if f.steps.is_empty() {
            return Err(format!("{}: the flight has no steps", path.display()));
        }
        Ok(f)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    Move,
    Wait,
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
    startup_ms: Option<f64>,
    cache_peak: usize,
    bytes_read_start: u64,
    finished: bool,
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
            startup_ms: None,
            cache_peak: 0,
            bytes_read_start: 0,
            finished: false,
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
        let c = lonlat_to_world(s.lon.unwrap_or(lon), s.lat.unwrap_or(lat));
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
            } else {
                Phase::Hold
            };
            self.phase_start = now;
            return None;
        }
        if self.phase == Phase::Wait {
            if f.detailed {
                log.detail_ms = Some(t * 1e3);
            } else if t < DETAIL_TIMEOUT.as_secs_f64() {
                return None;
            } else {
                log.timed_out = true;
            }
            self.phase = Phase::Hold;
            self.phase_start = now;
            return None;
        }
        if t < step.hold {
            return None;
        }
        if self.step + 1 < self.flight.steps.len() {
            self.begin_step(self.step + 1, cam, now, f.bytes_read);
            return None;
        }
        self.finished = true;
        Some(self.report(f.bytes_read, ctx))
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
            cache_budget: ctx.cache_budget,
            cache_peak: self.cache_peak,
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
    pub cache_budget: usize,
    pub cache_peak: usize,
    pub steps: Vec<StepReport>,
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
}

impl Report {
    /// A few lines for the terminal.
    pub fn summary(&self) -> String {
        let mib = |b: u64| b as f64 / (1024.0 * 1024.0);
        let mut s = format!(
            "{}: {} frames, interval p50 {:.2} / p95 {:.2} / max {:.1} ms, cpu p95 {:.2} ms, labels p95 {:.2} ms\n\
             start-up {} ms; tiles {} loaded + {} absent, {} errors; latency p50 {:.2} / p95 {:.2} ms, read p95 {:.2} ms, decode p95 {:.2} ms, prepare p95 {:.2} ms\n\
             {:.1} MiB read; RSS peak {} MiB; cache peak {:.1} MiB of {:.0}\n",
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
            mib(self.cache_peak as u64),
            mib(self.cache_budget as u64),
        );
        for st in &self.steps {
            let detail = match (st.detail_ms, st.timed_out) {
                (Some(d), _) => format!(", detailed in {d:.0} ms"),
                (None, true) => ", timed out".into(),
                _ => String::new(),
            };
            s += &format!(
                "  {:<28} {:>5} frames, p95 {:>6.2} ms, {:>4} tiles{detail}\n",
                st.label, st.frames, st.interval_ms.p95, st.tiles
            );
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
}
