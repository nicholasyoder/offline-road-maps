//! `SENT` (entries), `STWN` (towns), `SFST` (token → postings) and `SPST` (postings), as in
//! `docs/format.md`.

use std::io;

use anyhow::Result;
use omp_core::varint::{Reader, invalid, put, put_str, zigzag};
use serde::Serialize;

use super::{Entry, Index, Kind};
use crate::measure::geom::P;
use crate::street::SHIFT;
use crate::street::encode::{Blocks, blocked, read_blocks, string};

pub const SENT: [u8; 4] = *b"SENT";
pub const STWN: [u8; 4] = *b"STWN";
pub const SFST: [u8; 4] = *b"SFST";
pub const SPST: [u8; 4] = *b"SPST";
const LEVEL: i32 = 19;
/// Uncompressed bytes per `SENT` block, about.
pub const BLOCK: usize = 16 << 10;

#[derive(Serialize, Default)]
pub struct Sizes {
    pub block_kib: usize,
    pub entries: u64,
    pub sent_raw: u64,
    pub sent: u64,
    pub sent_blocks: u64,
    /// Raw bytes per kind, and the raw bytes of street names (also in `SSTR`).
    pub raw_by_kind: Vec<(Kind, u64)>,
    pub street_names_raw: u64,
    /// `SENT` with street names left empty, to price keeping them only in `SSTR`.
    pub sent_without_street_names: u64,
    pub stwn: u64,
    pub sfst: u64,
    pub spst: u64,
}

pub struct Sections {
    pub sent: Vec<u8>,
    pub stwn: Vec<u8>,
    pub sfst: Vec<u8>,
    pub spst: Vec<u8>,
}

/// Appends one entry, its point as a delta from `cur` in units of 2^[`SHIFT`].
fn record(e: &Entry, cur: &mut [i32; 2], out: &mut Vec<u8>, street_name: bool) {
    put(out, u64::from(e.class) << 3 | e.kind as u64);
    out.push(e.importance);
    let name = if e.kind == Kind::Street && !street_name {
        ""
    } else {
        &e.name
    };
    put_str(out, name);
    let q = [(e.p[0] >> SHIFT) as i32, (e.p[1] >> SHIFT) as i32];
    put(out, zigzag(q[0] - cur[0]));
    put(out, zigzag(q[1] - cur[1]));
    *cur = q;
    put(out, e.admin.map_or(0, |a| u64::from(a) + 1));
    put(out, u64::from(e.town));
    put(out, e.reference);
    put(out, e.alts.len() as u64);
    for a in &e.alts {
        put_str(out, a);
    }
}

/// The entries cut into blocks of about `block` raw bytes, each with a cursor from (0, 0);
/// also the raw bytes per kind.
fn blocks(entries: &[Entry], block: usize, street_name: bool) -> (Blocks, u64, Vec<u64>) {
    let mut out = Vec::new();
    let (mut cur, mut first, mut raw) = (Vec::new(), 0u32, 0u64);
    let mut by_kind = vec![0u64; Kind::ALL.len()];
    let mut cursor = [0i32; 2];
    for (i, e) in entries.iter().enumerate() {
        if cur.len() >= block {
            raw += cur.len() as u64;
            out.push((first, std::mem::take(&mut cur)));
            first = i as u32;
            cursor = [0, 0];
        }
        let before = cur.len();
        record(e, &mut cursor, &mut cur, street_name);
        by_kind[e.kind as usize] += (cur.len() - before) as u64;
    }
    if !cur.is_empty() {
        raw += cur.len() as u64;
        out.push((first, cur));
    }
    (out, raw, by_kind)
}

/// `n:varint total:varint bytes:varint` then `bytes` of ID deltas (the first from 0).
fn postings_list(ids: &[u32], total: u64, out: &mut Vec<u8>) {
    let mut deltas = Vec::with_capacity(ids.len() * 2);
    let mut prev = 0;
    for &id in ids {
        put(&mut deltas, u64::from(id - prev));
        prev = id;
    }
    put(out, ids.len() as u64);
    put(out, total);
    put(out, deltas.len() as u64);
    out.extend_from_slice(&deltas);
}

/// The four sections, with `SENT` blocks of about `block` raw bytes.
pub fn sections(index: &Index, block: usize) -> Result<(Sections, Sizes)> {
    let n = index.entries.len();
    let (b, sent_raw, by_kind) = blocks(&index.entries, block, true);
    let (sent, sent_blocks) = blocked(n, b)?;
    let (b, _, _) = blocks(&index.entries, block, false);
    let (without, _) = blocked(n, b)?;

    let mut towns_raw = Vec::new();
    put(&mut towns_raw, index.towns.len() as u64);
    for t in &index.towns {
        put_str(&mut towns_raw, t);
    }
    let stwn = zstd::bulk::compress(&towns_raw, LEVEL)?;

    let mut spst = Vec::new();
    let mut fst = fst::MapBuilder::memory();
    for (token, ids, total) in &index.postings {
        fst.insert(token, spst.len() as u64)?;
        postings_list(ids, *total, &mut spst);
    }
    let sfst = fst.into_inner()?;

    let raw_by_kind: Vec<(Kind, u64)> = Kind::ALL.iter().copied().zip(by_kind).collect();
    let street_names_raw = index
        .entries
        .iter()
        .filter(|e| e.kind == Kind::Street)
        .map(|e| e.name.len() as u64)
        .sum();
    let sizes = Sizes {
        block_kib: block >> 10,
        entries: n as u64,
        sent_raw,
        sent: sent.len() as u64,
        sent_blocks,
        raw_by_kind,
        street_names_raw,
        sent_without_street_names: without.len() as u64,
        stwn: stwn.len() as u64,
        sfst: sfst.len() as u64,
        spst: spst.len() as u64,
    };
    Ok((
        Sections {
            sent,
            stwn,
            sfst,
            spst,
        },
        sizes,
    ))
}

/// An entry as decoded from `SENT`.
#[derive(Debug, PartialEq)]
pub struct Decoded {
    pub kind: Kind,
    pub class: u16,
    pub importance: u8,
    pub name: String,
    pub p: P,
    pub admin: Option<u32>,
    pub town: u32,
    pub reference: u64,
    pub alts: Vec<String>,
}

/// Every entry in `SENT`, in order.
pub fn decode_entries(sent: &[u8]) -> io::Result<Vec<Decoded>> {
    let (count, blocks) = read_blocks(sent)?;
    let mut out = Vec::with_capacity(count);
    for (first, b) in &blocks {
        if *first as usize != out.len() {
            return Err(invalid("SENT block out of order"));
        }
        let mut r = Reader::new(b);
        let mut cur = [0i32; 2];
        while !r.at_end() {
            let kc = r.varint()?;
            let kind = Kind::from_code((kc & 7) as u8).ok_or_else(|| invalid("bad kind"))?;
            let importance = r.bytes(1)?[0];
            let name = string(&mut r)?;
            cur[0] += r.zvarint()?;
            cur[1] += r.zvarint()?;
            let admin = r.varint()?;
            let town = r.varint()? as u32;
            let reference = r.varint()?;
            let alts = (0..r.count()?)
                .map(|_| string(&mut r))
                .collect::<io::Result<_>>()?;
            out.push(Decoded {
                kind,
                class: (kc >> 3) as u16,
                importance,
                name,
                p: [(cur[0] as u32) << SHIFT, (cur[1] as u32) << SHIFT],
                admin: admin.checked_sub(1).map(|a| a as u32),
                town,
                reference,
                alts,
            });
        }
    }
    if out.len() != count {
        return Err(invalid("SENT holds a different number of entries"));
    }
    Ok(out)
}

pub fn decode_towns(stwn: &[u8]) -> io::Result<Vec<String>> {
    let raw = zstd::stream::decode_all(stwn)?;
    let mut r = Reader::new(&raw);
    (0..r.count()?).map(|_| string(&mut r)).collect()
}

/// The postings of `token`: its stored entry IDs and the number of entries holding it
/// (more than stored if it was left out of postings).
pub fn lookup(sfst: &[u8], spst: &[u8], token: &str) -> io::Result<Option<(Vec<u32>, u64)>> {
    let map = fst::Map::new(sfst).map_err(|_| invalid("bad SFST"))?;
    let Some(offset) = map.get(token) else {
        return Ok(None);
    };
    let mut r = Reader::new(
        spst.get(offset as usize..)
            .ok_or_else(|| invalid("postings past the end"))?,
    );
    let n = r.count()?;
    let total = r.varint()?;
    let bytes = r.count()?;
    let mut d = Reader::new(r.bytes(bytes)?);
    let mut ids = Vec::with_capacity(n);
    let mut prev = 0u32;
    for _ in 0..n {
        prev += d.varint()? as u32;
        ids.push(prev);
    }
    Ok(Some((ids, total)))
}
