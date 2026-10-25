//! Tile encodings from `docs/format.md`: the V2 layer payload, the Hilbert key, zoom bands,
//! and the `TIDX` and `TDIC` section layouts. Reading and writing them in a pack is in
//! [`crate::tiles`].

use std::collections::{BTreeMap, HashMap};
use std::io;
use std::ops::Range;

use crate::varint::{self, Reader, invalid, zigzag};

/// Tile-local grid: 4096 units per tile edge.
pub const EXTENT_BITS: u32 = 12;
/// The highest zoom stored; the app overzooms it.
pub const MAX_ZOOM: u8 = 14;
pub const ZOOMS: usize = MAX_ZOOM as usize + 1;
/// `TIDX` layer masks are 16 bits.
pub const MAX_LAYERS: usize = 16;
/// Tiles per `TIDX` block.
pub const BLOCK_TILES: usize = 256;

/// A tile-local point: x, y on the 4096 grid, y down, −64…4159 with the buffer.
pub type Point = [i32; 2];

/// Zoom bands that share a dictionary.
pub fn zoom_band(z: u8) -> u8 {
    match z {
        0..=7 => 0,
        8..=11 => 1,
        12..=13 => 2,
        _ => 3,
    }
}

pub const BAND_NAMES: [&str; 4] = ["z0-7", "z8-11", "z12-13", "z14"];

/// The tile's index `d` along the Hilbert curve over the 2^z × 2^z grid.
pub fn hilbert(z: u8, mut x: u32, mut y: u32) -> u64 {
    let n = 1u64 << z;
    let mut d = 0u64;
    let mut s = n / 2;
    while s > 0 {
        let rx = u64::from(u64::from(x) & s > 0);
        let ry = u64::from(u64::from(y) & s > 0);
        d += s * s * ((3 * rx) ^ ry);
        if ry == 0 {
            if rx == 1 {
                x = (n - 1) as u32 - x;
                y = (n - 1) as u32 - y;
            }
            std::mem::swap(&mut x, &mut y);
        }
        s /= 2;
    }
    d
}

/// The tile `(x, y)` at zoom `z` with Hilbert key `d`: the inverse of [`hilbert`].
pub fn hilbert_xy(z: u8, mut d: u64) -> (u32, u32) {
    let (mut x, mut y) = (0u64, 0u64);
    let mut s = 1u64;
    while s < 1u64 << z {
        let rx = 1 & (d / 2);
        let ry = 1 & (d ^ rx);
        if ry == 0 {
            if rx == 1 {
                x = s - 1 - x;
                y = s - 1 - y;
            }
            std::mem::swap(&mut x, &mut y);
        }
        x += s * rx;
        y += s * ry;
        d /= 4;
        s *= 2;
    }
    (x as u32, y as u32)
}

// --- Layer payload (V2) ---

#[derive(Clone, Debug, PartialEq)]
pub enum Geom {
    Point(Point),
    Lines(Vec<Vec<Point>>),
    Polygon(Vec<Vec<Point>>),
}

impl Geom {
    pub fn vertices(&self) -> usize {
        match self {
            Geom::Point(_) => 1,
            Geom::Lines(p) | Geom::Polygon(p) => p.iter().map(Vec::len).sum(),
        }
    }
}

/// A feature ready to encode: class index, props and tile-local geometry.
pub struct Enc<'a> {
    pub class: u16,
    pub props: &'a [(u8, Box<str>)],
    pub geom: Geom,
}

/// A small non-negative decimal integer without leading zeros is stored as a number.
pub fn int_value(v: &str) -> Option<u64> {
    if v.is_empty() || v.len() > 9 || (v.len() > 1 && v.starts_with('0')) {
        return None;
    }
    v.parse().ok()
}

/// Encodes one layer payload:
/// `nfeatures, nstrings, strings (len + bytes)`, then per feature
/// `class << 2 | geomtype, nprops, (key, value)*, geometry`. A value is `string << 1` or
/// `int << 1 | 1`. Geometry is zigzag deltas from a cursor carried across features:
/// point `dx dy`; lines `nparts (npoints deltas*)*`; polygon `nrings (npoints deltas*)*`
/// with rings implicitly closed.
pub fn encode_layer(features: &[Enc]) -> Vec<u8> {
    let mut strings: Vec<&str> = Vec::new();
    let mut index = HashMap::new();
    for f in features {
        for (_, v) in f.props {
            if int_value(v).is_none() && !index.contains_key(&**v) {
                index.insert(&**v, strings.len() as u64);
                strings.push(v);
            }
        }
    }
    let mut out = Vec::new();
    varint::put(&mut out, features.len() as u64);
    varint::put(&mut out, strings.len() as u64);
    for s in &strings {
        varint::put(&mut out, s.len() as u64);
        out.extend_from_slice(s.as_bytes());
    }
    let mut cur: Point = [0, 0];
    let pts = |out: &mut Vec<u8>, part: &[Point], cur: &mut Point| {
        varint::put(out, part.len() as u64);
        for p in part {
            varint::put(out, zigzag(p[0] - cur[0]));
            varint::put(out, zigzag(p[1] - cur[1]));
            *cur = *p;
        }
    };
    for f in features {
        let ty = match f.geom {
            Geom::Point(_) => 0,
            Geom::Lines(_) => 1,
            Geom::Polygon(_) => 2,
        };
        varint::put(&mut out, (u64::from(f.class) << 2) | ty);
        varint::put(&mut out, f.props.len() as u64);
        for (k, v) in f.props {
            varint::put(&mut out, u64::from(*k));
            match int_value(v) {
                Some(n) => varint::put(&mut out, (n << 1) | 1),
                None => varint::put(&mut out, index[&**v] << 1),
            }
        }
        match &f.geom {
            Geom::Point(p) => {
                varint::put(&mut out, zigzag(p[0] - cur[0]));
                varint::put(&mut out, zigzag(p[1] - cur[1]));
                cur = *p;
            }
            Geom::Lines(parts) | Geom::Polygon(parts) => {
                varint::put(&mut out, parts.len() as u64);
                for part in parts {
                    pts(&mut out, part, &mut cur);
                }
            }
        }
    }
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GeomType {
    Point,
    Lines,
    Polygon,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Value {
    /// Index into the layer's string table.
    Str(u32),
    Int(u64),
}

/// One decoded feature. Its props, parts and points live in the [`Layer`].
#[derive(Clone, Debug)]
pub struct Feature {
    pub class: u16,
    pub kind: GeomType,
    props: Range<u32>,
    parts: Range<u32>,
}

/// A decoded layer payload in flat arrays: a few allocations per layer, none per point.
#[derive(Debug)]
pub struct Layer {
    bytes: Vec<u8>,
    strings: Vec<Range<u32>>,
    pub features: Vec<Feature>,
    props: Vec<(u8, Value)>,
    /// End index in `points` of each part (line or ring); a point feature has one part.
    part_ends: Vec<u32>,
    points: Vec<Point>,
}

impl Layer {
    pub fn decode(bytes: Vec<u8>) -> io::Result<Self> {
        let mut r = Reader::new(&bytes);
        let nf = r.count()?;
        let ns = r.count()?;
        let mut strings = Vec::with_capacity(ns);
        for _ in 0..ns {
            let len = r.varint()? as usize;
            let start = r.i;
            let s = r.bytes(len)?;
            std::str::from_utf8(s).map_err(|_| invalid("layer string is not UTF-8"))?;
            strings.push(start as u32..(start + len) as u32);
        }
        let mut features = Vec::with_capacity(nf);
        let mut props = Vec::new();
        let mut part_ends = Vec::new();
        let mut points = Vec::new();
        let mut cur: Point = [0, 0];
        let mut point = |r: &mut Reader, points: &mut Vec<Point>| -> io::Result<()> {
            cur[0] = cur[0].wrapping_add(r.zvarint()?);
            cur[1] = cur[1].wrapping_add(r.zvarint()?);
            points.push(cur);
            Ok(())
        };
        for _ in 0..nf {
            let head = r.varint()?;
            let class = u16::try_from(head >> 2).map_err(|_| invalid("class out of range"))?;
            let p0 = props.len() as u32;
            for _ in 0..r.count()? {
                let k = u8::try_from(r.varint()?).map_err(|_| invalid("prop key out of range"))?;
                let v = r.varint()?;
                let v = if v & 1 == 1 {
                    Value::Int(v >> 1)
                } else {
                    let i = v >> 1;
                    if i >= ns as u64 {
                        return Err(invalid("string index out of range"));
                    }
                    Value::Str(i as u32)
                };
                props.push((k, v));
            }
            let s0 = part_ends.len() as u32;
            let kind = match head & 3 {
                0 => {
                    point(&mut r, &mut points)?;
                    part_ends.push(points.len() as u32);
                    GeomType::Point
                }
                t @ (1 | 2) => {
                    for _ in 0..r.count()? {
                        for _ in 0..r.count()? {
                            point(&mut r, &mut points)?;
                        }
                        part_ends.push(points.len() as u32);
                    }
                    if t == 1 {
                        GeomType::Lines
                    } else {
                        GeomType::Polygon
                    }
                }
                _ => return Err(invalid("unknown geometry type")),
            };
            features.push(Feature {
                class,
                kind,
                props: p0..props.len() as u32,
                parts: s0..part_ends.len() as u32,
            });
        }
        if !r.at_end() {
            return Err(invalid("trailing bytes after the last feature"));
        }
        Ok(Self {
            bytes,
            strings,
            features,
            props,
            part_ends,
            points,
        })
    }

    pub fn strings(&self) -> usize {
        self.strings.len()
    }

    pub fn string(&self, i: u32) -> &str {
        let r = &self.strings[i as usize];
        // Checked in `decode`.
        std::str::from_utf8(&self.bytes[r.start as usize..r.end as usize]).unwrap_or_default()
    }

    pub fn props(&self, f: &Feature) -> &[(u8, Value)] {
        &self.props[f.props.start as usize..f.props.end as usize]
    }

    /// The value of prop `key` as text (an integer is formatted).
    pub fn prop(&self, f: &Feature, key: u8) -> Option<std::borrow::Cow<'_, str>> {
        self.props(f)
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| match v {
                Value::Str(i) => self.string(*i).into(),
                Value::Int(n) => n.to_string().into(),
            })
    }

    /// The feature's lines or rings; a point feature has one part of one point.
    pub fn parts(&self, f: &Feature) -> impl ExactSizeIterator<Item = &[Point]> {
        (f.parts.start..f.parts.end).map(move |i| {
            let start = match i {
                0 => 0,
                _ => self.part_ends[i as usize - 1],
            };
            &self.points[start as usize..self.part_ends[i as usize] as usize]
        })
    }

    /// Back to the encoder's form; for tests and tools.
    pub fn geom(&self, f: &Feature) -> Geom {
        let parts = || self.parts(f).map(<[Point]>::to_vec).collect();
        match f.kind {
            GeomType::Point => Geom::Point(self.parts(f).next().unwrap_or(&[[0, 0]])[0]),
            GeomType::Lines => Geom::Lines(parts()),
            GeomType::Polygon => Geom::Polygon(parts()),
        }
    }

    pub fn vertices(&self) -> usize {
        self.points.len()
    }

    /// Heap bytes held, for the app's cache budget.
    pub fn heap_bytes(&self) -> usize {
        use std::mem::size_of;
        self.bytes.capacity()
            + self.strings.capacity() * size_of::<Range<u32>>()
            + self.features.capacity() * size_of::<Feature>()
            + self.props.capacity() * size_of::<(u8, Value)>()
            + self.part_ends.capacity() * size_of::<u32>()
            + self.points.capacity() * size_of::<Point>()
    }
}

// --- TDIC ---

/// zstd dictionaries per (layer, zoom band), as stored in `TDIC`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Dictionaries(pub BTreeMap<(u8, u8), Vec<u8>>);

impl Dictionaries {
    /// `ndicts:u32`, then `layer:u8 band:u8 reserved:u16 offset:u32 length:u32` per entry
    /// (offsets from the start of the section), then the dictionary bytes.
    pub fn encode(&self) -> Vec<u8> {
        let dicts: Vec<_> = self.0.iter().filter(|(_, d)| !d.is_empty()).collect();
        let mut out = Vec::new();
        out.extend_from_slice(&(dicts.len() as u32).to_le_bytes());
        let mut at = 4 + 12 * dicts.len();
        for &(&(layer, band), d) in &dicts {
            out.extend_from_slice(&[layer, band, 0, 0]);
            out.extend_from_slice(&(at as u32).to_le_bytes());
            out.extend_from_slice(&(d.len() as u32).to_le_bytes());
            at += d.len();
        }
        for (_, d) in dicts {
            out.extend_from_slice(d);
        }
        out
    }

    pub fn decode(b: &[u8]) -> io::Result<Self> {
        let u32_at = |i: usize| -> io::Result<u32> {
            b.get(i..i + 4)
                .map(|s| u32::from_le_bytes(s.try_into().unwrap()))
                .ok_or_else(|| invalid("truncated TDIC"))
        };
        let n = u32_at(0)? as usize;
        let mut map = BTreeMap::new();
        for e in 0..n {
            let at = 4 + 12 * e;
            let head = b.get(at..at + 4).ok_or_else(|| invalid("truncated TDIC"))?;
            let (off, len) = (u32_at(at + 4)? as usize, u32_at(at + 8)? as usize);
            let d = b
                .get(off..off + len)
                .ok_or_else(|| invalid("TDIC entry out of bounds"))?;
            map.insert((head[0], head[1]), d.to_vec());
        }
        Ok(Self(map))
    }
}

// --- TIDX ---

const ZOOM_DIR_LEN: usize = 8;
const DIR_ENTRY_LEN: usize = 24;
/// Bytes of the per-zoom header at the start of `TIDX`.
pub const TIDX_HEADER_LEN: usize = ZOOMS * ZOOM_DIR_LEN;

/// One block in a zoom's directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DirEntry {
    pub first_key: u64,
    /// Virtual offset in the pack (inside `TDAT`) of the block's first tile.
    pub data_offset: u64,
    /// From the start of `TIDX`.
    pub block_offset: u32,
    pub tiles: u32,
}

impl DirEntry {
    fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.first_key.to_le_bytes());
        out.extend_from_slice(&self.data_offset.to_le_bytes());
        out.extend_from_slice(&self.block_offset.to_le_bytes());
        out.extend_from_slice(&self.tiles.to_le_bytes());
    }

    fn decode(b: &[u8]) -> Self {
        Self {
            first_key: u64::from_le_bytes(b[0..8].try_into().unwrap()),
            data_offset: u64::from_le_bytes(b[8..16].try_into().unwrap()),
            block_offset: u32::from_le_bytes(b[16..20].try_into().unwrap()),
            tiles: u32::from_le_bytes(b[20..24].try_into().unwrap()),
        }
    }
}

/// Where one zoom's directory is, from the `TIDX` header.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ZoomDir {
    pub blocks: u32,
    /// From the start of `TIDX`; the zoom's blocks follow its directory.
    pub dir_offset: u32,
}

impl ZoomDir {
    pub fn dir_len(&self) -> usize {
        self.blocks as usize * DIR_ENTRY_LEN
    }
}

pub fn decode_tidx_header(b: &[u8]) -> io::Result<[ZoomDir; ZOOMS]> {
    if b.len() < TIDX_HEADER_LEN {
        return Err(invalid("truncated TIDX header"));
    }
    let mut z = [ZoomDir::default(); ZOOMS];
    for (i, d) in z.iter_mut().enumerate() {
        let at = i * ZOOM_DIR_LEN;
        d.blocks = u32::from_le_bytes(b[at..at + 4].try_into().unwrap());
        d.dir_offset = u32::from_le_bytes(b[at + 4..at + 8].try_into().unwrap());
    }
    Ok(z)
}

pub fn decode_directory(b: &[u8]) -> io::Result<Vec<DirEntry>> {
    if !b.len().is_multiple_of(DIR_ENTRY_LEN) {
        return Err(invalid("TIDX directory length"));
    }
    let dir: Vec<DirEntry> = b
        .as_chunks::<DIR_ENTRY_LEN>()
        .0
        .iter()
        .map(|e| DirEntry::decode(e))
        .collect();
    if dir.windows(2).any(|w| w[0].first_key >= w[1].first_key) {
        return Err(invalid("TIDX directory is not sorted"));
    }
    Ok(dir)
}

/// A tile's place in `TDAT`: its record offset and each layer's frame length.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TileLoc {
    pub offset: u64,
    pub layers: Vec<(u8, u32)>,
}

impl TileLoc {
    pub fn len(&self) -> u64 {
        self.layers.iter().map(|&(_, n)| u64::from(n)).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.layers.is_empty()
    }
}

/// A decoded `TIDX` block.
#[derive(Debug)]
pub struct Block {
    keys: Vec<u64>,
    offsets: Vec<u64>,
    masks: Vec<u16>,
    /// Start of each tile's frame lengths in `lens`; one extra at the end.
    len_starts: Vec<u32>,
    lens: Vec<u32>,
}

impl Block {
    pub fn decode(b: &[u8], dir: &DirEntry) -> io::Result<Self> {
        let n = dir.tiles as usize;
        if n == 0 || n > BLOCK_TILES {
            return Err(invalid("bad TIDX block size"));
        }
        let mut r = Reader::new(b);
        let mut blk = Self {
            keys: Vec::with_capacity(n),
            offsets: Vec::with_capacity(n),
            masks: Vec::with_capacity(n),
            len_starts: Vec::with_capacity(n + 1),
            lens: Vec::new(),
        };
        let (mut key, mut offset) = (dir.first_key, dir.data_offset);
        for i in 0..n {
            let delta = r.varint()?;
            if i > 0 && delta == 0 {
                return Err(invalid("TIDX keys are not increasing"));
            }
            key = key
                .checked_add(delta)
                .ok_or_else(|| invalid("TIDX key overflow"))?;
            let mask = u16::from_le_bytes(r.bytes(2)?.try_into().unwrap());
            blk.keys.push(key);
            blk.offsets.push(offset);
            blk.masks.push(mask);
            blk.len_starts.push(blk.lens.len() as u32);
            for _ in 0..mask.count_ones() {
                let len = u32::try_from(r.varint()?).map_err(|_| invalid("frame too long"))?;
                offset += u64::from(len);
                blk.lens.push(len);
            }
        }
        blk.len_starts.push(blk.lens.len() as u32);
        Ok(blk)
    }

    pub fn find(&self, key: u64) -> Option<TileLoc> {
        let i = self.keys.binary_search(&key).ok()?;
        Some(self.loc(i))
    }

    /// Every tile in the block, in key order.
    pub fn iter(&self) -> impl Iterator<Item = (u64, TileLoc)> + '_ {
        (0..self.keys.len()).map(|i| (self.keys[i], self.loc(i)))
    }

    fn loc(&self, i: usize) -> TileLoc {
        let lens = &self.lens[self.len_starts[i] as usize..self.len_starts[i + 1] as usize];
        let layers = (0..MAX_LAYERS as u8).filter(|l| self.masks[i] & (1 << l) != 0);
        TileLoc {
            offset: self.offsets[i],
            layers: layers.zip(lens.iter().copied()).collect(),
        }
    }

    /// Bytes this block holds in memory, for cache accounting.
    pub fn heap_bytes(&self) -> usize {
        self.keys.len() * 18 + self.lens.len() * 4 + self.len_starts.len() * 4
    }
}

/// Builds `TIDX` from tiles added in (zoom, Hilbert key) order with their `TDAT` offsets.
#[derive(Default)]
pub struct IndexBuilder {
    zooms: Vec<ZoomBuild>,
    last: Option<(u8, u64)>,
}

#[derive(Default)]
struct ZoomBuild {
    dir: Vec<DirEntry>,
    blocks: Vec<u8>,
    /// Key of the last tile and the offset just past it.
    prev: (u64, u64),
}

impl IndexBuilder {
    pub fn new() -> Self {
        Self {
            zooms: (0..ZOOMS).map(|_| ZoomBuild::default()).collect(),
            last: None,
        }
    }

    /// Whether [`IndexBuilder::push`] would accept this tile.
    pub fn check(&self, z: u8, key: u64, layers: &[(u8, u32)]) -> io::Result<()> {
        let bad = |msg: &str| Err(io::Error::new(io::ErrorKind::InvalidInput, msg.to_owned()));
        if z > MAX_ZOOM {
            return bad("zoom above 14");
        }
        if self.last.is_some_and(|l| l >= (z, key)) {
            return bad("tiles must be added in (zoom, Hilbert key) order");
        }
        if layers.is_empty() || layers.windows(2).any(|w| w[0].0 >= w[1].0) {
            return bad("a tile needs layers in increasing order");
        }
        if layers.iter().any(|&(l, _)| usize::from(l) >= MAX_LAYERS) {
            return bad("layer ID above 15");
        }
        Ok(())
    }

    /// Adds a tile. A new block starts every [`BLOCK_TILES`] tiles and wherever the tile
    /// does not follow the previous one directly in `TDAT` (padding at a chunk boundary).
    pub fn push(&mut self, z: u8, key: u64, offset: u64, layers: &[(u8, u32)]) -> io::Result<()> {
        self.check(z, key, layers)?;
        self.last = Some((z, key));
        let zb = &mut self.zooms[z as usize];
        let fresh = match zb.dir.last() {
            None => true,
            Some(d) => d.tiles as usize == BLOCK_TILES || zb.prev.1 != offset,
        };
        let delta = if fresh {
            zb.dir.push(DirEntry {
                first_key: key,
                data_offset: offset,
                block_offset: zb.blocks.len() as u32,
                tiles: 0,
            });
            0
        } else {
            key - zb.prev.0
        };
        varint::put(&mut zb.blocks, delta);
        let mask: u16 = layers.iter().map(|&(l, _)| 1 << l).sum();
        zb.blocks.extend_from_slice(&mask.to_le_bytes());
        let mut end = offset;
        for &(_, n) in layers {
            varint::put(&mut zb.blocks, u64::from(n));
            end += u64::from(n);
        }
        zb.dir.last_mut().unwrap().tiles += 1;
        zb.prev = (key, end);
        Ok(())
    }

    /// `zoom_dir[15]`, then per zoom its directory followed by its blocks.
    pub fn finish(self) -> io::Result<Vec<u8>> {
        let mut out = vec![0; TIDX_HEADER_LEN];
        for (z, zb) in self.zooms.into_iter().enumerate() {
            let dir_offset = out.len();
            let base = dir_offset + zb.dir.len() * DIR_ENTRY_LEN;
            if base + zb.blocks.len() > u32::MAX as usize {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "TIDX larger than 4 GiB",
                ));
            }
            for mut d in zb.dir.iter().copied() {
                d.block_offset += base as u32;
                d.encode(&mut out);
            }
            out.extend_from_slice(&zb.blocks);
            let at = z * ZOOM_DIR_LEN;
            out[at..at + 4].copy_from_slice(&(zb.dir.len() as u32).to_le_bytes());
            out[at + 4..at + 8].copy_from_slice(&(dir_offset as u32).to_le_bytes());
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hilbert_inverts() {
        for z in [0, 1, 3, 14] {
            for &(x, y) in &[(0, 0), (1, 0), (5, 7), (16383, 1), (9000, 12000)] {
                let (x, y) = (x % (1 << z), y % (1 << z));
                assert_eq!(hilbert_xy(z, hilbert(z, x, y)), (x, y), "z{z} {x},{y}");
            }
        }
    }

    #[test]
    fn hilbert_order_2() {
        // The order-2 curve visits (0,0) (0,1) (1,1) (1,0).
        let d: Vec<u64> = [(0, 0), (0, 1), (1, 1), (1, 0)]
            .iter()
            .map(|&(x, y)| hilbert(1, x, y))
            .collect();
        assert_eq!(d, [0, 1, 2, 3]);
    }

    #[test]
    fn layer_round_trip() {
        let props: Vec<(u8, Box<str>)> = vec![(0, "Main St".into()), (1, "42".into())];
        let feats = vec![
            Enc {
                class: 3,
                props: &props,
                geom: Geom::Lines(vec![
                    vec![[0, 0], [100, -5], [4100, 7]],
                    vec![[1, 1], [2, 2]],
                ]),
            },
            Enc {
                class: 0,
                props: &[],
                geom: Geom::Polygon(vec![vec![[0, 0], [10, 0], [10, 10]]]),
            },
            Enc {
                class: 1,
                props: &props[..1],
                geom: Geom::Point([-64, 4159]),
            },
        ];
        let layer = Layer::decode(encode_layer(&feats)).unwrap();
        assert_eq!(layer.strings(), 1);
        assert_eq!(layer.string(0), "Main St");
        assert_eq!(layer.features.len(), 3);
        for (a, b) in feats.iter().zip(&layer.features) {
            assert_eq!(a.class, b.class);
            assert_eq!(a.geom, layer.geom(b));
        }
        let f = &layer.features[0];
        assert_eq!(layer.props(f), [(0, Value::Str(0)), (1, Value::Int(42))]);
        assert_eq!(layer.prop(f, 1).as_deref(), Some("42"));
        assert_eq!(layer.prop(f, 2), None);
        assert_eq!(layer.parts(f).len(), 2);
        assert_eq!(layer.vertices(), 9);

        let empty = Layer::decode(encode_layer(&[])).unwrap();
        assert!(empty.features.is_empty());
    }

    /// Tile frames have no checksum (format 0.3), so a damaged payload must fail in the
    /// decoder, or decode to something the accessors can walk, and never panic.
    #[test]
    fn damaged_payloads_never_panic() {
        let props: Vec<(u8, Box<str>)> = vec![(0, "Main St".into()), (1, "42".into())];
        let ring: Vec<Point> = (0..40).map(|i| [i * 97 % 4096, i * 31 % 4096]).collect();
        let good = encode_layer(&[
            Enc {
                class: 3,
                props: &props,
                geom: Geom::Lines(vec![ring.clone(), ring[..5].to_vec()]),
            },
            Enc {
                class: 1,
                props: &props[..1],
                geom: Geom::Polygon(vec![ring.clone(), ring[10..20].to_vec()]),
            },
            Enc {
                class: 0,
                props: &[],
                geom: Geom::Point([5, 6]),
            },
        ]);
        // xorshift, so the cases are the same on every run.
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut rand = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let walk = |l: &Layer| {
            let mut n = l.vertices();
            for f in &l.features {
                n += l.parts(f).map(<[Point]>::len).sum::<usize>();
                for &(k, _) in l.props(f) {
                    n += l.prop(f, k).map_or(0, |v| v.len());
                }
                let _ = l.geom(f);
            }
            n
        };
        let mut errors = 0;
        for case in 0..20_000 {
            let mut b = good.clone();
            match case % 3 {
                0 => b.truncate(rand() as usize % b.len()),
                1 => {
                    for _ in 0..1 + rand() % 4 {
                        let i = rand() as usize % b.len();
                        b[i] ^= 1 << (rand() % 8);
                    }
                }
                _ => {
                    let i = rand() as usize % b.len();
                    b[i] = rand() as u8;
                }
            }
            match Layer::decode(b) {
                Ok(l) => {
                    walk(&l);
                }
                Err(_) => errors += 1,
            }
        }
        assert!(errors > 1000, "only {errors} damaged payloads were refused");
    }

    #[test]
    fn layer_rejects_garbage() {
        let mut b = encode_layer(&[Enc {
            class: 1,
            props: &[],
            geom: Geom::Lines(vec![vec![[0, 0], [5, 5]]]),
        }]);
        assert!(Layer::decode(b[..b.len() - 1].to_vec()).is_err());
        b.push(0);
        assert!(Layer::decode(b).is_err());
        // A huge feature count must not allocate.
        assert!(Layer::decode(vec![0xff, 0xff, 0xff, 0xff, 0x0f, 0]).is_err());
    }

    #[test]
    fn dictionaries_round_trip() {
        let mut d = Dictionaries::default();
        d.0.insert((0, 3), vec![1, 2, 3]);
        d.0.insert((4, 1), vec![9; 10]);
        d.0.insert((5, 0), Vec::new());
        let back = Dictionaries::decode(&d.encode()).unwrap();
        d.0.remove(&(5, 0));
        assert_eq!(back, d);
    }

    #[test]
    fn index_blocks() {
        let mut b = IndexBuilder::new();
        let mut off = 1000;
        let mut tiles = Vec::new();
        for i in 0..600u64 {
            // Padding before tile 400, as at a chunk boundary.
            if i == 400 {
                off += 77;
            }
            let layers = vec![(0, 10 + i as u32 % 7), (5, 3)];
            b.push(14, i * 3, off, &layers).unwrap();
            tiles.push((i * 3, off, layers.clone()));
            off += layers.iter().map(|l| u64::from(l.1)).sum::<u64>();
        }
        b.push(2, 0, off, &[(1, 1)]).unwrap_err();
        let bytes = b.finish().unwrap();
        let zd = decode_tidx_header(&bytes).unwrap();
        assert_eq!(zd[13].blocks, 0);
        let z = zd[14];
        // 256 + 144 before the padding, then 200.
        assert_eq!(z.blocks, 3);
        let dir_bytes = &bytes[z.dir_offset as usize..z.dir_offset as usize + z.dir_len()];
        let dir = decode_directory(dir_bytes).unwrap();
        assert_eq!(
            dir.iter().map(|d| d.tiles).collect::<Vec<_>>(),
            [256, 144, 200]
        );
        let blocks: Vec<Block> = dir
            .iter()
            .map(|d| Block::decode(&bytes[d.block_offset as usize..], d).unwrap())
            .collect();
        for (key, offset, layers) in tiles {
            let i = dir.partition_point(|d| d.first_key <= key) - 1;
            let loc = blocks[i].find(key).unwrap();
            assert_eq!(loc, TileLoc { offset, layers });
        }
        assert!(blocks[0].find(1).is_none());
    }
}
