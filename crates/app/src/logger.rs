//! Prints what eframe, winit and wgpu log to stderr, so a failed start leaves a trace.
//! `OMP_LOG` sets the level (error, warn, info, debug, trace); the default is warn.

use log::{Level, Log, Metadata, Record};

struct Stderr(Level);

impl Log for Stderr {
    fn enabled(&self, m: &Metadata) -> bool {
        m.level() <= self.0
    }

    fn log(&self, r: &Record) {
        if self.enabled(r.metadata()) {
            eprintln!("[{} {}] {}", r.level(), r.target(), r.args());
        }
    }

    fn flush(&self) {}
}

pub fn init() {
    let level = std::env::var("OMP_LOG")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(Level::Warn);
    if log::set_boxed_logger(Box::new(Stderr(level))).is_ok() {
        log::set_max_level(level.to_level_filter());
    }
}
