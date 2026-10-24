//! Cuts features into a z0–z14 tile pyramid and measures the uncompressed encodings.
//!
//! Each feature is clipped down a quadtree: a child tile clips its parent's fragment, and
//! fragments that fit entirely in a child are borrowed, not copied. Zooms below
//! [`SPLIT_ZOOM`] are cut from only the features shown there; from it down, each split tile
//! is cut on its own from the features its bbox touches, so memory stays at a few split
//! tiles' features rather than the whole region's at every low zoom. V2 sections are written
//! to a temp file for the compression variants in `compress`.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::Mutex;

use anyhow::Result;
use omp_core::whitelist::Whitelist;
use rayon::prelude::*;
use serde::Serialize;

use super::encode::{self, EXTENT_BITS, Enc};
use super::geom::{self, BBox, Geometry, Rect};
use super::osm::Feature;

pub const MAX_ZOOM: u8 = 14;
const SPLIT_ZOOM: u8 = 6;
/// Clip buffer around each tile, in grid units (of 4096).
const BUFFER: i64 = 64;

/// V2 thinning per zoom: (DP tolerance, min line length, min ring area), in grid units.
/// 8 grid units are one pixel on a 512 px tile. z14 is overzoomed in the app, so it keeps
/// more detail and drops nothing.
fn thinning(z: u8) -> (f64, f64, f64) {
    if z >= MAX_ZOOM {
        (1.0, 0.0, 0.0)
    } else {
        (4.0, 8.0, 256.0)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Variant {
    V0,
    V1,
    V2,
    V3,
    V4,
    V5,
}

#[derive(Default, Clone, Copy, Serialize)]
pub struct Cell {
    pub tiles: u64,
    pub features: u64,
    pub vertices: u64,
    pub bytes: u64,
}

impl Cell {
    pub fn add(&mut self, o: &Cell) {
        self.tiles += o.tiles;
        self.features += o.features;
        self.vertices += o.vertices;
        self.bytes += o.bytes;
    }
}

#[derive(Default)]
pub struct Stats {
    /// `(variant, layer, zoom)`.
    pub cells: BTreeMap<(Variant, u8, u8), Cell>,
    /// Per zoom: tiles, raw MVT bytes, gzipped MVT bytes.
    pub mvt: BTreeMap<u8, (u64, u64, u64)>,
    /// `(layer, class)` → V1 bytes and features at z14, each feature encoded on its own.
    pub class_z14: BTreeMap<(u8, u16), (u64, u64)>,
}

impl Stats {
    fn merge(&mut self, o: Stats) {
        for (k, c) in o.cells {
            self.cells.entry(k).or_default().add(&c);
        }
        for (z, (a, b, c)) in o.mvt {
            let e = self.mvt.entry(z).or_default();
            e.0 += a;
            e.1 += b;
            e.2 += c;
        }
        for (k, (a, b)) in o.class_z14 {
            let e = self.class_z14.entry(k).or_default();
            e.0 += a;
            e.1 += b;
        }
    }
}

/// One V2 layer section in the temp file.
#[derive(Clone, Copy)]
pub struct Entry {
    pub z: u8,
    pub x: u32,
    pub y: u32,
    pub layer: u8,
    pub offset: u64,
    pub len: u32,
}

struct Store {
    out: BufWriter<File>,
    offset: u64,
    entries: Vec<Entry>,
}

struct Frag<'a> {
    f: &'a Feature,
    geom: Cow<'a, Geometry>,
    bbox: BBox,
}

struct Ctx<'a> {
    wl: &'a Whitelist,
    /// `[layer][class]`, `u8::MAX` for classes not in tiles.
    minzoom: Vec<Vec<u8>>,
    stats: Vec<Mutex<Stats>>,
    store: Mutex<Store>,
}

pub struct Pyramid {
    pub stats: Stats,
    pub entries: Vec<Entry>,
    pub store_bytes: u64,
}

pub fn build(features: &[Feature], wl: &Whitelist, store_path: &Path) -> Result<Pyramid> {
    let minzoom: Vec<Vec<u8>> = wl
        .layers
        .iter()
        .map(|l| {
            l.classes
                .iter()
                .map(|c| if c.tile { c.minzoom } else { u8::MAX })
                .collect()
        })
        .collect();
    let ctx = Ctx {
        wl,
        stats: (0..rayon::current_num_threads() + 1)
            .map(|_| Mutex::default())
            .collect(),
        store: Mutex::new(Store {
            out: BufWriter::with_capacity(8 << 20, File::create(store_path)?),
            offset: 0,
            entries: Vec::new(),
        }),
        minzoom,
    };
    let minzoom = |f: &Feature| ctx.minzoom[f.layer as usize][f.class as usize];
    let low: Vec<Frag> = features
        .iter()
        .filter(|f| minzoom(f) < SPLIT_ZOOM)
        .map(|f| Frag {
            f,
            bbox: f.geom.bbox(),
            geom: Cow::Borrowed(&f.geom),
        })
        .collect();
    walk(&ctx, 0, 0, 0, &low, SPLIT_ZOOM - 1);
    drop(low);

    // (split tile, feature) for every tile a feature's bbox touches.
    let n = 1u32 << SPLIT_ZOOM;
    let mut split: Vec<(u32, u32)> = features
        .par_iter()
        .enumerate()
        .filter(|(_, f)| minzoom(f) <= MAX_ZOOM)
        .flat_map_iter(|(i, f)| {
            let b = f.geom.bbox();
            let size = 1i64 << (32 - u32::from(SPLIT_ZOOM));
            let range = |lo: u32, hi: u32| {
                let first = ((i64::from(lo) - size) / size).max(0) as u32;
                let last = ((i64::from(hi) + size) / size).min(i64::from(n) - 1) as u32;
                first..=last
            };
            let (xs, ys) = (range(b.min[0], b.max[0]), range(b.min[1], b.max[1]));
            ys.flat_map(move |y| xs.clone().map(move |x| (x, y)))
                .filter(move |&(x, y)| geom::bbox_overlaps(&b, &tile_rect(SPLIT_ZOOM, x, y)))
                .map(move |(x, y)| (y * n + x, i as u32))
        })
        .collect();
    split.par_sort_unstable();
    split.par_chunk_by(|a, b| a.0 == b.0).for_each(|group| {
        let (x, y) = (group[0].0 % n, group[0].0 / n);
        let rect = tile_rect(SPLIT_ZOOM, x, y);
        let frags: Vec<Frag> = group
            .iter()
            .filter_map(|&(_, i)| {
                let f = &features[i as usize];
                cut(f, &f.geom, f.geom.bbox(), &rect)
            })
            .collect();
        walk(&ctx, SPLIT_ZOOM, x, y, &frags, MAX_ZOOM);
    });

    let mut stats = Stats::default();
    for s in ctx.stats {
        stats.merge(s.into_inner().unwrap());
    }
    let mut store = ctx.store.into_inner().unwrap();
    store.out.flush()?;
    // Tiles finish in parallel; sort so later stages see the same order every run.
    store
        .entries
        .sort_unstable_by_key(|e| (e.z, e.x, e.y, e.layer));
    Ok(Pyramid {
        stats,
        entries: store.entries,
        store_bytes: store.offset,
    })
}

fn tile_rect(z: u8, x: u32, y: u32) -> Rect {
    let size = 1i64 << (32 - u32::from(z));
    let buf = BUFFER << (32 - u32::from(z) - EXTENT_BITS);
    let (x0, y0) = (i64::from(x) * size, i64::from(y) * size);
    [x0 - buf, y0 - buf, x0 + size + buf, y0 + size + buf]
}

/// The part of `geom` (of feature `f`, with bounds `bbox`) in `rect`, borrowed if it lies
/// entirely inside.
fn cut<'a>(f: &'a Feature, geom: &'a Geometry, bbox: BBox, rect: &Rect) -> Option<Frag<'a>> {
    if !geom::bbox_overlaps(&bbox, rect) {
        None
    } else if geom::bbox_inside(&bbox, rect) {
        Some(Frag {
            f,
            geom: Cow::Borrowed(geom),
            bbox,
        })
    } else {
        geom::clip(geom, rect).map(|g| Frag {
            f,
            bbox: g.bbox(),
            geom: Cow::Owned(g),
        })
    }
}

/// Emits the tile and its descendants down to zoom `last`.
fn walk(ctx: &Ctx, z: u8, x: u32, y: u32, frags: &[Frag], last: u8) {
    if frags.is_empty() {
        return;
    }
    emit(ctx, z, x, y, frags);
    if z == last {
        return;
    }
    (0..4u32).into_par_iter().for_each(|i| {
        let (cx, cy) = (2 * x + (i & 1), 2 * y + (i >> 1));
        let rect = tile_rect(z + 1, cx, cy);
        let kids: Vec<Frag> = frags
            .iter()
            .filter_map(|f| cut(f.f, &f.geom, f.bbox, &rect))
            .collect();
        walk(ctx, z + 1, cx, cy, &kids, last);
    });
}

fn emit(ctx: &Ctx, z: u8, x: u32, y: u32, frags: &[Frag]) {
    let mut by_layer: Vec<Vec<&Frag>> = vec![Vec::new(); ctx.wl.layers.len()];
    for f in frags {
        if ctx.minzoom[f.f.layer as usize][f.f.class as usize] <= z {
            by_layer[f.f.layer as usize].push(f);
        }
    }
    if by_layer.iter().all(Vec::is_empty) {
        return;
    }
    let mut local = Stats::default();
    let mut sections: Vec<(u8, Vec<u8>)> = Vec::new();
    let mut mvt_layers = Vec::new();
    let (tol, min_len, min_area) = thinning(z);
    for (li, frags) in by_layer.iter().enumerate() {
        if frags.is_empty() {
            continue;
        }
        let layer = &ctx.wl.layers[li];
        let li = li as u8;
        let class_names: Vec<String> = layer.classes.iter().map(|c| c.name.clone()).collect();

        let mut v0 = Cell::default();
        for f in frags {
            v0.features += 1;
            v0.vertices += f.geom.vertices() as u64;
            v0.bytes += encode::v0_size(
                &f.geom,
                &class_names[f.f.class as usize],
                &f.f.props,
                &layer.props,
            );
        }

        let v1: Vec<Enc> = frags
            .iter()
            .filter_map(|f| {
                encode::quantize(&f.geom, z, x, y).map(|g| Enc {
                    class: f.f.class,
                    props: &f.f.props,
                    geom: g,
                })
            })
            .collect();
        let v1_bytes = encode::encode_v1(&v1);
        if z == MAX_ZOOM {
            for e in &v1 {
                // Encoded alone, so its strings are counted in full rather than shared.
                let one = encode::encode_v1(std::slice::from_ref(e));
                let c = local.class_z14.entry((li, e.class)).or_default();
                c.0 += one.len() as u64 - 1;
                c.1 += 1;
            }
        }
        let v1_cell = Cell {
            tiles: 1,
            features: v1.len() as u64,
            vertices: v1.iter().map(|e| e.geom.vertices() as u64).sum(),
            bytes: v1_bytes.len() as u64,
        };

        let v2: Vec<Enc> = v1
            .into_iter()
            .filter_map(|e| {
                encode::simplify(e.geom, tol, min_len, min_area).map(|g| Enc {
                    class: e.class,
                    props: e.props,
                    geom: g,
                })
            })
            .collect();
        v0.tiles = 1;
        local
            .cells
            .entry((Variant::V0, li, z))
            .or_default()
            .add(&v0);
        local
            .cells
            .entry((Variant::V1, li, z))
            .or_default()
            .add(&v1_cell);
        if v2.is_empty() {
            continue;
        }
        let v2_bytes = encode::encode_v1(&v2);
        local
            .cells
            .entry((Variant::V2, li, z))
            .or_default()
            .add(&Cell {
                tiles: 1,
                features: v2.len() as u64,
                vertices: v2.iter().map(|e| e.geom.vertices() as u64).sum(),
                bytes: v2_bytes.len() as u64,
            });
        mvt_layers.push(encode::mvt_layer(
            &layer.name,
            &v2,
            &class_names,
            &layer.props,
        ));
        sections.push((li, v2_bytes));
    }
    if !mvt_layers.is_empty() {
        let (raw, gz) = encode::mvt_tile_gzip(&mvt_layers);
        let e = local.mvt.entry(z).or_default();
        e.0 += 1;
        e.1 += raw;
        e.2 += gz;
    }
    if !sections.is_empty() {
        let mut store = ctx.store.lock().unwrap();
        for (layer, bytes) in sections {
            let offset = store.offset;
            store.out.write_all(&bytes).expect("write tile store");
            store.offset += bytes.len() as u64;
            store.entries.push(Entry {
                z,
                x,
                y,
                layer,
                offset,
                len: bytes.len() as u32,
            });
        }
    }
    let slot = rayon::current_thread_index().map_or(0, |i| i + 1);
    ctx.stats[slot].lock().unwrap().merge(local);
}
