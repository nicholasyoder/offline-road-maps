//! Time zones (`TZON`, `docs/format.md`, phase 4 step 4.5): each zone's IANA name and
//! polygon, held in RAM, and its rules from `jiff`'s bundled tz database, so every computer
//! gives the same answers from a stick. A point's zone is found by point in polygon, over
//! each zone's edges bucketed in horizontal bands.

use std::io;

use jiff::tz::{TimeZone, TimeZoneDatabase};
use zstd::zstd_safe::DCtx;

use crate::geo::{self, P};
use crate::pack::Pack;
use crate::varint::{Reader, invalid, put, put_str, zigzag};
use crate::zframe;

pub const TZON: [u8; 4] = *b"TZON";
/// The first minor version with `TZON` (format 0.6).
pub const MIN_MINOR: u16 = 6;
/// `TZON` points are in units of 2^6 world units, as `SAPL`'s.
pub const SHIFT: u32 = 6;
/// Bands of 2^20 world units: about 10 km at the equator, 5 km at 60°.
const BAND_SHIFT: u32 = 20;
/// A point in no zone takes the nearest within this, for the slivers between zones that
/// were simplified apart.
const NEAR_M: f64 = 1_000.0;

/// One zone: its name, its rules, and its edges by band.
pub struct Zone {
    /// The IANA name, `America/Chicago`.
    pub name: String,
    /// `min_x, min_y, max_x, max_y` in world coordinates.
    pub bbox: [u32; 4],
    /// `None` when the bundled tz database does not know the name.
    pub tz: Option<TimeZone>,
    first_band: u32,
    /// Per band from `first_band`, where its edges start in `edges`, and the end.
    starts: Vec<u32>,
    edges: Vec<[P; 2]>,
}

impl Zone {
    fn new(name: String, bbox: [u32; 4], rings: &[Vec<P>], db: &TimeZoneDatabase) -> Self {
        let first_band = bbox[1] >> BAND_SHIFT;
        let n = ((bbox[3] >> BAND_SHIFT) - first_band + 1) as usize;
        let mut by_band: Vec<Vec<[P; 2]>> = vec![Vec::new(); n];
        for r in rings {
            for i in 0..r.len() {
                let (a, b) = (r[i], r[(i + 1) % r.len()]);
                let lo = (a[1].min(b[1]) >> BAND_SHIFT).max(first_band);
                let hi = (a[1].max(b[1]) >> BAND_SHIFT).min(first_band + n as u32 - 1);
                for band in lo..=hi {
                    by_band[(band - first_band) as usize].push([a, b]);
                }
            }
        }
        let mut starts = Vec::with_capacity(n + 1);
        let mut edges = Vec::with_capacity(by_band.iter().map(Vec::len).sum());
        for b in by_band {
            starts.push(edges.len() as u32);
            edges.extend(b);
        }
        starts.push(edges.len() as u32);
        let tz = db.get(&name).ok();
        Zone {
            name,
            bbox,
            tz,
            first_band,
            starts,
            edges,
        }
    }

    /// The edges of band `band`, if the zone reaches it.
    fn band(&self, band: i64) -> &[[P; 2]] {
        let i = band - i64::from(self.first_band);
        if i < 0 || i + 1 >= self.starts.len() as i64 {
            return &[];
        }
        let i = i as usize;
        &self.edges[self.starts[i] as usize..self.starts[i + 1] as usize]
    }

    fn bbox_contains(&self, p: P, margin: u32) -> bool {
        let b = self.bbox;
        b[0].saturating_sub(margin) <= p[0]
            && p[0] <= b[2].saturating_add(margin)
            && b[1].saturating_sub(margin) <= p[1]
            && p[1] <= b[3].saturating_add(margin)
    }

    /// Even-odd over the edges of `p`'s band, as [`geo::rings_contain`].
    pub fn contains(&self, p: P) -> bool {
        if !self.bbox_contains(p, 0) {
            return false;
        }
        let (px, py) = (f64::from(p[0]), f64::from(p[1]));
        let mut inside = false;
        for [a, b] in self.band(i64::from(p[1] >> BAND_SHIFT)) {
            let (ay, by) = (f64::from(a[1]), f64::from(b[1]));
            if (ay > py) != (by > py) {
                let (ax, bx) = (f64::from(a[0]), f64::from(b[0]));
                if ax + (py - ay) / (by - ay) * (bx - ax) > px {
                    inside = !inside;
                }
            }
        }
        inside
    }

    /// World units from `p` to the nearest edge in its band and the bands beside it.
    fn distance(&self, p: P) -> f64 {
        let band = i64::from(p[1] >> BAND_SHIFT);
        let q = [f64::from(p[0]), f64::from(p[1])];
        (band - 1..=band + 1)
            .flat_map(|b| self.band(b))
            .map(|[a, b]| {
                let (a, b) = (
                    [f64::from(a[0]), f64::from(a[1])],
                    [f64::from(b[0]), f64::from(b[1])],
                );
                let d = [b[0] - a[0], b[1] - a[1]];
                let len2 = d[0] * d[0] + d[1] * d[1];
                let t = match len2 > 0.0 {
                    true => (((q[0] - a[0]) * d[0] + (q[1] - a[1]) * d[1]) / len2).clamp(0.0, 1.0),
                    false => 0.0,
                };
                (q[0] - a[0] - t * d[0]).hypot(q[1] - a[1] - t * d[1])
            })
            .fold(f64::INFINITY, f64::min)
    }
}

/// The pack's zones.
pub struct Zones {
    zones: Vec<Zone>,
}

/// Encodes `TZON` from `(name, rings)` per zone, rings in world coordinates and not closed:
/// one zstd frame of `count:varint (name:str  bbox:u32×4  nrings:varint (npts:varint
/// (dx:zvarint dy:zvarint)*)*)*`, points in 2^[`SHIFT`] world units as deltas from a cursor
/// that starts at (0, 0) for each zone.
pub fn encode(zones: &[(String, Vec<Vec<P>>)], level: i32) -> io::Result<Vec<u8>> {
    let mut raw = Vec::new();
    put(&mut raw, zones.len() as u64);
    for (name, rings) in zones {
        put_str(&mut raw, name);
        let mut b = [u32::MAX, u32::MAX, 0, 0];
        for p in rings.iter().flatten() {
            b = [
                b[0].min(p[0]),
                b[1].min(p[1]),
                b[2].max(p[0]),
                b[3].max(p[1]),
            ];
        }
        for v in b {
            raw.extend_from_slice(&v.to_le_bytes());
        }
        put(&mut raw, rings.len() as u64);
        let mut cur = [0i32; 2];
        for r in rings {
            put(&mut raw, r.len() as u64);
            for p in r {
                let q = [(p[0] >> SHIFT) as i32, (p[1] >> SHIFT) as i32];
                put(&mut raw, zigzag(q[0].wrapping_sub(cur[0])));
                put(&mut raw, zigzag(q[1].wrapping_sub(cur[1])));
                cur = q;
            }
        }
    }
    zframe::compressor(level, &[])?.compress(&raw)
}

impl Zones {
    /// Reads `TZON`; `None` if the pack has none (built before format 0.6, or without the
    /// zone polygons).
    pub fn open(pack: &Pack) -> io::Result<Option<Self>> {
        match pack.section(TZON) {
            Some(s) => Self::decode(&pack.read_section(s)?).map(Some),
            None => Ok(None),
        }
    }

    pub fn decode(section: &[u8]) -> io::Result<Self> {
        let raw = zframe::decompress(&mut DCtx::create(), section, None)?;
        let db = TimeZoneDatabase::bundled();
        let mut r = Reader::new(&raw);
        let n = r.count()?;
        let mut zones = Vec::with_capacity(n);
        for _ in 0..n {
            let len = r.count()?;
            let name = String::from_utf8(r.bytes(len)?.to_vec())
                .map_err(|_| invalid("TZON name is not UTF-8"))?;
            let fixed = r.bytes(16)?;
            let u = |i: usize| u32::from_le_bytes(fixed[4 * i..4 * i + 4].try_into().unwrap());
            let bbox = [u(0), u(1), u(2), u(3)];
            if bbox[0] > bbox[2] || bbox[1] > bbox[3] {
                return Err(invalid("TZON zone with an empty bbox"));
            }
            let mut cur = [0i32; 2];
            let rings = (0..r.count()?)
                .map(|_| {
                    (0..r.count()?)
                        .map(|_| {
                            cur[0] = cur[0].wrapping_add(r.zvarint()?);
                            cur[1] = cur[1].wrapping_add(r.zvarint()?);
                            Ok([(cur[0] as u32) << SHIFT, (cur[1] as u32) << SHIFT])
                        })
                        .collect::<io::Result<Vec<P>>>()
                })
                .collect::<io::Result<Vec<_>>>()?;
            zones.push(Zone::new(name, bbox, &rings, &db));
        }
        if !r.at_end() {
            return Err(invalid("TZON has trailing bytes"));
        }
        Ok(Zones { zones })
    }

    pub fn zones(&self) -> &[Zone] {
        &self.zones
    }

    pub fn zone(&self, i: usize) -> &Zone {
        &self.zones[i]
    }

    /// The zone at `p` whose rules are known: the first holding it, else the nearest within
    /// [`NEAR_M`].
    pub fn at(&self, p: P) -> Option<usize> {
        if let Some(i) = self
            .zones
            .iter()
            .position(|z| z.tz.is_some() && z.contains(p))
        {
            return Some(i);
        }
        let margin = (NEAR_M / geo::metres_per_unit(p)).ceil() as u32;
        self.zones
            .iter()
            .enumerate()
            .filter(|(_, z)| z.tz.is_some() && z.bbox_contains(p, margin))
            .map(|(i, z)| (i, z.distance(p)))
            .filter(|&(_, d)| d <= f64::from(margin))
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    }

    /// The rules of the zone at `p`, with its index.
    pub fn tz_at(&self, p: P) -> Option<(usize, &TimeZone)> {
        let i = self.at(p)?;
        Some((i, self.zones[i].tz.as_ref()?))
    }

    /// Bytes held in RAM: the names and edges (not the tz rules, which `jiff` shares).
    pub fn heap_bytes(&self) -> usize {
        self.zones.capacity() * std::mem::size_of::<Zone>()
            + self
                .zones
                .iter()
                .map(|z| z.name.capacity() + z.starts.capacity() * 4 + z.edges.capacity() * 16)
                .sum::<usize>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::project;

    /// A lat/lon box as a ring.
    pub fn boxed(s: f64, w: f64, n: f64, e: f64) -> Vec<P> {
        vec![project(n, w), project(n, e), project(s, e), project(s, w)]
    }

    #[test]
    fn finds_zones_and_rules() {
        let zones = [
            (
                "America/Chicago".to_string(),
                vec![boxed(30.0, -95.0, 40.0, -87.0)],
            ),
            // A hole, and a zone in it.
            (
                "America/New_York".to_string(),
                vec![
                    boxed(30.0, -87.0, 40.0, -75.0),
                    boxed(34.0, -84.0, 36.0, -80.0),
                ],
            ),
            (
                "America/Phoenix".to_string(),
                vec![boxed(34.0, -84.0, 36.0, -80.0)],
            ),
            (
                "Mars/Olympus_Mons".to_string(),
                vec![boxed(41.0, -95.0, 45.0, -75.0)],
            ),
        ];
        let z = Zones::decode(&encode(&zones, 3).unwrap()).unwrap();
        assert_eq!(z.zones().len(), 4);
        let name = |lat: f64, lon: f64| z.at(project(lat, lon)).map(|i| z.zone(i).name.as_str());
        assert_eq!(name(35.0, -90.0), Some("America/Chicago"));
        assert_eq!(name(31.0, -80.0), Some("America/New_York"));
        assert_eq!(name(35.0, -82.0), Some("America/Phoenix"));
        // Unknown to the tz database: never answered.
        assert_eq!(name(43.0, -85.0), None);
        // Just outside every zone: the nearest, within a kilometre.
        assert_eq!(name(29.996, -90.0), Some("America/Chicago"));
        assert_eq!(name(29.9, -90.0), None);
        let (_, tz) = z.tz_at(project(35.0, -90.0)).unwrap();
        assert_eq!(tz.iana_name(), Some("America/Chicago"));
    }

    #[test]
    fn long_edges_span_their_bands() {
        // A tall thin zone: its sides cross hundreds of bands.
        let zones = [(
            "America/Regina".to_string(),
            vec![boxed(10.0, -100.0, 70.0, -99.9)],
        )];
        let z = Zones::decode(&encode(&zones, 3).unwrap()).unwrap();
        for lat in [10.5, 30.0, 49.0, 69.5] {
            assert_eq!(z.at(project(lat, -99.95)), Some(0), "{lat}");
            assert_eq!(z.at(project(lat, -99.5)), None, "{lat}");
        }
    }
}
