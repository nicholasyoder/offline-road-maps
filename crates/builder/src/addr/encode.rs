//! Address store encodings A1–A4 (per-street runs), the side tables (suffixes, units,
//! unattached points) and TIGER ranges. Only sizes are measured; nothing is decoded.

use anyhow::Result;
use rayon::prelude::*;
use serde::Serialize;

use super::streets::morton;
use super::tiger::RangeSrc;
use crate::measure::encode::put_varint;
use crate::measure::geom::P;

/// An address attached to a street component.
pub struct Entry {
    pub comp: u32,
    pub num: u32,
    pub offset_m: f64,
    pub lateral_m: f64,
    pub left: bool,
    pub osm: bool,
    pub approx: bool,
    /// Letter, fraction or range suffix; empty for plain numbers.
    pub suffix: String,
    pub units: Vec<String>,
}

/// An address with no street to attach to: stored with its own position and name.
pub struct Unattached {
    pub p: P,
    pub num: u32,
    pub name: String,
    pub osm: bool,
    pub approx: bool,
    pub suffix: String,
    pub units: Vec<String>,
}

/// A range kept for interpolation (TIGER or OSM), on an OSM street component.
pub struct TigerEntry {
    pub comp: u32,
    pub start_m: f64,
    pub end_m: f64,
    pub from: u32,
    pub to: u32,
    pub parity: u8,
    pub left: bool,
}

#[derive(Serialize)]
pub struct VariantRow {
    pub variant: &'static str,
    pub streets: u64,
    /// Per zstd block: first street and compressed length.
    pub block_index: u64,
    pub suffixes: u64,
    pub units: u64,
    pub unattached: u64,
    pub total: u64,
    pub bytes_per_address: f64,
}

#[derive(Serialize)]
pub struct BlockRow {
    pub block_kib: usize,
    pub streets_zstd: u64,
    pub block_index: u64,
    pub blocks: u64,
}

#[derive(Serialize)]
pub struct EncodingReport {
    pub addresses: u64,
    pub street_records: u64,
    pub variants: Vec<VariantRow>,
    /// A3 street bytes for other block sizes.
    pub a3_block_sizes: Vec<BlockRow>,
    /// Fallback ranges, per range source.
    pub ranges: Vec<RangeEncRow>,
}

#[derive(Serialize)]
pub struct RangeEncRow {
    pub source: RangeSrc,
    pub ranges: u64,
    pub raw: u64,
    pub zstd: u64,
}

fn zigzag(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    put_varint(out, s.len() as u64);
    out.extend_from_slice(s.as_bytes());
}

fn zstd_len(data: &[u8], level: i32) -> Result<u64> {
    Ok(zstd::bulk::compress(data, level)?.len() as u64)
}

/// One byte string per street: `comp delta, count`, then per entry
/// `(number delta << 3 | left << 2 | osm << 1 | approx)`, the zigzag offset delta in units of
/// `quant_m`, and with `lateral` the distance from the street in the same units.
fn street_runs(entries: &[Entry], quant_m: f64, lateral: bool) -> Vec<(u32, Vec<u8>)> {
    let mut out = Vec::new();
    let mut prev_comp = 0;
    let mut i = 0;
    while i < entries.len() {
        let comp = entries[i].comp;
        let end = i + entries[i..].iter().take_while(|e| e.comp == comp).count();
        let mut b = Vec::new();
        put_varint(&mut b, u64::from(comp - prev_comp));
        put_varint(&mut b, (end - i) as u64);
        let (mut num, mut off) = (0u32, 0i64);
        for e in &entries[i..end] {
            let flags = u64::from(e.left) << 2 | u64::from(e.osm) << 1 | u64::from(e.approx);
            put_varint(&mut b, u64::from(e.num - num) << 3 | flags);
            let o = (e.offset_m / quant_m).round() as i64;
            put_varint(&mut b, zigzag(o - off));
            if lateral {
                put_varint(&mut b, (e.lateral_m / quant_m).round() as u64);
            }
            num = e.num;
            off = o;
        }
        out.push((comp, b));
        prev_comp = comp;
        i = end;
    }
    out
}

/// Packs street runs into zstd blocks of about `block` raw bytes (a street never spans two
/// blocks). Returns (compressed bytes, block index bytes, blocks).
fn blocked(runs: &[(u32, Vec<u8>)], block: usize, level: i32) -> Result<(u64, u64, u64)> {
    let mut blocks: Vec<(u32, Vec<u8>)> = Vec::new();
    for (comp, b) in runs {
        match blocks.last_mut() {
            Some((_, buf)) if buf.len() < block => buf.extend_from_slice(b),
            _ => blocks.push((*comp, b.clone())),
        }
    }
    let sizes: Vec<u64> = blocks
        .par_iter()
        .map(|(_, b)| zstd_len(b, level))
        .collect::<Result<_>>()?;
    let mut index = Vec::new();
    let mut prev = 0;
    for ((comp, _), len) in blocks.iter().zip(&sizes) {
        put_varint(&mut index, u64::from(comp - prev));
        put_varint(&mut index, *len);
        prev = *comp;
    }
    Ok((sizes.iter().sum(), index.len() as u64, blocks.len() as u64))
}

/// Suffix and unit side tables: `(entry index delta, payload)` in entry order.
fn side_tables(entries: &[Entry]) -> (Vec<u8>, Vec<u8>) {
    let (mut suffixes, mut units) = (Vec::new(), Vec::new());
    let (mut ps, mut pu) = (0, 0);
    for (i, e) in entries.iter().enumerate() {
        if !e.suffix.is_empty() {
            put_varint(&mut suffixes, (i - ps) as u64);
            put_str(&mut suffixes, &e.suffix);
            ps = i;
        }
        if !e.units.is_empty() {
            put_varint(&mut units, (i - pu) as u64);
            put_varint(&mut units, e.units.len() as u64);
            for u in &e.units {
                put_str(&mut units, u);
            }
            pu = i;
        }
    }
    (suffixes, units)
}

/// Unattached points sorted along a Morton curve: a name table, then per point the zigzag
/// position delta in units of 128 world units (about 1 m), name id, number and flags.
fn unattached_table(points: &mut [Unattached]) -> Vec<u8> {
    points.sort_unstable_by_key(|u| (morton(u.p), u.num));
    let mut names: Vec<&str> = points.iter().map(|u| u.name.as_str()).collect();
    names.sort_unstable();
    names.dedup();
    let mut out = Vec::new();
    put_varint(&mut out, names.len() as u64);
    for n in &names {
        put_str(&mut out, n);
    }
    let mut prev = [0i64; 2];
    for u in points.iter() {
        let q = [i64::from(u.p[0] >> 7), i64::from(u.p[1] >> 7)];
        put_varint(&mut out, zigzag(q[0] - prev[0]));
        put_varint(&mut out, zigzag(q[1] - prev[1]));
        prev = q;
        put_varint(
            &mut out,
            names.binary_search(&u.name.as_str()).unwrap() as u64,
        );
        let flags = u64::from(u.osm) << 1 | u64::from(u.approx);
        put_varint(&mut out, u64::from(u.num) << 2 | flags);
        put_str(&mut out, &u.suffix);
        put_varint(&mut out, u.units.len() as u64);
        for unit in &u.units {
            put_str(&mut out, unit);
        }
    }
    out
}

/// Per range, sorted by street and start: `comp delta, start delta, length` in 5 m units,
/// `from`, zigzag `to - from`, and `parity << 1 | left`.
fn tiger_table(ranges: &mut [TigerEntry]) -> Vec<u8> {
    ranges.sort_unstable_by(|a, b| {
        (a.comp, a.start_m.min(a.end_m))
            .partial_cmp(&(b.comp, b.start_m.min(b.end_m)))
            .unwrap()
    });
    let mut out = Vec::new();
    let (mut pc, mut ps) = (0u32, 0i64);
    for r in ranges.iter() {
        if r.comp != pc {
            ps = 0;
        }
        let s = (r.start_m / 5.0).round() as i64;
        let e = (r.end_m / 5.0).round() as i64;
        put_varint(&mut out, u64::from(r.comp - pc));
        put_varint(&mut out, zigzag(s - ps));
        put_varint(&mut out, zigzag(e - s));
        put_varint(&mut out, u64::from(r.from));
        put_varint(&mut out, zigzag(i64::from(r.to) - i64::from(r.from)));
        let parity = match r.parity {
            b'O' => 1,
            b'E' => 2,
            _ => 0,
        };
        put_varint(&mut out, parity << 1 | u64::from(r.left));
        pc = r.comp;
        ps = s;
    }
    out
}

pub const BLOCK_KIB: usize = 32;

pub fn measure(
    entries: &mut [Entry],
    unattached: &mut [Unattached],
    ranges: &mut [(RangeSrc, Vec<TigerEntry>)],
    level: i32,
) -> Result<EncodingReport> {
    entries.sort_unstable_by(|a, b| {
        (a.comp, a.num, &a.suffix)
            .cmp(&(b.comp, b.num, &b.suffix))
            .then(a.offset_m.total_cmp(&b.offset_m))
    });
    let addresses = (entries.len() + unattached.len()) as u64;
    let (suffixes, units) = side_tables(entries);
    let loose = unattached_table(unattached);
    let (suffixes_z, units_z, loose_z) = (
        zstd_len(&suffixes, level)?,
        zstd_len(&units, level)?,
        zstd_len(&loose, level)?,
    );

    let a1 = street_runs(entries, 1.0, false);
    let a2 = street_runs(entries, 5.0, false);
    let a4 = street_runs(entries, 5.0, true);
    let raw = |runs: &[(u32, Vec<u8>)]| runs.iter().map(|(_, b)| b.len() as u64).sum::<u64>();
    let block = BLOCK_KIB << 10;
    let (a3_z, a3_index, _) = blocked(&a2, block, level)?;
    let (a4_z, a4_index, _) = blocked(&a4, block, level)?;
    let row = |variant, streets, block_index, compressed: bool| {
        let (s, u, l) = if compressed {
            (suffixes_z, units_z, loose_z)
        } else {
            (
                suffixes.len() as u64,
                units.len() as u64,
                loose.len() as u64,
            )
        };
        let total = streets + block_index + s + u + l;
        VariantRow {
            variant,
            streets,
            block_index,
            suffixes: s,
            units: u,
            unattached: l,
            total,
            bytes_per_address: total as f64 / addresses.max(1) as f64,
        }
    };
    let variants = vec![
        row("a1", raw(&a1), 0, false),
        row("a2", raw(&a2), 0, false),
        row("a3", a3_z, a3_index, true),
        row("a4", a4_z, a4_index, true),
    ];
    let mut a3_block_sizes = Vec::new();
    for kib in [4, 8, 16, 32, 64, 128] {
        let (z, index, blocks) = blocked(&a2, kib << 10, level)?;
        a3_block_sizes.push(BlockRow {
            block_kib: kib,
            streets_zstd: z,
            block_index: index,
            blocks,
        });
    }
    let mut range_rows = Vec::new();
    for (source, entries) in ranges.iter_mut() {
        let t = tiger_table(entries);
        range_rows.push(RangeEncRow {
            source: *source,
            ranges: entries.len() as u64,
            raw: t.len() as u64,
            zstd: zstd_len(&t, level)?,
        });
    }
    Ok(EncodingReport {
        addresses,
        street_records: a1.len() as u64,
        variants,
        a3_block_sizes,
        ranges: range_rows,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(comp: u32, num: u32, offset_m: f64) -> Entry {
        Entry {
            comp,
            num,
            offset_m,
            lateral_m: 12.0,
            left: num % 2 == 1,
            osm: false,
            approx: false,
            suffix: String::new(),
            units: Vec::new(),
        }
    }

    #[test]
    fn runs_are_compact() {
        let e = [entry(3, 101, 10.0), entry(3, 103, 30.0), entry(5, 2, 0.0)];
        let runs = street_runs(&e, 5.0, false);
        // Street 3: comp delta 3, count 2, (101 << 3 | left) needs 2 bytes, offset 2 → 1;
        // second entry: (2 << 3 | left) and +4 → 1 + 1.
        assert_eq!(runs[0].1, [3, 2, 0xac, 0x06, 4, 0x14, 8]);
        assert_eq!(runs[1].1, [2, 1, 16, 0]);
        let with_lateral = street_runs(&e, 5.0, true);
        assert_eq!(with_lateral[1].1, [2, 1, 16, 0, 2]);
    }
}
