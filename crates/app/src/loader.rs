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
        // A tile that makes `prepare` panic is an error like one that cannot be read; the
        // thread goes on, and the tile is not left in flight for good.
        let result = result.and_then(|t| match t {
            None => Ok(None),
            Some(t) => std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| prepare(id, t)))
                .map(Some)
                .map_err(|_| io::Error::other("the tile's data could not be drawn")),
        });
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

#[cfg(test)]
mod tests {
    use super::*;
    use omp_core::pack::{Info, Pack, Writer};
    use omp_core::tile::{Dictionaries, encode_layer};
    use omp_core::tiles::{FrameCompressor, TileWriter};

    /// A pack with an empty tile at 0/0/0 and 1/0/0.
    fn reader(dir: &std::path::Path) -> Arc<TileReader> {
        let dicts = Dictionaries::default();
        let mut w = Writer::create(dir, "t", 1 << 20).unwrap();
        let mut fc = FrameCompressor::new(&dicts, 3).unwrap();
        let mut tw = TileWriter::begin(&mut w, &dicts).unwrap();
        for (z, x, y) in [(0, 0, 0), (1, 0, 0)] {
            let frame = fc.compress(0, z, &encode_layer(&[])).unwrap();
            tw.add(z, x, y, &[(0, &frame[..])]).unwrap();
        }
        tw.finish().unwrap();
        let info = Info {
            pack_id: [1; 16],
            bbox: [0, 0, 1, 1],
            build_time: 0,
            name: "t".into(),
            snapshot: String::new(),
        };
        let first = w.finish(&info).unwrap().remove(0);
        Arc::new(TileReader::open(Pack::open(&first).unwrap()).unwrap())
    }

    fn wait<T: Send + 'static>(l: &mut Loader<T>) -> Done<T> {
        let t = Instant::now();
        loop {
            if let Some(d) = l.poll().next() {
                return d;
            }
            assert!(t.elapsed() < Duration::from_secs(10), "no answer");
            std::thread::yield_now();
        }
    }

    #[test]
    fn a_tile_that_panics_is_an_error_and_the_loader_goes_on() {
        let dir = tempfile::tempdir().unwrap();
        let mut l = Loader::new(
            reader(dir.path()),
            1,
            |id: TileId, _| {
                assert!(id.z == 0, "bad data");
                id.z
            },
            || {},
        );
        l.request(&[TileId { z: 1, x: 0, y: 0 }]);
        let d = wait(&mut l);
        assert!(d.result.is_err());
        // The one thread is still there, and the tile is no longer in flight.
        l.request(&[TileId { z: 0, x: 0, y: 0 }]);
        assert_eq!(wait(&mut l).result.unwrap(), Some(0));
    }
}
