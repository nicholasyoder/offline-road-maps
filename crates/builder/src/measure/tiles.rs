//! Cuts features into a z0–z14 tile pyramid and measures the uncompressed encodings.
//!
//! Each feature is clipped down a quadtree: a child tile clips its parent's fragment, and
//! fragments that fit entirely in a child are borrowed, not copied. V2 sections are written
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
    let root: Vec<Frag> = features
        .iter()
        .filter(|f| ctx.minzoom[f.layer as usize][f.class as usize] <= MAX_ZOOM)
        .map(|f| Frag {
            f,
            bbox: f.geom.bbox(),
            geom: Cow::Borrowed(&f.geom),
        })
        .collect();
    walk(&ctx, 0, 0, 0, &root);

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

fn walk(ctx: &Ctx, z: u8, x: u32, y: u32, frags: &[Frag]) {
    if frags.is_empty() {
        return;
    }
    emit(ctx, z, x, y, frags);
    if z == MAX_ZOOM {
        return;
    }
    (0..4u32).into_par_iter().for_each(|i| {
        let (cx, cy) = (2 * x + (i & 1), 2 * y + (i >> 1));
        let rect = tile_rect(z + 1, cx, cy);
        let child: Vec<Frag> = frags
            .iter()
            .filter(|f| geom::bbox_overlaps(&f.bbox, &rect))
            .filter_map(|f| {
                if geom::bbox_inside(&f.bbox, &rect) {
                    Some(Frag {
                        f: f.f,
                        geom: Cow::Borrowed(&*f.geom),
                        bbox: f.bbox,
                    })
                } else {
                    geom::clip(&f.geom, &rect).map(|g| Frag {
                        f: f.f,
                        bbox: g.bbox(),
                        geom: Cow::Owned(g),
                    })
                }
            })
            .collect();
        walk(ctx, z + 1, cx, cy, &child);
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
