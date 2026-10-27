//! Per-tile layer encodings measured in phase 0: V0 (raw sizing only) and MVT (reference).
//! V1/V2, the pack's layer payload, is `omp_core::tile`.

use std::io::Write;

use omp_core::tile::{EXTENT_BITS, Enc, Geom, Point};
use omp_core::varint::{put as put_varint, zigzag};

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

/// V0: what the features would take with `f64` lon/lat and tags as NUL-terminated strings.
pub fn v0_size(
    g: &Geometry,
    class_name: &str,
    props: &[(u8, Box<str>)],
    prop_names: &[String],
) -> u64 {
    let geom = match g {
        Geometry::Point(_) => 16,
        Geometry::Lines(p) | Geometry::Polygon(p) => {
            4 + p.iter().map(|part| 4 + 16 * part.len() as u64).sum::<u64>()
        }
    };
    let tags: u64 = props
        .iter()
        .map(|(k, v)| prop_names[*k as usize].len() as u64 + v.len() as u64 + 2)
        .sum();
    1 + geom + "class".len() as u64 + class_name.len() as u64 + 2 + tags
}

// --- MVT (Mapbox Vector Tile 2.1) reference writer ---

fn pb_key(out: &mut Vec<u8>, field: u32, wire: u8) {
    put_varint(out, (u64::from(field) << 3) | u64::from(wire));
}

fn pb_bytes(out: &mut Vec<u8>, field: u32, b: &[u8]) {
    pb_key(out, field, 2);
    put_varint(out, b.len() as u64);
    out.extend_from_slice(b);
}

fn pb_varint(out: &mut Vec<u8>, field: u32, v: u64) {
    pb_key(out, field, 0);
    put_varint(out, v);
}

/// One MVT layer message (without the enclosing tile field). Props become string or uint
/// values; the class becomes a `class` string value.
pub fn mvt_layer<'a>(
    name: &str,
    features: &'a [Enc<'a>],
    class_names: &'a [String],
    prop_names: &[String],
) -> Vec<u8> {
    #[derive(PartialEq, Eq, Hash, Clone, Copy)]
    enum V<'a> {
        S(&'a str),
        U(u64),
    }
    let mut values: Vec<V<'a>> = Vec::new();
    let mut vindex: std::collections::HashMap<V<'a>, u64> = std::collections::HashMap::new();
    let mut val = |v: V<'a>| -> u64 {
        *vindex.entry(v).or_insert_with(|| {
            values.push(v);
            (values.len() - 1) as u64
        })
    };
    let mut feats = Vec::new();
    for f in features {
        let mut tags = Vec::new();
        put_varint(&mut tags, 0);
        put_varint(&mut tags, val(V::S(&class_names[f.class as usize])));
        for (k, v) in f.props {
            put_varint(&mut tags, u64::from(*k) + 1);
            put_varint(
                &mut tags,
                val(omp_core::tile::int_value(v).map_or(V::S(v), V::U)),
            );
        }
        let mut g = Vec::new();
        let mut cur: Point = [0, 0];
        let cmd = |g: &mut Vec<u8>, id: u64, count: u64| put_varint(g, (id & 7) | (count << 3));
        let pt = |g: &mut Vec<u8>, p: Point, cur: &mut Point| {
            put_varint(g, zigzag(p[0] - cur[0]));
            put_varint(g, zigzag(p[1] - cur[1]));
            *cur = p;
        };
        let ty = match &f.geom {
            Geom::Point(p) => {
                cmd(&mut g, 1, 1);
                pt(&mut g, *p, &mut cur);
                1
            }
            Geom::Lines(parts) => {
                for part in parts {
                    cmd(&mut g, 1, 1);
                    pt(&mut g, part[0], &mut cur);
                    cmd(&mut g, 2, part.len() as u64 - 1);
                    for p in &part[1..] {
                        pt(&mut g, *p, &mut cur);
                    }
                }
                2
            }
            Geom::Polygon(rings) => {
                for ring in rings {
                    cmd(&mut g, 1, 1);
                    pt(&mut g, ring[0], &mut cur);
                    cmd(&mut g, 2, ring.len() as u64 - 1);
                    for p in &ring[1..] {
                        pt(&mut g, *p, &mut cur);
                    }
                    cmd(&mut g, 7, 1);
                }
                3
            }
        };
        let mut feat = Vec::new();
        pb_bytes(&mut feat, 2, &tags);
        pb_varint(&mut feat, 3, ty);
        pb_bytes(&mut feat, 4, &g);
        feats.push(feat);
    }
    let mut layer = Vec::new();
    pb_varint(&mut layer, 15, 2);
    pb_bytes(&mut layer, 1, name.as_bytes());
    for f in &feats {
        pb_bytes(&mut layer, 2, f);
    }
    pb_bytes(&mut layer, 3, b"class");
    for k in prop_names {
        pb_bytes(&mut layer, 3, k.as_bytes());
    }
    for v in &values {
        let mut m = Vec::new();
        match v {
            V::S(s) => pb_bytes(&mut m, 1, s.as_bytes()),
            V::U(n) => pb_varint(&mut m, 5, *n),
        }
        pb_bytes(&mut layer, 4, &m);
    }
    pb_varint(&mut layer, 5, 1 << EXTENT_BITS);
    layer
}

/// Wraps layer messages into a tile and gzips it the way MVT servers usually do.
pub fn mvt_tile_gzip(layers: &[Vec<u8>]) -> (u64, u64) {
    let mut tile = Vec::new();
    for l in layers {
        pb_bytes(&mut tile, 3, l);
    }
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gz.write_all(&tile).expect("write to Vec");
    let gz = gz.finish().expect("write to Vec");
    (tile.len() as u64, gz.len() as u64)
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
