//! `SSTR` (street table) and `SGEO` (street geometry), as in `docs/format.md`.

use std::io;

use anyhow::Result;
use omp_core::varint::{Reader, invalid, put, put_str, zigzag};
use rayon::prelude::*;
use serde::Serialize;

use super::{SHIFT, Street, Table};
use crate::admin::towns::Source;
#[cfg(test)]
use crate::geom::P;

pub const SSTR: [u8; 4] = *b"SSTR";
pub const SGEO: [u8; 4] = *b"SGEO";
const LEVEL: i32 = 19;
/// Uncompressed bytes per block, about.
pub const BLOCK: usize = 32 << 10;

#[derive(Serialize, Default)]
pub struct Sizes {
    pub block_kib: usize,
    pub streets: u64,
    pub sstr_raw: u64,
    pub sstr: u64,
    pub sstr_blocks: u64,
    pub sgeo_raw: u64,
    pub sgeo: u64,
    pub sgeo_blocks: u64,
}

pub fn source_code(s: Source) -> u8 {
    match s {
        Source::Addresses => 0,
        Source::Admin => 1,
        Source::Neighbours => 2,
        Source::Place => 3,
        Source::None => 4,
    }
}

fn record(s: &Street, out: &mut Vec<u8>) {
    put(
        out,
        u64::from(s.class) | u64::from(source_code(s.town_source)) << 5,
    );
    put_str(out, &s.name);
    put_str(out, &s.town);
    put(out, s.admin.map_or(0, |a| u64::from(a) + 1));
    put(out, s.along.length_m().round() as u64);
    put(out, s.alt_names.len() as u64);
    for a in &s.alt_names {
        put_str(out, a);
    }
    put(out, s.refs.len() as u64);
    for r in &s.refs {
        put_str(out, r);
    }
}

/// Appends a street's parts as deltas from `cur`, in units of 2^[`SHIFT`].
fn geometry(s: &Street, cur: &mut [i32; 2], out: &mut Vec<u8>) {
    put(out, s.parts.len() as u64);
    for part in &s.parts {
        put(out, part.len() as u64);
        for p in part {
            let q = [(p[0] >> SHIFT) as i32, (p[1] >> SHIFT) as i32];
            put(out, zigzag(q[0] - cur[0]));
            put(out, zigzag(q[1] - cur[1]));
            *cur = q;
        }
    }
}

/// `count:u32 nblocks:u32 (first:u32 offset:u32 length:u32)* block*`, offsets from the
/// section's start.
pub fn blocked(count: usize, blocks: Blocks) -> Result<(Vec<u8>, u64)> {
    let frames: Vec<(u32, Vec<u8>)> = blocks
        .into_par_iter()
        .map(|(f, b)| Ok((f, zstd::bulk::compress(&b, LEVEL)?)))
        .collect::<Result<_>>()?;
    let mut out = Vec::new();
    out.extend_from_slice(&(count as u32).to_le_bytes());
    out.extend_from_slice(&(frames.len() as u32).to_le_bytes());
    let mut offset = 8 + 12 * frames.len();
    for (f, b) in &frames {
        out.extend_from_slice(&f.to_le_bytes());
        out.extend_from_slice(&(offset as u32).to_le_bytes());
        out.extend_from_slice(&(b.len() as u32).to_le_bytes());
        offset += b.len();
    }
    for (_, b) in &frames {
        out.extend_from_slice(b);
    }
    Ok((out, frames.len() as u64))
}

/// Cuts the streets into blocks of about `block` bytes; `enc` appends one street, with a
/// cursor that starts at (0, 0) in each block.
fn blocks(
    streets: &[Street],
    block: usize,
    enc: impl Fn(&Street, &mut [i32; 2], &mut Vec<u8>),
) -> (Blocks, u64) {
    let mut out = Vec::new();
    let (mut cur, mut first, mut raw) = (Vec::new(), 0u32, 0u64);
    let mut cursor = [0i32; 2];
    for (i, s) in streets.iter().enumerate() {
        if cur.len() >= block {
            raw += cur.len() as u64;
            out.push((first, std::mem::take(&mut cur)));
            first = i as u32;
            cursor = [0, 0];
        }
        enc(s, &mut cursor, &mut cur);
    }
    if !cur.is_empty() {
        raw += cur.len() as u64;
        out.push((first, cur));
    }
    (out, raw)
}

/// The two sections' bytes and their sizes, with blocks of about `block` bytes.
pub fn sections(table: &Table, block: usize) -> Result<(Vec<u8>, Vec<u8>, Sizes)> {
    let n = table.streets.len();
    let (b, sstr_raw) = blocks(&table.streets, block, |s, _, out| record(s, out));
    let (sstr, sstr_blocks) = blocked(n, b)?;
    let (b, sgeo_raw) = blocks(&table.streets, block, geometry);
    let (sgeo, sgeo_blocks) = blocked(n, b)?;
    let sizes = Sizes {
        block_kib: block >> 10,
        streets: n as u64,
        sstr_raw,
        sstr: sstr.len() as u64,
        sstr_blocks,
        sgeo_raw,
        sgeo: sgeo.len() as u64,
        sgeo_blocks,
    };
    Ok((sstr, sgeo, sizes))
}

/// A street record as decoded from `SSTR`.
#[cfg(test)]
#[derive(Debug, PartialEq)]
pub struct Record {
    pub class: u8,
    pub town_source: u8,
    pub name: String,
    pub town: String,
    pub admin: Option<u32>,
    pub length_m: u64,
    pub alt_names: Vec<String>,
    pub refs: Vec<String>,
}

pub fn string(r: &mut Reader) -> io::Result<String> {
    let n = r.count()?;
    String::from_utf8(r.bytes(n)?.to_vec()).map_err(|_| invalid("bad UTF-8"))
}

/// Blocks with their first street.
pub type Blocks = Vec<(u32, Vec<u8>)>;

/// The street count and decompressed blocks of a blocked section.
pub fn read_blocks(section: &[u8]) -> io::Result<(usize, Blocks)> {
    let u32_at = |i: usize| -> io::Result<u32> {
        section
            .get(i..i + 4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
            .ok_or_else(|| invalid("truncated block index"))
    };
    let count = u32_at(0)? as usize;
    let n = u32_at(4)? as usize;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let first = u32_at(8 + 12 * i)?;
        let offset = u32_at(12 + 12 * i)? as usize;
        let len = u32_at(16 + 12 * i)? as usize;
        let frame = section
            .get(offset..offset + len)
            .ok_or_else(|| invalid("block past the end"))?;
        out.push((first, zstd::stream::decode_all(frame)?));
    }
    Ok((count, out))
}

/// Every street in `SSTR` and `SGEO`, in order (for tests; the app reads them with
/// `omp_core`).
#[cfg(test)]
pub fn decode(sstr: &[u8], sgeo: &[u8]) -> io::Result<Vec<(Record, Vec<Vec<P>>)>> {
    let (count, blocks) = read_blocks(sstr)?;
    let mut records = Vec::with_capacity(count);
    for (first, b) in &blocks {
        if *first as usize != records.len() {
            return Err(invalid("SSTR block out of order"));
        }
        let mut r = Reader::new(b);
        while !r.at_end() {
            let flags = r.varint()?;
            let name = string(&mut r)?;
            let town = string(&mut r)?;
            let admin = r.varint()?;
            let length_m = r.varint()?;
            let alt_names = (0..r.count()?)
                .map(|_| string(&mut r))
                .collect::<io::Result<_>>()?;
            let refs = (0..r.count()?)
                .map(|_| string(&mut r))
                .collect::<io::Result<_>>()?;
            records.push(Record {
                class: (flags & 31) as u8,
                town_source: (flags >> 5) as u8,
                name,
                town,
                admin: admin.checked_sub(1).map(|a| a as u32),
                length_m,
                alt_names,
                refs,
            });
        }
    }
    let (gcount, blocks) = read_blocks(sgeo)?;
    if gcount != count || records.len() != count {
        return Err(invalid("SSTR and SGEO counts differ"));
    }
    let mut parts_of: Vec<Vec<Vec<P>>> = Vec::with_capacity(count);
    for (first, b) in &blocks {
        if *first as usize != parts_of.len() {
            return Err(invalid("SGEO block out of order"));
        }
        let mut r = Reader::new(b);
        let mut cur = [0i32; 2];
        while !r.at_end() {
            let nparts = r.count()?;
            let mut parts = Vec::with_capacity(nparts);
            for _ in 0..nparts {
                let npts = r.count()?;
                let mut part = Vec::with_capacity(npts);
                for _ in 0..npts {
                    cur[0] += r.zvarint()?;
                    cur[1] += r.zvarint()?;
                    part.push([(cur[0] as u32) << SHIFT, (cur[1] as u32) << SHIFT]);
                }
                parts.push(part);
            }
            parts_of.push(parts);
        }
    }
    if parts_of.len() != count {
        return Err(invalid("SGEO holds a different number of streets"));
    }
    Ok(records.into_iter().zip(parts_of).collect())
}
