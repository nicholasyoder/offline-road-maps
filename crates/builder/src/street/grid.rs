//! `SGRD`, the spatial grid (`docs/format.md`): per z14 cell, the streets with a part in it
//! and whether it has unattached addresses, for "what's here" and snapping.

use anyhow::Result;
use omp_core::grid::ZOOM;
use omp_core::tile::hilbert;
use omp_core::varint::{put, zigzag};
use rayon::prelude::*;
use serde::Serialize;

use super::Table;
use super::encode::{Blocks, blocked};
use crate::measure::geom::P;

pub const SGRD: [u8; 4] = omp_core::grid::SGRD;
/// Uncompressed bytes per block, about.
pub const BLOCK: usize = 16 << 10;
/// Other zooms `omp-builder streets` measures the grid at.
pub const ZOOM_MEASURED: &[u8] = &[13, 15];

#[derive(Serialize, Default, Clone)]
pub struct Sizes {
    pub zoom: u8,
    pub cells: u64,
    pub loose_cells: u64,
    /// (cell, street) pairs.
    pub pairs: u64,
    pub streets_per_cell_p50: u64,
    pub streets_per_cell_p99: u64,
    pub streets_per_cell_max: u64,
    pub raw: u64,
    pub bytes: u64,
    pub blocks: u64,
}

/// The cells of zoom `z` that segment `a`–`b` passes through, by walking the grid lines it
/// crosses. A segment through a corner also takes one of the cells beside it.
pub fn segment_cells(a: P, b: P, z: u8, out: &mut Vec<(u32, u32)>) {
    let size = f64::from(1u32 << (32 - u32::from(z)));
    let (x0, y0) = (f64::from(a[0]) / size, f64::from(a[1]) / size);
    let (x1, y1) = (f64::from(b[0]) / size, f64::from(b[1]) / size);
    let (mut cx, mut cy) = (x0.floor() as i64, y0.floor() as i64);
    let (ex, ey) = (x1.floor() as i64, y1.floor() as i64);
    out.push((cx as u32, cy as u32));
    let (dx, dy) = (x1 - x0, y1 - y0);
    let axis = |d: f64, c: i64, o: f64| -> (i64, f64, f64) {
        if d > 0.0 {
            (1, ((c + 1) as f64 - o) / d, 1.0 / d)
        } else if d < 0.0 {
            (-1, (c as f64 - o) / d, -1.0 / d)
        } else {
            (0, f64::INFINITY, f64::INFINITY)
        }
    };
    let (sx, mut tx, ddx) = axis(dx, cx, x0);
    let (sy, mut ty, ddy) = axis(dy, cy, y0);
    let steps = (ex - cx).abs() + (ey - cy).abs();
    for _ in 0..steps {
        if tx < ty {
            cx += sx;
            tx += ddx;
        } else {
            cy += sy;
            ty += ddy;
        }
        out.push((cx as u32, cy as u32));
    }
}

/// A cell's key and its streets, ascending, and whether it has unattached addresses.
type Cell = (u32, Vec<u32>, bool);

fn cells(table: &Table, loose: &[P], z: u8) -> Vec<Cell> {
    let key = |(x, y): (u32, u32)| hilbert(z, x, y) as u32;
    let mut pairs: Vec<(u32, u32)> = table
        .streets
        .par_iter()
        .enumerate()
        .flat_map_iter(|(s, st)| {
            let mut c = Vec::new();
            for part in &st.parts {
                let shift = 32 - u32::from(z);
                c.push((part[0][0] >> shift, part[0][1] >> shift));
                for w in part.windows(2) {
                    segment_cells(w[0], w[1], z, &mut c);
                }
            }
            c.sort_unstable();
            c.dedup();
            c.into_iter().map(move |xy| (key(xy), s as u32))
        })
        .collect();
    pairs.par_sort_unstable();
    pairs.dedup();
    let shift = 32 - u32::from(z);
    let mut loose: Vec<u32> = loose
        .par_iter()
        .map(|p| key((p[0] >> shift, p[1] >> shift)))
        .collect();
    loose.par_sort_unstable();
    loose.dedup();
    let mut out: Vec<Cell> = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < pairs.len() || j < loose.len() {
        let k = match (pairs.get(i), loose.get(j)) {
            (Some(p), Some(&l)) => p.0.min(l),
            (Some(p), None) => p.0,
            (None, Some(&l)) => l,
            (None, None) => unreachable!(),
        };
        let mut streets = Vec::new();
        while let Some(&(pk, s)) = pairs.get(i).filter(|p| p.0 == k) {
            debug_assert_eq!(pk, k);
            streets.push(s);
            i += 1;
        }
        let has_loose = loose.get(j) == Some(&k);
        j += usize::from(has_loose);
        out.push((k, streets, has_loose));
    }
    out
}

fn encode(cells: &[Cell], block: usize) -> (Blocks, u64) {
    let mut out = Vec::new();
    let mut cur: Vec<u8> = Vec::new();
    let (mut first_key, mut prev_key, mut prev_first, mut raw) = (0u32, 0u32, 0u32, 0u64);
    for (k, streets, loose) in cells {
        if cur.len() >= block {
            raw += cur.len() as u64;
            out.push((first_key, std::mem::take(&mut cur)));
        }
        if cur.is_empty() {
            (first_key, prev_key, prev_first) = (*k, *k, 0);
        }
        put(&mut cur, u64::from(k - prev_key));
        put(&mut cur, (streets.len() as u64) << 1 | u64::from(*loose));
        if let Some(&s) = streets.first() {
            put(&mut cur, zigzag(s.wrapping_sub(prev_first) as i32));
            prev_first = s;
            for w in streets.windows(2) {
                put(&mut cur, u64::from(w[1] - w[0]));
            }
        }
        prev_key = *k;
    }
    if !cur.is_empty() {
        raw += cur.len() as u64;
        out.push((first_key, cur));
    }
    (out, raw)
}

/// `SGRD` at zoom `z` (only [`ZOOM`] is written; other zooms are for measuring) and its
/// sizes. `loose` holds the unattached addresses' points.
pub fn section(table: &Table, loose: &[P], z: u8, block: usize) -> Result<(Vec<u8>, Sizes)> {
    let cells = cells(table, loose, z);
    let mut per: Vec<u64> = cells.iter().map(|c| c.1.len() as u64).collect();
    per.sort_unstable();
    let pct = |q: f64| {
        per.get(((per.len() as f64 - 1.0) * q).round() as usize)
            .copied()
            .unwrap_or(0)
    };
    let (blocks, raw) = encode(&cells, block);
    let (bytes, nblocks) = blocked(cells.len(), blocks)?;
    let sizes = Sizes {
        zoom: z,
        cells: cells.len() as u64,
        loose_cells: cells.iter().filter(|c| c.2).count() as u64,
        pairs: per.iter().sum(),
        streets_per_cell_p50: pct(0.5),
        streets_per_cell_p99: pct(0.99),
        streets_per_cell_max: per.last().copied().unwrap_or(0),
        raw,
        bytes: bytes.len() as u64,
        blocks: nblocks,
    };
    Ok((bytes, sizes))
}

pub fn print_sizes(s: &Sizes) {
    eprintln!(
        "  grid z{}{}: {} cells ({} with unattached addresses), {} cell-street pairs; streets \
         per cell p50 {} p99 {} max {}; SGRD {:.2} MiB ({:.2} raw, {} blocks)",
        s.zoom,
        if s.zoom == ZOOM {
            ""
        } else {
            " (measured only)"
        },
        s.cells,
        s.loose_cells,
        s.pairs,
        s.streets_per_cell_p50,
        s.streets_per_cell_p99,
        s.streets_per_cell_max,
        s.bytes as f64 / (1 << 20) as f64,
        s.raw as f64 / (1 << 20) as f64,
        s.blocks
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_cover_the_cells_they_cross() {
        let c = |x: u32, y: u32| [x << 18 | 1000, y << 18 | 1000];
        let mut out = Vec::new();
        segment_cells(c(3, 3), c(3, 3), 14, &mut out);
        assert_eq!(out, [(3, 3)]);
        out.clear();
        segment_cells(c(3, 3), c(6, 3), 14, &mut out);
        assert_eq!(out, [(3, 3), (4, 3), (5, 3), (6, 3)]);
        out.clear();
        // Down and to the left, two cells across and one down.
        segment_cells(
            [5 << 18 | 9000, 2 << 18 | 100],
            [3 << 18 | 9000, 3 << 18 | 200],
            14,
            &mut out,
        );
        assert_eq!(out.first(), Some(&(5, 2)));
        assert_eq!(out.last(), Some(&(3, 3)));
        assert_eq!(out.len(), 4);
        // Every sampled point of a long diagonal is in a listed cell.
        out.clear();
        let (a, b) = ([123_456_789, 987_654_321], [130_000_000, 980_000_000]);
        segment_cells(a, b, 14, &mut out);
        for k in 0..=1000u64 {
            let t = k as f64 / 1000.0;
            let p = |u: u32, v: u32| (f64::from(u) + t * (f64::from(v) - f64::from(u))) as u32;
            let q = (p(a[0], b[0]) >> 18, p(a[1], b[1]) >> 18);
            assert!(out.contains(&q), "{q:?}");
        }
    }
}
