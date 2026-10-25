//! How data reaches GPU buffers.
//!
//! `Queue::write_buffer` makes a staging buffer for every call and drops it after the copy.
//! Intel's Windows GL driver for the HD 4000 seems to keep the memory of every buffer
//! deleted, so RSS grew by about 0.35 MiB a frame (phase1-plan.md, 1.10). By default writes
//! go through a [`StagingBelt`], whose staging buffers are mapped again and reused; with
//! `OMP_UPLOAD=queue` they go through the queue, to compare.

use eframe::egui_wgpu::wgpu;
use wgpu::util::StagingBelt;

pub enum Upload {
    Belt(StagingBelt),
    Queue,
}

impl Upload {
    /// A belt with chunks of `chunk` bytes, unless `OMP_UPLOAD=queue`.
    pub fn new(device: &wgpu::Device, chunk: u64) -> Self {
        match std::env::var("OMP_UPLOAD").as_deref() {
            Ok("queue") => Self::Queue,
            _ => Self::Belt(StagingBelt::new(device.clone(), chunk)),
        }
    }

    /// Copies `data`, whose length is a multiple of 4, to `target` at `offset`; with a belt,
    /// the copy is recorded in `encoder`.
    pub fn write(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::Buffer,
        offset: u64,
        data: &[u8],
    ) {
        let Some(size) = wgpu::BufferSize::new(data.len() as u64) else {
            return;
        };
        match self {
            Self::Belt(b) => b
                .write_buffer(encoder, target, offset, size)
                .copy_from_slice(data),
            Self::Queue => queue.write_buffer(target, offset, data),
        }
    }

    /// Call once the writes for `encoder` are done, before it is submitted.
    pub fn finish(&mut self, encoder: &wgpu::CommandEncoder) {
        if let Self::Belt(b) = self {
            b.finish_and_recall_on_submit(encoder);
        }
    }
}
