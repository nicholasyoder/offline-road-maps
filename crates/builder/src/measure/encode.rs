//! Per-tile layer encodings: V0 (raw sizing only), V1/V2 (compact binary) and MVT (reference).

use std::io::Write;

use anyhow::{Result, bail};

use super::geom::{self, Geometry, P};

/// Tile-local grid: 4096 units per tile edge.
pub const EXTENT_BITS: u32 = 12;

pub type L = [i32; 2];

#[derive(Clone, Debug, PartialEq)]
pub enum LocalGeom {
    Point(L),
    Lines(Vec<Vec<L>>),
    Polygon(Vec<Vec<L>>),
}

impl LocalGeom {
    pub fn vertices(&self) -> usize {
        match self {
            LocalGeom::Point(_) => 1,
            LocalGeom::Lines(p) | LocalGeom::Polygon(p) => p.iter().map(Vec::len).sum(),
        }
    }
}

/// A feature ready to encode: class index, props and tile-local geometry.
pub struct Enc<'a> {
    pub class: u16,
    pub props: &'a [(u8, Box<str>)],
    pub geom: LocalGeom,
}

/// World to tile-local coordinates. Consecutive duplicate points (from the grid) are dropped.
pub fn quantize(g: &Geometry, z: u8, x: u32, y: u32) -> Option<LocalGeom> {
    let shift = 32 - u32::from(z) - EXTENT_BITS;
    let ox = i64::from(x) << (32 - u32::from(z));
    let oy = i64::from(y) << (32 - u32::from(z));
    let q = |p: &P| -> L {
        [
            ((i64::from(p[0]) - ox) >> shift) as i32,
            ((i64::from(p[1]) - oy) >> shift) as i32,
        ]
    };
    let dedup = |pts: &[P]| -> Vec<L> {
        let mut v: Vec<L> = Vec::with_capacity(pts.len());
        for p in pts {
            let l = q(p);
            if v.last() != Some(&l) {
                v.push(l);
            }
        }
        v
    };
    match g {
        Geometry::Point(p) => Some(LocalGeom::Point(q(p))),
        Geometry::Lines(parts) => {
            let v: Vec<Vec<L>> = parts
                .iter()
                .map(|p| dedup(p))
                .filter(|p| p.len() >= 2)
                .collect();
            (!v.is_empty()).then_some(LocalGeom::Lines(v))
        }
        Geometry::Polygon(rings) => {
            let v: Vec<Vec<L>> = rings
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
                .then_some(LocalGeom::Polygon(v))
        }
    }
}

/// V2 thinning: Douglas–Peucker with `tolerance` grid units, then drop lines shorter than
/// `min_len` and rings smaller than `min_area` (both 0 to keep everything).
///
/// With `floor`, a ring that would collapse (fewer than 3 points, or no area) is simplified
/// again at half the tolerance, down to `floor`, so small areas keep a shape.
pub fn simplify(
    g: LocalGeom,
    tolerance: f64,
    min_len: f64,
    min_area: f64,
    floor: Option<f64>,
) -> Option<LocalGeom> {
    match g {
        LocalGeom::Point(_) => Some(g),
        LocalGeom::Lines(parts) => {
            let v: Vec<Vec<L>> = parts
                .into_iter()
                .map(|p| geom::simplify(&p, tolerance))
                .filter(|p| p.len() >= 2 && line_len(p) >= min_len)
                .collect();
            (!v.is_empty()).then_some(LocalGeom::Lines(v))
        }
        LocalGeom::Polygon(rings) => {
            let v: Vec<Vec<L>> = rings
                .into_iter()
                .filter_map(|mut r| {
                    r.push(r[0]);
                    let ok =
                        |s: &[L]| s.len() >= 3 && geom::ring_area(s).abs() >= min_area.max(0.5);
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
                .then_some(LocalGeom::Polygon(v))
        }
    }
}

fn line_len(p: &[L]) -> f64 {
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

pub fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

pub fn varint_len(mut v: u64) -> u64 {
    let mut n = 1;
    while v >= 0x80 {
        v >>= 7;
        n += 1;
    }
    n
}

fn zigzag(v: i32) -> u64 {
    u64::from(((v << 1) ^ (v >> 31)) as u32)
}

fn unzigzag(v: u64) -> i32 {
    ((v >> 1) as i32) ^ -((v & 1) as i32)
}

/// A small non-negative decimal integer without leading zeros is stored as a number.
fn as_int(v: &str) -> Option<u64> {
    if v.is_empty() || v.len() > 9 || (v.len() > 1 && v.starts_with('0')) {
        return None;
    }
    v.parse().ok()
}

/// V1/V2 layer section:
/// `nfeatures, nstrings, strings (len + bytes)`, then per feature
/// `class << 2 | geomtype, nprops, (key, value)*, geometry`. A value is `string << 1` or
/// `int << 1 | 1`. Geometry is zigzag deltas from a cursor carried across features:
/// point `dx dy`; lines `nparts (npoints deltas*)*`; polygon `nrings (npoints deltas*)*`
/// with rings implicitly closed.
pub fn encode_v1(features: &[Enc]) -> Vec<u8> {
    let mut strings: Vec<&str> = Vec::new();
    let mut index = std::collections::HashMap::new();
    for f in features {
        for (_, v) in f.props {
            if as_int(v).is_none() && !index.contains_key(&**v) {
                index.insert(&**v, strings.len() as u64);
                strings.push(v);
            }
        }
    }
    let mut out = Vec::new();
    put_varint(&mut out, features.len() as u64);
    put_varint(&mut out, strings.len() as u64);
    for s in &strings {
        put_varint(&mut out, s.len() as u64);
        out.extend_from_slice(s.as_bytes());
    }
    let mut cur: L = [0, 0];
    let pts = |out: &mut Vec<u8>, part: &[L], cur: &mut L| {
        put_varint(out, part.len() as u64);
        for p in part {
            put_varint(out, zigzag(p[0] - cur[0]));
            put_varint(out, zigzag(p[1] - cur[1]));
            *cur = *p;
        }
    };
    for f in features {
        let ty = match f.geom {
            LocalGeom::Point(_) => 0,
            LocalGeom::Lines(_) => 1,
            LocalGeom::Polygon(_) => 2,
        };
        put_varint(&mut out, (u64::from(f.class) << 2) | ty);
        put_varint(&mut out, f.props.len() as u64);
        for (k, v) in f.props {
            put_varint(&mut out, u64::from(*k));
            match as_int(v) {
                Some(n) => put_varint(&mut out, (n << 1) | 1),
                None => put_varint(&mut out, index[&**v] << 1),
            }
        }
        match &f.geom {
            LocalGeom::Point(p) => {
                put_varint(&mut out, zigzag(p[0] - cur[0]));
                put_varint(&mut out, zigzag(p[1] - cur[1]));
                cur = *p;
            }
            LocalGeom::Lines(parts) | LocalGeom::Polygon(parts) => {
                put_varint(&mut out, parts.len() as u64);
                for part in parts {
                    pts(&mut out, part, &mut cur);
                }
            }
        }
    }
    out
}

// The spike only times decoding; the app will read these fields.
#[allow(dead_code)]
pub struct Decoded {
    pub class: u16,
    pub props: Vec<(u8, Value)>,
    pub geom: LocalGeom,
}

#[derive(Debug, PartialEq)]
pub enum Value {
    Str(u32),
    Int(u64),
}

struct Reader<'a> {
    b: &'a [u8],
    i: usize,
}

impl Reader<'_> {
    fn varint(&mut self) -> Result<u64> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let Some(&byte) = self.b.get(self.i) else {
                bail!("truncated varint");
            };
            self.i += 1;
            v |= u64::from(byte & 0x7f) << shift;
            if byte < 0x80 {
                return Ok(v);
            }
        }
        bail!("varint too long")
    }
}

/// Decodes a V1/V2 section into its string table and features.
pub fn decode_v1(bytes: &[u8]) -> Result<(Vec<String>, Vec<Decoded>)> {
    let mut r = Reader { b: bytes, i: 0 };
    let nf = r.varint()? as usize;
    let ns = r.varint()? as usize;
    let mut strings = Vec::with_capacity(ns);
    for _ in 0..ns {
        let len = r.varint()? as usize;
        let Some(s) = bytes.get(r.i..r.i + len) else {
            bail!("truncated string");
        };
        strings.push(String::from_utf8_lossy(s).into_owned());
        r.i += len;
    }
    let mut cur: L = [0, 0];
    let mut features = Vec::with_capacity(nf);
    for _ in 0..nf {
        let head = r.varint()?;
        let np = r.varint()? as usize;
        let mut props = Vec::with_capacity(np);
        for _ in 0..np {
            let k = r.varint()? as u8;
            let v = r.varint()?;
            props.push((
                k,
                if v & 1 == 1 {
                    Value::Int(v >> 1)
                } else {
                    Value::Str((v >> 1) as u32)
                },
            ));
        }
        let point = |r: &mut Reader, cur: &mut L| -> Result<L> {
            cur[0] += unzigzag(r.varint()?);
            cur[1] += unzigzag(r.varint()?);
            Ok(*cur)
        };
        let geom = match head & 3 {
            0 => LocalGeom::Point(point(&mut r, &mut cur)?),
            t => {
                let n = r.varint()? as usize;
                let mut parts = Vec::with_capacity(n);
                for _ in 0..n {
                    let m = r.varint()? as usize;
                    let mut part = Vec::with_capacity(m);
                    for _ in 0..m {
                        part.push(point(&mut r, &mut cur)?);
                    }
                    parts.push(part);
                }
                if t == 1 {
                    LocalGeom::Lines(parts)
                } else {
                    LocalGeom::Polygon(parts)
                }
            }
        };
        features.push(Decoded {
            class: (head >> 2) as u16,
            props,
            geom,
        });
    }
    Ok((strings, features))
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
            put_varint(&mut tags, val(as_int(v).map_or(V::S(v), V::U)));
        }
        let mut g = Vec::new();
        let mut cur: L = [0, 0];
        let cmd = |g: &mut Vec<u8>, id: u64, count: u64| put_varint(g, (id & 7) | (count << 3));
        let pt = |g: &mut Vec<u8>, p: L, cur: &mut L| {
            put_varint(g, zigzag(p[0] - cur[0]));
            put_varint(g, zigzag(p[1] - cur[1]));
            *cur = p;
        };
        let ty = match &f.geom {
            LocalGeom::Point(p) => {
                cmd(&mut g, 1, 1);
                pt(&mut g, *p, &mut cur);
                1
            }
            LocalGeom::Lines(parts) => {
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
            LocalGeom::Polygon(rings) => {
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
    fn v1_round_trip() {
        let props: Vec<(u8, Box<str>)> = vec![(0, "Main St".into()), (1, "42".into())];
        let feats = vec![
            Enc {
                class: 3,
                props: &props,
                geom: LocalGeom::Lines(vec![vec![[0, 0], [100, -5], [4100, 7]]]),
            },
            Enc {
                class: 0,
                props: &[],
                geom: LocalGeom::Polygon(vec![vec![[0, 0], [10, 0], [10, 10]]]),
            },
            Enc {
                class: 1,
                props: &props[..1],
                geom: LocalGeom::Point([-64, 4159]),
            },
        ];
        let bytes = encode_v1(&feats);
        let (strings, dec) = decode_v1(&bytes).unwrap();
        assert_eq!(strings, ["Main St"]);
        assert_eq!(dec.len(), 3);
        for (a, b) in feats.iter().zip(&dec) {
            assert_eq!(a.class, b.class);
            assert_eq!(a.geom, b.geom);
        }
        assert_eq!(dec[0].props, [(0, Value::Str(0)), (1, Value::Int(42))]);
    }

    #[test]
    fn quantize_and_simplify() {
        // z14 tile 0/0: one grid unit is 2^6 world units.
        let g = Geometry::Lines(vec![vec![[0, 0], [10, 0], [64 * 100, 64], [64 * 200, 0]]]);
        let q = quantize(&g, 14, 0, 0).unwrap();
        assert_eq!(q, LocalGeom::Lines(vec![vec![[0, 0], [100, 1], [200, 0]]]));
        let s = simplify(q, 2.0, 0.0, 0.0, None).unwrap();
        assert_eq!(s, LocalGeom::Lines(vec![vec![[0, 0], [200, 0]]]));
        assert!(simplify(s, 2.0, 500.0, 0.0, None).is_none());
    }

    #[test]
    fn small_rings_survive_with_a_floor() {
        let pond = || LocalGeom::Polygon(vec![vec![[0, 0], [4, 0], [4, 4], [0, 4]]]);
        assert!(simplify(pond(), 16.0, 0.0, 0.0, None).is_none());
        let kept = simplify(pond(), 16.0, 0.0, 0.0, Some(1.0)).unwrap();
        assert_eq!(kept, pond());
        // A larger ring is simplified at the full tolerance.
        let lake = LocalGeom::Polygon(vec![vec![[0, 0], [50, 1], [100, 0], [100, 100], [0, 100]]]);
        let s = simplify(lake, 16.0, 0.0, 0.0, Some(1.0)).unwrap();
        assert_eq!(
            s,
            LocalGeom::Polygon(vec![vec![[0, 0], [100, 0], [100, 100], [0, 100]]])
        );
    }
}
