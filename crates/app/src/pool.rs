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
use std::sync::{Arc, Mutex, MutexGuard};

use eframe::egui_wgpu::wgpu;

/// Size of a slab; a larger upload gets a slab of its own size.
pub const SLAB: u64 = 8 << 20;
/// Offsets are kept to this, which covers vertex, index and copy alignment.
const ALIGN: u64 = 16;
/// Frames a range given back waits before it is reused.
const HOLD: u64 = 3;

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
