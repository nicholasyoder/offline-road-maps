//! Admin areas (`SADM`, `SAPL`, `docs/format.md`): names, levels and chains, and point in
//! polygon. The table is held in RAM; polygons are read a block at a time and cached.

use std::io;
use std::sync::Arc;

use zstd::zstd_safe::DCtx;

use crate::blocked::{Blocked, Cache, string};
use crate::geo::{P, rings_contain};
use crate::pack::Pack;
use crate::varint::{Reader, invalid};
use crate::zframe;

pub const SADM: [u8; 4] = *b"SADM";
pub const SAPL: [u8; 4] = *b"SAPL";
/// `SAPL` points are in units of 2^6 world units.
pub const SAPL_SHIFT: u32 = 6;
pub const FLAG_TOWN: u64 = 1;
pub const FLAG_BROKEN: u64 = 2;
/// Decoded polygon blocks kept, in bytes.
const CACHE_BYTES: usize = 4 << 20;

#[derive(Clone, Debug, PartialEq)]
pub struct Area {
    /// OSM `admin_level`: 2 country, 4 state or province, 6 county, 8 city or town, …
    pub level: u8,
    pub parent: Option<u32>,
    /// A level 8 area that is a city, town or village rather than a township.
    pub town: bool,
    /// Some member ways were missing, so the polygon is partial.
    pub broken: bool,
    pub name: String,
    /// `OH`, `QC`, or a country's ISO code; empty if none.
    pub abbr: String,
    pub population: u64,
    pub label: P,
    /// `min_x, min_y, max_x, max_y` in world coordinates.
    pub bbox: [u32; 4],
    pub osm_id: u64,
    /// `(key, name)`, sorted by key.
    pub alt_names: Vec<(String, String)>,
}

impl Area {
    pub fn bbox_contains(&self, p: P) -> bool {
        let b = self.bbox;
        b[0] <= p[0] && p[0] <= b[2] && b[1] <= p[1] && p[1] <= b[3]
    }

    fn heap_bytes(&self) -> usize {
        self.name.capacity()
            + self.abbr.capacity()
            + self
                .alt_names
                .iter()
                .map(|(k, v)| 48 + k.capacity() + v.capacity())
                .sum::<usize>()
    }
}

/// An area's rings, in world coordinates, not closed.
pub type Polygon = Vec<Vec<P>>;

pub struct Admin {
    pack: Arc<Pack>,
    areas: Vec<Area>,
    sapl: Blocked,
    polygons: Cache<usize, Vec<Polygon>>,
}

/// Decodes `SADM`.
pub fn decode_areas(sadm: &[u8]) -> io::Result<Vec<Area>> {
    let raw = zframe::decompress(&mut DCtx::create(), sadm, None)?;
    let mut r = Reader::new(&raw);
    let n = r.count()?;
    let mut areas = Vec::with_capacity(n);
    for _ in 0..n {
        let level = r.bytes(1)?[0];
        let parent = r.varint()?;
        let flags = r.varint()?;
        let name = string(&mut r)?;
        let abbr = string(&mut r)?;
        let population = r.varint()?;
        let fixed = r.bytes(24)?;
        let u = |i: usize| u32::from_le_bytes(fixed[4 * i..4 * i + 4].try_into().unwrap());
        let osm_id = r.varint()?;
        let alt_names = (0..r.count()?)
            .map(|_| Ok((string(&mut r)?, string(&mut r)?)))
            .collect::<io::Result<_>>()?;
        let parent = match parent.checked_sub(1) {
            Some(p) if p >= n as u64 => return Err(invalid("SADM parent out of range")),
            p => p.map(|p| p as u32),
        };
        areas.push(Area {
            level,
            parent,
            town: flags & FLAG_TOWN != 0,
            broken: flags & FLAG_BROKEN != 0,
            name,
            abbr,
            population,
            label: [u(0), u(1)],
            bbox: [u(2), u(3), u(4), u(5)],
            osm_id,
            alt_names,
        });
    }
    if !r.at_end() {
        return Err(invalid("SADM has trailing bytes"));
    }
    Ok(areas)
}

/// Decodes one `SAPL` block: the polygons of its areas, in order.
fn decode_polygons(raw: &[u8]) -> io::Result<Vec<Polygon>> {
    let mut r = Reader::new(raw);
    let mut out = Vec::new();
    while !r.at_end() {
        let mut cur = [0i32; 2];
        let rings = (0..r.count()?)
            .map(|_| {
                (0..r.count()?)
                    .map(|_| {
                        cur[0] = cur[0].wrapping_add(r.zvarint()?);
                        cur[1] = cur[1].wrapping_add(r.zvarint()?);
                        Ok([(cur[0] as u32) << SAPL_SHIFT, (cur[1] as u32) << SAPL_SHIFT])
                    })
                    .collect::<io::Result<Vec<P>>>()
            })
            .collect::<io::Result<_>>()?;
        out.push(rings);
    }
    Ok(out)
}

impl Admin {
    /// Reads `SADM` and the `SAPL` index; `None` if the pack has neither.
    pub fn open(pack: Arc<Pack>) -> io::Result<Option<Self>> {
        let (Some(&sadm), Some(sapl)) = (pack.section(SADM), Blocked::open(&pack, SAPL)?) else {
            return Ok(None);
        };
        let areas = decode_areas(&pack.read_section(&sadm)?)?;
        if sapl.count as usize != areas.len() {
            return Err(invalid("SAPL and SADM hold different numbers of areas"));
        }
        Ok(Some(Self {
            pack,
            areas,
            sapl,
            polygons: Cache::new(CACHE_BYTES),
        }))
    }

    pub fn areas(&self) -> &[Area] {
        &self.areas
    }

    pub fn area(&self, id: u32) -> Option<&Area> {
        self.areas.get(id as usize)
    }

    /// `id` and its parents, from `id` down to the country.
    pub fn parents(&self, id: u32) -> Vec<u32> {
        let mut out = vec![id];
        while let Some(p) = self.areas[*out.last().unwrap() as usize].parent {
            // A parent is always of a lower level, so this ends; guard against bad data.
            if out.len() > 16 {
                break;
            }
            out.push(p);
        }
        out
    }

    /// The polygon of area `id`.
    pub fn polygon(&self, id: u32) -> io::Result<Polygon> {
        let block = self
            .sapl
            .find(id)
            .filter(|_| (id as usize) < self.areas.len())
            .ok_or_else(|| invalid("no such admin area"))?;
        let polys = self.polygons.get(block, || {
            let (raw, read) = self.sapl.read(&self.pack, block)?;
            let polys = decode_polygons(&raw)?;
            let bytes = polys
                .iter()
                .flatten()
                .map(|r| 24 + r.len() * 8)
                .sum::<usize>()
                + raw.len() / 4;
            Ok((polys, bytes, read))
        })?;
        polys
            .get((id - self.sapl.first(block)) as usize)
            .cloned()
            .ok_or_else(|| invalid("SAPL block holds fewer areas"))
    }

    pub fn contains(&self, id: u32, p: P) -> io::Result<bool> {
        let a = self.area(id).ok_or_else(|| invalid("no such admin area"))?;
        Ok(a.bbox_contains(p) && rings_contain(&self.polygon(id)?, p))
    }

    /// The areas containing `p`, from the lowest level (country) up, one per level. Levels
    /// 5 and up are tested polygon by polygon, deepest first; states and countries are taken
    /// from the deepest area's parents when it has them, so their large polygons are rarely
    /// read.
    pub fn chain(&self, p: P) -> io::Result<Vec<u32>> {
        // Areas are ordered by level, so a reverse scan is deepest first.
        let mut found: Vec<u32> = Vec::new();
        let mut last_level = None;
        for (i, a) in self.areas.iter().enumerate().rev() {
            if last_level == Some(a.level) || !a.bbox_contains(p) {
                continue;
            }
            if a.level <= 4
                && let Some(&deepest) = found.first()
            {
                let up: Vec<u32> = self.parents(deepest)[1..]
                    .iter()
                    .copied()
                    .filter(|&q| self.areas[q as usize].level <= 4)
                    .collect();
                if !up.is_empty() {
                    found.extend(up);
                    break;
                }
            }
            if self.contains(i as u32, p)? {
                found.push(i as u32);
                last_level = Some(a.level);
            }
        }
        found.reverse();
        Ok(found)
    }

    /// Bytes held in RAM: the table, the block index and the cached polygons.
    pub fn heap_bytes(&self) -> usize {
        self.areas.capacity() * std::mem::size_of::<Area>()
            + self.areas.iter().map(Area::heap_bytes).sum::<usize>()
            + self.sapl.heap_bytes()
            + self.polygons.held()
    }

    /// Compressed bytes read for polygons so far.
    pub fn bytes_read(&self) -> u64 {
        self.polygons
            .bytes_read
            .load(std::sync::atomic::Ordering::Relaxed)
    }
}
