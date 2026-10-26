//! The search sections (`SENT`, `STWN`, `SFST`, `SPST`, `docs/format.md`), and [`Search`],
//! which opens them with the admin, street, address and grid readers and answers the
//! lookups that need several: placing an address, resolving a house number, and what is
//! near a point.
//!
//! The `fst`, the towns and the block indexes stay in RAM (phase 2 decision 1); entries,
//! postings, streets and addresses are read with positioned reads into small caches.

use std::io;
use std::sync::Arc;

use crate::addr::{self, Address, Addresses, Loose, Range};
use crate::admin::Admin;
use crate::blocked::{Blocked, Cache, string, strings};
use crate::geo::{self, P};
use crate::grid::Grid;
use crate::pack::{Pack, Section};
use crate::street::{STEP_M, Spot, Streets};
use crate::varint::{Reader, invalid};
use crate::zframe;

pub const SENT: [u8; 4] = *b"SENT";
pub const STWN: [u8; 4] = *b"STWN";
pub const SFST: [u8; 4] = *b"SFST";
pub const SPST: [u8; 4] = *b"SPST";
/// `SENT` points are in units of 2^6 world units.
pub const SHIFT: u32 = 6;
/// Decoded entry blocks kept, in bytes.
const CACHE_BYTES: usize = 4 << 20;
/// The first minor version whose search sections this reader understands (format 0.4).
pub const MIN_MINOR: u16 = 4;

/// What an entry is (`SENT` `kind`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    /// An admin area no place node stands for; `class` is its level, `reference` its ID.
    Admin = 0,
    /// A `places` node; `reference` is the admin ID + 1 of the area it stands for, or 0.
    Place = 1,
    /// A street; `class` is its road class, `reference` its street ID.
    Street = 2,
    Poi = 3,
    /// `class` 0 five digits, 1 Canadian; `reference` metres holding 90% of its addresses.
    Postcode = 4,
    /// A `labels` feature: lake, river, park, …
    Feature = 5,
    /// A street known only from unattached addresses; `reference` as for postcodes.
    AddressStreet = 6,
}

impl Kind {
    pub fn from_code(c: u64) -> Option<Self> {
        Some(match c {
            0 => Self::Admin,
            1 => Self::Place,
            2 => Self::Street,
            3 => Self::Poi,
            4 => Self::Postcode,
            5 => Self::Feature,
            6 => Self::AddressStreet,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub kind: Kind,
    pub class: u16,
    pub importance: u8,
    pub name: String,
    /// To 2^6 world units.
    pub p: P,
    /// The deepest admin area holding it (for an area, its parent).
    pub admin: Option<u32>,
    /// Index into the towns.
    pub town: Option<u32>,
    pub reference: u64,
    pub alts: Vec<String>,
}

impl Entry {
    fn heap_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.name.capacity()
            + self.alts.iter().map(|a| 24 + a.capacity()).sum::<usize>()
    }
}

/// A token's postings: entry IDs, ascending.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Postings {
    pub ids: Vec<u32>,
    /// Entries holding the token: more than `ids` for a generic token left out of postings,
    /// or when the list was read only in part.
    pub total: u64,
    /// IDs in the whole list.
    pub stored: u64,
}

fn decode_entries(raw: &[u8]) -> io::Result<Vec<Entry>> {
    let mut r = Reader::new(raw);
    let mut cur = [0i32; 2];
    let mut out = Vec::new();
    while !r.at_end() {
        let kc = r.varint()?;
        let kind = Kind::from_code(kc & 7).ok_or_else(|| invalid("bad entry kind"))?;
        let importance = r.bytes(1)?[0];
        let name = string(&mut r)?;
        cur[0] = cur[0].wrapping_add(r.zvarint()?);
        cur[1] = cur[1].wrapping_add(r.zvarint()?);
        let admin = r.varint()?;
        let town = r.varint()?;
        let reference = r.varint()?;
        let alts = strings(&mut r)?;
        out.push(Entry {
            kind,
            class: (kc >> 3) as u16,
            importance,
            name,
            p: [(cur[0] as u32) << SHIFT, (cur[1] as u32) << SHIFT],
            admin: admin.checked_sub(1).map(|a| a as u32),
            town: town.checked_sub(1).map(|t| t as u32),
            reference,
            alts,
        });
    }
    Ok(out)
}

/// Entries, towns and the token index.
pub struct Index {
    pack: Arc<Pack>,
    sent: Blocked,
    spst: Section,
    towns: Vec<String>,
    fst: fst::Map<Vec<u8>>,
    entries: Cache<usize, Vec<Entry>>,
}

impl Index {
    pub fn open(pack: Arc<Pack>) -> io::Result<Option<Self>> {
        let sent = Blocked::open(&pack, SENT)?;
        let (Some(sent), Some(&stwn), Some(&sfst), Some(&spst)) = (
            sent,
            pack.section(STWN),
            pack.section(SFST),
            pack.section(SPST),
        ) else {
            return Ok(None);
        };
        let raw = zframe::decompress(
            &mut zstd::zstd_safe::DCtx::create(),
            &pack.read_section(&stwn)?,
            None,
        )?;
        let towns = strings(&mut Reader::new(&raw))?;
        let fst = fst::Map::new(pack.read_section(&sfst)?).map_err(|_| invalid("bad SFST"))?;
        Ok(Some(Self {
            pack,
            sent,
            spst,
            towns,
            fst,
            entries: Cache::new(CACHE_BYTES),
        }))
    }

    pub fn count(&self) -> u32 {
        self.sent.count
    }

    pub fn towns(&self) -> &[String] {
        &self.towns
    }

    pub fn town(&self, i: u32) -> Option<&str> {
        self.towns.get(i as usize).map(String::as_str)
    }

    /// The token map, for prefix and fuzzy matching: token → offset of its postings.
    pub fn tokens(&self) -> &fst::Map<Vec<u8>> {
        &self.fst
    }

    pub fn entry(&self, id: u32) -> io::Result<Entry> {
        let block = self
            .sent
            .find(id)
            .filter(|_| id < self.count())
            .ok_or_else(|| invalid("no such entry"))?;
        let entries = self.entries.get(block, || {
            let (raw, read) = self.sent.read(&self.pack, block)?;
            let v = decode_entries(&raw)?;
            let bytes = v.iter().map(Entry::heap_bytes).sum();
            Ok((v, bytes, read))
        })?;
        entries
            .get((id - self.sent.first(block)) as usize)
            .cloned()
            .ok_or_else(|| invalid("SENT block holds fewer entries"))
    }

    /// The postings of `token`, or `None` if no entry holds it.
    pub fn postings(&self, token: &str) -> io::Result<Option<Postings>> {
        match self.fst.get(token) {
            Some(offset) => self.postings_at(offset, usize::MAX).map(Some),
            None => Ok(None),
        }
    }

    /// The postings list at `offset` in `SPST` (an `fst` value), reading at most its first
    /// `limit` IDs, which are the most important.
    pub fn postings_at(&self, offset: u64, limit: usize) -> io::Result<Postings> {
        let len = self.spst.len;
        if offset >= len {
            return Err(invalid("postings past the end of SPST"));
        }
        let head = self
            .pack
            .read(self.spst.offset + offset, (len - offset).min(30) as usize)?;
        let mut r = Reader::new(&head);
        let (n, total, bytes) = (r.varint()?, r.varint()?, r.varint()?);
        let at = offset + r.i as u64;
        if at + bytes > len || n > bytes {
            return Err(invalid("postings list past the end of SPST"));
        }
        let take = n.min(limit as u64);
        let read = bytes.min(take.saturating_mul(5)) as usize;
        let deltas = self.pack.read(self.spst.offset + at, read)?;
        let mut d = Reader::new(&deltas);
        let mut ids = Vec::with_capacity(take as usize);
        let mut prev = 0u32;
        for _ in 0..take {
            prev = prev
                .checked_add(d.varint()? as u32)
                .ok_or_else(|| invalid("posting out of range"))?;
            ids.push(prev);
        }
        Ok(Postings {
            ids,
            total,
            stored: n,
        })
    }

    pub fn heap_bytes(&self) -> usize {
        self.sent.heap_bytes()
            + self.towns.iter().map(|t| 24 + t.capacity()).sum::<usize>()
            + self.fst.as_fst().as_bytes().len()
            + self.entries.held()
    }

    pub fn bytes_read(&self) -> u64 {
        self.entries
            .bytes_read
            .load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// Where a house number on a street is.
#[derive(Clone, Debug, PartialEq)]
pub struct Located {
    pub p: P,
    /// Interpolated from a range, or an NAR blockface point.
    pub approx: bool,
    pub found: Found,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Found {
    Point(Address),
    Range(Range),
}

/// An address near a point.
#[derive(Clone, Debug, PartialEq)]
pub struct Nearby {
    pub p: P,
    pub dist_m: f64,
    pub address: NearAddress,
}

#[derive(Clone, Debug, PartialEq)]
pub enum NearAddress {
    Street(Address),
    Loose(Loose),
}

/// Every reader search and "what's here" need, over one pack.
pub struct Search {
    pub index: Index,
    pub admin: Admin,
    pub streets: Streets,
    pub addresses: Option<Addresses>,
    pub grid: Grid,
}

impl Search {
    /// Opens the search readers; `None` for a pack without search (built before format
    /// 0.4). The address sections are optional.
    pub fn open(pack: Arc<Pack>) -> io::Result<Option<Self>> {
        if pack.minor < MIN_MINOR {
            return Ok(None);
        }
        let (Some(index), Some(admin), Some(streets), Some(grid)) = (
            Index::open(pack.clone())?,
            Admin::open(pack.clone())?,
            Streets::open(pack.clone())?,
            Grid::open(pack.clone())?,
        ) else {
            return Ok(None);
        };
        let addresses = Addresses::open(pack)?;
        Ok(Some(Self {
            index,
            admin,
            streets,
            addresses,
            grid,
        }))
    }

    /// Where an address on a street is: the point at its offset.
    pub fn address_point(&self, a: &Address) -> io::Result<P> {
        Ok(self
            .streets
            .geometry(a.street)?
            .point_at(f64::from(a.offset) * STEP_M))
    }

    /// Number `number` (with `suffix`, "" for none) on `street`: its points if it has any
    /// (all of them, the exact suffix first), else the ranges holding it, interpolated.
    pub fn locate(&self, street: u32, number: u32, suffix: &str) -> io::Result<Vec<Located>> {
        let Some(addresses) = &self.addresses else {
            return Ok(Vec::new());
        };
        let geom = self.streets.geometry(street)?;
        let mut points: Vec<Address> = addresses
            .run(street)?
            .into_iter()
            .filter(|a| a.number == number)
            .collect();
        if !points.is_empty() {
            points.sort_by_key(|a| !a.suffix.eq_ignore_ascii_case(suffix));
            return Ok(points
                .into_iter()
                .map(|a| Located {
                    p: geom.point_at(f64::from(a.offset) * STEP_M),
                    approx: a.approx,
                    found: Found::Point(a),
                })
                .collect());
        }
        Ok(addresses
            .ranges(street)?
            .into_iter()
            .filter(|r| r.holds(number))
            .map(|r| Located {
                p: geom.point_at(r.offset_of(number) * STEP_M),
                approx: true,
                found: Found::Range(r),
            })
            .collect())
    }

    /// The streets within `radius_m` of `p`, nearest first, with where `p` lies on each.
    pub fn streets_near(&self, p: P, radius_m: f64) -> io::Result<Vec<(u32, Spot)>> {
        let mut ids: Vec<u32> = self
            .grid
            .around(p, radius_m)?
            .into_iter()
            .flat_map(|(_, c)| c.streets)
            .collect();
        ids.sort_unstable();
        ids.dedup();
        let mut out = Vec::new();
        for s in ids {
            let spot = self.streets.geometry(s)?.project(p);
            if spot.dist_m <= radius_m {
                out.push((s, spot));
            }
        }
        out.sort_by(|a, b| a.1.dist_m.total_cmp(&b.1.dist_m).then(a.0.cmp(&b.0)));
        Ok(out)
    }

    /// The point addresses within `radius_m` of `p`, on streets and unattached, nearest
    /// first. Ranges are not included.
    pub fn addresses_near(&self, p: P, radius_m: f64) -> io::Result<Vec<Nearby>> {
        let Some(addresses) = &self.addresses else {
            return Ok(Vec::new());
        };
        let mpu = geo::metres_per_unit(p);
        let cells = self.grid.around(p, radius_m)?;
        let mut out = Vec::new();
        for (s, _) in self.streets_near(p, radius_m)? {
            let geom = self.streets.geometry(s)?;
            for a in addresses.run(s)? {
                let q = geom.point_at(f64::from(a.offset) * STEP_M);
                let d = geo::dist(p, q) * mpu;
                if d <= radius_m {
                    out.push(Nearby {
                        p: q,
                        dist_m: d,
                        address: NearAddress::Street(a),
                    });
                }
            }
        }
        for ((x, y), c) in cells {
            if !c.loose {
                continue;
            }
            for l in addresses.loose_in(loose_cell(x, y))? {
                let d = geo::dist(p, l.p) * mpu;
                if d <= radius_m {
                    out.push(Nearby {
                        p: l.p,
                        dist_m: d,
                        address: NearAddress::Loose(l),
                    });
                }
            }
        }
        out.sort_by(|a, b| a.dist_m.total_cmp(&b.dist_m));
        Ok(out)
    }

    /// Bytes the readers hold in RAM: the `fst`, tables, block indexes and caches.
    pub fn heap_bytes(&self) -> usize {
        self.index.heap_bytes()
            + self.admin.heap_bytes()
            + self.streets.heap_bytes()
            + self.addresses.as_ref().map_or(0, Addresses::heap_bytes)
            + self.grid.heap_bytes()
    }

    /// Compressed bytes read from the pack into the caches so far.
    pub fn bytes_read(&self) -> u64 {
        self.index.bytes_read()
            + self.admin.bytes_read()
            + self.streets.bytes_read()
            + self.addresses.as_ref().map_or(0, Addresses::bytes_read)
            + self.grid.bytes_read()
    }
}

/// The `ALSE` cell of a grid cell: both are z14.
pub fn loose_cell(x: u32, y: u32) -> u32 {
    debug_assert_eq!(u32::from(crate::grid::ZOOM), addr::CELL_ZOOM);
    geo::morton([x, y]) as u32
}
