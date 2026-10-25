//! Worker threads that read, decode and prepare tiles (tessellate and upload them) off the
//! UI thread.
//!
//! The UI hands over the tiles it still needs every frame, in priority order, and that list
//! replaces the queue: newest requests come first and tiles the view has moved away from are
//! dropped before they are read.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use omp_core::tiles::{Tile, TileReader, TileTiming};

use crate::tiles::TileId;

/// A finished request.
pub struct Done<T> {
    pub id: TileId,
    /// `Ok(None)`: the pack has no such tile, which means it is empty.
    pub result: io::Result<Option<T>>,
    pub timing: TileTiming,
    /// Turning the decoded tile into `T`.
    pub prepare: Duration,
    /// From the first request to the result being ready.
    pub latency: Duration,
}

#[derive(Default)]
struct Queue {
    wanted: VecDeque<(TileId, Instant)>,
    in_flight: HashSet<TileId>,
    quit: bool,
}

struct Shared {
    queue: Mutex<Queue>,
    ready: Condvar,
}

pub struct Loader<T> {
    shared: Arc<Shared>,
    rx: Receiver<Done<T>>,
    threads: Vec<JoinHandle<()>>,
    /// When each queued tile was first asked for, so latency counts the time spent queued.
    asked: HashMap<TileId, Instant>,
}

/// A worker count that leaves the UI thread a core.
pub fn default_threads() -> usize {
    std::thread::available_parallelism().map_or(2, |n| n.get().saturating_sub(1).clamp(2, 6))
}

impl<T: Send + 'static> Loader<T> {
    /// Starts `threads` workers, which turn each decoded tile into a `T` with `prepare`.
    /// `wake` is called after each finished tile, to repaint.
    pub fn new(
        reader: Arc<TileReader>,
        threads: usize,
        prepare: impl Fn(TileId, Tile) -> T + Send + Sync + 'static,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue::default()),
            ready: Condvar::new(),
        });
        let (tx, rx) = channel();
        let wake = Arc::new(wake);
        let prepare = Arc::new(prepare);
        let threads = (0..threads.max(1))
            .map(|i| {
                let (shared, reader, tx, wake, prepare) = (
                    shared.clone(),
                    reader.clone(),
                    tx.clone(),
                    wake.clone(),
                    prepare.clone(),
                );
                std::thread::Builder::new()
                    .name(format!("tile-loader-{i}"))
                    .spawn(move || work(&shared, &reader, &*prepare, &tx, &*wake))
                    .expect("spawn tile loader")
            })
            .collect();
        Self {
            shared,
            rx,
            threads,
            asked: HashMap::new(),
        }
    }

    /// Replaces the queue with `wanted`, highest priority first. Tiles being read finish anyway.
    pub fn request(&mut self, wanted: &[TileId]) {
        let now = Instant::now();
        let asked: HashMap<_, _> = wanted
            .iter()
            .map(|&id| (id, self.asked.get(&id).copied().unwrap_or(now)))
            .collect();
        let mut q = self.shared.queue.lock().unwrap();
        q.wanted.clear();
        for &id in wanted {
            if !q.in_flight.contains(&id) {
                q.wanted.push_back((id, asked[&id]));
            }
        }
        let any = !q.wanted.is_empty();
        drop(q);
        self.asked = asked;
        if any {
            self.shared.ready.notify_all();
        }
    }

    /// Results that have arrived since the last call.
    pub fn poll(&mut self) -> impl Iterator<Item = Done<T>> + '_ {
        self.rx.try_iter()
    }

    /// Tiles queued and tiles being read.
    pub fn pending(&self) -> (usize, usize) {
        let q = self.shared.queue.lock().unwrap();
        (q.wanted.len(), q.in_flight.len())
    }
}

impl<T> Drop for Loader<T> {
    fn drop(&mut self) {
        self.shared.queue.lock().unwrap().quit = true;
        self.shared.ready.notify_all();
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

fn work<T>(
    shared: &Shared,
    reader: &TileReader,
    prepare: &dyn Fn(TileId, Tile) -> T,
    tx: &Sender<Done<T>>,
    wake: &dyn Fn(),
) {
    loop {
        let (id, asked) = {
            let mut q = shared.queue.lock().unwrap();
            loop {
                if q.quit {
                    return;
                }
                if let Some(item) = q.wanted.pop_front() {
                    q.in_flight.insert(item.0);
                    break item;
                }
                q = shared.ready.wait(q).unwrap();
            }
        };
        let (result, timing) = match reader.tile_timed(id.z, id.x, id.y) {
            Ok((t, timing)) => (Ok(t), timing),
            Err(e) => (Err(e), TileTiming::default()),
        };
        let t0 = Instant::now();
        let result = result.map(|t| t.map(|t| prepare(id, t)));
        let done = Done {
            id,
            result,
            timing,
            prepare: t0.elapsed(),
            latency: asked.elapsed(),
        };
        // Sent before leaving `in_flight`, so the UI never asks for a tile it is about to get.
        let sent = tx.send(done).is_ok();
        shared.queue.lock().unwrap().in_flight.remove(&id);
        if !sent {
            return;
        }
        wake();
    }
}
