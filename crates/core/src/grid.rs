//! The spatial grid (`SGRD`, `docs/format.md`): per z14 cell, the streets with a part in it
//! and whether it has unattached addresses. Cells are Hilbert-ordered, as tiles are.

use std::io;
use std::sync::Arc;

use crate::blocked::{Blocked, Cache};
use crate::geo::{self, P};
use crate::pack::Pack;
use crate::tile::hilbert;
use crate::varint::{Reader, invalid};

pub const SGRD: [u8; 4] = *b"SGRD";
/// The grid's cells are tiles of this zoom.
pub const ZOOM: u8 = 14;
/// Decoded blocks kept, in bytes.
const CACHE_BYTES: usize = 2 << 20;

/// What the grid holds for one cell.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cell {
    /// Street IDs, ascending.
    pub streets: Vec<u32>,
    /// The cell has unattached addresses in `ALSE`.
    pub loose: bool,
}

/// The cell of a point.
pub fn cell_xy(p: P) -> (u32, u32) {
    let s = 32 - u32::from(ZOOM);
    (p[0] >> s, p[1] >> s)
}

/// The cell's Hilbert key, as `first` and the key deltas count it.
pub fn key(x: u32, y: u32) -> u32 {
    hilbert(ZOOM, x, y) as u32
}

fn decode_block(first: u32, raw: &[u8]) -> io::Result<Vec<(u32, Cell)>> {
    let mut r = Reader::new(raw);
    let mut out = Vec::new();
    let (mut k, mut street) = (first, 0u32);
    while !r.at_end() {
        k = k
            .checked_add(r.varint()? as u32)
            .ok_or_else(|| invalid("SGRD key out of range"))?;
        let v = r.varint()?;
        let n = usize::try_from(v >> 1).map_err(|_| invalid("bad SGRD count"))?;
        if n > raw.len() {
            return Err(invalid("SGRD count larger than the data"));
        }
        let mut streets = Vec::with_capacity(n);
        if n > 0 {
            street = street.wrapping_add(r.zvarint()? as u32);
            let mut s = street;
            streets.push(s);
            for _ in 1..n {
                s = s.wrapping_add(r.varint()? as u32);
                streets.push(s);
            }
        }
        out.push((
            k,
            Cell {
                streets,
                loose: v & 1 != 0,
            },
        ));
    }
    Ok(out)
}

pub struct Grid {
    pack: Arc<Pack>,
    sgrd: Blocked,
    blocks: Cache<usize, Vec<(u32, Cell)>>,
}

impl Grid {
    pub fn open(pack: Arc<Pack>) -> io::Result<Option<Self>> {
        let Some(sgrd) = Blocked::open(&pack, SGRD)? else {
            return Ok(None);
        };
        Ok(Some(Self {
            pack,
            sgrd,
            blocks: Cache::new(CACHE_BYTES),
        }))
    }

    /// Cells stored.
    pub fn count(&self) -> u32 {
        self.sgrd.count
    }

    /// Cell `(x, y)` at [`ZOOM`]; empty if not stored.
    pub fn cell(&self, x: u32, y: u32) -> io::Result<Cell> {
        if x >> ZOOM != 0 || y >> ZOOM != 0 {
            return Ok(Cell::default());
        }
        let k = key(x, y);
        let Some(block) = self.sgrd.find(k) else {
            return Ok(Cell::default());
        };
        let cells = self.blocks.get(block, || {
            let (raw, read) = self.sgrd.read(&self.pack, block)?;
            let v = decode_block(self.sgrd.first(block), &raw)?;
            let bytes = v.iter().map(|(_, c)| 40 + c.streets.capacity() * 4).sum();
            Ok((v, bytes, read))
        })?;
        Ok(match cells.binary_search_by_key(&k, |c| c.0) {
            Ok(i) => cells[i].1.clone(),
            Err(_) => Cell::default(),
        })
    }

    /// The cells within `radius_m` of `p` (by their squares), with their coordinates.
    pub fn around(&self, p: P, radius_m: f64) -> io::Result<Vec<((u32, u32), Cell)>> {
        let r = (radius_m / geo::metres_per_unit(p))
            .ceil()
            .min(f64::from(u32::MAX)) as u32;
        let s = 32 - u32::from(ZOOM);
        let lo = |v: u32| v.saturating_sub(r) >> s;
        let hi = |v: u32| v.saturating_add(r) >> s;
        let mut out = Vec::new();
        for y in lo(p[1])..=hi(p[1]) {
            for x in lo(p[0])..=hi(p[0]) {
                let c = self.cell(x, y)?;
                if !c.streets.is_empty() || c.loose {
                    out.push(((x, y), c));
                }
            }
        }
        Ok(out)
    }

    /// Calls `f` for every stored cell in Hilbert order, reading each block once.
    pub fn for_each(&self, mut f: impl FnMut(u32, &Cell)) -> io::Result<()> {
        for b in 0..self.sgrd.blocks() {
            let (raw, _) = self.sgrd.read(&self.pack, b)?;
            for (k, c) in decode_block(self.sgrd.first(b), &raw)? {
                f(k, &c);
            }
        }
        Ok(())
    }

    pub fn heap_bytes(&self) -> usize {
        self.sgrd.heap_bytes() + self.blocks.held()
    }

    pub fn bytes_read(&self) -> u64 {
        self.blocks
            .bytes_read
            .load(std::sync::atomic::Ordering::Relaxed)
    }
}
