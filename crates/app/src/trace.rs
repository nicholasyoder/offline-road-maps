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

/// `OMP_WINDOW=WxH`: the size the bench opens its window with instead of the flight's.
pub fn window_override() -> Option<[f32; 2]> {
    let v = std::env::var("OMP_WINDOW").ok()?;
    let (w, h) = v.split_once(['x', 'X'])?;
    Some([w.trim().parse().ok()?, h.trim().parse().ok()?])
}

/// Logs the window's size and position when they change (the first few times), to see which
/// resizes the system makes after the window opens.
pub fn window(ctx: &egui::Context) {
    use std::sync::Mutex;
    static LAST: Mutex<Option<String>> = Mutex::new(None);
    static CHANGES: AtomicU32 = AtomicU32::new(0);
    if START.get().is_none() || CHANGES.load(Ordering::Relaxed) >= 20 {
        return;
    }
    let (inner, outer, mon) = ctx.input(|i| {
        let v = i.viewport();
        (v.inner_rect, v.outer_rect, v.monitor_size)
    });
    let now = format!(
        "inner {inner:?}, outer {outer:?}, monitor {mon:?}, ppp {}",
        ctx.pixels_per_point()
    );
    if let Ok(mut last) = LAST.lock()
        && last.as_deref() != Some(now.as_str())
    {
        CHANGES.fetch_add(1, Ordering::Relaxed);
        mark(&format!("window: {now}"));
        *last = Some(now);
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

/// `OMP_SURVIVE`: replaces wgpu's default handler for errors nothing captures (which panics)
/// with one that logs them. A diagnostic only (5.14): after a failed `Surface::configure` the
/// surface keeps its old size while egui_wgpu has already resized its multisample view, and
/// the next frame fails with "Attachments have differing sizes", so this is not a fix.
/// Errors before `App::new` (the first configure) are not reached.
pub fn survive(device: &eframe::wgpu::Device) {
    if std::env::var_os("OMP_SURVIVE").is_none() {
        return;
    }
    device.on_uncaptured_error(std::sync::Arc::new(|e: eframe::wgpu::Error| {
        eprintln!(
            "[trace +{} ms] wgpu error survived: {e}",
            elapsed_ms().unwrap_or(0)
        );
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
