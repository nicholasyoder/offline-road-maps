//! Cuts features into a z0–z14 tile pyramid.
//!
//! Each feature is clipped down a quadtree: a child tile clips its parent's fragment, and
//! fragments that fit entirely in a child are borrowed, not copied. Zooms below
//! [`SPLIT_ZOOM`] are cut from only the features shown there; from it down, each split tile
//! is cut on its own from the features its bbox touches, so memory stays at a few split
//! tiles' features rather than the whole region's at every low zoom. V2 sections are written
//! to a temp file ([`Store`]), compressed when the pack is written.
//!
//! Also here: quantizing and simplifying a tile's features ([`encode`]), dictionary training
//! ([`dict`]), and the ocean polygons ([`water`]).

pub mod dict;
pub mod encode;
pub mod water;

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::Mutex;

use anyhow::Result;
use omp_core::whitelist::{Layer, MergeLines, Whitelist};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use omp_core::tile::{EXTENT_BITS, Enc, Geom, Point, encode_layer};

use crate::geom::{self, BBox, Geometry, Rect};
use crate::pbf::features::Feature;

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

#[derive(Default, Serialize, Deserialize)]
pub struct Stats {
    /// `(zoom, layer, class)` → tiles left out because all they held was one polygon of that
    /// class covering the tile and its buffer, and the V2 bytes they would have had.
    pub left_out: BTreeMap<(u8, u8, u16), (u64, u64)>,
}

impl Stats {
    pub fn merge(&mut self, o: Stats) {
        for (k, (a, b)) in o.left_out {
            let e = self.left_out.entry(k).or_default();
            e.0 += a;
            e.1 += b;
        }
    }
}

/// One V2 layer section in the temp file.
#[derive(Clone, Copy, Serialize, Deserialize)]
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
    /// Per layer, the last zoom it is put in (its `maxzoom`, else z14).
    maxzoom: Vec<u8>,
    /// The water layer's external ocean class, whose pieces are dissolved in each tile.
    ocean: Option<(u8, u16)>,
    stats: Vec<Mutex<Stats>>,
    store: Mutex<&'a mut Store>,
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

/// The part of the pyramid one band of `build --bands n` cuts: the split tiles in columns
/// with `x % n == k`, and z0–z5 in band 0. Each tile is cut in exactly one band, so the
/// bands' tile bytes add up. Every feature is "owned" by the band of the column its bbox
/// starts in, for per-feature totals that should not count a feature twice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Band {
    pub k: u32,
    pub n: u32,
}

impl Band {
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

/// Cuts band `band` of the pyramid and appends its V2 payloads to `store`.
pub fn cut_pyramid(features: &[Feature], wl: &Whitelist, store: &mut Store, band: Band) -> Stats {
    // A left-out tile is drawn from its nearest stored ancestor (format.md). That relies on
    // simplification moving an edge less than the half buffer a child's buffer is in its
    // parent's grid, so no simplified coastline reaches into a full-ocean child's square.
    for layer in &wl.layers {
        for z in 0..=MAX_ZOOM {
            let tol = thinning(z, layer).0;
            assert!(
                tol < (BUFFER / 2) as f64,
                "layer {} z{z}: tolerance {tol} must stay under half the {BUFFER}-unit buffer",
                layer.name
            );
        }
    }
    let minzoom = minzooms(wl);
    let ocean = wl.layer_index("water").and_then(|l| {
        let c = wl.layers[l]
            .classes
            .iter()
            .position(|c| c.external && c.name == "ocean")?;
        Some((l as u8, c as u16))
    });
    let ctx = Ctx {
        ocean,
        wl,
        stats: (0..rayon::current_num_threads() + 1)
            .map(|_| Mutex::default())
            .collect(),
        store: Mutex::new(store),
        minzoom,
        maxzoom: wl
            .layers
            .iter()
            .map(|l| l.maxzoom.unwrap_or(MAX_ZOOM))
            .collect(),
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
            .filter(|f| ctx.maxzoom[f.f.layer as usize] > z)
            .filter_map(|f| cut(f.f, &f.geom, f.bbox, &rect))
            .collect();
        walk(ctx, z + 1, cx, cy, &kids, last);
    });
}

fn emit(ctx: &Ctx, z: u8, x: u32, y: u32, frags: &[Frag]) {
    let mut by_layer: Vec<Vec<&Frag>> = vec![Vec::new(); ctx.wl.layers.len()];
    for f in frags {
        if feature_minzoom(&ctx.minzoom, f.f) <= z && z <= ctx.maxzoom[f.f.layer as usize] {
            by_layer[f.f.layer as usize].push(f);
        }
    }
    if by_layer.iter().all(Vec::is_empty) {
        return;
    }
    let mut local = Stats::default();
    let mut sections: Vec<(u8, Vec<u8>)> = Vec::new();
    // z14 drops nothing: rings that collapse are simplified again, down to the default.
    let floor = (z >= MAX_ZOOM).then_some(Z14_TOLERANCE);
    // The layer, class and V2 bytes of a layer that is one feature covering the whole tile.
    let mut full: Option<(u8, u16, u64)> = None;
    for (li, frags) in by_layer.iter().enumerate() {
        if frags.is_empty() {
            continue;
        }
        let layer = &ctx.wl.layers[li];
        let (tol, min_len, min_area) = thinning(z, layer);
        let li = li as u8;
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
        let v1 = match layer.merge_lines {
            Some(mode) => merge_lines(v1, mode, layer.props.iter().position(|p| p == "oneway")),
            None => v1,
        };
        let v1 = match ctx.ocean {
            Some((ol, oc)) if ol == li => dissolve_ocean(v1, oc),
            _ => v1,
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
        // The parent must show the feature too, so it has to start at a lower zoom: a park
        // whose class starts at z6 cannot be left to a z5 ancestor that does not have it.
        let shown_above = |class: u16| {
            frags
                .iter()
                .filter(|f| f.f.class == class)
                .all(|f| feature_minzoom(&ctx.minzoom, f.f) < z)
        };
        if v2.len() == 1 && covers_tile(&v2[0].geom) && shown_above(v2[0].class) {
            full = Some((li, v2[0].class, v2_bytes.len() as u64));
        }
        sections.push((li, v2_bytes));
    }
    // A tile holding only one polygon over its whole square and buffer (open ocean, a large
    // park or lake) adds nothing to its parent, which the app draws in its place (format.md,
    // "Left-out tiles").
    if z > 0
        && sections.len() == 1
        && let Some((li, class, bytes)) = full
    {
        local = Stats::default();
        local.left_out.insert((z, li, class), (1, bytes));
        sections.clear();
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

/// Replaces the tile's ocean pieces (class `ocean`) with one square when together they cover
/// the tile and its buffer, so a full-ocean tile is one feature wherever the split lines run.
fn dissolve_ocean(feats: Vec<Enc>, ocean: u16) -> Vec<Enc> {
    let (lo, hi) = (-(BUFFER as i32), (1 << EXTENT_BITS) + BUFFER as i32);
    let pieces = feats.iter().filter(|e| e.class == ocean).count();
    let rings: Vec<&[Point]> = feats
        .iter()
        .filter(|e| e.class == ocean)
        .filter_map(|e| match &e.geom {
            Geom::Polygon(r) if r.len() == 1 => Some(r[0].as_slice()),
            _ => None,
        })
        .collect();
    if pieces < 2 || rings.len() < pieces || !geom::rects_cover(rings, [lo, lo, hi, hi]) {
        return feats;
    }
    let mut out = Vec::with_capacity(feats.len() + 1 - pieces);
    let mut square = Some(vec![vec![[lo, lo], [hi, lo], [hi, hi], [lo, hi]]]);
    for e in feats {
        if e.class != ocean {
            out.push(e);
        } else if let Some(rings) = square.take() {
            out.push(Enc {
                geom: Geom::Polygon(rings),
                ..e
            });
        }
    }
    out
}

/// Whether `g` is a polygon covering the whole tile and its buffer.
fn covers_tile(g: &Geom) -> bool {
    let Geom::Polygon(rings) = g else {
        return false;
    };
    let side = f64::from((1 << EXTENT_BITS) + 2 * BUFFER as i32);
    // Rings are inside the clip rectangle and do not overlap, so only a full cover has its
    // area. Corner points rule out a ring of the right area shifted by rounding.
    rings.iter().map(|r| geom::ring_area(r)).sum::<f64>() == side * side
        && rings.iter().any(|r| {
            let lo = -(BUFFER as i32);
            let hi = (1 << EXTENT_BITS) + BUFFER as i32;
            [[lo, lo], [hi, lo], [hi, hi], [lo, hi]]
                .iter()
                .all(|c| r.contains(c))
        })
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
    fn full_ocean_pieces_become_one_square() {
        let props: Vec<(u8, Box<str>)> = Vec::new();
        let poly = |x0, x1| Geom::Polygon(vec![vec![[x0, -64], [x1, -64], [x1, 4160], [x0, 4160]]]);
        let ocean = |geom| Enc {
            class: 0,
            props: &props,
            geom,
        };
        // Two pieces overlapping along a split line: one square that covers the tile.
        let out = dissolve_ocean(vec![ocean(poly(-64, 2849)), ocean(poly(2611, 4160))], 0);
        assert_eq!(out.len(), 1);
        assert!(covers_tile(&out[0].geom));
        // With a gap they stay apart, and neither covers the tile.
        let out = dissolve_ocean(vec![ocean(poly(-64, 2000)), ocean(poly(2100, 4160))], 0);
        assert_eq!(out.len(), 2);
        assert!(!covers_tile(&out[0].geom));
        // One piece over all of it covers the tile as it is.
        assert!(covers_tile(&poly(-64, 4160)));
        assert!(!covers_tile(&poly(-63, 4160)));
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
    }
}
