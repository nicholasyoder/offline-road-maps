//! Tile geometry on the GPU, sub-allocated from a few large buffers.
//!
//! Each tile used to get buffers of its own. On Windows that cost far more than the data:
//! DX12 puts every buffer on a 64 KiB boundary, and Intel's GL driver for the HD 4000 never
//! gave deleted buffers' memory back, so RSS grew with every tile loaded (phase1-plan.md,
//! 1.10). Now tiles take ranges of shared slabs, written with `write_buffer` from the loader
//! threads, and give them back when their mesh is dropped.
//!
//! A range given back is reused only [`HOLD`] frames later, so a frame still in flight never
//! sees it overwritten. An empty slab is released when another slab of its kind is empty
//! too, so one spare stays.

use std::ops::Range;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use eframe::egui_wgpu::wgpu;

/// Size of a slab; a larger upload gets a slab of its own size.
pub const SLAB: u64 = 8 << 20;
/// Offsets are kept to this, which covers vertex, index and copy alignment.
const ALIGN: u64 = 16;
/// Frames a range given back waits before it is reused.
const HOLD: u64 = 3;

/// Keeps the loader threads' GPU calls out of the window's frame, from the map's `prepare` to
/// its `paint` (render.rs). egui configures the surface in between, when the window was
/// resized, and wgpu fails that with "Failed to wait for GPU to come idle" if another thread
/// submits meanwhile; on the E6430's GL driver, where the wait is long, that happened at
/// start-up and ended the app (phase5-plan.md 5.10). The loaders also take the GL context for
/// seconds at a time in that wait and wgpu panics a thread that waits for it over a second.
pub struct Gate {
    state: Mutex<GateState>,
    changed: Condvar,
}

#[derive(Default)]
struct GateState {
    closed: bool,
    /// Uploads under way.
    active: u32,
}

/// Longest a loader waits at a closed gate, and the frame for an upload under way to finish:
/// a frame that never reaches its `paint` (no surface texture) must not stop the loaders.
const GATE_WAIT: Duration = Duration::from_millis(100);

pub static GATE: Gate = Gate {
    state: Mutex::new(GateState {
        closed: false,
        active: 0,
    }),
    changed: Condvar::new(),
};

/// An upload under way; dropping it lets the frame go on.
struct Upload(&'static Gate);

impl Gate {
    fn lock(&self) -> MutexGuard<'_, GateState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn enter(&'static self) -> Upload {
        let st = self.lock();
        let (mut st, _) = self
            .changed
            .wait_timeout_while(st, GATE_WAIT, |s| s.closed)
            .unwrap_or_else(|e| e.into_inner());
        st.active += 1;
        Upload(self)
    }

    /// Closes the gate and waits for the uploads under way. Called by the frame, before it
    /// can configure the surface.
    pub fn close(&self) {
        let mut st = self.lock();
        st.closed = true;
        let _ = self
            .changed
            .wait_timeout_while(st, 2 * GATE_WAIT, |s| s.active > 0)
            .unwrap_or_else(|e| e.into_inner());
    }

    /// Lets the loaders go on.
    pub fn open(&self) {
        self.lock().closed = false;
        self.changed.notify_all();
    }
}

impl Drop for Upload {
    fn drop(&mut self) {
        self.0.lock().active -= 1;
        self.0.changed.notify_all();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Vertex,
    Index,
}

/// The free ranges of one slab, sorted and merged.
#[derive(Debug)]
struct Free {
    ranges: Vec<Range<u64>>,
    size: u64,
}

impl Free {
    fn new(size: u64) -> Self {
        Self {
            ranges: std::iter::once(0..size).collect(),
            size,
        }
    }

    /// First fit.
    fn take(&mut self, len: u64) -> Option<Range<u64>> {
        let i = self.ranges.iter().position(|r| r.end - r.start >= len)?;
        let r = &mut self.ranges[i];
        let start = r.start;
        r.start += len;
        if r.is_empty() {
            self.ranges.remove(i);
        }
        Some(start..start + len)
    }

    fn give(&mut self, r: Range<u64>) {
        let i = self.ranges.partition_point(|f| f.start < r.start);
        let mut r = r;
        if i < self.ranges.len() && self.ranges[i].start == r.end {
            r.end = self.ranges.remove(i).end;
        }
        if i > 0 && self.ranges[i - 1].end == r.start {
            self.ranges[i - 1].end = r.end;
        } else {
            self.ranges.insert(i, r);
        }
    }

    fn is_empty(&self) -> bool {
        self.ranges.len() == 1 && self.ranges[0] == (0..self.size)
    }
}

struct Slab {
    buffer: wgpu::Buffer,
    kind: Kind,
    free: Free,
}

#[derive(Default)]
struct State {
    slabs: Vec<Option<Slab>>,
    /// Ranges given back: the frame, the slab and the range.
    held: Vec<(u64, usize, Range<u64>)>,
    frame: u64,
    bytes: u64,
    peak: u64,
}

pub struct Pool {
    device: wgpu::Device,
    queue: wgpu::Queue,
    state: Mutex<State>,
}

/// A range of a slab, given back when dropped.
pub struct Alloc {
    pool: Arc<Pool>,
    buffer: wgpu::Buffer,
    slab: usize,
    /// In the slab, rounded up to [`ALIGN`].
    range: Range<u64>,
    /// Bytes uploaded.
    len: u64,
}

impl Alloc {
    /// Bytes `r` of the data uploaded.
    pub fn slice(&self, r: Range<u64>) -> wgpu::BufferSlice<'_> {
        let s = self.range.start;
        self.buffer.slice(s + r.start..s + r.end)
    }

    /// All the data uploaded.
    pub fn all(&self) -> wgpu::BufferSlice<'_> {
        self.slice(0..self.len)
    }
}

impl Drop for Alloc {
    fn drop(&mut self) {
        let mut st = self.pool.lock();
        let frame = st.frame;
        st.held.push((frame, self.slab, self.range.clone()));
    }
}

impl Pool {
    pub fn new(device: wgpu::Device, queue: wgpu::Queue) -> Arc<Self> {
        Arc::new(Self {
            device,
            queue,
            state: Mutex::default(),
        })
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Copies `data`, whose length is a multiple of 4, into a free range.
    pub fn upload(self: &Arc<Self>, kind: Kind, data: &[u8]) -> Alloc {
        debug_assert!(data.len().is_multiple_of(4));
        let _gate = GATE.enter();
        let len = (data.len() as u64).next_multiple_of(ALIGN).max(ALIGN);
        let (slab, range, buffer) = {
            let mut st = self.lock();
            let found = st.slabs.iter_mut().enumerate().find_map(|(i, s)| {
                let s = s.as_mut().filter(|s| s.kind == kind)?;
                Some((i, s.free.take(len)?, s.buffer.clone()))
            });
            match found {
                Some(f) => f,
                None => {
                    let size = len.max(SLAB);
                    let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some(match kind {
                            Kind::Vertex => "tile vertex slab",
                            Kind::Index => "tile index slab",
                        }),
                        size,
                        usage: wgpu::BufferUsages::COPY_DST
                            | match kind {
                                Kind::Vertex => wgpu::BufferUsages::VERTEX,
                                Kind::Index => wgpu::BufferUsages::INDEX,
                            },
                        mapped_at_creation: false,
                    });
                    let mut free = Free::new(size);
                    let range = free.take(len).expect("slab fits the upload");
                    let slab = Slab {
                        buffer: buffer.clone(),
                        kind,
                        free,
                    };
                    let i = match st.slabs.iter().position(Option::is_none) {
                        Some(i) => {
                            st.slabs[i] = Some(slab);
                            i
                        }
                        None => {
                            st.slabs.push(Some(slab));
                            st.slabs.len() - 1
                        }
                    };
                    st.bytes += size;
                    st.peak = st.peak.max(st.bytes);
                    (i, range, buffer)
                }
            }
        };
        self.queue.write_buffer(&buffer, range.start, data);
        Alloc {
            pool: self.clone(),
            buffer,
            slab,
            range,
            len: data.len() as u64,
        }
    }

    /// Once a frame: ranges held long enough become free, and spare empty slabs go.
    pub fn end_frame(&self) {
        let mut st = self.lock();
        st.frame += 1;
        let frame = st.frame;
        let held = std::mem::take(&mut st.held);
        let mut touched = Vec::new();
        for (f, i, r) in held {
            if frame < f + HOLD {
                st.held.push((f, i, r));
            } else if let Some(s) = st.slabs[i].as_mut() {
                s.free.give(r);
                touched.push(i);
            }
        }
        for i in touched {
            let Some(s) = &st.slabs[i] else { continue };
            let (kind, size) = (s.kind, s.free.size);
            let spare = |j: usize, t: &Option<Slab>| {
                j != i
                    && t.as_ref()
                        .is_some_and(|t| t.kind == kind && t.free.is_empty())
            };
            if s.free.is_empty() && st.slabs.iter().enumerate().any(|(j, t)| spare(j, t)) {
                st.bytes -= size;
                st.slabs[i] = None;
            }
        }
    }

    /// The GPU has finished all that was drawn, as after an offscreen readback: ranges given
    /// back are free at once. Offscreen drawing has no frames, so without this nothing it
    /// gave back would ever be reused.
    pub fn idle(&self) {
        for _ in 0..HOLD {
            self.end_frame();
        }
    }

    /// Bytes in slabs now, and at most so far.
    pub fn bytes(&self) -> (u64, u64) {
        let st = self.lock();
        (st.bytes, st.peak)
    }
}

#[cfg(test)]
mod tests {
    use super::Free;

    #[test]
    fn free_ranges_take_and_merge() {
        let mut f = Free::new(100);
        let a = f.take(10).unwrap();
        let b = f.take(20).unwrap();
        let c = f.take(30).unwrap();
        assert_eq!((a.clone(), b.clone(), c.clone()), (0..10, 10..30, 30..60));
        assert!(f.take(50).is_none());
        f.give(b);
        assert_eq!(f.ranges, [10..30, 60..100]);
        // First fit reuses the hole.
        assert_eq!(f.take(15).unwrap(), 10..25);
        f.give(10..25);
        f.give(a);
        assert_eq!(f.ranges, [0..30, 60..100]);
        f.give(c);
        assert!(f.is_empty());
    }
}
