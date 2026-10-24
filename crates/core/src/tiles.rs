//! Tiles in a pack: [`TileWriter`] writes `TDIC`, `TDAT` and `TIDX`; [`TileReader`] looks
//! tiles up and decodes them. See `docs/format.md`.
//!
//! The reader uses positioned reads through [`Pack`], never `mmap`, so pack bytes stay in the
//! OS page cache and out of the app's RSS. `TIDX` is read one zoom directory and one block at
//! a time, and the blocks are cached.

use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use zstd::bulk::Compressor;
use zstd::zstd_safe::{DCtx, DDict};

use crate::pack::{Pack, Section, Writer};
use crate::tile::{
    self, Block, Dictionaries, DirEntry, IndexBuilder, Layer, MAX_ZOOM, TIDX_HEADER_LEN, TileLoc,
    ZOOMS, ZoomDir, zoom_band,
};
use crate::varint::invalid;
use crate::zframe;

pub const TDIC: [u8; 4] = *b"TDIC";
pub const TDAT: [u8; 4] = *b"TDAT";
pub const TIDX: [u8; 4] = *b"TIDX";

/// Level for tile frames (format.md).
pub const LEVEL: i32 = 19;
/// `TIDX` blocks kept decoded, about 7 KiB each.
const BLOCK_CACHE: usize = 512;

/// Compresses layer payloads with the pack's dictionary for their (layer, band). One per
/// thread; the frames it makes go to [`TileWriter::add`].
pub struct FrameCompressor {
    plain: Compressor<'static>,
    with_dict: HashMap<(u8, u8), Compressor<'static>>,
}

impl FrameCompressor {
    pub fn new(dicts: &Dictionaries, level: i32) -> io::Result<Self> {
        let mut with_dict = HashMap::new();
        for (&k, d) in dicts.0.iter().filter(|(_, d)| !d.is_empty()) {
            with_dict.insert(k, zframe::compressor(level, d)?);
        }
        Ok(Self {
            plain: zframe::compressor(level, &[])?,
            with_dict,
        })
    }

    pub fn compress(&mut self, layer: u8, z: u8, payload: &[u8]) -> io::Result<Vec<u8>> {
        self.with_dict
            .get_mut(&(layer, zoom_band(z)))
            .unwrap_or(&mut self.plain)
            .compress(payload)
    }
}

/// Writes the tile sections into a pack: `TDIC` on [`TileWriter::begin`], then one `TDAT`
/// record per tile, then `TIDX` on [`TileWriter::finish`].
pub struct TileWriter<'w> {
    w: &'w mut Writer,
    index: IndexBuilder,
    record: Vec<u8>,
}

impl<'w> TileWriter<'w> {
    pub fn begin(w: &'w mut Writer, dicts: &Dictionaries) -> io::Result<Self> {
        w.section(TDIC, &dicts.encode())?;
        w.begin_spanning(TDAT);
        Ok(Self {
            w,
            index: IndexBuilder::new(),
            record: Vec::new(),
        })
    }

    /// Adds a tile's compressed layer frames, in layer order. Tiles must come in zoom, then
    /// Hilbert order. A tile without frames is not stored.
    pub fn add(&mut self, z: u8, x: u32, y: u32, frames: &[(u8, &[u8])]) -> io::Result<()> {
        if frames.is_empty() {
            return Ok(());
        }
        if z > MAX_ZOOM || x >> z != 0 || y >> z != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("no tile {z}/{x}/{y}"),
            ));
        }
        let key = tile::hilbert(z, x, y);
        let lens: Vec<(u8, u32)> = frames.iter().map(|&(l, f)| (l, f.len() as u32)).collect();
        self.index.check(z, key, &lens)?;
        self.record.clear();
        for (_, f) in frames {
            self.record.extend_from_slice(f);
        }
        let offset = self.w.record(&self.record)?;
        self.index.push(z, key, offset, &lens)
    }

    pub fn finish(self) -> io::Result<(Section, Section)> {
        let tdat = self.w.end_spanning();
        let tidx = self.w.section(TIDX, &self.index.finish()?)?;
        Ok((tdat, tidx))
    }
}

/// A decoded tile: its layers in layer-ID order, in tile-local coordinates.
#[derive(Debug)]
pub struct Tile {
    pub layers: Vec<(u8, Layer)>,
    /// Compressed bytes read from `TDAT`.
    pub bytes: u64,
}

impl Tile {
    pub fn layer(&self, id: u8) -> Option<&Layer> {
        self.layers.iter().find(|(l, _)| *l == id).map(|(_, l)| l)
    }

    /// Heap bytes held by the decoded layers.
    pub fn heap_bytes(&self) -> usize {
        self.layers.capacity() * std::mem::size_of::<(u8, Layer)>()
            + self
                .layers
                .iter()
                .map(|(_, l)| l.heap_bytes())
                .sum::<usize>()
    }
}

/// Where the time of one [`TileReader::tile_timed`] went.
#[derive(Clone, Copy, Debug, Default)]
pub struct TileTiming {
    /// Finding the tile in `TIDX` (a block read on a cache miss) and reading its record.
    pub read: Duration,
    /// Decompressing and decoding the layers.
    pub decode: Duration,
}

#[derive(Default)]
struct BlockCache {
    map: HashMap<(u8, u32), (std::sync::Arc<Block>, u64)>,
    tick: u64,
}

/// Looks up and decodes tiles. `Sync`: the app's loader threads share one.
pub struct TileReader {
    pack: Pack,
    tdat: Section,
    tidx: Section,
    zooms: [ZoomDir; ZOOMS],
    /// End of each zoom's directory and blocks in `TIDX`.
    zoom_ends: [u32; ZOOMS],
    dirs: [OnceLock<Vec<DirEntry>>; ZOOMS],
    blocks: Mutex<BlockCache>,
    dicts: HashMap<(u8, u8), DDict<'static>>,
    dctx: Mutex<Vec<DCtx<'static>>>,
    bytes_read: AtomicU64,
}

impl TileReader {
    /// Reads `TDIC` and the `TIDX` header. Zoom directories load on first use.
    pub fn open(pack: Pack) -> io::Result<Self> {
        let need = |id: [u8; 4]| {
            pack.section(id).copied().ok_or_else(|| {
                invalid(format!(
                    "pack has no {} section",
                    String::from_utf8_lossy(&id)
                ))
            })
        };
        let (tdic, tdat, tidx) = (need(TDIC)?, need(TDAT)?, need(TIDX)?);
        let mut dicts = HashMap::new();
        for (k, d) in Dictionaries::decode(&pack.read_section(&tdic)?)?.0 {
            let dd = DDict::try_create(&d).ok_or_else(|| invalid("bad zstd dictionary"))?;
            dicts.insert(k, dd);
        }
        if tidx.spanning() || (tidx.len as usize) < TIDX_HEADER_LEN || tidx.len > u32::MAX.into() {
            return Err(invalid("bad TIDX section"));
        }
        let zooms = tile::decode_tidx_header(&pack.read(tidx.offset, TIDX_HEADER_LEN)?)?;
        let mut zoom_ends = [tidx.len as u32; ZOOMS];
        for z in 0..ZOOMS {
            if z + 1 < ZOOMS {
                zoom_ends[z] = zooms[z + 1].dir_offset;
            }
            let d = zooms[z];
            let ok = d.dir_offset as usize >= TIDX_HEADER_LEN
                && d.dir_offset as u64 + d.dir_len() as u64 <= u64::from(zoom_ends[z])
                && zoom_ends[z] as u64 <= tidx.len;
            if !ok {
                return Err(invalid(format!("TIDX zoom {z} is out of bounds")));
            }
        }
        Ok(Self {
            bytes_read: AtomicU64::new(TIDX_HEADER_LEN as u64 + tdic.len),
            pack,
            tdat,
            tidx,
            zooms,
            zoom_ends,
            dirs: Default::default(),
            blocks: Mutex::new(BlockCache::default()),
            dicts,
            dctx: Mutex::new(Vec::new()),
        })
    }

    pub fn pack(&self) -> &Pack {
        &self.pack
    }

    /// Bytes read from the pack so far: tiles, index and dictionaries.
    pub fn bytes_read(&self) -> u64 {
        self.bytes_read.load(Ordering::Relaxed)
    }

    fn read(&self, tidx_offset: u32, len: usize) -> io::Result<Vec<u8>> {
        self.bytes_read.fetch_add(len as u64, Ordering::Relaxed);
        self.pack
            .read(self.tidx.offset + u64::from(tidx_offset), len)
    }

    fn dir(&self, z: u8) -> io::Result<&[DirEntry]> {
        let cell = &self.dirs[z as usize];
        if let Some(d) = cell.get() {
            return Ok(d);
        }
        let zd = self.zooms[z as usize];
        let dir = tile::decode_directory(&self.read(zd.dir_offset, zd.dir_len())?)?;
        // Each block starts after the directory and before the next block.
        let blocks_start = zd.dir_offset + zd.dir_len() as u32;
        let ends = dir
            .iter()
            .skip(1)
            .map(|d| d.block_offset)
            .chain([self.zoom_ends[z as usize]]);
        let in_bounds = dir
            .iter()
            .zip(ends)
            .all(|(d, end)| d.block_offset >= blocks_start && d.block_offset < end);
        if !in_bounds {
            return Err(invalid(format!("TIDX zoom {z} blocks are out of bounds")));
        }
        Ok(cell.get_or_init(|| dir))
    }

    /// Tiles stored at zoom `z`, from its directory alone.
    pub fn tile_count(&self, z: u8) -> io::Result<u64> {
        if z > MAX_ZOOM {
            return Ok(0);
        }
        Ok(self.dir(z)?.iter().map(|d| u64::from(d.tiles)).sum())
    }

    /// Calls `f(key, loc)` for every tile at zoom `z`, in Hilbert order. Reads each block
    /// once and does not fill the block cache.
    pub fn for_each_tile(&self, z: u8, mut f: impl FnMut(u64, &TileLoc)) -> io::Result<()> {
        if z > MAX_ZOOM {
            return Ok(());
        }
        let dir = self.dir(z)?;
        for i in 0..dir.len() {
            for (key, loc) in self.read_block(z, i, dir)?.iter() {
                f(key, &loc);
            }
        }
        Ok(())
    }

    fn read_block(&self, z: u8, i: usize, dir: &[DirEntry]) -> io::Result<Block> {
        let d = &dir[i];
        let end = dir
            .get(i + 1)
            .map_or(self.zoom_ends[z as usize], |n| n.block_offset);
        let bytes = self.read(d.block_offset, (end - d.block_offset) as usize)?;
        Block::decode(&bytes, d)
    }

    fn block(&self, z: u8, i: usize, dir: &[DirEntry]) -> io::Result<std::sync::Arc<Block>> {
        let key = (z, i as u32);
        {
            let mut c = self.blocks.lock().unwrap();
            c.tick += 1;
            let tick = c.tick;
            if let Some((b, t)) = c.map.get_mut(&key) {
                *t = tick;
                return Ok(b.clone());
            }
        }
        let block = std::sync::Arc::new(self.read_block(z, i, dir)?);
        let mut c = self.blocks.lock().unwrap();
        if c.map.len() >= BLOCK_CACHE
            && let Some(old) = c.map.iter().min_by_key(|(_, (_, t))| *t).map(|(k, _)| *k)
        {
            c.map.remove(&old);
        }
        let tick = c.tick;
        c.map.insert(key, (block.clone(), tick));
        Ok(block)
    }

    /// Where tile `z/x/y` is in `TDAT`, or `None` if the pack has no such tile.
    pub fn locate(&self, z: u8, x: u32, y: u32) -> io::Result<Option<TileLoc>> {
        if z > MAX_ZOOM || x >> z != 0 || y >> z != 0 {
            return Ok(None);
        }
        let key = tile::hilbert(z, x, y);
        let dir = self.dir(z)?;
        let i = dir.partition_point(|d| d.first_key <= key);
        if i == 0 {
            return Ok(None);
        }
        Ok(self.block(z, i - 1, dir)?.find(key))
    }

    /// Reads and decodes tile `z/x/y`: one positioned read, then one zstd frame per layer.
    pub fn tile(&self, z: u8, x: u32, y: u32) -> io::Result<Option<Tile>> {
        self.tile_timed(z, x, y).map(|(t, _)| t)
    }

    /// [`Self::tile`], also saying how long the read and the decode took.
    pub fn tile_timed(&self, z: u8, x: u32, y: u32) -> io::Result<(Option<Tile>, TileTiming)> {
        let t0 = Instant::now();
        let mut timing = TileTiming::default();
        let Some(loc) = self.locate(z, x, y)? else {
            timing.read = t0.elapsed();
            return Ok((None, timing));
        };
        let len = loc.len();
        let end = self.tdat.offset + self.tdat.len;
        if loc.offset < self.tdat.offset || loc.offset + len > end {
            return Err(invalid(format!("tile {z}/{x}/{y} is outside TDAT")));
        }
        let bytes = self.pack.read(loc.offset, len as usize)?;
        self.bytes_read.fetch_add(len, Ordering::Relaxed);
        let t1 = Instant::now();
        timing.read = t1 - t0;
        let mut dctx = self.dctx.lock().unwrap().pop().unwrap_or_else(DCtx::create);
        let mut layers = Vec::with_capacity(loc.layers.len());
        let mut at = 0;
        let res = (|| {
            for &(id, n) in &loc.layers {
                let frame = &bytes[at..at + n as usize];
                at += n as usize;
                let dict = self.dicts.get(&(id, zoom_band(z)));
                let raw = zframe::decompress(&mut dctx, frame, dict)?;
                layers.push((id, Layer::decode(raw)?));
            }
            Ok::<_, io::Error>(())
        })();
        self.dctx.lock().unwrap().push(dctx);
        res.map_err(|e| invalid(format!("tile {z}/{x}/{y}: {e}")))?;
        timing.decode = t1.elapsed();
        Ok((Some(Tile { layers, bytes: len }), timing))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack::{self, Info, PAGE};
    use crate::tile::{Enc, Geom, encode_layer};

    fn info() -> Info {
        Info {
            pack_id: [3; 16],
            bbox: [0, 0, 10_000_000, 10_000_000],
            build_time: 0,
            name: "Tiles".into(),
            snapshot: "test".into(),
        }
    }

    /// Deterministic features for a tile: every geometry type, props, an empty layer, and
    /// some layers left out.
    fn layers_for(z: u8, x: u32, y: u32) -> Vec<(u8, Vec<u8>)> {
        let seed = (u32::from(z) * 7919 + x * 31 + y * 17) as i32;
        let names: Vec<(u8, Box<str>)> = vec![
            (0, format!("Road {seed}").into()),
            (1, (seed % 500).abs().to_string().into()),
        ];
        let line: Vec<[i32; 2]> = (0..20 + seed % 40)
            .map(|i| [i * 97 % 4224 - 64, (i * i + seed) % 4224 - 64])
            .collect();
        let ring = vec![[0, 0], [4000, 0], [4000, 4000], [0, 4000]];
        let hole = vec![[100, 100], [100, 200], [200, 200], [200, 100]];
        let mut out = vec![(
            0,
            encode_layer(&[
                Enc {
                    class: (seed % 5) as u16,
                    props: &names,
                    geom: Geom::Lines(vec![line.clone(), line[..3].to_vec()]),
                },
                Enc {
                    class: 1,
                    props: &names[1..],
                    geom: Geom::Lines(vec![line[2..].to_vec()]),
                },
            ]),
        )];
        if seed % 3 != 0 {
            out.push((
                3,
                encode_layer(&[Enc {
                    class: 2,
                    props: &[],
                    geom: Geom::Polygon(vec![ring, hole]),
                }]),
            ));
        }
        if seed % 4 == 1 {
            out.push((5, encode_layer(&[])));
        }
        out.push((
            8,
            encode_layer(&[Enc {
                class: 0,
                props: &names[..1],
                geom: Geom::Point([seed % 4096, -64]),
            }]),
        ));
        out
    }

    fn tiles() -> Vec<(u8, u32, u32)> {
        let mut t = vec![(0, 0, 0), (1, 1, 0), (6, 17, 20)];
        // Enough z14 tiles for several TIDX blocks.
        t.extend((0..30).flat_map(|x| (0..20).map(move |y| (14, 4000 + x, 6000 + y * 2))));
        t.sort_by_key(|&(z, x, y)| (z, tile::hilbert(z, x, y)));
        t
    }

    #[test]
    fn round_trips_tiles_across_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let mut dicts = Dictionaries::default();
        // A raw-content dictionary is enough to check that the right one is used.
        dicts.0.insert((0, 3), layers_for(14, 1, 1).remove(0).1);
        let cap = 4 * PAGE;
        let mut w = Writer::create(dir.path(), "t", cap).unwrap();
        let mut fc = FrameCompressor::new(&dicts, 3).unwrap();
        let mut tw = TileWriter::begin(&mut w, &dicts).unwrap();
        for &(z, x, y) in &tiles() {
            let frames: Vec<(u8, Vec<u8>)> = layers_for(z, x, y)
                .into_iter()
                .map(|(l, p)| (l, fc.compress(l, z, &p).unwrap()))
                .collect();
            let refs: Vec<(u8, &[u8])> = frames.iter().map(|(l, f)| (*l, &f[..])).collect();
            tw.add(z, x, y, &refs).unwrap();
        }
        // Out of order, and empty.
        assert!(tw.add(14, 0, 0, &[(0, &[1][..])]).is_err());
        tw.add(14, 4999, 0, &[]).unwrap();
        let (tdat, _) = tw.finish().unwrap();
        let paths = w.finish(&info()).unwrap();
        assert!(
            tdat.spanning() && tdat.len > 3 * cap,
            "TDAT {} bytes",
            tdat.len
        );
        assert!(paths.len() >= 4);

        let r = TileReader::open(Pack::open(&paths[0]).unwrap()).unwrap();
        assert!(r.pack().verify().unwrap().is_empty());
        // 600 z14 tiles fill 3 blocks; padding at chunk boundaries starts more.
        assert!(r.dir(14).unwrap().len() > 3);
        for _pass in 0..2 {
            for &(z, x, y) in &tiles() {
                let t = r.tile(z, x, y).unwrap().unwrap();
                let want = layers_for(z, x, y);
                assert_eq!(t.layers.len(), want.len());
                for ((id, got), (wid, payload)) in t.layers.iter().zip(&want) {
                    assert_eq!(id, wid);
                    let want = Layer::decode(payload.clone()).unwrap();
                    assert_eq!(got.features.len(), want.features.len());
                    for (a, b) in got.features.iter().zip(&want.features) {
                        assert_eq!((a.class, got.geom(a)), (b.class, want.geom(b)));
                        for k in 0..2 {
                            assert_eq!(got.prop(a, k), want.prop(b, k));
                        }
                    }
                }
                let loc = r.locate(z, x, y).unwrap().unwrap();
                assert!(
                    loc.offset % cap + loc.len() <= cap,
                    "record crosses a chunk"
                );
            }
        }
        for (z, x, y) in [
            (14, 4999, 0),
            (14, 4001, 6001),
            (2, 0, 0),
            (15, 0, 0),
            (1, 2, 0),
        ] {
            assert!(r.tile(z, x, y).unwrap().is_none(), "{z}/{x}/{y}");
        }
        assert!(r.bytes_read() > tdat.len);

        // Walking the index visits every tile once, in order, with the same locations.
        let mut seen = Vec::new();
        for z in 0..=MAX_ZOOM {
            let before = seen.len();
            r.for_each_tile(z, |key, loc| {
                let (x, y) = tile::hilbert_xy(z, key);
                assert_eq!(r.locate(z, x, y).unwrap().as_ref(), Some(loc));
                seen.push((z, x, y));
            })
            .unwrap();
            assert_eq!(r.tile_count(z).unwrap(), (seen.len() - before) as u64);
        }
        assert_eq!(seen, tiles());
    }

    #[test]
    fn reader_is_shareable() {
        fn sync<T: Send + Sync>() {}
        sync::<TileReader>();
    }

    #[cfg(unix)]
    #[test]
    fn damaged_tile_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let dicts = Dictionaries::default();
        let mut w = Writer::create(dir.path(), "t", pack::DEFAULT_CHUNK_CAPACITY).unwrap();
        let mut fc = FrameCompressor::new(&dicts, 3).unwrap();
        let mut tw = TileWriter::begin(&mut w, &dicts).unwrap();
        let (l, p) = layers_for(0, 0, 0).remove(0);
        tw.add(0, 0, 0, &[(l, &fc.compress(l, 0, &p).unwrap())])
            .unwrap();
        let (tdat, _) = tw.finish().unwrap();
        let paths = w.finish(&info()).unwrap();
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(&paths[0])
            .unwrap();
        std::os::unix::fs::FileExt::write_all_at(&f, &[0x55; 8], PAGE + tdat.offset + 20).unwrap();
        let r = TileReader::open(Pack::open(&paths[0]).unwrap()).unwrap();
        assert!(r.tile(0, 0, 0).is_err());
    }
}
