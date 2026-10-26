//! The street table (`SSTR`) and street geometry (`SGEO`, `docs/format.md`): records,
//! polylines, and offsets along them, as the builder measures them.

use std::io;
use std::sync::Arc;

use crate::blocked::{Blocked, Cache, string, strings};
use crate::geo::{self, P};
use crate::pack::Pack;
use crate::varint::{Reader, invalid};

pub const SSTR: [u8; 4] = *b"SSTR";
pub const SGEO: [u8; 4] = *b"SGEO";
/// Stored points are in units of 2^6 world units.
pub const SHIFT: u32 = 6;
/// Address offsets are in steps of this many metres; each part starts on a fresh step.
pub const STEP_M: f64 = 5.0;
/// Decoded blocks kept, in bytes: records and geometry.
const RECORD_CACHE: usize = 2 << 20;
const GEOMETRY_CACHE: usize = 4 << 20;

/// Road classes, as in the `SSTR` flags.
pub const CLASSES: [&str; 19] = [
    "motorway",
    "trunk",
    "primary",
    "secondary",
    "tertiary",
    "motorway_link",
    "trunk_link",
    "primary_link",
    "secondary_link",
    "tertiary_link",
    "unclassified",
    "residential",
    "living_street",
    "busway",
    "road",
    "service",
    "pedestrian",
    "track",
    "other",
];

/// Where a street's town comes from (`SSTR` flags bits 5–7).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TownSource {
    Addresses,
    Admin,
    Neighbours,
    Place,
    None,
}

impl TownSource {
    fn from_code(c: u64) -> io::Result<Self> {
        Ok(match c {
            0 => Self::Addresses,
            1 => Self::Admin,
            2 => Self::Neighbours,
            3 => Self::Place,
            4 => Self::None,
            _ => return Err(invalid("bad street town source")),
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Street {
    /// Index into [`CLASSES`].
    pub class: u8,
    pub town_source: TownSource,
    pub name: String,
    /// The postal town shown with it; empty if none.
    pub town: String,
    /// The deepest admin area holding its middle.
    pub admin: Option<u32>,
    pub length_m: u64,
    pub alt_names: Vec<String>,
    pub refs: Vec<String>,
}

impl Street {
    fn heap_bytes(&self) -> usize {
        self.name.capacity()
            + self.town.capacity()
            + self
                .alt_names
                .iter()
                .chain(&self.refs)
                .map(|s| 24 + s.capacity())
                .sum::<usize>()
    }
}

/// Where a point lies relative to a street.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spot {
    /// Metres along the street.
    pub offset_m: f64,
    /// Metres from the street.
    pub dist_m: f64,
    /// On the left when walking in the offset direction.
    pub left: bool,
    /// The nearest point on the street.
    pub foot: P,
}

/// A street's parts, with the metres along them at each point.
#[derive(Clone, Debug)]
pub struct Geometry {
    pub parts: Vec<Vec<P>>,
    /// Metres per world unit at the first point.
    pub mpu: f64,
    /// Per part, metres from the street's start at each point.
    pub cum: Vec<Vec<f64>>,
}

impl Geometry {
    /// Offsets as format.md defines them: segment lengths in world units times the metres
    /// per unit at the first point; part k + 1 starts at `5 × (⌈end of part k / 5⌉ + 1)`.
    pub fn new(parts: Vec<Vec<P>>) -> Self {
        let mpu = parts
            .first()
            .and_then(|p| p.first())
            .map_or(1.0, |&p| geo::metres_per_unit(p));
        let mut at = 0.0;
        let cum = parts
            .iter()
            .enumerate()
            .map(|(k, part)| {
                if k > 0 {
                    at = STEP_M * ((at / STEP_M).ceil() + 1.0);
                }
                let mut c = Vec::with_capacity(part.len());
                for (i, p) in part.iter().enumerate() {
                    if i > 0 {
                        at += geo::dist(part[i - 1], *p) * mpu;
                    }
                    c.push(at);
                }
                c
            })
            .collect();
        Self { parts, mpu, cum }
    }

    pub fn length_m(&self) -> f64 {
        self.cum
            .last()
            .and_then(|c| c.last())
            .copied()
            .unwrap_or(0.0)
    }

    /// The point `m` metres along: on the last part that starts at or before `m`, clamped
    /// to its ends.
    pub fn point_at(&self, m: f64) -> P {
        let k = self
            .cum
            .partition_point(|c| c[0] <= m)
            .saturating_sub(1)
            .min(self.parts.len() - 1);
        let (part, cum) = (&self.parts[k], &self.cum[k]);
        if part.len() == 1 {
            return part[0];
        }
        let i = cum.partition_point(|&d| d < m).clamp(1, part.len() - 1);
        let (a, b) = (part[i - 1], part[i]);
        let seg = cum[i] - cum[i - 1];
        let t = if seg > 0.0 {
            ((m - cum[i - 1]) / seg).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let lerp = |a: u32, b: u32| (f64::from(a) + (f64::from(b) - f64::from(a)) * t) as u32;
        [lerp(a[0], b[0]), lerp(a[1], b[1])]
    }

    /// The nearest point of the street to `p`.
    pub fn project(&self, p: P) -> Spot {
        let (px, py) = (f64::from(p[0]), f64::from(p[1]));
        // (squared distance, offset, left, foot)
        let mut best = (f64::INFINITY, 0.0, false, p);
        for (part, cum) in self.parts.iter().zip(&self.cum) {
            if part.len() == 1 {
                let d = geo::dist(part[0], p);
                if d * d < best.0 {
                    best = (d * d, cum[0], false, part[0]);
                }
                continue;
            }
            for i in 0..part.len() - 1 {
                let (a, b) = (part[i], part[i + 1]);
                let (ax, ay) = (f64::from(a[0]), f64::from(a[1]));
                let (dx, dy) = (f64::from(b[0]) - ax, f64::from(b[1]) - ay);
                let len2 = dx * dx + dy * dy;
                let t = if len2 > 0.0 {
                    (((px - ax) * dx + (py - ay) * dy) / len2).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let (fx, fy) = (ax + t * dx, ay + t * dy);
                let d2 = (px - fx).powi(2) + (py - fy).powi(2);
                if d2 < best.0 {
                    // y grows downward, so a negative cross product is the left side.
                    let left = dx * (py - ay) - dy * (px - ax) < 0.0;
                    let off = cum[i] + t * (cum[i + 1] - cum[i]);
                    best = (d2, off, left, [fx.round() as u32, fy.round() as u32]);
                }
            }
        }
        Spot {
            offset_m: best.1,
            dist_m: best.0.sqrt() * self.mpu,
            left: best.2,
            foot: best.3,
        }
    }

    fn heap_bytes(&self) -> usize {
        self.parts
            .iter()
            .map(|p| 48 + p.capacity() * 8)
            .sum::<usize>()
            + self.cum.iter().map(|c| c.capacity() * 8).sum::<usize>()
    }
}

/// Reads street records and geometry by street ID.
pub struct Streets {
    pack: Arc<Pack>,
    sstr: Blocked,
    sgeo: Blocked,
    records: Cache<usize, Vec<Street>>,
    geometry: Cache<usize, Vec<Arc<Geometry>>>,
}

fn decode_records(raw: &[u8]) -> io::Result<Vec<Street>> {
    let mut r = Reader::new(raw);
    let mut out = Vec::new();
    while !r.at_end() {
        let flags = r.varint()?;
        let name = string(&mut r)?;
        let town = string(&mut r)?;
        let admin = r.varint()?;
        let length_m = r.varint()?;
        let alt_names = strings(&mut r)?;
        let refs = strings(&mut r)?;
        out.push(Street {
            class: (flags & 31).min(CLASSES.len() as u64 - 1) as u8,
            town_source: TownSource::from_code(flags >> 5)?,
            name,
            town,
            admin: admin.checked_sub(1).map(|a| a as u32),
            length_m,
            alt_names,
            refs,
        });
    }
    Ok(out)
}

fn decode_geometry(raw: &[u8]) -> io::Result<Vec<Arc<Geometry>>> {
    let mut r = Reader::new(raw);
    let mut out = Vec::new();
    let mut cur = [0i32; 2];
    while !r.at_end() {
        let nparts = r.count()?;
        if nparts == 0 {
            return Err(invalid("street without parts"));
        }
        let mut parts = Vec::with_capacity(nparts);
        for _ in 0..nparts {
            let npts = r.count()?;
            if npts == 0 {
                return Err(invalid("street part without points"));
            }
            let mut part = Vec::with_capacity(npts);
            for _ in 0..npts {
                cur[0] = cur[0].wrapping_add(r.zvarint()?);
                cur[1] = cur[1].wrapping_add(r.zvarint()?);
                part.push([(cur[0] as u32) << SHIFT, (cur[1] as u32) << SHIFT]);
            }
            parts.push(part);
        }
        out.push(Arc::new(Geometry::new(parts)));
    }
    Ok(out)
}

impl Streets {
    /// Reads both block indexes; `None` if the pack has no street table.
    pub fn open(pack: Arc<Pack>) -> io::Result<Option<Self>> {
        let (Some(sstr), Some(sgeo)) = (Blocked::open(&pack, SSTR)?, Blocked::open(&pack, SGEO)?)
        else {
            return Ok(None);
        };
        if sstr.count != sgeo.count {
            return Err(invalid("SSTR and SGEO hold different numbers of streets"));
        }
        Ok(Some(Self {
            pack,
            sstr,
            sgeo,
            records: Cache::new(RECORD_CACHE),
            geometry: Cache::new(GEOMETRY_CACHE),
        }))
    }

    pub fn count(&self) -> u32 {
        self.sstr.count
    }

    fn check(&self, id: u32) -> io::Result<()> {
        match id < self.count() {
            true => Ok(()),
            false => Err(invalid("no such street")),
        }
    }

    pub fn street(&self, id: u32) -> io::Result<Street> {
        self.check(id)?;
        let block = self
            .sstr
            .find(id)
            .ok_or_else(|| invalid("no such street"))?;
        let recs = self.records.get(block, || {
            let (raw, read) = self.sstr.read(&self.pack, block)?;
            let recs = decode_records(&raw)?;
            let bytes = recs
                .iter()
                .map(|s| std::mem::size_of::<Street>() + s.heap_bytes())
                .sum();
            Ok((recs, bytes, read))
        })?;
        recs.get((id - self.sstr.first(block)) as usize)
            .cloned()
            .ok_or_else(|| invalid("SSTR block holds fewer streets"))
    }

    pub fn geometry(&self, id: u32) -> io::Result<Arc<Geometry>> {
        self.check(id)?;
        let block = self
            .sgeo
            .find(id)
            .ok_or_else(|| invalid("no such street"))?;
        let geoms = self.geometry.get(block, || {
            let (raw, read) = self.sgeo.read(&self.pack, block)?;
            let geoms = decode_geometry(&raw)?;
            let bytes = geoms.iter().map(|g| 64 + g.heap_bytes()).sum();
            Ok((geoms, bytes, read))
        })?;
        geoms
            .get((id - self.sgeo.first(block)) as usize)
            .cloned()
            .ok_or_else(|| invalid("SGEO block holds fewer streets"))
    }

    /// Calls `f` for every street in order, reading each block once without caching it.
    pub fn for_each(&self, mut f: impl FnMut(u32, Street, &Geometry)) -> io::Result<()> {
        if self.sstr.blocks() != self.sgeo.blocks() {
            // Both are cut at the same streets by the builder; fall back to lookups if not.
            for id in 0..self.count() {
                f(id, self.street(id)?, &*self.geometry(id)?);
            }
            return Ok(());
        }
        for b in 0..self.sstr.blocks() {
            let (first, gfirst) = (self.sstr.first(b), self.sgeo.first(b));
            let recs = decode_records(&self.sstr.read(&self.pack, b)?.0)?;
            let geoms = decode_geometry(&self.sgeo.read(&self.pack, b)?.0)?;
            if first != gfirst || recs.len() != geoms.len() {
                return Err(invalid("SSTR and SGEO blocks differ"));
            }
            for (i, (s, g)) in recs.into_iter().zip(&geoms).enumerate() {
                f(first + i as u32, s, g);
            }
        }
        Ok(())
    }

    pub fn heap_bytes(&self) -> usize {
        self.sstr.heap_bytes() + self.sgeo.heap_bytes() + self.records.held() + self.geometry.held()
    }

    pub fn bytes_read(&self) -> u64 {
        use std::sync::atomic::Ordering::Relaxed;
        self.records.bytes_read.load(Relaxed) + self.geometry.bytes_read.load(Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_run_on_across_parts() {
        let p0 = geo::project(40.0, -83.0);
        let mpu = geo::metres_per_unit(p0);
        let east = |m: f64| [p0[0] + (m / mpu) as u32, p0[1]];
        let g = Geometry::new(vec![vec![p0, east(12.0)], vec![east(100.0), east(110.0)]]);
        // Part 0 ends at 12 m, so part 1 starts at 5 × (⌈12 / 5⌉ + 1) = 20 m.
        assert!((g.cum[0][1] - 12.0).abs() < 0.01);
        assert_eq!(g.cum[1][0], 20.0);
        assert!((g.length_m() - 30.0).abs() < 0.01);
        // 6 m is on part 0; 25 m is half way along part 1.
        let near = |a: P, b: P| geo::dist(a, b) * mpu < 0.05;
        assert!(near(g.point_at(6.0), east(6.0)));
        assert!(near(g.point_at(25.0), east(105.0)));
        assert!(
            near(g.point_at(16.0), east(12.0)),
            "clamped to part 0's end"
        );
        assert!(near(g.point_at(99.0), east(110.0)));

        // North of the street (y smaller) is on the left walking east.
        let s = g.project([east(104.0)[0], p0[1] - (3.0 / mpu) as u32]);
        assert!((s.offset_m - 24.0).abs() < 0.05 && (s.dist_m - 3.0).abs() < 0.05);
        assert!(s.left);
        assert!(near(s.foot, east(104.0)));
    }
}
