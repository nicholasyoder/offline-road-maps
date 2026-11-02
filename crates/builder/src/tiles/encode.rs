//! A feature's geometry in one tile: [`quantize`] to the tile's grid, then [`simplify`] for
//! its zoom. The layer payload they are encoded into is `omp_core::tile`'s.

use omp_core::tile::{EXTENT_BITS, Geom, Point};

use crate::geom::{self, Geometry, P};

/// World to tile-local coordinates. Consecutive duplicate points (from the grid) are dropped.
pub fn quantize(g: &Geometry, z: u8, x: u32, y: u32) -> Option<Geom> {
    let shift = 32 - u32::from(z) - EXTENT_BITS;
    let ox = i64::from(x) << (32 - u32::from(z));
    let oy = i64::from(y) << (32 - u32::from(z));
    let q = |p: &P| -> Point {
        [
            ((i64::from(p[0]) - ox) >> shift) as i32,
            ((i64::from(p[1]) - oy) >> shift) as i32,
        ]
    };
    let dedup = |pts: &[P]| -> Vec<Point> {
        let mut v: Vec<Point> = Vec::with_capacity(pts.len());
        for p in pts {
            let l = q(p);
            if v.last() != Some(&l) {
                v.push(l);
            }
        }
        v
    };
    match g {
        Geometry::Point(p) => Some(Geom::Point(q(p))),
        Geometry::Lines(parts) => {
            let v: Vec<Vec<Point>> = parts
                .iter()
                .map(|p| dedup(p))
                .filter(|p| p.len() >= 2)
                .collect();
            (!v.is_empty()).then_some(Geom::Lines(v))
        }
        Geometry::Polygon(rings) => {
            let v: Vec<Vec<Point>> = rings
                .iter()
                .map(|r| {
                    let mut r = dedup(r);
                    if r.len() > 1 && r.first() == r.last() {
                        r.pop();
                    }
                    r
                })
                .filter(|r| r.len() >= 3)
                .collect();
            v.iter()
                .any(|r| geom::ring_area(r) > 0.0)
                .then_some(Geom::Polygon(v))
        }
    }
}

/// V2 thinning: Douglas–Peucker with `tolerance` grid units, then drop lines shorter than
/// `min_len` and rings smaller than `min_area` (both 0 to keep everything).
///
/// With `floor`, a ring that would collapse (fewer than 3 points, or no area) is simplified
/// again at half the tolerance, down to `floor`, so small areas keep a shape.
pub fn simplify(
    g: Geom,
    tolerance: f64,
    min_len: f64,
    min_area: f64,
    floor: Option<f64>,
) -> Option<Geom> {
    match g {
        Geom::Point(_) => Some(g),
        Geom::Lines(parts) => {
            let v: Vec<Vec<Point>> = parts
                .into_iter()
                .map(|p| geom::simplify(&p, tolerance))
                .filter(|p| p.len() >= 2 && line_len(p) >= min_len)
                .collect();
            (!v.is_empty()).then_some(Geom::Lines(v))
        }
        Geom::Polygon(rings) => {
            let v: Vec<Vec<Point>> = rings
                .into_iter()
                .filter_map(|mut r| {
                    r.push(r[0]);
                    let ok =
                        |s: &[Point]| s.len() >= 3 && geom::ring_area(s).abs() >= min_area.max(0.5);
                    let mut tol = tolerance;
                    let mut s = geom::simplify(&r, tol);
                    while let Some(f) = floor
                        && tol > f
                        && !ok(&s[..s.len() - 1])
                    {
                        tol = (tol / 2.0).max(f);
                        s = geom::simplify(&r, tol);
                    }
                    s.pop();
                    ok(&s).then_some(s)
                })
                .collect();
            v.iter()
                .any(|r| geom::ring_area(r) > 0.0)
                .then_some(Geom::Polygon(v))
        }
    }
}

fn line_len(p: &[Point]) -> f64 {
    p.windows(2)
        .map(|w| {
            let dx = f64::from(w[1][0] - w[0][0]);
            let dy = f64::from(w[1][1] - w[0][1]);
            (dx * dx + dy * dy).sqrt()
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantize_and_simplify() {
        // z14 tile 0/0: one grid unit is 2^6 world units.
        let g = Geometry::Lines(vec![vec![[0, 0], [10, 0], [64 * 100, 64], [64 * 200, 0]]]);
        let q = quantize(&g, 14, 0, 0).unwrap();
        assert_eq!(q, Geom::Lines(vec![vec![[0, 0], [100, 1], [200, 0]]]));
        let s = simplify(q, 2.0, 0.0, 0.0, None).unwrap();
        assert_eq!(s, Geom::Lines(vec![vec![[0, 0], [200, 0]]]));
        assert!(simplify(s, 2.0, 500.0, 0.0, None).is_none());
    }

    #[test]
    fn small_rings_survive_with_a_floor() {
        let pond = || Geom::Polygon(vec![vec![[0, 0], [4, 0], [4, 4], [0, 4]]]);
        assert!(simplify(pond(), 16.0, 0.0, 0.0, None).is_none());
        let kept = simplify(pond(), 16.0, 0.0, 0.0, Some(1.0)).unwrap();
        assert_eq!(kept, pond());
        // A larger ring is simplified at the full tolerance.
        let lake = Geom::Polygon(vec![vec![[0, 0], [50, 1], [100, 0], [100, 100], [0, 100]]]);
        let s = simplify(lake, 16.0, 0.0, 0.0, Some(1.0)).unwrap();
        assert_eq!(
            s,
            Geom::Polygon(vec![vec![[0, 0], [100, 0], [100, 100], [0, 100]]])
        );
    }
}
