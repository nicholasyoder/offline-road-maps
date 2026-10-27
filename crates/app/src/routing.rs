//! The pack's routing readers (phase 3 step 3.3), opened off the UI thread when a pack
//! opens, so the debug overlay shows what they hold. Step 3.6 routes with them on a worker.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use omp_core::pack::Pack;
use omp_core::route;

enum State {
    Opening,
    Ready {
        reader: Box<route::Routing>,
        open: Duration,
    },
    /// No routing sections; the pack's minor version.
    Missing(u16),
    Failed(String),
}

pub struct Routing {
    state: Arc<Mutex<State>>,
}

impl Routing {
    /// Opens the readers on a thread of their own; `wake` asks for a repaint when done.
    pub fn start(first_chunk: PathBuf, wake: impl Fn() + Send + 'static) -> Self {
        let state = Arc::new(Mutex::new(State::Opening));
        let slot = state.clone();
        std::thread::Builder::new()
            .name("routing open".into())
            .spawn(move || {
                let t0 = Instant::now();
                let opened = Pack::open(&first_chunk).map(Arc::new).and_then(|p| {
                    let minor = p.minor;
                    route::Routing::open(p).map(|r| (r, minor))
                });
                *slot.lock().unwrap() = match opened {
                    Ok((Some(reader), _)) => State::Ready {
                        reader: Box::new(reader),
                        open: t0.elapsed(),
                    },
                    Ok((None, minor)) => State::Missing(minor),
                    Err(e) => State::Failed(e.to_string()),
                };
                wake();
            })
            .expect("spawn routing open");
        Self { state }
    }

    /// The overlay's line: how long opening took and what the readers hold and have read.
    pub fn overlay_line(&self) -> String {
        let mib = |b: f64| b / (1024.0 * 1024.0);
        match &*self.state.lock().unwrap() {
            State::Opening => "routing: opening…".into(),
            State::Ready { reader, open } => {
                let h = &reader.header;
                let r = reader.reads();
                format!(
                    "routing: opened in {:.0} ms, {} nodes, {} segments; readers hold {:.1} MiB, {:.1} MiB read",
                    open.as_secs_f64() * 1e3,
                    h.nodes,
                    h.segments,
                    mib(reader.heap_bytes() as f64),
                    mib((r.nodes + r.segments + r.other) as f64)
                )
            }
            State::Missing(minor) if *minor < route::MIN_MINOR => {
                format!("routing: none (format 0.{minor} predates it)")
            }
            State::Missing(_) => "routing: none in this pack".into(),
            State::Failed(e) => format!("routing: failed: {e}"),
        }
    }
}
