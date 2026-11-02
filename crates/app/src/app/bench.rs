//! What the bench ([`crate::bench`]) asks of the window's frame, and what it is told back:
//! its stops and the answers of routing and *Optimize*, the print page it opens, and the
//! frame's sample and the report at the end.

use std::time::Duration;

use eframe::egui;

use super::{App, MapState};
use crate::bench::{
    Bench, FrameSample, OptimizeReport, Poll, ReportContext, RouteReport, SearchContext,
};
use crate::printui::{self, PrintUi};
use crate::routing::{Place, RouteUi, StopAction};
use crate::search::Status;
use crate::side::Page;

/// The bench's stops, set as the right-click menu would: the first as the start, the last
/// as the destination. Replaces any stops there were.
pub(super) fn set_stops(routing: &mut RouteUi, stops: &[crate::camera::Vec2]) {
    if routing.has_stops() {
        routing.apply(StopAction::Clear, Place::at([0.0, 0.0]));
    }
    let last = stops.len().saturating_sub(1);
    for (i, &w) in stops.iter().enumerate() {
        let a = match i {
            0 => StopAction::From,
            i if i == last => StopAction::To,
            _ => StopAction::Add,
        };
        routing.apply(a, Place::at(w));
    }
}

/// After the route page has taken its answers: tells the bench of a route, *Optimize* or
/// alternatives found, and asks for the next. `waiting` and `looking` are as they were
/// before the answers were taken.
pub(super) fn routing(b: &mut Bench, m: &mut MapState, waiting: bool, looking: bool) {
    if looking
        && !m.routing.looking()
        && let Some(t) = m.routing.alt_taken
    {
        b.alternatives_shown(m.routing.found(), t.latency.as_secs_f64() * 1e3);
    }
    if waiting && m.routing.waiting.is_none() {
        let r = &m.routing;
        if b.optimize_pending() {
            b.optimize_answer(match (&r.last, &r.route) {
                (Some(t), Some(s)) => Ok(OptimizeReport {
                    latency_ms: 0.0,
                    order_ms: t.optimize.as_secs_f64() * 1e3,
                    km: s.trip.metres() / 1000.0,
                    minutes: s.trip.time_s() / 60.0,
                }),
                _ => Err("no route"),
            });
        } else {
            b.route_answer(match (&r.last, &r.route, &r.error) {
                (Some(t), Some(s), _) => Ok(RouteReport {
                    stops: s.trip.snaps.len(),
                    latency_ms: t.latency.as_secs_f64() * 1e3,
                    trip_ms: t.trip.as_secs_f64() * 1e3,
                    directions_ms: t.directions.as_secs_f64() * 1e3,
                    steps: s.steps.len(),
                    km: s.trip.metres() / 1000.0,
                    minutes: s.trip.time_s() / 60.0,
                    points: t.points,
                    alternatives: None,
                    alternatives_ms: None,
                }),
                (_, _, Some(e)) => Err(e.clone()),
                _ => Err("no route".into()),
            });
        }
        if b.flight.alternatives {
            m.routing.expand_all();
        }
    }
    if b.optimize_ask() {
        let why = m.routing.optimize_blocked();
        m.routing.optimize();
        if m.routing.waiting.is_none() {
            b.optimize_answer(Err(why.unwrap_or("not asked")));
        }
    }
    if b.alternatives_ask() {
        m.routing.expand_all();
    }
}

/// While the print page is shown: its preview and printout, as the bench asks for them.
/// Returns whether the printout is done, so the page closes.
pub(super) fn print(
    b: &mut Bench,
    print: &mut PrintUi,
    ctx: &egui::Context,
    inputs: &printui::Inputs,
) -> bool {
    if b.print_previewing()
        && let Some(r) = print.bench_previewed()
    {
        b.print_previewed(r);
    }
    if let Some((path, dpi)) = b.print_output() {
        print.bench_start(ctx, inputs, path, dpi);
    }
    if b.print_running()
        && let Some(r) = print.bench_finished()
    {
        b.print_done(r);
        return true;
    }
    false
}

impl App {
    /// Opens the print page when the bench asks for it.
    pub(super) fn bench_open_print(&mut self, ctx: &egui::Context) {
        if let (Some(b), Some(m)) = (&mut self.bench, &mut self.map)
            && let Some(kind) = b.print_open()
        {
            // As the Print tab opens it.
            self.print.back = m.page;
            m.page = Page::Print;
            let route = m.routing.route_bounds();
            self.print
                .open(ctx, kind, &m.camera, route, &self.cfg.print);
        }
    }

    /// The end of a frame in the bench: its screenshot, its sample, and the report once the
    /// flight is over. Returns whether the bench wants the next frame at once.
    pub(super) fn bench_frame(
        &mut self,
        ctx: &egui::Context,
        interval: Duration,
        cpu: Duration,
        allocs: Option<(u64, u64)>,
    ) -> bool {
        let mut animate = false;
        if let Some(path) = self.bench.as_mut().and_then(Bench::take_shot) {
            ctx.request_screenshot(move |img| {
                let size = img.size.map(|v| v as u32);
                if let Err(e) = crate::snapshot::write_png(&path, size, img.as_raw()) {
                    eprintln!("could not write {}: {e}", path.display());
                }
            });
        }
        if let (Some(b), Some(m)) = (&mut self.bench, &mut self.map) {
            let sample = FrameSample {
                interval,
                cpu,
                detailed: m.view.detailed,
                cache_bytes: m.cache.bytes(),
                labels: m.view.label_time,
                bytes_read: m.opened.reader.bytes_read(),
                allocs,
            };
            let st = m.search.stats();
            let rc = ReportContext {
                pack: m.opened.first_chunk.display().to_string(),
                adapter: self.adapter.clone(),
                pixels_per_point: ctx.pixels_per_point(),
                cache_budget: m.cache.budget,
                search: (m.search.status() == Status::Ready).then(|| SearchContext {
                    open_ms: st.open.map(|d| d.as_secs_f64() * 1e3),
                    heap_bytes: st.heap_bytes,
                    bytes_read: st.bytes_read,
                }),
            };
            if let Some(report) = b.end_frame(sample, &mut m.camera, &rc) {
                b.start_idle(report);
            }
            if b.measuring() {
                b.idle_wake(
                    ctx.repaint_causes()
                        .iter()
                        .map(|c| format!("{}:{} {}", c.file, c.line, c.reason)),
                );
            }
            match b.poll() {
                Poll::Flying => animate = true,
                Poll::Idle(wait) => ctx.request_repaint_after(wait),
                Poll::Done(mut report) => {
                    if let Some(r) = self
                        .device
                        .as_ref()
                        .and_then(|d| d.generate_allocator_report())
                    {
                        report.gpu_allocated = Some(r.total_allocated_bytes);
                        report.gpu_reserved = Some(r.total_reserved_bytes);
                    }
                    report.tile_slabs_peak = self.pool.as_ref().map(|p| p.bytes().1);
                    eprint!("{}", report.summary());
                    let out = b.out().to_path_buf();
                    match serde_json::to_string_pretty(&report)
                        .map_err(std::io::Error::other)
                        .and_then(|j| std::fs::write(&out, j + "\n"))
                    {
                        Ok(()) => eprintln!("report written to {}", out.display()),
                        Err(e) => eprintln!("could not write {}: {e}", out.display()),
                    }
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        } else if self.bench.is_some() {
            eprintln!("bench: no pack is open");
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        animate
    }
}
