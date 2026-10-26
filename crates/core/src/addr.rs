//! Addresses (`ASTR`, `ASUF`, `AUNI`, `ALSE`, `ARNG`, `APCD`, `docs/format.md`): a street's
//! run with suffixes, postcodes and units, its interpolation ranges, the unattached points of
//! a z14 cell, and the postcodes.

use std::io;
use std::sync::Arc;

use crate::blocked::{Blocked, Cache, string, strings};
use crate::geo::{self, P};
use crate::pack::Pack;
use crate::varint::{Reader, invalid};

pub const ASTR: [u8; 4] = *b"ASTR";
pub const ASUF: [u8; 4] = *b"ASUF";
pub const AUNI: [u8; 4] = *b"AUNI";
pub const ALSE: [u8; 4] = *b"ALSE";
pub const ARNG: [u8; 4] = *b"ARNG";
pub const APCD: [u8; 4] = *b"APCD";
/// `ALSE` blocks hold whole cells of this zoom.
pub const CELL_ZOOM: u32 = 14;
/// `ALSE` positions are in units of 2^7 world units.
pub const LSE_SHIFT: u32 = 7;
/// Decoded blocks kept, in bytes: street runs, unattached points, ranges, postcodes.
const CACHE_BYTES: [usize; 4] = [4 << 20, 2 << 20, 1 << 20, 1 << 20];

/// An address on a street.
#[derive(Clone, Debug, PartialEq)]
pub struct Address {
    pub street: u32,
    pub number: u32,
    /// "A", "1/2", "-125"; empty for most.
    pub suffix: String,
    /// Steps of [`crate::street::STEP_M`] along the street.
    pub offset: i32,
    pub left: bool,
    pub osm: bool,
    /// An NAR blockface point: its position is approximate.
    pub approx: bool,
    /// Index into `APCD`: its own postcode, else its street's.
    pub postcode: Option<u32>,
    pub units: Vec<String>,
}

/// An address that matched no street, with its own position and street name.
#[derive(Clone, Debug, PartialEq)]
pub struct Loose {
    /// To 2^7 world units.
    pub p: P,
    /// As the source spells it.
    pub street: String,
    pub number: u32,
    pub suffix: String,
    pub osm: bool,
    pub approx: bool,
    pub postcode: Option<u32>,
    pub units: Vec<String>,
}

/// A TIGER or OSM interpolation range on a street.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Range {
    pub street: u32,
    /// Offsets in steps of 5 m; `end` may be before `start`.
    pub start: i32,
    pub end: i32,
    /// The numbers at `start` and at `end`.
    pub from: u32,
    pub to: u32,
    /// 0 all numbers, 1 odd, 2 even.
    pub parity: u8,
    pub left: bool,
}

impl Range {
    pub fn holds(&self, number: u32) -> bool {
        let (lo, hi) = (self.from.min(self.to), self.from.max(self.to));
        let parity_ok = match self.parity {
            1 => number % 2 == 1,
            2 => number.is_multiple_of(2),
            _ => true,
        };
        lo <= number && number <= hi && parity_ok
    }

    /// The offset of `number` in steps, interpolated linearly; the middle for a one-number
    /// range.
    pub fn offset_of(&self, number: u32) -> f64 {
        let (s, e) = (f64::from(self.start), f64::from(self.end));
        if self.from == self.to {
            return (s + e) / 2.0;
        }
        let t = (f64::from(number) - f64::from(self.from))
            / (f64::from(self.to) - f64::from(self.from));
        s + t.clamp(0.0, 1.0) * (e - s)
    }

    /// The number at `offset` (steps), rounded to the range's parity.
    pub fn number_at(&self, offset: f64) -> u32 {
        let (s, e) = (f64::from(self.start), f64::from(self.end));
        let t = if e == s {
            0.5
        } else {
            ((offset - s) / (e - s)).clamp(0.0, 1.0)
        };
        let n = f64::from(self.from) + t * (f64::from(self.to) - f64::from(self.from));
        let mut n = n.round() as u32;
        if (self.parity == 1 && n.is_multiple_of(2)) || (self.parity == 2 && n % 2 == 1) {
            n = if n > self.from.min(self.to) {
                n - 1
            } else {
                n + 1
            };
        }
        n
    }
}

/// One decoded `ASTR` block with its `ASUF` and `AUNI` blocks.
struct RunBlock {
    /// `(street, first address)`, in order; a street's run ends where the next begins.
    runs: Vec<(u32, u32)>,
    addresses: Vec<Address>,
}

impl RunBlock {
    fn run(&self, street: u32) -> &[Address] {
        match self.runs.binary_search_by_key(&street, |r| r.0) {
            Ok(i) => {
                let end = self
                    .runs
                    .get(i + 1)
                    .map_or(self.addresses.len(), |r| r.1 as usize);
                &self.addresses[self.runs[i].1 as usize..end]
            }
            Err(_) => &[],
        }
    }
}

fn code(v: u64) -> Option<u32> {
    v.checked_sub(1).map(|c| c as u32)
}

fn decode_runs(first: u32, astr: &[u8], asuf: &[u8], auni: &[u8]) -> io::Result<RunBlock> {
    let mut runs = Vec::new();
    let mut addresses = Vec::new();
    let mut r = Reader::new(astr);
    let mut street = first;
    while !r.at_end() {
        street = street
            .checked_add(r.varint()? as u32)
            .ok_or_else(|| invalid("ASTR street out of range"))?;
        let n = r.count()?;
        let postcode = code(r.varint()?);
        runs.push((street, addresses.len() as u32));
        let (mut number, mut offset) = (0u32, 0i32);
        for _ in 0..n {
            let v = r.varint()?;
            number = number.wrapping_add((v >> 3) as u32);
            offset = offset.wrapping_add(r.zvarint()?);
            addresses.push(Address {
                street,
                number,
                suffix: String::new(),
                offset,
                left: v & 4 != 0,
                osm: v & 2 != 0,
                approx: v & 1 != 0,
                postcode,
                units: Vec::new(),
            });
        }
    }
    let len = addresses.len();
    let at = |o: u64| -> io::Result<usize> {
        usize::try_from(o)
            .ok()
            .filter(|&i| i < len)
            .ok_or_else(|| invalid("ordinal past the block"))
    };
    let mut r = Reader::new(asuf);
    let mut o = 0;
    for _ in 0..r.count()? {
        o += r.varint()?;
        addresses[at(o)?].suffix = string(&mut r)?;
    }
    let mut o = 0;
    for _ in 0..r.count()? {
        o += r.varint()?;
        addresses[at(o)?].postcode = code(r.varint()?);
    }
    let mut r = Reader::new(auni);
    let mut o = 0;
    while !r.at_end() {
        o += r.varint()?;
        addresses[at(o)?].units = strings(&mut r)?;
    }
    Ok(RunBlock { runs, addresses })
}

/// The z14 cell of a point, as a Morton code (x bits even).
pub fn cell_of(p: P) -> u32 {
    let s = 32 - CELL_ZOOM;
    geo::morton([p[0] >> s, p[1] >> s]) as u32
}

fn decode_loose(raw: &[u8]) -> io::Result<Vec<Loose>> {
    let mut r = Reader::new(raw);
    let names = strings(&mut r)?;
    let mut cur = [0i32; 2];
    let mut out = Vec::new();
    while !r.at_end() {
        cur[0] = cur[0].wrapping_add(r.zvarint()?);
        cur[1] = cur[1].wrapping_add(r.zvarint()?);
        let street = names
            .get(r.varint()? as usize)
            .ok_or_else(|| invalid("bad ALSE name"))?
            .clone();
        let v = r.varint()?;
        let suffix = string(&mut r)?;
        let postcode = code(r.varint()?);
        let units = strings(&mut r)?;
        out.push(Loose {
            p: [(cur[0] as u32) << LSE_SHIFT, (cur[1] as u32) << LSE_SHIFT],
            street,
            number: (v >> 2) as u32,
            suffix,
            osm: v & 2 != 0,
            approx: v & 1 != 0,
            postcode,
            units,
        });
    }
    Ok(out)
}

fn decode_ranges(first: u32, raw: &[u8]) -> io::Result<Vec<Range>> {
    let mut r = Reader::new(raw);
    let (mut street, mut start) = (first, 0i32);
    let mut out = Vec::new();
    while !r.at_end() {
        let d = r.varint()? as u32;
        if d != 0 || out.is_empty() {
            start = 0;
        }
        street = street
            .checked_add(d)
            .ok_or_else(|| invalid("ARNG street out of range"))?;
        start = start.wrapping_add(r.zvarint()?);
        let end = start.wrapping_add(r.zvarint()?);
        let from = r.varint()? as u32;
        let to = (i64::from(from) + i64::from(r.zvarint()?)) as u32;
        let pl = r.varint()?;
        out.push(Range {
            street,
            start,
            end,
            from,
            to,
            parity: (pl >> 1) as u8,
            left: pl & 1 != 0,
        });
    }
    Ok(out)
}

fn decode_codes(raw: &[u8]) -> io::Result<Vec<String>> {
    let mut r = Reader::new(raw);
    let mut out = Vec::new();
    while !r.at_end() {
        out.push(string(&mut r)?);
    }
    Ok(out)
}

fn address_bytes(a: &Address) -> usize {
    std::mem::size_of::<Address>()
        + a.suffix.capacity()
        + a.units.iter().map(|u| 24 + u.capacity()).sum::<usize>()
}

fn loose_bytes(l: &Loose) -> usize {
    std::mem::size_of::<Loose>()
        + l.street.capacity()
        + l.suffix.capacity()
        + l.units.iter().map(|u| 24 + u.capacity()).sum::<usize>()
}

/// Reads the address sections.
pub struct Addresses {
    pack: Arc<Pack>,
    astr: Blocked,
    asuf: Blocked,
    auni: Blocked,
    alse: Blocked,
    arng: Blocked,
    apcd: Blocked,
    runs: Cache<usize, RunBlock>,
    loose: Cache<usize, Vec<Loose>>,
    ranges: Cache<usize, Vec<Range>>,
    codes: Cache<usize, Vec<String>>,
}

impl Addresses {
    /// Reads the six block indexes; `None` if the pack has no addresses.
    pub fn open(pack: Arc<Pack>) -> io::Result<Option<Self>> {
        let open = |id| Blocked::open(&pack, id);
        let (Some(astr), Some(asuf), Some(auni), Some(alse), Some(arng), Some(apcd)) = (
            open(ASTR)?,
            open(ASUF)?,
            open(AUNI)?,
            open(ALSE)?,
            open(ARNG)?,
            open(APCD)?,
        ) else {
            return Ok(None);
        };
        let n = astr.blocks();
        let aligned = asuf.blocks() == n
            && auni.blocks() == n
            && (0..n).all(|b| asuf.first(b) == astr.first(b) && auni.first(b) == astr.first(b));
        if !aligned {
            return Err(invalid("ASUF or AUNI blocks do not match ASTR's"));
        }
        Ok(Some(Self {
            pack,
            astr,
            asuf,
            auni,
            alse,
            arng,
            apcd,
            runs: Cache::new(CACHE_BYTES[0]),
            loose: Cache::new(CACHE_BYTES[1]),
            ranges: Cache::new(CACHE_BYTES[2]),
            codes: Cache::new(CACHE_BYTES[3]),
        }))
    }

    /// Attached addresses, unattached addresses, ranges and postcodes.
    pub fn counts(&self) -> [u32; 4] {
        [
            self.astr.count,
            self.alse.count,
            self.arng.count,
            self.apcd.count,
        ]
    }

    fn run_block(&self, block: usize) -> io::Result<Arc<RunBlock>> {
        self.runs.get(block, || {
            let (a, ra) = self.astr.read(&self.pack, block)?;
            let (s, rs) = self.asuf.read(&self.pack, block)?;
            let (u, ru) = self.auni.read(&self.pack, block)?;
            let b = decode_runs(self.astr.first(block), &a, &s, &u)?;
            let bytes = b.addresses.iter().map(address_bytes).sum::<usize>() + b.runs.len() * 8;
            Ok((b, bytes, ra + rs + ru))
        })
    }

    /// The addresses of `street`, sorted by number, suffix and offset.
    pub fn run(&self, street: u32) -> io::Result<Vec<Address>> {
        let Some(block) = self.astr.find(street) else {
            return Ok(Vec::new());
        };
        Ok(self.run_block(block)?.run(street).to_vec())
    }

    /// The interpolation ranges of `street`, sorted by start.
    pub fn ranges(&self, street: u32) -> io::Result<Vec<Range>> {
        let Some(block) = self.arng.find(street) else {
            return Ok(Vec::new());
        };
        let ranges = self.ranges.get(block, || {
            let (raw, read) = self.arng.read(&self.pack, block)?;
            let v = decode_ranges(self.arng.first(block), &raw)?;
            let bytes = v.len() * std::mem::size_of::<Range>();
            Ok((v, bytes, read))
        })?;
        let lo = ranges.partition_point(|r| r.street < street);
        let hi = ranges.partition_point(|r| r.street <= street);
        Ok(ranges[lo..hi].to_vec())
    }

    /// The unattached addresses in z14 cell `cell` (a Morton code, as [`cell_of`]).
    pub fn loose_in(&self, cell: u32) -> io::Result<Vec<Loose>> {
        let Some(block) = self.alse.find(cell) else {
            return Ok(Vec::new());
        };
        let points = self.loose.get(block, || {
            let (raw, read) = self.alse.read(&self.pack, block)?;
            let v = decode_loose(&raw)?;
            let bytes = v.iter().map(loose_bytes).sum();
            Ok((v, bytes, read))
        })?;
        Ok(points
            .iter()
            .filter(|l| cell_of(l.p) == cell)
            .cloned()
            .collect())
    }

    /// Postcode `index` in display form ("45201", "K7L 3N6").
    pub fn postcode(&self, index: u32) -> io::Result<String> {
        let block = self
            .apcd
            .find(index)
            .filter(|_| index < self.apcd.count)
            .ok_or_else(|| invalid("no such postcode"))?;
        let codes = self.codes.get(block, || {
            let (raw, read) = self.apcd.read(&self.pack, block)?;
            let v = decode_codes(&raw)?;
            let bytes = v.iter().map(|c| 24 + c.capacity()).sum();
            Ok((v, bytes, read))
        })?;
        codes
            .get((index - self.apcd.first(block)) as usize)
            .cloned()
            .ok_or_else(|| invalid("APCD block holds fewer codes"))
    }

    /// Calls `f` for every attached address in order, reading each block once without
    /// caching it.
    pub fn for_each(&self, mut f: impl FnMut(&Address)) -> io::Result<()> {
        for b in 0..self.astr.blocks() {
            let (a, _) = self.astr.read(&self.pack, b)?;
            let (s, _) = self.asuf.read(&self.pack, b)?;
            let (u, _) = self.auni.read(&self.pack, b)?;
            decode_runs(self.astr.first(b), &a, &s, &u)?
                .addresses
                .iter()
                .for_each(&mut f);
        }
        Ok(())
    }

    /// Calls `f` for every unattached address in order.
    pub fn for_each_loose(&self, mut f: impl FnMut(&Loose)) -> io::Result<()> {
        for b in 0..self.alse.blocks() {
            decode_loose(&self.alse.read(&self.pack, b)?.0)?
                .iter()
                .for_each(&mut f);
        }
        Ok(())
    }

    /// Calls `f` for every range in order.
    pub fn for_each_range(&self, mut f: impl FnMut(&Range)) -> io::Result<()> {
        for b in 0..self.arng.blocks() {
            decode_ranges(self.arng.first(b), &self.arng.read(&self.pack, b)?.0)?
                .iter()
                .for_each(&mut f);
        }
        Ok(())
    }

    pub fn heap_bytes(&self) -> usize {
        [
            &self.astr, &self.asuf, &self.auni, &self.alse, &self.arng, &self.apcd,
        ]
        .iter()
        .map(|b| b.heap_bytes())
        .sum::<usize>()
            + self.runs.held()
            + self.loose.held()
            + self.ranges.held()
            + self.codes.held()
    }

    pub fn bytes_read(&self) -> u64 {
        use std::sync::atomic::Ordering::Relaxed;
        [
            &self.runs.bytes_read,
            &self.loose.bytes_read,
            &self.ranges.bytes_read,
            &self.codes.bytes_read,
        ]
        .iter()
        .map(|b| b.load(Relaxed))
        .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_interpolate_by_parity() {
        let r = Range {
            street: 0,
            start: 10,
            end: 0,
            from: 101,
            to: 199,
            parity: 1,
            left: true,
        };
        assert!(r.holds(151) && !r.holds(150) && !r.holds(201));
        assert_eq!(r.offset_of(101), 10.0);
        assert_eq!(r.offset_of(150), 5.0);
        assert_eq!(r.number_at(5.0), 149);
        assert_eq!(r.number_at(10.0), 101);
        assert_eq!(r.number_at(-3.0), 199);
        let even = Range {
            parity: 2,
            from: 2,
            to: 2,
            ..r
        };
        assert_eq!(even.offset_of(2), 5.0);
        assert_eq!(even.number_at(7.0), 2);
    }
}
