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
use omp_core::whitelist::{Layer, MergeLines, Whitelist};
use rayon::prelude::*;
use serde::Serialize;

use omp_core::tile::{EXTENT_BITS, Enc, Geom, Point, encode_layer};

use super::encode;
use super::geom::{self, BBox, Geometry, Rect};
use super::osm::Feature;

pub const MAX_ZOOM: u8 = 14;
const SPLIT_ZOOM: u8 = 6;
/// Clip buffer around each tile, in grid units (of 4096).
const BUFFER: i64 = 64;

/// Default DP tolerance at z14, in grid units, and the floor a collapsing ring falls back to.
const Z14_TOLERANCE: f64 = 1.0;

/// V2 thinning per zoom: (DP tolerance, min line length, min ring area), in grid units.
/// 8 grid units are one pixel on a 512 px tile. z14 is overzoomed in the app, so by default
/// it keeps more detail, and it drops nothing. A layer's `tolerance` overrides the default.
fn thinning(z: u8, layer: &Layer) -> (f64, f64, f64) {
    let (tol, min_len, min_area) = if z >= MAX_ZOOM {
        (Z14_TOLERANCE, 0.0, 0.0)
    } else {
        (4.0, 8.0, 256.0)
    };
    (layer.tolerance(z).unwrap_or(tol), min_len, min_area)
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

/// The temp file of V2 layer payloads, and where each one is.
pub struct Store {
    out: BufWriter<File>,
    offset: u64,
    entries: Vec<Entry>,
}

impl Store {
    pub fn create(path: &Path) -> Result<Self> {
        Ok(Self {
            out: BufWriter::with_capacity(8 << 20, File::create(path)?),
            offset: 0,
            entries: Vec::new(),
        })
    }

    /// Flushes the file; returns the entries sorted by `(z, x, y, layer)` and the bytes
    /// written. Tiles finish in parallel, so the sort makes later stages see the same order
    /// every run.
    pub fn finish(mut self) -> Result<(Vec<Entry>, u64)> {
        self.out.flush()?;
        self.entries
            .sort_unstable_by_key(|e| (e.z, e.x, e.y, e.layer));
        Ok((self.entries, self.offset))
    }
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
    /// Also measure V0, V1 and MVT; without it only V2 is encoded (for `build`).
    measure: bool,
    stats: Vec<Mutex<Stats>>,
    store: Mutex<&'a mut Store>,
}

pub struct Pyramid {
    pub stats: Stats,
    pub entries: Vec<Entry>,
    pub store_bytes: u64,
}

/// `[layer][class]` minimum zoom, `u8::MAX` for classes not in tiles.
pub fn minzooms(wl: &Whitelist) -> Vec<Vec<u8>> {
    wl.layers
        .iter()
        .map(|l| {
            l.classes
                .iter()
                .map(|c| if c.tile { c.minzoom } else { u8::MAX })
                .collect()
        })
        .collect()
}

/// The first zoom feature `f` appears in, `u8::MAX` if never; `minzoom` is [`minzooms`].
pub fn feature_minzoom(minzoom: &[Vec<u8>], f: &Feature) -> u8 {
    let class = minzoom[f.layer as usize][f.class as usize];
    f.minzoom.map_or(class, |z| z.max(class))
}

/// The split tiles (z6) whose buffered rect `b` touches.
fn split_tiles(b: BBox) -> impl Iterator<Item = (u32, u32)> {
    let n = 1u32 << SPLIT_ZOOM;
    let size = 1i64 << (32 - u32::from(SPLIT_ZOOM));
    let range = |lo: u32, hi: u32| {
        let first = ((i64::from(lo) - size) / size).max(0) as u32;
        let last = ((i64::from(hi) + size) / size).min(i64::from(n) - 1) as u32;
        first..=last
    };
    let (xs, ys) = (range(b.min[0], b.max[0]), range(b.min[1], b.max[1]));
    ys.flat_map(move |y| xs.clone().map(move |x| (x, y)))
        .filter(move |&(x, y)| geom::bbox_overlaps(&b, &tile_rect(SPLIT_ZOOM, x, y)))
}

/// The part of the pyramid one `measure --band k/n` run cuts: the split tiles in columns
/// with `x % n == k`, and z0–z5 in band 0. Each tile is cut in exactly one band, so the
/// bands' tile bytes add up. Every feature is "owned" by the band of the column its bbox
/// starts in, for per-feature totals that should not count a feature twice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Band {
    pub k: u32,
    pub n: u32,
}

impl Band {
    pub const ALL: Band = Band { k: 0, n: 1 };

    pub fn parse(s: &str) -> Option<Self> {
        let (k, n) = s.split_once('/')?;
        let (k, n) = (k.parse().ok()?, n.parse().ok()?);
        (n > 0 && k < n).then_some(Band { k, n })
    }

    fn has(self, x: u32) -> bool {
        x % self.n == self.k
    }

    pub fn owns(self, b: BBox) -> bool {
        self.has(b.min[0] >> (32 - u32::from(SPLIT_ZOOM)))
    }

    /// Whether this band needs a feature with bounds `b` and minimum zoom `minzoom`.
    pub fn keeps(self, b: BBox, minzoom: u8) -> bool {
        self.n == 1
            || self.owns(b)
            || (self.k == 0 && minzoom < SPLIT_ZOOM)
            || (minzoom <= MAX_ZOOM && split_tiles(b).any(|(x, _)| self.has(x)))
    }
}

/// Cuts the pyramid for `measure`, into a new temp file at `store_path`.
pub fn build(
    features: &[Feature],
    wl: &Whitelist,
    store_path: &Path,
    band: Band,
) -> Result<Pyramid> {
    let mut store = Store::create(store_path)?;
    let stats = cut_pyramid(features, wl, &mut store, band, true);
    let (entries, store_bytes) = store.finish()?;
    Ok(Pyramid {
        stats,
        entries,
        store_bytes,
    })
}

/// Cuts band `band` of the pyramid and appends its V2 payloads to `store`. With `measure`,
/// also collects the V0, V1 and MVT statistics.
pub fn cut_pyramid(
    features: &[Feature],
    wl: &Whitelist,
    store: &mut Store,
    band: Band,
    measure: bool,
) -> Stats {
    let minzoom = minzooms(wl);
    let ctx = Ctx {
        wl,
        stats: (0..rayon::current_num_threads() + 1)
            .map(|_| Mutex::default())
            .collect(),
        store: Mutex::new(store),
        minzoom,
        measure,
    };
    let minzoom = |f: &Feature| feature_minzoom(&ctx.minzoom, f);
    let low: Vec<Frag> = features
        .iter()
        .filter(|f| minzoom(f) < SPLIT_ZOOM)
        .map(|f| Frag {
            f,
            bbox: f.geom.bbox(),
            geom: Cow::Borrowed(&f.geom),
        })
        .collect();
    if band.k == 0 {
        walk(&ctx, 0, 0, 0, &low, SPLIT_ZOOM - 1);
    }
    drop(low);

    // (split tile, feature) for every tile a feature's bbox touches.
    let n = 1u32 << SPLIT_ZOOM;
    let mut split: Vec<(u32, u32)> = features
        .par_iter()
        .enumerate()
        .filter(|(_, f)| minzoom(f) <= MAX_ZOOM)
        .flat_map_iter(|(i, f)| {
            split_tiles(f.geom.bbox())
                .filter(|&(x, _)| band.has(x))
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
    stats
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
        if feature_minzoom(&ctx.minzoom, f.f) <= z {
            by_layer[f.f.layer as usize].push(f);
        }
    }
    if by_layer.iter().all(Vec::is_empty) {
        return;
    }
    let mut local = Stats::default();
    let mut sections: Vec<(u8, Vec<u8>)> = Vec::new();
    let mut mvt_layers = Vec::new();
    // z14 drops nothing: rings that collapse are simplified again, down to the default.
    let floor = (z >= MAX_ZOOM).then_some(Z14_TOLERANCE);
    for (li, frags) in by_layer.iter().enumerate() {
        if frags.is_empty() {
            continue;
        }
        let layer = &ctx.wl.layers[li];
        let (tol, min_len, min_area) = thinning(z, layer);
        let li = li as u8;
        let class_names: Vec<String> = match ctx.measure {
            true => layer.classes.iter().map(|c| c.name.clone()).collect(),
            false => Vec::new(),
        };
        let v1: Vec<Enc> = frags
            .iter()
            .filter_map(|f| {
                encode::quantize(&f.geom, z, x, y).map(|g| Enc {
                    class: f.f.class,
                    props: &f.f.props,
                    geom: g,
                })
            })
            // A point goes only in the tile that contains it, not in its neighbours' buffers.
            .filter(|e| match e.geom {
                Geom::Point(p) => p.iter().all(|&v| (0..1 << EXTENT_BITS).contains(&v)),
                _ => true,
            })
            .collect();
        if ctx.measure {
            measure_v0_v1(&mut local, z, li, layer, &class_names, frags, &v1);
        }
        let v1 = match layer.merge_lines {
            Some(mode) => merge_lines(v1, mode, layer.props.iter().position(|p| p == "oneway")),
            None => v1,
        };
        let v2: Vec<Enc> = v1
            .into_iter()
            .filter_map(|e| {
                encode::simplify(e.geom, tol, min_len, min_area, floor).map(|g| Enc {
                    class: e.class,
                    props: e.props,
                    geom: g,
                })
            })
            .collect();
        if v2.is_empty() {
            continue;
        }
        let v2_bytes = encode_layer(&v2);
        if ctx.measure {
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
        }
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

/// Joins line pieces of the same class and props that meet end to end (decision 5): each
/// group becomes one feature whose parts are the joined chains. Pieces join only where
/// exactly two piece ends of the group meet, so intersections stay part ends. Other features
/// pass through. Groups keep the order of their first feature.
fn merge_lines(feats: Vec<Enc>, mode: MergeLines, oneway: Option<usize>) -> Vec<Enc> {
    use std::collections::HashMap;

    let mut groups: Vec<(Enc, Vec<Vec<Point>>)> = Vec::new();
    // (class, props) → index into `groups`.
    let mut by_key: HashMap<(u16, &[_]), usize> = HashMap::new();
    let mut out_order: Vec<Result<Enc, usize>> = Vec::new();
    for e in feats {
        let Geom::Lines(parts) = e.geom else {
            out_order.push(Ok(e));
            continue;
        };
        match by_key.get(&(e.class, e.props)) {
            Some(&g) => groups[g].1.extend(parts),
            None => {
                by_key.insert((e.class, e.props), groups.len());
                out_order.push(Err(groups.len()));
                groups.push((
                    Enc {
                        class: e.class,
                        props: e.props,
                        geom: Geom::Lines(Vec::new()),
                    },
                    parts,
                ));
            }
        }
    }
    let mut groups: Vec<Option<Enc>> = groups
        .into_iter()
        .map(|(mut e, parts)| {
            let directed = mode == MergeLines::Directed
                || oneway.is_some_and(|k| {
                    e.props
                        .iter()
                        .any(|(pk, v)| usize::from(*pk) == k && &**v != "no")
                });
            e.geom = Geom::Lines(join_parts(parts, !directed));
            Some(e)
        })
        .collect();
    out_order
        .into_iter()
        .map(|o| match o {
            Ok(e) => e,
            Err(g) => groups[g].take().expect("each group once"),
        })
        .collect()
}

/// Joins polyline parts into chains at points where exactly two part ends meet.
fn join_parts(parts: Vec<Vec<Point>>, reversible: bool) -> Vec<Vec<Point>> {
    use std::collections::HashMap;

    if parts.len() < 2 {
        return parts;
    }
    // Point → the part ends there: (part, is_start).
    let mut ends: HashMap<Point, Vec<(usize, bool)>> = HashMap::new();
    for (i, p) in parts.iter().enumerate() {
        ends.entry(p[0]).or_default().push((i, true));
        ends.entry(*p.last().unwrap()).or_default().push((i, false));
    }
    // The other part end at `p`, if exactly two meet there.
    let other = |p: &Point, me: (usize, bool)| -> Option<(usize, bool)> {
        match ends.get(p).map(Vec::as_slice) {
            Some(&[a, b]) if a == me => Some(b),
            Some(&[a, b]) if b == me => Some(a),
            _ => None,
        }
    };
    let mut used = vec![false; parts.len()];
    let mut out = Vec::new();
    for start in 0..parts.len() {
        if used[start] {
            continue;
        }
        used[start] = true;
        let mut chain = parts[start].clone();
        // Forward from the chain's end, then backward from its start.
        let (mut tail, mut head) = ((start, false), (start, true));
        while let Some((j, at_start)) = other(chain.last().unwrap(), tail) {
            if used[j] || (!at_start && !reversible) {
                break;
            }
            used[j] = true;
            if at_start {
                chain.extend_from_slice(&parts[j][1..]);
            } else {
                chain.extend(parts[j].iter().rev().skip(1));
            }
            tail = (j, !at_start);
        }
        while let Some((j, at_start)) = other(&chain[0], head) {
            if used[j] || (at_start && !reversible) {
                break;
            }
            used[j] = true;
            let mut piece: Vec<Point> = if at_start {
                parts[j].iter().rev().copied().collect()
            } else {
                parts[j].clone()
            };
            piece.pop();
            piece.extend_from_slice(&chain);
            chain = piece;
            head = (j, !at_start);
        }
        out.push(chain);
    }
    out
}

/// `measure`'s V0 and V1 cells for one layer of a tile, and its per-class z14 bytes.
fn measure_v0_v1(
    local: &mut Stats,
    z: u8,
    li: u8,
    layer: &Layer,
    class_names: &[String],
    frags: &[&Frag],
    v1: &[Enc],
) {
    let mut v0 = Cell {
        tiles: 1,
        ..Cell::default()
    };
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
    let v1_bytes = encode_layer(v1);
    if z == MAX_ZOOM {
        for e in v1 {
            // Encoded alone, so its strings are counted in full rather than shared.
            let one = encode_layer(std::slice::from_ref(e));
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_line_parts() {
        // a→b→c, with a branch c→d and c→e: the chain stops at the junction c.
        let a = vec![[0, 0], [10, 0]];
        let b = vec![[10, 0], [20, 0]];
        let d = vec![[20, 0], [30, 0]];
        let e = vec![[20, 0], [20, 10]];
        let joined = join_parts(vec![a.clone(), b.clone(), d.clone(), e.clone()], true);
        assert_eq!(
            joined,
            vec![vec![[0, 0], [10, 0], [20, 0]], d.clone(), e.clone()]
        );
        // Reversed pieces join when reversible, not when directed.
        let b_rev = vec![[20, 0], [10, 0]];
        assert_eq!(
            join_parts(vec![a.clone(), b_rev.clone()], true),
            vec![vec![[0, 0], [10, 0], [20, 0]]]
        );
        assert_eq!(
            join_parts(vec![b.clone(), a.clone()], false),
            vec![vec![[0, 0], [10, 0], [20, 0]]]
        );
        assert_eq!(join_parts(vec![a.clone(), b_rev.clone()], false).len(), 2);
        // A closed loop of two pieces ends where it started.
        let back = vec![[20, 0], [0, 0]];
        let loop_ = join_parts(vec![a, b, back], false);
        assert_eq!(loop_, vec![vec![[0, 0], [10, 0], [20, 0], [0, 0]]]);
    }

    #[test]
    fn merges_by_class_and_props() {
        let p1: Vec<(u8, Box<str>)> = vec![(0, "Main".into())];
        let p2: Vec<(u8, Box<str>)> = vec![(0, "Main".into()), (2, "yes".into())];
        let line = |pts: &[Point]| Geom::Lines(vec![pts.to_vec()]);
        let feats = vec![
            Enc {
                class: 1,
                props: &p1,
                geom: line(&[[0, 0], [5, 0]]),
            },
            Enc {
                class: 1,
                props: &p2,
                geom: line(&[[5, 0], [9, 0]]),
            },
            Enc {
                class: 1,
                props: &p1,
                geom: line(&[[9, 9], [5, 0]]),
            },
            Enc {
                class: 2,
                props: &p1,
                geom: Geom::Point([1, 1]),
            },
        ];
        let out = merge_lines(feats, MergeLines::Reversible, Some(2));
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].geom, line(&[[0, 0], [5, 0], [9, 9]]));
        assert_eq!(out[1].props, &p2[..]);
        assert_eq!(out[2].geom, Geom::Point([1, 1]));
    }

    #[test]
    fn bands_partition_split_tiles() {
        let bands: Vec<Band> = (0..3).map(|k| Band { k, n: 3 }).collect();
        // A bbox spanning several z6 columns: every band keeps it, exactly one owns it.
        let col = 1u32 << (32 - u32::from(SPLIT_ZOOM));
        let b = BBox {
            min: [10 * col + 5, 20 * col],
            max: [13 * col, 20 * col + 9],
        };
        assert!(bands.iter().all(|band| band.keeps(b, MAX_ZOOM)));
        assert_eq!(bands.iter().filter(|band| band.owns(b)).count(), 1);
        // A small feature inside one column, away from its edges.
        let small = BBox {
            min: [10 * col + col / 2, 20 * col + col / 2],
            max: [10 * col + col / 2 + 9, 20 * col + col / 2 + 9],
        };
        let keepers: Vec<u32> = bands
            .iter()
            .filter(|band| band.keeps(small, MAX_ZOOM))
            .map(|band| band.k)
            .collect();
        assert_eq!(keepers, [10 % 3]);
        // Low-zoom features also go to band 0, for z0–z5.
        assert!(bands[0].keeps(small, 3));
        assert_eq!(Band::parse("2/3"), Some(bands[2]));
        assert_eq!(Band::parse("3/3"), None);
    }
}
