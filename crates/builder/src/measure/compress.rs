//! V3 (zstd per section), V4 (zstd with trained dictionaries), V5 (4×4 tile groups per zstd
//! frame), the tile index overhead, and the random-read cost of each.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result};
use rayon::prelude::*;
use serde::Serialize;
use zstd::bulk::{Compressor, Decompressor};
use zstd::zstd_safe::CParameter;

use super::encode::{self, put_varint, varint_len};
use super::tiles::{Cell, Entry, Variant};

const DICT_SIZE: usize = 64 << 10;
/// Training input per dictionary is capped at this many bytes, sampled evenly.
const TRAIN_BYTES: usize = 16 << 20;
const VIEWPORTS: usize = 100;
/// A 1920×1080 screen of 512 px tiles needs at most 5×4 tiles.
const VIEWPORT_TILES: (u32, u32) = (5, 4);
const BENCH_ZOOMS: [u8; 3] = [10, 12, 14];

/// Zoom bands that share a dictionary.
fn band(z: u8) -> u8 {
    match z {
        0..=7 => 0,
        8..=11 => 1,
        12..=13 => 2,
        _ => 3,
    }
}

const BAND_NAMES: [&str; 4] = ["z0-7", "z8-11", "z12-13", "z14"];

#[derive(Serialize)]
pub struct DictInfo {
    pub layer: String,
    pub band: &'static str,
    pub sections: u64,
    pub section_bytes: u64,
    /// 0 when the group was too small to train on (sections are compressed without one).
    pub dict_bytes: u64,
}

#[derive(Serialize)]
pub struct IndexStats {
    pub zoom: u8,
    pub tiles: u64,
    /// Tiles in the bounding box of the non-empty tiles.
    pub bbox_tiles: u64,
    /// Sorted by Hilbert index: varint index delta, layer bitmask, varint length per layer.
    pub sparse_bytes: u64,
    /// One u64 offset per bbox tile.
    pub dense_bytes: u64,
}

#[derive(Serialize)]
pub struct ReadBench {
    pub variant: Variant,
    pub zoom: u8,
    pub viewports: u64,
    pub mean_tiles: f64,
    pub mean_bytes: f64,
    pub p95_bytes: u64,
    pub mean_decode_ms: f64,
    pub p95_decode_ms: f64,
    pub decode_us_per_tile: f64,
}

pub struct Output {
    pub cells: BTreeMap<(Variant, u8, u8), Cell>,
    pub dictionaries: Vec<DictInfo>,
    pub index: Vec<IndexStats>,
    pub random_read: Vec<ReadBench>,
}

fn read(file: &File, e: &Entry) -> Vec<u8> {
    let mut buf = vec![0; e.len as usize];
    read_at(file, &mut buf, e.offset).expect("read tile store");
    buf
}

#[cfg(unix)]
fn read_at(file: &File, buf: &mut [u8], offset: u64) -> std::io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(file, buf, offset)
}

#[cfg(windows)]
fn read_at(file: &File, mut buf: &mut [u8], mut offset: u64) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        let n = file.seek_read(buf, offset)?;
        if n == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        buf = &mut buf[n..];
        offset += n as u64;
    }
    Ok(())
}

fn compressor(level: i32, dict: &[u8]) -> Compressor<'static> {
    let mut c = Compressor::with_dictionary(level, dict).expect("zstd compressor");
    c.set_parameter(CParameter::DictIdFlag(false))
        .expect("zstd param");
    c.set_parameter(CParameter::ChecksumFlag(false))
        .expect("zstd param");
    c
}

/// Simple deterministic generator so viewport samples repeat across runs.
struct XorShift(u64);

impl XorShift {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

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

type Tile = (u8, u32, u32);
type GroupKey = (u8, u8, u32, u32);
/// Benchmark tiles kept after compression: `(layer, tile)` → (raw length, V3 frame, V4 frame).
type Kept = HashMap<(u8, Tile), (u32, Vec<u8>, Vec<u8>)>;
/// Per section: entry index, V3 bytes, V4 bytes, frames if the tile is benchmarked.
type SectionResult = (usize, u64, u64, Option<(Vec<u8>, Vec<u8>)>);
/// Per V5 group: key, bytes, (raw length, frame) if the group is benchmarked.
type GroupResult = (GroupKey, u64, Option<(u32, Vec<u8>)>);
/// Zoom → tile → (layer, V4 bytes).
type TileLayers = BTreeMap<u8, BTreeMap<(u32, u32), Vec<(u8, u64)>>>;

fn group_of(layer: u8, t: Tile) -> GroupKey {
    (layer, t.0, t.1 >> 2, t.2 >> 2)
}

pub fn run(store: &Path, entries: &[Entry], layer_names: &[String], level: i32) -> Result<Output> {
    let file = File::open(store).with_context(|| format!("open {}", store.display()))?;

    // Dictionaries per (layer, band).
    let mut groups: BTreeMap<(u8, u8), Vec<&Entry>> = BTreeMap::new();
    for e in entries {
        groups.entry((e.layer, band(e.z))).or_default().push(e);
    }
    let trained: Vec<((u8, u8), Vec<u8>, DictInfo)> = groups
        .par_iter()
        .map(|(&(layer, b), es)| {
            let total: u64 = es.iter().map(|e| u64::from(e.len)).sum();
            let step = (total as usize / TRAIN_BYTES).max(1);
            let samples: Vec<Vec<u8>> = es.iter().step_by(step).map(|e| read(&file, e)).collect();
            let dict = if total >= 128 << 10 && samples.len() >= 64 {
                zstd::dict::from_samples(&samples, DICT_SIZE).unwrap_or_default()
            } else {
                Vec::new()
            };
            let info = DictInfo {
                layer: layer_names[layer as usize].clone(),
                band: BAND_NAMES[b as usize],
                sections: es.len() as u64,
                section_bytes: total,
                dict_bytes: dict.len() as u64,
            };
            ((layer, b), dict, info)
        })
        .collect();
    let mut dicts: HashMap<(u8, u8), Vec<u8>> = HashMap::new();
    let mut dictionaries = Vec::new();
    for (k, d, info) in trained {
        dicts.insert(k, d);
        dictionaries.push(info);
    }

    // Viewports to benchmark, chosen before compressing so their bytes can be kept.
    let mut nonempty: BTreeMap<u8, Vec<(u32, u32)>> = BTreeMap::new();
    for e in entries {
        nonempty.entry(e.z).or_default().push((e.x, e.y));
    }
    for v in nonempty.values_mut() {
        v.sort_unstable();
        v.dedup();
    }
    let mut rng = XorShift(0x9e37_79b9_7f4a_7c15);
    let mut viewports: Vec<(u8, Vec<Tile>)> = Vec::new();
    for z in BENCH_ZOOMS {
        let Some(tiles) = nonempty.get(&z) else {
            continue;
        };
        for _ in 0..VIEWPORTS {
            let (cx, cy) = tiles[(rng.next() % tiles.len() as u64) as usize];
            let (w, h) = VIEWPORT_TILES;
            let x0 = cx.saturating_sub(w / 2);
            let y0 = cy.saturating_sub(h / 2);
            let vp = (x0..x0 + w)
                .flat_map(|x| (y0..y0 + h).map(move |y| (z, x, y)))
                .collect();
            viewports.push((z, vp));
        }
    }
    let sample: HashSet<Tile> = viewports
        .iter()
        .flat_map(|(_, v)| v.iter().copied())
        .collect();
    let sample_groups: HashSet<(u8, u32, u32)> =
        sample.iter().map(|t| (t.0, t.1 >> 2, t.2 >> 2)).collect();

    // V3 and V4, per section.
    let mut cells: BTreeMap<(Variant, u8, u8), Cell> = BTreeMap::new();
    let per_section: Vec<SectionResult> = entries
        .par_iter()
        .enumerate()
        .map_init(
            || {
                (
                    compressor(level, &[]),
                    HashMap::<(u8, u8), Compressor<'static>>::new(),
                )
            },
            |(plain, with_dict), (i, e)| {
                let raw = read(&file, e);
                let v3 = plain.compress(&raw).expect("zstd");
                let key = (e.layer, band(e.z));
                let c = with_dict
                    .entry(key)
                    .or_insert_with(|| compressor(level, &dicts[&key]));
                let v4 = c.compress(&raw).expect("zstd");
                let (n3, n4) = (v3.len() as u64, v4.len() as u64);
                let keep = sample.contains(&(e.z, e.x, e.y)).then_some((v3, v4));
                (i, n3, n4, keep)
            },
        )
        .collect();
    let mut v4_len: Vec<u64> = vec![0; entries.len()];
    let mut kept: Kept = HashMap::new();
    for (i, n3, n4, keep) in per_section {
        let e = &entries[i];
        for (v, n) in [(Variant::V3, n3), (Variant::V4, n4)] {
            cells.entry((v, e.layer, e.z)).or_default().add(&Cell {
                tiles: 1,
                features: 0,
                vertices: 0,
                bytes: n,
            });
        }
        v4_len[i] = n4;
        if let Some((a, b)) = keep {
            kept.insert((e.layer, (e.z, e.x, e.y)), (e.len, a, b));
        }
    }

    // V5: 4×4 tile groups per layer and zoom, one frame each, with the V4 dictionary.
    let mut by_group: BTreeMap<GroupKey, Vec<&Entry>> = BTreeMap::new();
    for e in entries {
        by_group
            .entry(group_of(e.layer, (e.z, e.x, e.y)))
            .or_default()
            .push(e);
    }
    let v5: Vec<GroupResult> = by_group
        .par_iter()
        .map_init(
            HashMap::<(u8, u8), Compressor<'static>>::new,
            |cs, (&key, es)| {
                let mut payload = Vec::new();
                put_varint(&mut payload, es.len() as u64);
                for e in es {
                    put_varint(&mut payload, u64::from((e.x & 3) + 4 * (e.y & 3)));
                    put_varint(&mut payload, u64::from(e.len));
                }
                for e in es {
                    payload.extend_from_slice(&read(&file, e));
                }
                let dk = (key.0, band(key.1));
                let c = cs
                    .entry(dk)
                    .or_insert_with(|| compressor(level, &dicts[&dk]));
                let out = c.compress(&payload).expect("zstd");
                let n = out.len() as u64;
                let keep = sample_groups
                    .contains(&(key.1, key.2, key.3))
                    .then_some((payload.len() as u32, out));
                (key, n, keep)
            },
        )
        .collect();
    let mut kept_groups: HashMap<GroupKey, (u32, Vec<u8>)> = HashMap::new();
    for (key, n, keep) in v5 {
        cells
            .entry((Variant::V5, key.0, key.1))
            .or_default()
            .add(&Cell {
                tiles: by_group[&key].len() as u64,
                features: 0,
                vertices: 0,
                bytes: n,
            });
        if let Some(k) = keep {
            kept_groups.insert(key, k);
        }
    }

    let index = index_stats(entries, &v4_len);
    let random_read = bench(&viewports, &kept, &kept_groups, &dicts)?;
    Ok(Output {
        cells,
        dictionaries,
        index,
        random_read,
    })
}

fn index_stats(entries: &[Entry], v4_len: &[u64]) -> Vec<IndexStats> {
    let mut per_zoom: TileLayers = BTreeMap::new();
    for (e, &n) in entries.iter().zip(v4_len) {
        per_zoom
            .entry(e.z)
            .or_default()
            .entry((e.x, e.y))
            .or_default()
            .push((e.layer, n));
    }
    per_zoom
        .into_iter()
        .map(|(z, tiles)| {
            let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
            let mut keyed: Vec<(u64, &Vec<(u8, u64)>)> = tiles
                .iter()
                .map(|(&(x, y), layers)| {
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x);
                    y1 = y1.max(y);
                    (hilbert(z, x, y), layers)
                })
                .collect();
            keyed.sort_unstable_by_key(|k| k.0);
            let mut prev = 0;
            let mut sparse = 0;
            for (d, layers) in keyed {
                sparse += varint_len(d - prev) + 2;
                sparse += layers.iter().map(|(_, n)| varint_len(*n)).sum::<u64>();
                prev = d;
            }
            let bbox_tiles = u64::from(x1 - x0 + 1) * u64::from(y1 - y0 + 1);
            IndexStats {
                zoom: z,
                tiles: tiles.len() as u64,
                bbox_tiles,
                sparse_bytes: sparse,
                dense_bytes: bbox_tiles * 8,
            }
        })
        .collect()
}

fn percentile(v: &mut [f64], p: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(f64::total_cmp);
    v[((v.len() - 1) as f64 * p).round() as usize]
}

fn bench(
    viewports: &[(u8, Vec<Tile>)],
    kept: &Kept,
    groups: &HashMap<GroupKey, (u32, Vec<u8>)>,
    dicts: &HashMap<(u8, u8), Vec<u8>>,
) -> Result<Vec<ReadBench>> {
    let layers: Vec<u8> = {
        let mut l: Vec<u8> = kept.keys().map(|k| k.0).collect();
        l.sort_unstable();
        l.dedup();
        l
    };
    let mut plain = Decompressor::new()?;
    let mut with_dict: HashMap<(u8, u8), Decompressor<'static>> = HashMap::new();
    for (k, d) in dicts {
        with_dict.insert(*k, Decompressor::with_dictionary(d)?);
    }
    let mut out = Vec::new();
    for variant in [Variant::V3, Variant::V4, Variant::V5] {
        for z in BENCH_ZOOMS {
            let (mut bytes, mut ms, mut tiles_n) = (Vec::new(), Vec::new(), 0u64);
            for (_, vp) in viewports.iter().filter(|(vz, _)| *vz == z) {
                let start = Instant::now();
                let mut read = 0u64;
                let mut seen_groups = HashSet::new();
                let mut tiles_with_data = HashSet::new();
                for &t in vp {
                    for &l in &layers {
                        match variant {
                            Variant::V5 => {
                                let key = group_of(l, t);
                                let Some((raw_len, frame)) = groups.get(&key) else {
                                    continue;
                                };
                                if !seen_groups.insert(key) {
                                    continue;
                                }
                                read += frame.len() as u64;
                                let d = with_dict.get_mut(&(l, band(z))).unwrap_or(&mut plain);
                                let payload = d.decompress(frame, *raw_len as usize)?;
                                for sec in group_sections(&payload, vp, key)? {
                                    tiles_with_data.insert(sec.0);
                                    encode::decode_v1(sec.1)?;
                                }
                            }
                            _ => {
                                let Some((raw_len, v3, v4)) = kept.get(&(l, t)) else {
                                    continue;
                                };
                                tiles_with_data.insert(t);
                                let (frame, d) = if variant == Variant::V3 {
                                    (v3, &mut plain)
                                } else {
                                    (v4, with_dict.get_mut(&(l, band(z))).unwrap_or(&mut plain))
                                };
                                read += frame.len() as u64;
                                let raw = d.decompress(frame, *raw_len as usize)?;
                                encode::decode_v1(&raw)?;
                            }
                        }
                    }
                }
                ms.push(start.elapsed().as_secs_f64() * 1e3);
                bytes.push(read as f64);
                tiles_n += tiles_with_data.len() as u64;
            }
            let n = ms.len();
            if n == 0 {
                continue;
            }
            let total_ms: f64 = ms.iter().sum();
            out.push(ReadBench {
                variant,
                zoom: z,
                viewports: n as u64,
                mean_tiles: tiles_n as f64 / n as f64,
                mean_bytes: bytes.iter().sum::<f64>() / n as f64,
                p95_bytes: percentile(&mut bytes, 0.95) as u64,
                mean_decode_ms: total_ms / n as f64,
                p95_decode_ms: percentile(&mut ms, 0.95),
                decode_us_per_tile: total_ms * 1e3 / tiles_n.max(1) as f64,
            });
        }
    }
    Ok(out)
}

/// The sections in a V5 group payload that belong to tiles in `vp`.
fn group_sections<'a>(
    payload: &'a [u8],
    vp: &[Tile],
    key: GroupKey,
) -> Result<Vec<(Tile, &'a [u8])>> {
    let mut i = 0;
    let mut varint = || -> Result<u64> {
        let mut v = 0;
        for shift in (0..64).step_by(7) {
            let b = *payload.get(i).context("truncated group header")?;
            i += 1;
            v |= u64::from(b & 0x7f) << shift;
            if b < 0x80 {
                return Ok(v);
            }
        }
        anyhow::bail!("bad varint")
    };
    let n = varint()?;
    let mut heads = Vec::new();
    for _ in 0..n {
        heads.push((varint()? as u32, varint()? as usize));
    }
    let mut pos = i;
    let mut out = Vec::new();
    for (local, len) in heads {
        let t = (key.1, key.2 * 4 + local % 4, key.3 * 4 + local / 4);
        if vp.contains(&t) {
            out.push((t, &payload[pos..pos + len]));
        }
        pos += len;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hilbert_order_2() {
        // The order-2 curve visits (0,0) (0,1) (1,1) (1,0).
        let d: Vec<u64> = [(0, 0), (0, 1), (1, 1), (1, 0)]
            .iter()
            .map(|&(x, y)| hilbert(1, x, y))
            .collect();
        assert_eq!(d, [0, 1, 2, 3]);
    }
}
