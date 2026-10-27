//! Point in polygon for admin areas on a grid. Each area marks the cells its rings pass
//! through (with the edges in them) and the cells wholly inside it, so a lookup tests only
//! the few edges in one cell against a short segment to the cell's centre, whose side is
//! known from a scanline at build time.

use std::collections::HashMap;

use rayon::prelude::*;

use crate::geom::{self, BBox, P};

/// Cells of 2^20 world units: z12 tiles, about 7.5 km at 40° N.
const CELL_SHIFT: u32 = 20;
const HALF: f64 = (1u64 << (CELL_SHIFT - 1)) as f64;

fn cell(p: P) -> (u32, u32) {
    (p[0] >> CELL_SHIFT, p[1] >> CELL_SHIFT)
}

/// The centre of a cell, half a unit off the integer grid so no vertex lies on its
/// scanline or on the segment from it to a vertex-aligned point.
fn centre(c: (u32, u32)) -> [f64; 2] {
    [
        f64::from(c.0) * (1u64 << CELL_SHIFT) as f64 + HALF + 0.5,
        f64::from(c.1) * (1u64 << CELL_SHIFT) as f64 + HALF + 0.5,
    ]
}

/// Even-odd test against every ring.
pub fn ring_contains(rings: &[Vec<P>], p: P) -> bool {
    let (px, py) = (f64::from(p[0]), f64::from(p[1]));
    let mut inside = false;
    for ring in rings {
        let n = ring.len();
        for i in 0..n {
            let (a, b) = (ring[i], ring[(i + 1) % n]);
            let (ay, by) = (f64::from(a[1]), f64::from(b[1]));
            if (ay > py) != (by > py) {
                let (ax, bx) = (f64::from(a[0]), f64::from(b[0]));
                let x = ax + (py - ay) / (by - ay) * (bx - ax);
                if x > px {
                    inside = !inside;
                }
            }
        }
    }
    inside
}

/// Douglas–Peucker per ring at `tolerance_m`; rings that fall under 3 points are dropped.
pub fn simplify_rings(rings: &[Vec<P>], tolerance_m: f64) -> Vec<Vec<P>> {
    let Some(&first) = rings.first().and_then(|r| r.first()) else {
        return Vec::new();
    };
    let tol = tolerance_m / geom::metres_per_unit(first);
    rings
        .iter()
        .filter_map(|r| {
            let b = BBox::of(r.iter().copied());
            let span = (b.max[0] - b.min[0]).max(b.max[1] - b.min[1]);
            // Shift so local coordinates fit in i32.
            let k = (64 - u64::from(span).leading_zeros()).saturating_sub(30);
            let mut local: Vec<[i32; 2]> = r
                .iter()
                .map(|p| {
                    [
                        ((p[0] - b.min[0]) >> k) as i32,
                        ((p[1] - b.min[1]) >> k) as i32,
                    ]
                })
                .collect();
            local.push(local[0]);
            let mut s = geom::simplify(&local, tol / f64::from(1u32 << k));
            s.pop();
            (s.len() >= 3).then(|| {
                s.iter()
                    .map(|q| {
                        [
                            ((q[0] as u32) << k) + b.min[0],
                            ((q[1] as u32) << k) + b.min[1],
                        ]
                    })
                    .collect()
            })
        })
        .collect()
}

type Seg = [[u32; 2]; 2];
/// A boundary cell, whether its centre is inside, and the segments in it.
type EdgeCell = ((u32, u32), bool, Vec<Seg>);

enum Entry {
    Full(u32),
    Edge {
        area: u32,
        centre_inside: bool,
        segs: Box<[Seg]>,
    },
}

pub struct Index {
    cells: HashMap<(u32, u32), Vec<Entry>>,
    pub boundary_cells: u64,
    pub full_cells: u64,
    pub segments: u64,
}

/// Whether segment `a`–`b` meets the cell (Liang–Barsky against its square).
fn seg_meets_cell(a: [f64; 2], b: [f64; 2], c: (u32, u32)) -> bool {
    let size = (1u64 << CELL_SHIFT) as f64;
    let lo = [f64::from(c.0) * size, f64::from(c.1) * size];
    let hi = [lo[0] + size, lo[1] + size];
    let d = [b[0] - a[0], b[1] - a[1]];
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    for axis in 0..2 {
        if d[axis] == 0.0 {
            if a[axis] < lo[axis] || a[axis] > hi[axis] {
                return false;
            }
            continue;
        }
        let (mut ta, mut tb) = (
            (lo[axis] - a[axis]) / d[axis],
            (hi[axis] - a[axis]) / d[axis],
        );
        if ta > tb {
            std::mem::swap(&mut ta, &mut tb);
        }
        t0 = t0.max(ta);
        t1 = t1.min(tb);
        if t0 > t1 {
            return false;
        }
    }
    true
}

fn f(p: [u32; 2]) -> [f64; 2] {
    [f64::from(p[0]), f64::from(p[1])]
}

/// Whether segments `p`–`q` and `a`–`b` cross (strictly; touching is rare with the
/// half-unit centre and does not matter here).
fn crosses(p: [f64; 2], q: [f64; 2], a: [f64; 2], b: [f64; 2]) -> bool {
    let orient = |o: [f64; 2], u: [f64; 2], v: [f64; 2]| {
        (u[0] - o[0]) * (v[1] - o[1]) - (u[1] - o[1]) * (v[0] - o[0])
    };
    let (d1, d2) = (orient(a, b, p), orient(a, b, q));
    let (d3, d4) = (orient(p, q, a), orient(p, q, b));
    (d1 > 0.0) != (d2 > 0.0) && (d3 > 0.0) != (d4 > 0.0) && d1 != 0.0 && d2 != 0.0
}

/// The cells of one area: boundary cells with their segments and centre side, and the
/// cells wholly inside.
fn rasterize(rings: &[Vec<P>]) -> (Vec<EdgeCell>, Vec<(u32, u32)>) {
    let mut boundary: HashMap<(u32, u32), Vec<Seg>> = HashMap::new();
    // (row, x) where an edge crosses the row's centre line.
    let mut crossings: Vec<(u32, f64)> = Vec::new();
    for ring in rings {
        let n = ring.len();
        for i in 0..n {
            let (a, b) = (ring[i], ring[(i + 1) % n]);
            let (ca, cb) = (cell(a), cell(b));
            let (fa, fb) = (f(a), f(b));
            for cx in ca.0.min(cb.0)..=ca.0.max(cb.0) {
                for cy in ca.1.min(cb.1)..=ca.1.max(cb.1) {
                    if (cx, cy) == ca || (cx, cy) == cb || seg_meets_cell(fa, fb, (cx, cy)) {
                        boundary.entry((cx, cy)).or_default().push([a, b]);
                    }
                }
            }
            let (ylo, yhi) = (fa[1].min(fb[1]), fa[1].max(fb[1]));
            for row in ca.1.min(cb.1)..=ca.1.max(cb.1) {
                let y = centre((0, row))[1];
                if y > ylo && y < yhi {
                    let x = fa[0] + (y - fa[1]) / (fb[1] - fa[1]) * (fb[0] - fa[0]);
                    crossings.push((row, x));
                }
            }
        }
    }
    crossings.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
    let b = BBox::of(rings.iter().flatten().copied());
    let (lo, hi) = (cell(b.min), cell(b.max));
    let mut edge_cells = Vec::with_capacity(boundary.len());
    let mut full = Vec::new();
    let mut at = 0;
    for row in lo.1..=hi.1 {
        let start = at;
        while at < crossings.len() && crossings[at].0 == row {
            at += 1;
        }
        let xs = &crossings[start..at];
        let mut k = 0;
        for cx in lo.0..=hi.0 {
            let x = centre((cx, row))[0];
            while k < xs.len() && xs[k].1 < x {
                k += 1;
            }
            let inside = k % 2 == 1;
            match boundary.remove(&(cx, row)) {
                Some(segs) => edge_cells.push(((cx, row), inside, segs)),
                None if inside => full.push((cx, row)),
                None => {}
            }
        }
    }
    (edge_cells, full)
}

impl Index {
    /// Indexes `rings[i]` as area `i`.
    pub fn build(rings: &[&[Vec<P>]]) -> Self {
        let parts: Vec<_> = rings.par_iter().map(|r| rasterize(r)).collect();
        let mut cells: HashMap<(u32, u32), Vec<Entry>> = HashMap::new();
        let (mut boundary_cells, mut full_cells, mut segments) = (0, 0, 0);
        for (area, (edges, full)) in parts.into_iter().enumerate() {
            let area = area as u32;
            for (c, centre_inside, segs) in edges {
                boundary_cells += 1;
                segments += segs.len() as u64;
                cells.entry(c).or_default().push(Entry::Edge {
                    area,
                    centre_inside,
                    segs: segs.into_boxed_slice(),
                });
            }
            for c in full {
                full_cells += 1;
                cells.entry(c).or_default().push(Entry::Full(area));
            }
        }
        Self {
            cells,
            boundary_cells,
            full_cells,
            segments,
        }
    }

    /// The areas that contain `p`, in index order.
    pub fn containing(&self, p: P) -> Vec<u32> {
        let c = cell(p);
        let Some(entries) = self.cells.get(&c) else {
            return Vec::new();
        };
        let q = f(p);
        let m = centre(c);
        let mut out: Vec<u32> = entries
            .iter()
            .filter_map(|e| match e {
                Entry::Full(a) => Some(*a),
                Entry::Edge {
                    area,
                    centre_inside,
                    segs,
                } => {
                    let n = segs
                        .iter()
                        .filter(|s| crosses(q, m, f(s[0]), f(s[1])))
                        .count();
                    (*centre_inside != (n % 2 == 1)).then_some(*area)
                }
            })
            .collect();
        out.sort_unstable();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x0: u32, y0: u32, x1: u32, y1: u32) -> Vec<P> {
        vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1]]
    }

    #[test]
    fn matches_brute_force() {
        let s = 1u32 << CELL_SHIFT;
        // A big square with a hole, a diagonal triangle across cells, and a small square
        // inside one cell.
        let areas = [
            vec![
                square(10 * s + 5, 10 * s + 7, 20 * s + 3, 18 * s + 11),
                square(12 * s, 12 * s, 14 * s + 100, 13 * s + 50),
            ],
            vec![vec![
                [11 * s, 11 * s],
                [25 * s, 12 * s + 9],
                [13 * s, 22 * s],
            ]],
            vec![square(15 * s + 10, 15 * s + 10, 15 * s + 900, 15 * s + 700)],
        ];
        let index = Index::build(&areas.iter().map(Vec::as_slice).collect::<Vec<_>>());
        let mut state = 7u64;
        for _ in 0..20_000 {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let x = 9 * s + ((state >> 33) as u32 % (17 * s));
            let y = 9 * s + ((state >> 7) as u32 % (15 * s));
            let want: Vec<u32> = (0..areas.len() as u32)
                .filter(|&i| ring_contains(&areas[i as usize], [x, y]))
                .collect();
            assert_eq!(index.containing([x, y]), want, "at {x}, {y}");
        }
        // Points near the small square's corner, at sub-cell scale.
        for dx in 0..40 {
            for dy in 0..40 {
                let p = [15 * s + dx * 25, 15 * s + dy * 20];
                assert_eq!(
                    index.containing(p).contains(&2),
                    ring_contains(&areas[2], p)
                );
            }
        }
    }

    #[test]
    fn simplify_keeps_shape() {
        let p = geom::project(40.0, -83.0);
        let ring: Vec<P> = (0..400)
            .map(|i| {
                let a = f64::from(i) / 400.0 * std::f64::consts::TAU;
                [
                    p[0] + (a.cos() * 1_000_000.0) as u32 + 2_000_000,
                    p[1] + (a.sin() * 1_000_000.0) as u32 + 2_000_000,
                ]
            })
            .collect();
        let s = simplify_rings(&[ring], 50.0);
        assert_eq!(s.len(), 1);
        assert!(s[0].len() < 100 && s[0].len() > 8, "{}", s[0].len());
    }
}
