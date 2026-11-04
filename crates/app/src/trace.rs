//! Start-up diagnostics for the E6430 crash hunt (phase5-plan.md 5.14). Off unless `OMP_TRACE`
//! is set; then the log carries milliseconds since the start, the first frames are marked, and
//! a panic prints its thread and a backtrace before the process ends (the backtrace names
//! functions only in a build with symbols: `strip = false`, and the `.pdb` beside the exe on
//! Windows). `OMP_SURVIVE` (see [`survive`]) keeps wgpu's uncaptured errors from panicking.

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

/// `OMP_SURVIVE`: wgpu's errors that nothing captures are logged instead of panicking, so a
/// failed `Surface::configure` shows whether the next frame recovers (5.14, item 5). Only
/// errors after the app is created are caught; the first configure runs before that.
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
    mark("OMP_SURVIVE: uncaptured-error handler set");
}
