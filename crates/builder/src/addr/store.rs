//! The address sections `build` writes (phase 2 step 2.4, `docs/format.md`):
//! - `ASTR`: per street, its postcode and its addresses' numbers, sides and 5 m offsets;
//! - `ASUF` and `AUNI`: suffixes with postcode exceptions, and units, in blocks aligned
//!   with `ASTR`'s;
//! - `ALSE`: addresses with no street, in blocks of z14 cells;
//! - `ARNG`: interpolation ranges, by street;
//! - `APCD`: the postcodes the others refer to.
//!
//! All six are zstd blocks behind an index, as [`blocked`] writes them.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::io;
use std::time::Instant;

use anyhow::Result;
use omp_core::varint::{Reader, invalid, put, put_str, zigzag};
use rayon::prelude::*;
use serde::Serialize;

use super::merge::{APPROX, Attached, Merged, OSM, UNATTACHED};
use super::streets::morton;
use crate::geom::P;
use crate::street::encode::{Blocks, blocked, read_blocks, string};
use crate::street::{STEP_M, Table};

pub const ASTR: [u8; 4] = *b"ASTR";
pub const ASUF: [u8; 4] = *b"ASUF";
pub const AUNI: [u8; 4] = *b"AUNI";
pub const ALSE: [u8; 4] = *b"ALSE";
pub const ARNG: [u8; 4] = *b"ARNG";
pub const APCD: [u8; 4] = *b"APCD";
/// Uncompressed bytes per block, about.
pub const BLOCK: usize = 16 << 10;
/// `ALSE` blocks hold whole cells of this zoom.
pub const CELL_ZOOM: u32 = 14;
/// `ALSE` positions are in units of 2^7 world units (about 1 m at the equator).
const LSE_SHIFT: u32 = 7;

/// An attached address on its street. `suffix` and `code` are ranks in the sorted suffix and
/// postcode lists (0 for none); `addr` finds its units.
#[derive(Clone, Copy)]
pub struct Entry {
    pub street: u32,
    pub num: u32,
    pub suffix: u32,
    /// Offset in steps of [`STEP_M`].
    pub off: i32,
    pub left: bool,
    pub osm: bool,
    pub approx: bool,
    pub code: u32,
    pub addr: u32,
}

/// An address with no street.
#[derive(Clone, Copy)]
pub struct Point {
    pub p: P,
    pub num: u32,
    pub suffix: u32,
    pub name: u32,
    pub osm: bool,
    pub approx: bool,
    pub code: u32,
    pub addr: u32,
}

/// An interpolation range on its street, offsets in steps of [`STEP_M`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RangeRec {
    pub street: u32,
    pub start: i32,
    pub end: i32,
    pub from: u32,
    pub to: u32,
    /// 0 all numbers, 1 odd, 2 even.
    pub parity: u8,
    pub left: bool,
}

/// Everything the sections hold, in the order they store it.
pub struct Input<'a> {
    /// Sorted by street, number, suffix, offset.
    pub entries: Vec<Entry>,
    pub points: Vec<Point>,
    pub ranges: Vec<RangeRec>,
    /// Sorted; rank 0 is the empty suffix.
    pub suffixes: Vec<&'a str>,
    /// Sorted; the postcode with rank `r` is `codes[r - 1]`.
    pub codes: Vec<&'a str>,
    pub names: &'a [String],
    pub unit_names: &'a [String],
    /// `(addr, unit)`, sorted.
    pub units: &'a [(u32, u32)],
}

pub struct Sections {
    pub astr: Vec<u8>,
    pub asuf: Vec<u8>,
    pub auni: Vec<u8>,
    pub alse: Vec<u8>,
    pub arng: Vec<u8>,
    pub apcd: Vec<u8>,
}

impl Sections {
    pub fn list(self) -> [([u8; 4], Vec<u8>); 6] {
        [
            (ASTR, self.astr),
            (ASUF, self.asuf),
            (AUNI, self.auni),
            (ALSE, self.alse),
            (ARNG, self.arng),
            (APCD, self.apcd),
        ]
    }
}

#[derive(Serialize, Default, Clone)]
pub struct Sizes {
    pub block_kib: usize,
    pub addresses: u64,
    pub streets_with_addresses: u64,
    pub suffixes: u64,
    pub units: u64,
    /// Addresses whose postcode is not their street's.
    pub exceptions: u64,
    pub points: u64,
    pub ranges: u64,
    /// Ranges whose ends lie on another street than their middle (dropped).
    pub ranges_dropped: u64,
    pub codes: u64,
    pub astr_raw: u64,
    pub astr: u64,
    pub astr_blocks: u64,
    pub asuf: u64,
    pub auni: u64,
    pub alse_raw: u64,
    pub alse: u64,
    pub alse_blocks: u64,
    pub arng: u64,
    pub apcd: u64,
    /// Time to decompress and parse one `ASTR` block (with its `ASUF` and `AUNI` blocks).
    pub decode_us_per_block: f64,
}

impl Sizes {
    pub fn total(&self) -> u64 {
        self.astr + self.asuf + self.auni + self.alse + self.arng + self.apcd
    }
}

/// The indexes `keep` lets through, sorted by their strings in `list`, and each index's
/// rank among them (0 for the others).
fn ranks(list: &[String], keep: impl Fn(u32) -> bool + Sync) -> (Vec<u32>, Vec<u32>) {
    let mut order: Vec<u32> = (0..list.len() as u32).filter(|&i| keep(i)).collect();
    order.par_sort_unstable_by(|&a, &b| list[a as usize].cmp(&list[b as usize]));
    let mut rank = vec![0; list.len()];
    for (r, &i) in order.iter().enumerate() {
        rank[i as usize] = r as u32;
    }
    (order, rank)
}

/// Places the merged addresses (`attached`, taken out of `m` so it is freed here) and
/// ranges on `table`.
pub fn place<'a>(table: &Table, attached: Vec<Attached>, m: &'a Merged) -> (Input<'a>, u64) {
    let (suf_order, suf_rank) = ranks(&m.suffixes.list, |_| true);
    // Interner ID 0 is the empty string, which sorts first.
    debug_assert!(suf_order[0] == 0);
    // Only the postcodes an address shows; ID 0 (none) is left out, so ranks start at 1.
    let mut used = vec![false; m.codes.list.len()];
    for c in attached
        .iter()
        .map(|a| a.code)
        .chain(m.unattached.iter().map(|u| u.code))
    {
        used[c as usize] = true;
    }
    let (code_order, mut code_rank) = ranks(&m.codes.list, |i| i != 0 && used[i as usize]);
    for &i in &code_order {
        code_rank[i as usize] += 1;
    }
    let mut entries: Vec<Entry> = attached
        .par_iter()
        .enumerate()
        .filter_map(|(i, a)| {
            let s = table.locate(a.way, a.p)?;
            Some(Entry {
                street: s.street,
                num: a.num,
                suffix: suf_rank[a.suffix as usize],
                off: (s.offset_m / STEP_M).round() as i32,
                left: s.left,
                osm: a.flags & OSM != 0,
                approx: a.flags & APPROX != 0,
                code: code_rank[a.code as usize],
                addr: i as u32,
            })
        })
        .collect();
    drop(attached);
    entries.par_sort_unstable_by_key(|e| (e.street, e.num, e.suffix, e.off, e.addr));
    let points = m
        .unattached
        .iter()
        .enumerate()
        .map(|(i, u)| Point {
            p: u.p,
            num: u.num,
            suffix: suf_rank[u.suffix as usize],
            name: u.name,
            osm: u.flags & OSM != 0,
            approx: u.flags & APPROX != 0,
            code: code_rank[u.code as usize],
            addr: i as u32 | UNATTACHED,
        })
        .collect();
    let placed: Vec<Option<RangeRec>> = m
        .ranges
        .par_iter()
        .map(|r| {
            let mid = table.locate(r.way, r.mid)?;
            let s = table.project_onto(mid.street, r.start);
            let e = table.project_onto(mid.street, r.end);
            // TIGER sides are relative to the segment; an OSM interpolation line lies on
            // its side of the street.
            let left = match r.right {
                Some(right) if e.offset_m >= s.offset_m => !right,
                Some(right) => right,
                None => mid.left,
            };
            let step = |m: f64| (m / STEP_M).round() as i32;
            Some(RangeRec {
                street: mid.street,
                start: step(s.offset_m),
                end: step(e.offset_m),
                from: r.from,
                to: r.to,
                parity: match r.parity {
                    b'O' => 1,
                    b'E' => 2,
                    _ => 0,
                },
                left,
            })
        })
        .collect();
    let dropped = placed.iter().filter(|r| r.is_none()).count() as u64;
    let mut ranges: Vec<RangeRec> = placed.into_iter().flatten().collect();
    ranges.par_sort_unstable_by_key(|r| (r.street, r.start.min(r.end), r.from, r.to));
    let input = Input {
        entries,
        points,
        ranges,
        suffixes: suf_order
            .iter()
            .map(|&i| m.suffixes.list[i as usize].as_str())
            .collect(),
        codes: code_order
            .iter()
            .map(|&i| m.codes.list[i as usize].as_str())
            .collect(),
        names: &m.names.list,
        unit_names: &m.unit_names.list,
        units: &m.units,
    };
    (input, dropped)
}

fn strip_zeros(s: &[u8]) -> &[u8] {
    let k = s.iter().position(|&c| c != b'0').unwrap_or(s.len());
    &s[k..]
}

/// "Apt 2" before "Apt 10": runs of digits compare as numbers.
fn natural(a: &str, b: &str) -> Ordering {
    let (mut x, mut y) = (a.as_bytes(), b.as_bytes());
    loop {
        match (x.first(), y.first()) {
            (None, None) => return a.cmp(b),
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(c), Some(d)) if c.is_ascii_digit() && d.is_ascii_digit() => {
                let n = |s: &[u8]| s.iter().take_while(|c| c.is_ascii_digit()).count();
                let (i, j) = (n(x), n(y));
                let (u, v) = (&x[..i], &y[..j]);
                let (u, v) = (strip_zeros(u), strip_zeros(v));
                match u.len().cmp(&v.len()).then_with(|| u.cmp(v)) {
                    Ordering::Equal => (x, y) = (&x[i..], &y[j..]),
                    o => return o,
                }
            }
            (Some(c), Some(d)) if c != d => return c.cmp(d),
            _ => (x, y) = (&x[1..], &y[1..]),
        }
    }
}

impl Input<'_> {
    /// The units of address `addr`, in natural order.
    fn units_of(&self, addr: u32) -> Vec<&str> {
        let lo = self.units.partition_point(|u| u.0 < addr);
        let hi = self.units.partition_point(|u| u.0 <= addr);
        let mut v: Vec<&str> = self.units[lo..hi]
            .iter()
            .map(|u| self.unit_names[u.1 as usize].as_str())
            .collect();
        v.sort_by(|a, b| natural(a, b));
        v
    }
}

/// One block's side tables: entries are counted from 0 in each block.
#[derive(Default)]
struct Side {
    suffixes: Vec<u8>,
    nsuffixes: u64,
    last_suffix: u64,
    codes: Vec<u8>,
    ncodes: u64,
    last_code: u64,
    units: Vec<u8>,
    last_unit: u64,
}

impl Side {
    fn asuf(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.suffixes.len() + self.codes.len() + 4);
        put(&mut out, self.nsuffixes);
        out.extend_from_slice(&self.suffixes);
        put(&mut out, self.ncodes);
        out.extend_from_slice(&self.codes);
        out
    }
}

/// `ASTR`, `ASUF` and `AUNI` blocks, and the number of postcode exceptions.
fn street_blocks(inp: &Input, block: usize) -> (Blocks, Blocks, Blocks, u64) {
    let (mut astr, mut asuf, mut auni) = (Vec::new(), Vec::new(), Vec::new());
    let mut cur: Vec<u8> = Vec::new();
    let mut side = Side::default();
    let (mut first, mut prev, mut ordinal, mut exceptions) = (0u32, 0u32, 0u64, 0u64);
    let e = &inp.entries;
    let mut i = 0;
    while i < e.len() {
        let s = e[i].street;
        let end = i + e[i..].iter().take_while(|x| x.street == s).count();
        if cur.len() >= block {
            astr.push((first, std::mem::take(&mut cur)));
            asuf.push((first, side.asuf()));
            auni.push((first, std::mem::take(&mut side.units)));
            side = Side::default();
            ordinal = 0;
        }
        if cur.is_empty() {
            (first, prev) = (s, s);
        }
        // The street's postcode: its addresses' commonest, the lowest on a tie.
        let mut counts: HashMap<u32, u32> = HashMap::new();
        for x in e[i..end].iter().filter(|x| x.code != 0) {
            *counts.entry(x.code).or_default() += 1;
        }
        let code = counts
            .into_iter()
            .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0)))
            .map_or(0, |c| c.0);
        put(&mut cur, u64::from(s - prev));
        put(&mut cur, (end - i) as u64);
        put(&mut cur, u64::from(code));
        let (mut num, mut off) = (0u32, 0i32);
        for x in &e[i..end] {
            let flags = u64::from(x.left) << 2 | u64::from(x.osm) << 1 | u64::from(x.approx);
            put(&mut cur, u64::from(x.num - num) << 3 | flags);
            put(&mut cur, zigzag(x.off - off));
            (num, off) = (x.num, x.off);
            if x.suffix != 0 {
                put(&mut side.suffixes, ordinal - side.last_suffix);
                put_str(&mut side.suffixes, inp.suffixes[x.suffix as usize]);
                side.nsuffixes += 1;
                side.last_suffix = ordinal;
            }
            if x.code != 0 && x.code != code {
                put(&mut side.codes, ordinal - side.last_code);
                put(&mut side.codes, u64::from(x.code));
                side.ncodes += 1;
                side.last_code = ordinal;
                exceptions += 1;
            }
            let units = inp.units_of(x.addr);
            if !units.is_empty() {
                put(&mut side.units, ordinal - side.last_unit);
                put(&mut side.units, units.len() as u64);
                for u in units {
                    put_str(&mut side.units, u);
                }
                side.last_unit = ordinal;
            }
            ordinal += 1;
        }
        prev = s;
        i = end;
    }
    if !cur.is_empty() {
        astr.push((first, cur));
        asuf.push((first, side.asuf()));
        auni.push((first, side.units));
    }
    (astr, asuf, auni, exceptions)
}

/// The z14 cell of a point, as a Morton code.
pub fn cell_of(p: P) -> u32 {
    let s = 32 - CELL_ZOOM;
    morton([p[0] >> s, p[1] >> s]) as u32
}

/// `ALSE` blocks: whole cells, about `block` bytes each.
fn point_blocks(inp: &Input, block: usize) -> Blocks {
    let mut order: Vec<u32> = (0..inp.points.len() as u32).collect();
    order.par_sort_unstable_by_key(|&i| {
        let p = &inp.points[i as usize];
        (cell_of(p.p), morton(p.p), p.num, p.suffix, p.addr)
    });
    let encode = |members: &[u32]| -> Vec<u8> {
        let mut names: Vec<&str> = members
            .iter()
            .map(|&i| inp.names[inp.points[i as usize].name as usize].as_str())
            .collect();
        names.sort_unstable();
        names.dedup();
        let mut out = Vec::new();
        put(&mut out, names.len() as u64);
        for n in &names {
            put_str(&mut out, n);
        }
        let mut cur = [0i32; 2];
        for &i in members {
            let pt = &inp.points[i as usize];
            let q = [(pt.p[0] >> LSE_SHIFT) as i32, (pt.p[1] >> LSE_SHIFT) as i32];
            put(&mut out, zigzag(q[0] - cur[0]));
            put(&mut out, zigzag(q[1] - cur[1]));
            cur = q;
            let name = inp.names[pt.name as usize].as_str();
            put(&mut out, names.binary_search(&name).unwrap() as u64);
            put(
                &mut out,
                u64::from(pt.num) << 2 | u64::from(pt.osm) << 1 | u64::from(pt.approx),
            );
            put_str(&mut out, inp.suffixes[pt.suffix as usize]);
            put(&mut out, u64::from(pt.code));
            let units = inp.units_of(pt.addr);
            put(&mut out, units.len() as u64);
            for u in units {
                put_str(&mut out, u);
            }
        }
        out
    };
    // Cut at cell boundaries once a block's estimate reaches `block`.
    let mut groups: Vec<(u32, Vec<u32>)> = Vec::new();
    let mut size = 0;
    let mut k = 0;
    while k < order.len() {
        let c = cell_of(inp.points[order[k] as usize].p);
        let end = k + order[k..]
            .iter()
            .take_while(|&&i| cell_of(inp.points[i as usize].p) == c)
            .count();
        if groups.is_empty() || size >= block {
            groups.push((c, Vec::new()));
            size = 0;
        }
        let g = &mut groups.last_mut().unwrap().1;
        for &i in &order[k..end] {
            let pt = &inp.points[i as usize];
            size += 10 + inp.suffixes[pt.suffix as usize].len();
            g.push(i);
        }
        k = end;
    }
    groups
        .par_iter()
        .map(|(c, members)| (*c, encode(members)))
        .collect()
}

/// `ARNG` blocks: whole streets, about `block` bytes each.
fn range_blocks(ranges: &[RangeRec], block: usize) -> Blocks {
    let mut out = Vec::new();
    let mut cur: Vec<u8> = Vec::new();
    let (mut first, mut prev, mut start) = (0u32, 0u32, 0i32);
    for (k, r) in ranges.iter().enumerate() {
        let new_street = k == 0 || r.street != ranges[k - 1].street;
        if new_street && cur.len() >= block {
            out.push((first, std::mem::take(&mut cur)));
        }
        if cur.is_empty() {
            (first, prev) = (r.street, r.street);
        }
        if r.street != prev || new_street {
            start = 0;
        }
        put(&mut cur, u64::from(r.street - prev));
        put(&mut cur, zigzag(r.start - start));
        put(&mut cur, zigzag(r.end - r.start));
        put(&mut cur, u64::from(r.from));
        put(
            &mut cur,
            zigzag(
                (i64::from(r.to) - i64::from(r.from)).clamp(i32::MIN.into(), i32::MAX.into())
                    as i32,
            ),
        );
        put(&mut cur, u64::from(r.parity) << 1 | u64::from(r.left));
        (prev, start) = (r.street, r.start);
    }
    if !cur.is_empty() {
        out.push((first, cur));
    }
    out
}

/// `APCD` blocks: the codes in order, `first` the index of a block's first code.
fn code_blocks(codes: &[&str], block: usize) -> Blocks {
    let mut out = Vec::new();
    let mut cur = Vec::new();
    let mut first = 0;
    for (i, c) in codes.iter().enumerate() {
        if cur.len() >= block {
            out.push((first, std::mem::take(&mut cur)));
            first = i as u32;
        }
        put_str(&mut cur, c);
    }
    if !cur.is_empty() {
        out.push((first, cur));
    }
    out
}

/// The six sections, with blocks of about `block` bytes, and their sizes.
pub fn sections(inp: &Input, block: usize) -> Result<(Sections, Sizes)> {
    let (astr_b, asuf_b, auni_b, exceptions) = street_blocks(inp, block);
    let astr_raw = astr_b.iter().map(|b| b.1.len() as u64).sum();
    let streets = {
        let mut n = 0;
        for (k, e) in inp.entries.iter().enumerate() {
            n += u64::from(k == 0 || e.street != inp.entries[k - 1].street);
        }
        n
    };
    let nsuf = inp.entries.iter().filter(|e| e.suffix != 0).count();
    let nunit = inp.units.iter().filter(|u| u.0 & UNATTACHED == 0).count();
    let (astr, astr_blocks) = blocked(inp.entries.len(), astr_b)?;
    let (asuf, _) = blocked(nsuf, asuf_b)?;
    let (auni, _) = blocked(nunit, auni_b)?;
    let alse_b = point_blocks(inp, block);
    let alse_raw = alse_b.iter().map(|b| b.1.len() as u64).sum();
    let (alse, alse_blocks) = blocked(inp.points.len(), alse_b)?;
    let (arng, _) = blocked(inp.ranges.len(), range_blocks(&inp.ranges, block))?;
    let (apcd, _) = blocked(inp.codes.len(), code_blocks(&inp.codes, block))?;

    // Decode every street block once, for the time per block.
    let t = Instant::now();
    let decoded = decode_streets(&astr, &asuf, &auni)?;
    let decode_us_per_block = t.elapsed().as_secs_f64() * 1e6 / astr_blocks.max(1) as f64;
    anyhow::ensure!(
        decoded.len() == inp.entries.len(),
        "ASTR decodes to {} addresses, not {}",
        decoded.len(),
        inp.entries.len()
    );
    drop(decoded);
    let points = decode_points(&alse)?;
    anyhow::ensure!(
        points.len() == inp.points.len() && points.iter().all(|p| !p.name.is_empty()),
        "ALSE decodes to {} points, not {}",
        points.len(),
        inp.points.len()
    );
    drop(points);
    anyhow::ensure!(
        decode_ranges(&arng)? == inp.ranges,
        "ARNG does not decode to its ranges"
    );
    anyhow::ensure!(
        decode_codes(&apcd)? == inp.codes,
        "APCD does not decode to its codes"
    );
    let sizes = Sizes {
        block_kib: block >> 10,
        addresses: inp.entries.len() as u64,
        streets_with_addresses: streets,
        suffixes: nsuf as u64,
        units: inp.units.len() as u64,
        exceptions,
        points: inp.points.len() as u64,
        ranges: inp.ranges.len() as u64,
        ranges_dropped: 0,
        codes: inp.codes.len() as u64,
        astr_raw,
        astr: astr.len() as u64,
        astr_blocks,
        asuf: asuf.len() as u64,
        auni: auni.len() as u64,
        alse_raw,
        alse: alse.len() as u64,
        alse_blocks,
        arng: arng.len() as u64,
        apcd: apcd.len() as u64,
        decode_us_per_block,
    };
    let s = Sections {
        astr,
        asuf,
        auni,
        alse,
        arng,
        apcd,
    };
    Ok((s, sizes))
}

/// An address as decoded from `ASTR`, `ASUF` and `AUNI`.
#[derive(Debug, PartialEq)]
pub struct Address {
    pub street: u32,
    pub num: u32,
    pub suffix: String,
    pub off: i32,
    pub left: bool,
    pub osm: bool,
    pub approx: bool,
    /// The address's postcode (its own, else its street's), or 0.
    pub code: u32,
    pub units: Vec<String>,
}

/// Every address in `ASTR`, with its suffix, postcode and units.
pub fn decode_streets(astr: &[u8], asuf: &[u8], auni: &[u8]) -> io::Result<Vec<Address>> {
    let (count, blocks) = read_blocks(astr)?;
    let (_, sufs) = read_blocks(asuf)?;
    let (_, units) = read_blocks(auni)?;
    if sufs.len() != blocks.len() || units.len() != blocks.len() {
        return Err(invalid("ASUF or AUNI blocks do not match ASTR's"));
    }
    let mut out = Vec::with_capacity(count);
    for (((first, b), (sf, s)), (uf, u)) in blocks.iter().zip(&sufs).zip(&units) {
        if sf != first || uf != first {
            return Err(invalid("ASUF or AUNI block for another street"));
        }
        let base = out.len();
        let mut r = Reader::new(b);
        let mut street = *first;
        while !r.at_end() {
            street += r.varint()? as u32;
            let n = r.count()?;
            let code = r.varint()? as u32;
            let (mut num, mut off) = (0u32, 0i32);
            for _ in 0..n {
                let v = r.varint()?;
                num += (v >> 3) as u32;
                off += r.zvarint()?;
                out.push(Address {
                    street,
                    num,
                    suffix: String::new(),
                    off,
                    left: v & 4 != 0,
                    osm: v & 2 != 0,
                    approx: v & 1 != 0,
                    code,
                    units: Vec::new(),
                });
            }
        }
        let len = out.len();
        let at = |o: u64| -> io::Result<usize> {
            let i = base + o as usize;
            (i < len)
                .then_some(i)
                .ok_or_else(|| invalid("ordinal past the block"))
        };
        let mut r = Reader::new(s);
        let mut o = 0;
        for _ in 0..r.count()? {
            o += r.varint()?;
            out[at(o)?].suffix = string(&mut r)?;
        }
        let mut o = 0;
        for _ in 0..r.count()? {
            o += r.varint()?;
            out[at(o)?].code = r.varint()? as u32;
        }
        let mut r = Reader::new(u);
        let mut o = 0;
        while !r.at_end() {
            o += r.varint()?;
            let i = at(o)?;
            out[i].units = (0..r.count()?)
                .map(|_| string(&mut r))
                .collect::<io::Result<_>>()?;
        }
    }
    if out.len() != count {
        return Err(invalid("ASTR holds a different number of addresses"));
    }
    Ok(out)
}

/// An address as decoded from `ALSE`: its point to 2^7 world units.
#[derive(Debug, PartialEq)]
pub struct Loose {
    pub cell: u32,
    pub p: P,
    pub name: String,
    pub num: u32,
    pub osm: bool,
    pub approx: bool,
    pub suffix: String,
    pub code: u32,
    pub units: Vec<String>,
}

pub fn decode_points(alse: &[u8]) -> io::Result<Vec<Loose>> {
    let (count, blocks) = read_blocks(alse)?;
    let mut out = Vec::with_capacity(count);
    for (cell, b) in &blocks {
        let mut r = Reader::new(b);
        let names: Vec<String> = (0..r.count()?)
            .map(|_| string(&mut r))
            .collect::<io::Result<_>>()?;
        let mut cur = [0i32; 2];
        while !r.at_end() {
            cur[0] += r.zvarint()?;
            cur[1] += r.zvarint()?;
            let p = [(cur[0] as u32) << LSE_SHIFT, (cur[1] as u32) << LSE_SHIFT];
            let name = names
                .get(r.varint()? as usize)
                .ok_or_else(|| invalid("bad name"))?
                .clone();
            let v = r.varint()?;
            let suffix = string(&mut r)?;
            let code = r.varint()? as u32;
            let units = (0..r.count()?)
                .map(|_| string(&mut r))
                .collect::<io::Result<_>>()?;
            if cell_of(p) < *cell {
                return Err(invalid("point before its block's first cell"));
            }
            out.push(Loose {
                cell: cell_of(p),
                p,
                name,
                num: (v >> 2) as u32,
                osm: v & 2 != 0,
                approx: v & 1 != 0,
                suffix,
                code,
                units,
            });
        }
    }
    Ok(out)
}

pub fn decode_ranges(arng: &[u8]) -> io::Result<Vec<RangeRec>> {
    let (count, blocks) = read_blocks(arng)?;
    let mut out = Vec::with_capacity(count);
    for (first, b) in &blocks {
        let mut r = Reader::new(b);
        let (mut street, mut start) = (*first, 0i32);
        let mut started = false;
        while !r.at_end() {
            let d = r.varint()? as u32;
            if d != 0 || !started {
                start = 0;
            }
            started = true;
            street += d;
            start += r.zvarint()?;
            let end = start + r.zvarint()?;
            let from = r.varint()? as u32;
            let to = (i64::from(from) + i64::from(r.zvarint()?)) as u32;
            let pl = r.varint()?;
            out.push(RangeRec {
                street,
                start,
                end,
                from,
                to,
                parity: (pl >> 1) as u8,
                left: pl & 1 != 0,
            });
        }
    }
    Ok(out)
}

pub fn decode_codes(apcd: &[u8]) -> io::Result<Vec<String>> {
    let (count, blocks) = read_blocks(apcd)?;
    let mut out = Vec::with_capacity(count);
    for (first, b) in &blocks {
        if *first as usize != out.len() {
            return Err(invalid("APCD block out of order"));
        }
        let mut r = Reader::new(b);
        while !r.at_end() {
            out.push(string(&mut r)?);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom;

    fn entry(street: u32, num: u32, suffix: u32, off: i32, code: u32, addr: u32) -> Entry {
        Entry {
            street,
            num,
            suffix,
            off,
            left: num % 2 == 1,
            osm: addr == 2,
            approx: addr == 3,
            code,
            addr,
        }
    }

    #[test]
    fn natural_order() {
        let mut v = vec!["Apt 10", "Apt 2", "B", "1A", "10", "2", "Apt 02"];
        v.sort_by(|a, b| natural(a, b));
        assert_eq!(v, ["1A", "2", "10", "Apt 02", "Apt 2", "Apt 10", "B"]);
    }

    #[test]
    fn sections_round_trip() {
        let mut names = vec!["".to_string(), "Kuup Tukia".into(), "Rue A".into()];
        // Many names in one block: late points' name indexes exceed the bytes left.
        names.extend((0..300).map(|i| format!("Rue {i}")));
        let unit_names = vec![
            "".to_string(),
            "Apt 10".into(),
            "Apt 2".into(),
            "Unit 1".into(),
        ];
        // Addresses 1 and 4 (attached) and unattached point 1 have units.
        let units = vec![(1, 1), (1, 2), (4, 3), (1 | UNATTACHED, 3)];
        let mut entries = vec![
            entry(3, 101, 0, 2, 1, 0),
            entry(3, 101, 1, 2, 1, 1),
            entry(3, 103, 0, 6, 2, 2),
            entry(3, 105, 0, 9, 1, 3),
            entry(7, 2, 2, 0, 0, 4),
            entry(8, 40, 0, 1, 2, 5),
        ];
        // Enough streets that tiny blocks cut between them.
        for s in 10..40 {
            entries.push(entry(s, 1, 0, 0, 1, 100 + s));
        }
        let pt = |lat, lon, num, name, addr| Point {
            p: geom::project(lat, lon),
            num,
            suffix: 0,
            name,
            osm: false,
            approx: num == 9,
            code: 2,
            addr,
        };
        let mut points = vec![
            pt(58.45, -78.10, 859, 1, UNATTACHED),
            pt(58.45, -78.1001, 9, 1, 1 | UNATTACHED),
            pt(45.5, -73.6, 12, 2, 2 | UNATTACHED),
        ];
        for i in 0..300 {
            let lon = -73.0 + f64::from(i) * 1e-5;
            points.push(pt(45.0, lon, 1, 3 + i, (3 + i) | UNATTACHED));
        }
        let ranges = vec![
            RangeRec {
                street: 3,
                start: 0,
                end: 40,
                from: 101,
                to: 199,
                parity: 1,
                left: true,
            },
            RangeRec {
                street: 3,
                start: 50,
                end: 44,
                from: 201,
                to: 219,
                parity: 1,
                left: false,
            },
            RangeRec {
                street: 9,
                start: 3,
                end: 8,
                from: 2,
                to: 10,
                parity: 2,
                left: false,
            },
        ];
        let inp = Input {
            entries,
            points,
            ranges: ranges.clone(),
            suffixes: vec!["", "1/2", "A"],
            codes: vec!["45201", "K7L 3N6"],
            names: &names,
            unit_names: &unit_names,
            units: &units,
        };
        for block in [16, 1 << 20] {
            let (s, sizes) = sections(&inp, block).unwrap();
            if block == 16 {
                assert!(sizes.astr_blocks > 3, "{}", sizes.astr_blocks);
            }
            // Street 3's postcode is 1; address 2's 2 is an exception.
            assert_eq!(sizes.exceptions, 1);
            let got = decode_streets(&s.astr, &s.asuf, &s.auni).unwrap();
            assert_eq!(got.len(), inp.entries.len());
            for (g, e) in got.iter().zip(&inp.entries) {
                assert_eq!((g.street, g.num, g.off), (e.street, e.num, e.off));
                assert_eq!((g.left, g.osm, g.approx), (e.left, e.osm, e.approx));
                assert_eq!(g.suffix, inp.suffixes[e.suffix as usize]);
                // An address without a postcode shows its street's (street 7 has none).
                let code = if e.code == 0 { 0 } else { e.code };
                assert_eq!(g.code, code);
            }
            assert_eq!(got[1].units, ["Apt 2", "Apt 10"]);
            assert_eq!(got[4].units, ["Unit 1"]);
            assert!(got[0].units.is_empty());

            let loose = decode_points(&s.alse).unwrap();
            assert_eq!(loose.len(), 303);
            // Inukjuak is west and north of Montreal, so its cell comes first.
            assert_eq!((loose[2].num, loose[2].name.as_str()), (12, "Rue A"));
            let mut many: Vec<&str> = loose[3..].iter().map(|l| l.name.as_str()).collect();
            many.sort_unstable();
            many.dedup();
            assert_eq!(many.len(), 300);
            let ku: Vec<_> = loose.iter().filter(|l| l.name == "Kuup Tukia").collect();
            assert_eq!(ku.len(), 2);
            let nine = ku.iter().find(|l| l.num == 9).unwrap();
            assert!(nine.approx && nine.code == 2);
            assert_eq!(nine.units, ["Unit 1"]);
            let d = geom::unproject(nine.p);
            assert!((d.0 - 58.45).abs() < 1e-4 && (d.1 + 78.1001).abs() < 1e-4);

            assert_eq!(decode_ranges(&s.arng).unwrap(), ranges);
            assert_eq!(decode_codes(&s.apcd).unwrap(), ["45201", "K7L 3N6"]);

            // The app's reader agrees, street by street and cell by cell.
            let dir = tempfile::tempdir().unwrap();
            let list = s.list();
            let refs: Vec<([u8; 4], &[u8])> = list.iter().map(|(id, b)| (*id, &b[..])).collect();
            let pack = crate::build::test_pack(dir.path(), &refs);
            let core = omp_core::addr::Addresses::open(pack).unwrap().unwrap();
            assert_eq!(core.counts(), [36, 303, 3, 2]);
            let mut streets: Vec<u32> = got.iter().map(|a| a.street).collect();
            streets.dedup();
            let mut all = Vec::new();
            for st in streets {
                all.extend(core.run(st).unwrap());
            }
            assert_eq!(all.len(), got.len());
            for (c, g) in all.iter().zip(&got) {
                assert_eq!(
                    (c.street, c.number, &c.suffix, c.offset),
                    (g.street, g.num, &g.suffix, g.off)
                );
                assert_eq!(
                    (c.left, c.osm, c.approx, &c.units),
                    (g.left, g.osm, g.approx, &g.units)
                );
                assert_eq!(c.postcode, g.code.checked_sub(1));
            }
            assert!(core.run(5).unwrap().is_empty());
            for want in &ranges {
                assert!(core.ranges(want.street).unwrap().iter().any(|r| (
                    r.start, r.end, r.from, r.to, r.parity, r.left
                ) == (
                    want.start,
                    want.end,
                    want.from,
                    want.to,
                    want.parity,
                    want.left
                )));
            }
            let mut cells: Vec<u32> = loose.iter().map(|l| l.cell).collect();
            cells.dedup();
            let n: usize = cells.iter().map(|&c| core.loose_in(c).unwrap().len()).sum();
            assert_eq!(n, loose.len());
            let ku = core.loose_in(loose[0].cell).unwrap();
            assert!(
                ku.iter()
                    .any(|l| l.number == 9 && l.units == ["Unit 1"] && l.postcode == Some(1))
            );
            assert_eq!(core.postcode(1).unwrap(), "K7L 3N6");
        }
    }
}
