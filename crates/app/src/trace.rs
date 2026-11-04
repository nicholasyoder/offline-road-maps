//! Start-up diagnostics for the E6430 crash hunt (phase5-plan.md 5.14). Off unless `OMP_TRACE`
//! is set; then the log carries milliseconds since the start, the first frames are marked, and
//! a panic prints its thread and a backtrace before the process ends (the backtrace names
//! functions only in a build with symbols: `strip = false`, and the `.pdb` beside the exe on
//! Windows). [`on_errors`] and `OMP_SURVIVE` deal with wgpu's uncaptured errors.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

static START: OnceLock<Instant> = OnceLock::new();
static FRAMES: AtomicU32 = AtomicU32::new(0);

/// Frames marked in the log after the start.
const MARKED_FRAMES: u32 = 12;

pub fn enabled() -> bool {
    std::env::var_os("OMP_TRACE").is_some()
}

/// Milliseconds since `init`, or `None` when tracing is off.
pub fn elapsed_ms() -> Option<u128> {
    START.get().map(|s| s.elapsed().as_millis())
}

/// Called first thing in `main`; installs the panic hook when tracing.
pub fn init(started: Instant) {
    if !enabled() {
        return;
    }
    let _ = START.set(started);
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let t = std::thread::current();
        eprintln!(
            "[trace +{} ms] PANIC on thread {:?}: {info}\n{}",
            elapsed_ms().unwrap_or(0),
            t.name().unwrap_or("unnamed"),
            std::backtrace::Backtrace::force_capture()
        );
        previous(info);
    }));
    mark("main");
}

pub fn mark(label: &str) {
    if let Some(ms) = elapsed_ms() {
        let t = std::thread::current();
        eprintln!(
            "[trace +{ms} ms] {label} ({})",
            t.name().unwrap_or("unnamed")
        );
    }
}

/// Called at the start of every frame; marks the first few.
pub fn frame() {
    if START.get().is_none() {
        return;
    }
    let n = FRAMES.fetch_add(1, Ordering::Relaxed);
    if n < MARKED_FRAMES {
        mark(&format!("frame {n}"));
    }
}

/// The message of the error wgpu raises when `Surface::configure`'s wait for the GPU runs out
/// (the GL backend gives up after about 2 s; the E6430's first frames take longer).
const IDLE_WAIT: &str = "Failed to wait for GPU to come idle";

/// Replaces wgpu's default handler for errors nothing captures, which panics (5.14). The
/// failed wait of a surface configure is logged and the frame goes on: the configure is
/// skipped, the surface keeps its size and the next resize or frame configures it again, as
/// the E6430 run with `OMP_SURVIVE` showed. Any other error panics as before, unless
/// `OMP_SURVIVE` is set (everything is logged). `OMP_STRICT` keeps wgpu's default for all, to
/// reproduce the crash. Errors before `App::new` (the first configure) are not reached.
pub fn on_errors(device: &eframe::wgpu::Device) {
    if std::env::var_os("OMP_STRICT").is_some() {
        return;
    }
    let all = std::env::var_os("OMP_SURVIVE").is_some();
    device.on_uncaptured_error(std::sync::Arc::new(move |e: eframe::wgpu::Error| {
        let text = e.to_string();
        // The message sits in the error's source chain, so look at the debug text too.
        if all || text.contains(IDLE_WAIT) || format!("{e:?}").contains(IDLE_WAIT) {
            eprintln!(
                "[trace +{} ms] wgpu error tolerated: {e}",
                elapsed_ms().unwrap_or(0)
            );
        } else {
            panic!("wgpu error: {e}");
        }
    }));
}

/// `OMP_TRACE_GPU`: at the start of each of the first frames, waits until the GPU is idle and
/// logs how long that took and the window's size, to see what the first frames cost on a slow
/// GL driver. Changes the timing (the frames no longer overlap), so it is a separate switch.
pub fn gpu_wait(frame: &eframe::Frame, ctx: &egui::Context) {
    static ON: OnceLock<bool> = OnceLock::new();
    if !*ON.get_or_init(|| std::env::var_os("OMP_TRACE_GPU").is_some())
        || FRAMES.load(Ordering::Relaxed) > MARKED_FRAMES
    {
        return;
    }
    let Some(rs) = frame.wgpu_render_state() else {
        return;
    };
    let t = Instant::now();
    let r = rs.device.poll(eframe::wgpu::PollType::wait_indefinitely());
    let rect = ctx.input(|i| i.viewport().inner_rect);
    mark(&format!(
        "GPU idle wait {} ms ({r:?}); inner {rect:?}, ppp {}",
        t.elapsed().as_millis(),
        ctx.pixels_per_point()
    ));
}
