//! The street table (phase 2 step 2.2, decision 2): one record per street, shared by search,
//! addresses, "what's here" and later routing, each with its own simplified geometry.
//! Addresses are offsets along that geometry. `omp-builder streets` reads a PBF and reports
//! counts and section sizes.
//!
//! A street is a run of connected named ways with the same normalized `name`, as in the
//! address spike, with two changes: it is split where its postal town changes (the town its
//! addresses give, step 2.1) or, where it has no addresses, at a city boundary (admin 8);
//! and it is split at about [`STREET_MAX_M`]. Ways are first cut into **pieces** where they
//! cross an admin 8 boundary and every [`PIECE_MAX_M`], and streets are made of pieces.

pub mod encode;
pub mod grid;

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result};
use clap::Args;
use omp_core::geo::dist;
use omp_core::normalize::street_key;
use rayon::prelude::*;
use serde::Serialize;

use crate::addr::osm::{self, HIGHWAYS, StreetWay};
use crate::addr::streets::morton;
use crate::admin::towns::{self, FALLBACK, Fallbacks, Located, Source};
use crate::admin::{self, Admin};
use crate::geom::{self, P};
use crate::peak_rss_mib;

/// Ways are cut into pieces at most about this long, so a long way can be split.
pub const PIECE_MAX_M: f64 = 5_000.0;
/// A street stops growing once it is this long, so a 200 km rural route is not one record.
pub const STREET_MAX_M: f64 = 20_000.0;
/// Douglas–Peucker tolerance of the stored geometry (addresses are quantized to 5 m).
pub const SIMPLIFY_M: f64 = 2.0;
/// Stored points are in units of 2^6 world units (about 0.6 m at the equator).
pub const SHIFT: u32 = 6;
/// Address offsets are stored in steps of this many metres (`ASTR`); each part starts on a
/// fresh step, so a rounded offset belongs to one part only.
pub const STEP_M: f64 = 5.0;

pub struct Street {
    /// The commonest `name` of its ways, by length (or the first alternate or ref).
    pub name: String,
    /// Other names of its ways whose key differs from `name`'s, sorted.
    pub alt_names: Vec<String>,
    pub refs: Vec<String>,
    /// The most important [`HIGHWAYS`] class of its ways.
    pub class: u8,
    pub town: String,
    pub town_source: Source,
    /// The deepest admin area holding its middle point.
    pub admin: Option<u32>,
    /// Stored points (multiples of 2^[`SHIFT`]); offsets run on from one part to the next.
    pub parts: Vec<Vec<P>>,
    pub along: Along,
}

/// Metres along a street's parts, as the app computes them: segment lengths in world units
/// times the metres per unit at the street's first point. Part 0 starts at 0; part k + 1 at
/// `STEP_M × (⌈end of part k / STEP_M⌉ + 1)`.
pub struct Along {
    pub mpu: f64,
    /// Per part, metres from the street's start at each point.
    pub cum: Vec<Vec<f64>>,
}

impl Along {
    pub fn of(parts: &[Vec<P>]) -> Self {
        let mpu = parts
            .first()
            .and_then(|p| p.first())
            .map_or(1.0, |&p| geom::metres_per_unit(p));
        let mut at = 0.0;
        let cum = parts
            .iter()
            .enumerate()
            .map(|(k, part)| {
                if k > 0 {
                    at = STEP_M * ((at / STEP_M).ceil() + 1.0);
                }
                let mut c = Vec::with_capacity(part.len());
                for (i, p) in part.iter().enumerate() {
                    if i > 0 {
                        at += dist(part[i - 1], *p) * mpu;
                    }
                    c.push(at);
                }
                c
            })
            .collect();
        Self { mpu, cum }
    }

    pub fn length_m(&self) -> f64 {
        self.cum
            .last()
            .and_then(|c| c.last())
            .copied()
            .unwrap_or(0.0)
    }
}

/// The point `m` metres along `parts`: on the last part that starts at or before `m`,
/// clamped to its ends.
pub fn point_at(parts: &[Vec<P>], along: &Along, m: f64) -> P {
    let k = along
        .cum
        .partition_point(|c| c[0] <= m)
        .saturating_sub(1)
        .min(parts.len() - 1);
    let (part, cum) = (&parts[k], &along.cum[k]);
    if part.len() == 1 {
        return part[0];
    }
    let i = cum.partition_point(|&d| d < m).clamp(1, part.len() - 1);
    let (a, b) = (part[i - 1], part[i]);
    let seg = cum[i] - cum[i - 1];
    let t = if seg > 0.0 {
        ((m - cum[i - 1]) / seg).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let lerp = |a: u32, b: u32| (f64::from(a) + (f64::from(b) - f64::from(a)) * t) as u32;
    [lerp(a[0], b[0]), lerp(a[1], b[1])]
}

/// Where a point lies relative to a street.
#[derive(Clone, Copy, Debug)]
pub struct Spot {
    pub street: u32,
    pub offset_m: f64,
    pub dist_m: f64,
    /// On the left when walking in the offset direction.
    pub left: bool,
}

/// Projects `p` onto points `lo..=hi` of `part`, whose cumulative metres are `cum`.
fn project(part: &[P], cum: &[f64], mpu: f64, lo: usize, hi: usize, p: P) -> (f64, f64, bool) {
    let (px, py) = (f64::from(p[0]), f64::from(p[1]));
    if lo == hi {
        return (cum[lo], dist(part[lo], p) * mpu, false);
    }
    let mut best = (f64::INFINITY, 0.0, false);
    for i in lo..hi {
        let (a, b) = (part[i], part[i + 1]);
        let (ax, ay) = (f64::from(a[0]), f64::from(a[1]));
        let (dx, dy) = (f64::from(b[0]) - ax, f64::from(b[1]) - ay);
        let len2 = dx * dx + dy * dy;
        let t = if len2 > 0.0 {
            (((px - ax) * dx + (py - ay) * dy) / len2).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let d2 = (px - ax - t * dx).powi(2) + (py - ay - t * dy).powi(2);
        if d2 < best.0 {
            // y grows downward, so a negative cross product is the left side.
            let left = dx * (py - ay) - dy * (px - ax) < 0.0;
            best = (d2, cum[i] + t * (cum[i + 1] - cum[i]), left);
        }
    }
    (best.1, best.0.sqrt() * mpu, best.2)
}

/// A run of a way's vertices inside one street part, between stored points `lo..=hi` of the
/// part.
struct Piece {
    street: u32,
    part: u32,
    lo: u32,
    hi: u32,
}

#[derive(Serialize, Default)]
pub struct Stats {
    pub ways: u64,
    pub way_vertices: u64,
    pub pieces: u64,
    /// Cuts where a way enters another admin 8 area, and at [`PIECE_MAX_M`].
    pub cuts_area: u64,
    pub cuts_length: u64,
    pub streets: u64,
    /// Streets labelled by their addresses' postal town, and by their admin 8 area.
    pub by_postal_town: u64,
    pub by_area: u64,
    /// Streets that stopped growing at [`STREET_MAX_M`].
    pub capped: u64,
    pub parts: u64,
    pub multi_part: u64,
    pub max_parts: u64,
    pub stored_points: u64,
    pub length_km: f64,
    pub longest_m: f64,
    pub with_alt_names: u64,
    pub with_refs: u64,
    pub without_admin: u64,
    pub town_by_source: BTreeMap<Source, u64>,
    /// Streets per class name.
    pub classes: BTreeMap<String, u64>,
    pub build_s: f64,
}

/// The street table, with what is needed to place addresses on it.
pub struct Table {
    pub streets: Vec<Street>,
    pieces: Vec<Piece>,
    /// Pieces of way `w`: `way_pieces[way_start[w]..way_start[w + 1]]`.
    way_start: Vec<u32>,
    pub stats: Stats,
}

/// A piece before it is in a street.
struct Cut {
    way: u32,
    from: u32,
    to: u32,
    area: Option<u32>,
    length_m: f64,
}

/// Cuts way `w` where it enters another admin 8 area and every [`PIECE_MAX_M`]; also
/// returns the number of each kind of cut.
fn cut_way(w: u32, way: &StreetWay, admin: &Admin) -> (Vec<Cut>, [u64; 2]) {
    let area8 = |p: P| admin.at_level(&admin.chain(p), 8);
    let c = &way.coords;
    let mpu = geom::metres_per_unit(c[0]);
    let mut out = Vec::new();
    let mut counts = [0; 2];
    let (mut start, mut area, mut len) = (0, area8(c[0]), 0.0);
    for i in 1..c.len() {
        len += dist(c[i - 1], c[i]) * mpu;
        if i == c.len() - 1 {
            break;
        }
        let a = area8(c[i]);
        let by_area = a != area;
        let by_length = len >= PIECE_MAX_M;
        if by_area || by_length {
            counts[usize::from(!by_area)] += 1;
            out.push(Cut {
                way: w,
                from: start,
                to: i as u32,
                area,
                length_m: len,
            });
            (start, area, len) = (i as u32, a, 0.0);
        }
    }
    out.push(Cut {
        way: w,
        from: start,
        to: c.len() as u32 - 1,
        area,
        length_m: len,
    });
    (out, counts)
}

/// One street's geometry: parts of `(piece, reversed)` and the pieces' stored ranges.
struct Built {
    street: Street,
    /// `(piece, part, lo, hi)`.
    placed: Vec<(u32, u32, u32, u32)>,
    first_way: i64,
}

/// Chains the pieces of one street into parts: from a free end (the westmost), on through
/// the straightest unused piece at each end, until none is left.
fn chain_parts(ways: &[StreetWay], cuts: &[Cut], members: &[u32]) -> Vec<Vec<(u32, bool)>> {
    let ends = |c: &Cut| {
        let r = &ways[c.way as usize].refs;
        (r[c.from as usize], r[c.to as usize])
    };
    let mut at: HashMap<i64, Vec<usize>> = HashMap::new();
    for (k, &m) in members.iter().enumerate() {
        let (a, b) = ends(&cuts[m as usize]);
        at.entry(a).or_default().push(k);
        if b != a {
            at.entry(b).or_default().push(k);
        }
    }
    // The points of piece k from its start (or its end, if reversed).
    let pts = |k: usize, rev: bool| -> Vec<P> {
        let c = &cuts[members[k] as usize];
        let s = &ways[c.way as usize].coords[c.from as usize..=c.to as usize];
        if rev {
            s.iter().rev().copied().collect()
        } else {
            s.to_vec()
        }
    };
    let mut used = vec![false; members.len()];
    let mut left = members.len();
    let mut parts = Vec::new();
    while left > 0 {
        let free = |node: i64, used: &[bool]| at[&node].iter().filter(|&&k| !used[k]).count();
        // Start at a free end if there is one, the westmost; else the westmost start.
        let mut start: Option<(P, usize, bool)> = None;
        for k in (0..members.len()).filter(|&k| !used[k]) {
            let c = &cuts[members[k] as usize];
            let (a, b) = ends(c);
            let coords = &ways[c.way as usize].coords;
            for (node, p, rev) in [
                (a, coords[c.from as usize], false),
                (b, coords[c.to as usize], true),
            ] {
                if free(node, &used) == 1 && start.is_none_or(|s| (p, k) < (s.0, s.1)) {
                    start = Some((p, k, rev));
                }
            }
        }
        let (_, k0, rev0) = start.unwrap_or_else(|| {
            let k = (0..members.len())
                .filter(|&k| !used[k])
                .min_by_key(|&k| {
                    let c = &cuts[members[k] as usize];
                    (ways[c.way as usize].coords[c.from as usize], k)
                })
                .unwrap();
            ([0, 0], k, false)
        });
        used[k0] = true;
        left -= 1;
        let mut part: VecDeque<(usize, bool)> = VecDeque::from([(k0, rev0)]);
        // Extend at the tail, then at the head. An oriented piece runs from its start node
        // to its end node.
        let start_end = |k: usize, rev: bool| {
            let (a, b) = ends(&cuts[members[k] as usize]);
            if rev { (b, a) } else { (a, b) }
        };
        for forward in [true, false] {
            loop {
                let (k, rev) = if forward {
                    *part.back().unwrap()
                } else {
                    *part.front().unwrap()
                };
                let p = pts(k, rev);
                let n = p.len();
                // The node to go on from, and the direction arriving at it.
                let (node, dir) = if forward {
                    (start_end(k, rev).1, (p[n - 2], p[n - 1]))
                } else {
                    (start_end(k, rev).0, (p[1], p[0]))
                };
                let mut best: Option<(f64, usize, bool)> = None;
                for &j in at.get(&node).into_iter().flatten() {
                    if used[j] {
                        continue;
                    }
                    // Forward, the next piece starts at `node`; backward, it ends there.
                    let (ja, jb) = ends(&cuts[members[j] as usize]);
                    let jrev = if forward { ja != node } else { jb != node };
                    let q = pts(j, jrev);
                    let m = q.len();
                    let out = if forward {
                        (q[0], q[1])
                    } else {
                        (q[m - 1], q[m - 2])
                    };
                    let turn = angle(dir, out);
                    if best.is_none_or(|b| (turn, j) < (b.0, b.1)) {
                        best = Some((turn, j, jrev));
                    }
                }
                let Some((_, j, jrev)) = best else { break };
                used[j] = true;
                left -= 1;
                if forward {
                    part.push_back((j, jrev));
                } else {
                    part.push_front((j, jrev));
                }
            }
        }
        parts.push(part.into_iter().map(|(k, rev)| (members[k], rev)).collect());
    }
    parts
}

/// The turn between two directions, in radians (0 = straight on).
fn angle(a: (P, P), b: (P, P)) -> f64 {
    let d = |s: (P, P)| {
        (
            f64::from(s.1[0]) - f64::from(s.0[0]),
            f64::from(s.1[1]) - f64::from(s.0[1]),
        )
    };
    let (u, v) = (d(a), d(b));
    let (nu, nv) = (u.0.hypot(u.1), v.0.hypot(v.1));
    if nu == 0.0 || nv == 0.0 {
        return std::f64::consts::PI;
    }
    ((u.0 * v.0 + u.1 * v.1) / (nu * nv))
        .clamp(-1.0, 1.0)
        .acos()
}

/// Builds one street from its pieces: parts, stored points, names and class.
fn build_street(ways: &[StreetWay], cuts: &[Cut], members: &[u32], key: &str) -> Built {
    let chained = chain_parts(ways, cuts, members);
    let mut parts = Vec::with_capacity(chained.len());
    let mut placed = Vec::with_capacity(members.len());
    for (pi, pieces) in chained.iter().enumerate() {
        // Concatenated vertices, and where each piece starts and ends in them.
        let mut line: Vec<P> = Vec::new();
        let mut spans = Vec::with_capacity(pieces.len());
        for &(m, rev) in pieces {
            let c = &cuts[m as usize];
            let s = &ways[c.way as usize].coords[c.from as usize..=c.to as usize];
            // Each piece after the first shares its first point with the line's last.
            let skip = usize::from(!line.is_empty());
            let start = line.len() - skip;
            if rev {
                line.extend(s.iter().rev().skip(skip));
            } else {
                line.extend(s.iter().skip(skip));
            }
            spans.push((m, start, line.len() - 1));
        }
        // Quantize, dropping repeats, then simplify.
        let mut q: Vec<P> = Vec::with_capacity(line.len());
        let mut qi = Vec::with_capacity(line.len());
        for p in &line {
            let s = [(p[0] >> SHIFT) << SHIFT, (p[1] >> SHIFT) << SHIFT];
            if q.last() != Some(&s) {
                q.push(s);
            }
            qi.push(q.len() - 1);
        }
        let tolerance = SIMPLIFY_M / geom::metres_per_unit(q[0]);
        let kept = geom::simplify_kept(&q, tolerance);
        let stored: Vec<P> = kept.iter().map(|&i| q[i]).collect();
        for (m, a, b) in spans {
            let (qa, qb) = (qi[a], qi[b]);
            let mut lo = kept.partition_point(|&k| k <= qa) - 1;
            let mut hi = kept.partition_point(|&k| k < qb).min(kept.len() - 1);
            if lo == hi && stored.len() > 1 {
                if hi + 1 < stored.len() {
                    hi += 1;
                } else {
                    lo -= 1;
                }
            }
            placed.push((m, pi as u32, lo as u32, hi as u32));
        }
        parts.push(stored);
    }

    // Names: the commonest `name` by length; the others whose key differs, and refs.
    let mut by_name: HashMap<&str, f64> = HashMap::new();
    let mut alt: Vec<&str> = Vec::new();
    let mut refs: Vec<String> = Vec::new();
    let mut class = u8::MAX;
    for &m in members {
        let c = &cuts[m as usize];
        let w = &ways[c.way as usize];
        *by_name.entry(&w.names[0]).or_default() += c.length_m;
        alt.extend(w.names[1..w.nnames.max(1)].iter().map(String::as_str));
        refs.extend(w.names[w.nnames.max(1)..].iter().cloned());
        if w.nnames == 0 {
            // A way with only refs is named by its first.
            refs.push(w.names[0].clone());
        }
        class = class.min(w.class);
    }
    let name = by_name
        .iter()
        .max_by(|a, b| a.1.total_cmp(b.1).then(b.0.cmp(a.0)))
        .map(|(n, _)| (*n).to_owned())
        .unwrap();
    alt.extend(by_name.keys().copied());
    alt.sort_unstable();
    alt.dedup();
    let alt_names = alt
        .into_iter()
        .filter(|a| *a != name && street_key(a) != key)
        .map(str::to_owned)
        .collect();
    refs.sort_unstable();
    refs.dedup();
    refs.retain(|r| *r != name);
    let along = Along::of(&parts);
    let first_way = members
        .iter()
        .map(|&m| ways[cuts[m as usize].way as usize].id)
        .min()
        .unwrap();
    Built {
        street: Street {
            name,
            alt_names,
            refs,
            class,
            town: String::new(),
            town_source: Source::None,
            admin: None,
            parts,
            along,
        },
        placed,
        first_way,
    }
}

impl Table {
    /// `votes`: `(way, point, city)` for each address record on way `way` that names a
    /// postal town, indexing `cities`.
    pub fn build(
        ways: &[StreetWay],
        admin: &Admin,
        votes: &[(u32, P, u32)],
        cities: &[String],
    ) -> Self {
        let start = Instant::now();
        let mut stats = Stats {
            ways: ways.len() as u64,
            way_vertices: ways.iter().map(|w| w.coords.len() as u64).sum(),
            ..Default::default()
        };

        // Keys of each way's `name`.
        let key_strs: Vec<String> = ways.par_iter().map(|w| street_key(&w.names[0])).collect();
        let mut key_ids: HashMap<&str, u32> = HashMap::new();
        let keys: Vec<u32> = key_strs
            .iter()
            .map(|k| {
                let n = key_ids.len() as u32;
                *key_ids.entry(k.as_str()).or_insert(n)
            })
            .collect();

        // Pieces.
        let cut: Vec<(Vec<Cut>, [u64; 2])> = ways
            .par_iter()
            .enumerate()
            .map(|(w, way)| cut_way(w as u32, way, admin))
            .collect();
        let mut cuts: Vec<Cut> = Vec::new();
        let mut way_start = Vec::with_capacity(ways.len() + 1);
        for (c, n) in cut {
            way_start.push(cuts.len() as u32);
            cuts.extend(c);
            stats.cuts_area += n[0];
            stats.cuts_length += n[1];
        }
        way_start.push(cuts.len() as u32);
        stats.pieces = cuts.len() as u64;
        let key_of = |i: usize| keys[cuts[i].way as usize];

        // Pieces with the same key touch where one's end is any vertex of the other.
        let mut ends: HashMap<(u32, i64), Vec<u32>> = HashMap::new();
        for (i, c) in cuts.iter().enumerate() {
            let r = &ways[c.way as usize].refs;
            for n in [r[c.from as usize], r[c.to as usize]] {
                let v = ends.entry((key_of(i), n)).or_default();
                if v.last() != Some(&(i as u32)) {
                    v.push(i as u32);
                }
            }
        }
        let mut edges: Vec<(u32, u32)> = (0..cuts.len())
            .into_par_iter()
            .flat_map_iter(|i| {
                let c = &cuts[i];
                let r = &ways[c.way as usize].refs[c.from as usize..=c.to as usize];
                let k = key_of(i);
                let mut out = Vec::new();
                for &n in r {
                    for &j in ends.get(&(k, n)).into_iter().flatten() {
                        if j != i as u32 {
                            out.push((i as u32, j));
                            out.push((j, i as u32));
                        }
                    }
                }
                out
            })
            .collect();
        drop(ends);
        edges.par_sort_unstable();
        edges.dedup();
        let mut adj_start = vec![0u32; cuts.len() + 1];
        for &(a, _) in &edges {
            adj_start[a as usize + 1] += 1;
        }
        for i in 0..cuts.len() {
            adj_start[i + 1] += adj_start[i];
        }
        let adj: Vec<u32> = edges.into_iter().map(|(_, b)| b).collect();
        let neighbours = |i: usize| &adj[adj_start[i] as usize..adj_start[i + 1] as usize];

        // The postal town of each piece: the town most of its votes resolve to.
        let nearest_piece = |w: u32, p: P| -> u32 {
            let (a, b) = (way_start[w as usize], way_start[w as usize + 1]);
            if b - a == 1 {
                return a;
            }
            let coords = &ways[w as usize].coords;
            (a..b)
                .min_by(|&x, &y| {
                    let d = |i: u32| {
                        let c = &cuts[i as usize];
                        coords[c.from as usize..=c.to as usize]
                            .windows(2)
                            .map(|s| seg_dist(s[0], s[1], p))
                            .fold(f64::INFINITY, f64::min)
                    };
                    d(x).total_cmp(&d(y))
                })
                .unwrap()
        };
        let mut piece_votes: Vec<(u32, u32)> = votes
            .par_iter()
            .filter(|v| !towns::key(&cities[v.2 as usize]).is_empty())
            .map(|&(w, p, c)| (nearest_piece(w, p), c))
            .collect();
        piece_votes.par_sort_unstable();
        // A town spelling resolves the same way across one deepest admin area.
        let mid = |i: usize| {
            let c = &cuts[i];
            ways[c.way as usize].coords[(c.from as usize + c.to as usize) / 2]
        };
        let piece_chain: HashMap<u32, Vec<u32>> = {
            let mut ps: Vec<u32> = piece_votes.iter().map(|v| v.0).collect();
            ps.dedup();
            ps.into_par_iter()
                .map(|i| (i, admin.chain(mid(i as usize))))
                .collect()
        };
        let mut spellings: Vec<(u32, u32, u32)> = piece_votes
            .iter()
            .map(|&(i, c)| (c, piece_chain[&i].last().copied().unwrap_or(u32::MAX), i))
            .collect();
        spellings.sort_unstable();
        spellings.dedup_by_key(|s| (s.0, s.1));
        let resolved: HashMap<(u32, u32), String> = spellings
            .into_par_iter()
            .map(|(c, deepest, i)| {
                let name = towns::display(
                    admin,
                    &cities[c as usize],
                    &piece_chain[&i],
                    mid(i as usize),
                );
                ((c, deepest), name)
            })
            .collect();
        let mut town_ids: HashMap<String, u32> = HashMap::new();
        let mut town_names: Vec<(String, String)> = Vec::new();
        let mut postal: Vec<Option<u32>> = vec![None; cuts.len()];
        for group in piece_votes.chunk_by(|a, b| a.0 == b.0) {
            let i = group[0].0;
            let deepest = piece_chain[&i].last().copied().unwrap_or(u32::MAX);
            let mut counts: HashMap<&str, (u32, &str)> = HashMap::new();
            let mut keyed: Vec<(String, &str)> = Vec::new();
            for &(_, c) in group {
                let name = &resolved[&(c, deepest)];
                keyed.push((towns::key(name), name));
            }
            for (k, name) in &keyed {
                counts.entry(k).or_insert((0, name)).0 += 1;
            }
            let (k, (_, name)) = counts
                .into_iter()
                .max_by(|a, b| a.1.0.cmp(&b.1.0).then(b.0.cmp(a.0)))
                .unwrap();
            let n = town_names.len() as u32;
            let id = *town_ids.entry(k.to_owned()).or_insert_with(|| {
                town_names.push((name.to_owned(), k.to_owned()));
                n
            });
            postal[i as usize] = Some(id);
        }
        drop((piece_votes, piece_chain, resolved));

        // Labels: the postal town, spread to touching pieces in the same admin 8 area
        // that have none; else the admin 8 area.
        const AREA: u64 = 1 << 32;
        let mut label: Vec<Option<u64>> = postal.iter().map(|p| p.map(u64::from)).collect();
        let mut queue: VecDeque<usize> = (0..cuts.len()).filter(|&i| postal[i].is_some()).collect();
        while let Some(i) = queue.pop_front() {
            for &j in neighbours(i) {
                let j = j as usize;
                if label[j].is_none() && cuts[j].area == cuts[i].area {
                    label[j] = label[i];
                    queue.push_back(j);
                }
            }
        }
        let label: Vec<u64> = label
            .into_iter()
            .enumerate()
            .map(|(i, l)| l.unwrap_or(AREA | u64::from(cuts[i].area.unwrap_or(u32::MAX))))
            .collect();

        // Streets: touching pieces with the same label, up to the length cap.
        let mut street_of = vec![u32::MAX; cuts.len()];
        let mut stamp = vec![u32::MAX; cuts.len()];
        let mut groups: Vec<Vec<u32>> = Vec::new();
        for seed in 0..cuts.len() {
            if street_of[seed] != u32::MAX {
                continue;
            }
            let g = groups.len() as u32;
            let mut members = Vec::new();
            let mut total = 0.0;
            queue.clear();
            queue.push_back(seed);
            stamp[seed] = g;
            while let Some(i) = queue.pop_front() {
                if total >= STREET_MAX_M {
                    stats.capped += 1;
                    break;
                }
                street_of[i] = g;
                members.push(i as u32);
                total += cuts[i].length_m;
                for &j in neighbours(i) {
                    let j = j as usize;
                    if street_of[j] == u32::MAX && stamp[j] != g && label[j] == label[i] {
                        stamp[j] = g;
                        queue.push_back(j);
                    }
                }
            }
            if label[seed] & AREA == 0 {
                stats.by_postal_town += 1;
            } else {
                stats.by_area += 1;
            }
            groups.push(members);
        }
        drop((stamp, street_of, adj, adj_start));

        let mut built: Vec<Built> = groups
            .par_iter()
            .map(|members| {
                let k = &key_strs[cuts[members[0] as usize].way as usize];
                build_street(ways, &cuts, members, k)
            })
            .collect();

        // Towns: the postal town, else the fallbacks (decision 4).
        let rows: Vec<Located> = built
            .par_iter()
            .zip(&groups)
            .map(|(b, members)| {
                let s = &b.street;
                let p = point_at(&s.parts, &s.along, s.along.length_m() / 2.0);
                let chain = admin.chain(p);
                let postal = postal_of(&label, AREA, members[0]).map(|id| town_names[id].clone());
                Located { p, chain, postal }
            })
            .collect();
        let fb = Fallbacks::new(admin, &rows);
        let found: Vec<(String, Source)> = (0..rows.len())
            .into_par_iter()
            .map(|i| match &rows[i].postal {
                Some((name, _)) => (name.clone(), Source::Addresses),
                None => match fb.first(i, &FALLBACK) {
                    Some((m, (name, _))) => (name, m.source()),
                    None => (String::new(), Source::None),
                },
            })
            .collect();
        for ((b, row), (town, source)) in built.iter_mut().zip(&rows).zip(found) {
            b.street.town = town;
            b.street.town_source = source;
            b.street.admin = row.chain.last().copied();
        }

        // Street IDs along a Morton curve of their middle points.
        let mut order: Vec<u32> = (0..built.len() as u32).collect();
        order.par_sort_unstable_by_key(|&i| {
            let b = &built[i as usize];
            (
                morton(rows[i as usize].p),
                keys[cuts[groups[i as usize][0] as usize].way as usize],
                b.first_way,
            )
        });
        drop(rows);
        let mut slots: Vec<Option<Built>> = built.into_iter().map(Some).collect();
        let mut streets = Vec::with_capacity(slots.len());
        let mut pieces: Vec<Option<Piece>> = (0..cuts.len()).map(|_| None).collect();
        for (id, &i) in order.iter().enumerate() {
            let b = slots[i as usize].take().unwrap();
            for (m, part, lo, hi) in b.placed {
                pieces[m as usize] = Some(Piece {
                    street: id as u32,
                    part,
                    lo,
                    hi,
                });
            }
            streets.push(b.street);
        }
        let pieces: Vec<Piece> = pieces.into_iter().map(Option::unwrap).collect();

        stats.streets = streets.len() as u64;
        for s in &streets {
            let n = s.parts.len() as u64;
            stats.parts += n;
            stats.multi_part += u64::from(n > 1);
            stats.max_parts = stats.max_parts.max(n);
            stats.stored_points += s.parts.iter().map(|p| p.len() as u64).sum::<u64>();
            let len = s.along.length_m();
            stats.length_km += len / 1000.0;
            stats.longest_m = stats.longest_m.max(len);
            stats.with_alt_names += u64::from(!s.alt_names.is_empty());
            stats.with_refs += u64::from(!s.refs.is_empty());
            stats.without_admin += u64::from(s.admin.is_none());
            *stats.town_by_source.entry(s.town_source).or_default() += 1;
            let class = HIGHWAYS.get(s.class as usize).copied().unwrap_or("other");
            *stats.classes.entry(class.to_owned()).or_default() += 1;
        }
        stats.build_s = start.elapsed().as_secs_f64();
        Self {
            streets,
            pieces,
            way_start,
            stats,
        }
    }

    /// Where `p` lies on the street holding way `w` (the input index): the nearest of the
    /// way's pieces, projected onto the stored geometry.
    pub fn locate(&self, w: u32, p: P) -> Option<Spot> {
        let (a, b) = (self.way_start[w as usize], self.way_start[w as usize + 1]);
        let mut best: Option<Spot> = None;
        for piece in &self.pieces[a as usize..b as usize] {
            let s = &self.streets[piece.street as usize];
            let part = piece.part as usize;
            let (offset_m, dist_m, left) = project(
                &s.parts[part],
                &s.along.cum[part],
                s.along.mpu,
                piece.lo as usize,
                piece.hi as usize,
                p,
            );
            if best.is_none_or(|b| dist_m < b.dist_m) {
                best = Some(Spot {
                    street: piece.street,
                    offset_m,
                    dist_m,
                    left,
                });
            }
        }
        best
    }

    /// Where `p` lies on street `s`: projected onto the nearest of its parts.
    pub fn project_onto(&self, s: u32, p: P) -> Spot {
        let st = &self.streets[s as usize];
        let mut best = Spot {
            street: s,
            offset_m: 0.0,
            dist_m: f64::INFINITY,
            left: false,
        };
        for (k, part) in st.parts.iter().enumerate() {
            let (offset_m, dist_m, left) =
                project(part, &st.along.cum[k], st.along.mpu, 0, part.len() - 1, p);
            if dist_m < best.dist_m {
                best = Spot {
                    street: s,
                    offset_m,
                    dist_m,
                    left,
                };
            }
        }
        best
    }

    /// The street of each piece, for tests.
    #[cfg(test)]
    fn piece_streets(&self) -> Vec<u32> {
        self.pieces.iter().map(|p| p.street).collect()
    }
}

/// The postal town ID of a street whose first piece is `seed`, if it is labelled by one.
fn postal_of(label: &[u64], area: u64, seed: u32) -> Option<usize> {
    let l = label[seed as usize];
    (l & area == 0).then_some(l as usize)
}

/// Distance in world units from `p` to segment `a`–`b`.
fn seg_dist(a: P, b: P, p: P) -> f64 {
    let (px, py) = (f64::from(p[0]), f64::from(p[1]));
    let (ax, ay) = (f64::from(a[0]), f64::from(a[1]));
    let (dx, dy) = (f64::from(b[0]) - ax, f64::from(b[1]) - ay);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 {
        (((px - ax) * dx + (py - ay) * dy) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (px - ax - t * dx).hypot(py - ay - t * dy)
}

#[derive(Args)]
pub struct StreetsArgs {
    /// OSM extract (.osm.pbf).
    input: PathBuf,

    /// Write the counts and sizes here as JSON.
    #[arg(long)]
    json: Option<PathBuf>,

    /// Uncompressed KiB per `SSTR` and `SGEO` block; several may be given to compare.
    #[arg(long, value_delimiter = ',', default_value = "32")]
    block_kib: Vec<usize>,
}

#[derive(Serialize)]
struct Report {
    kind: &'static str,
    input: String,
    wall_s: f64,
    peak_rss_mib: f64,
    read_s: f64,
    stats: Stats,
    sizes: Vec<encode::Sizes>,
    /// `SGRD` at zooms 13–15, without unattached addresses.
    grid: Vec<grid::Sizes>,
}

/// `omp-builder streets`: the street table of a PBF without addresses (streets split at
/// admin 8 boundaries and the length cap only), with its section sizes.
pub fn run(args: StreetsArgs) -> Result<()> {
    let start = Instant::now();
    let admin = admin::load(&args.input)?;
    let t = Instant::now();
    let ways = osm::read_streets(&args.input)?;
    let read_s = t.elapsed().as_secs_f64();
    eprintln!(
        "  streets: {} ways read in {read_s:.1} s, peak RSS {:.0} MiB",
        ways.len(),
        peak_rss_mib()
    );
    let table = Table::build(&ways, &admin, &[], &[]);
    drop(ways);
    print_stats(&table.stats);
    let mut sizes = Vec::new();
    for &kib in &args.block_kib {
        let (_, _, s) = encode::sections(&table, kib << 10)?;
        print_sizes(&s);
        sizes.push(s);
    }
    let mut grids = Vec::new();
    for z in [grid::ZOOM_MEASURED, &[omp_core::grid::ZOOM]].concat() {
        let (_, s) = grid::section(&table, &[], z, grid::BLOCK)?;
        grid::print_sizes(&s);
        grids.push(s);
    }
    let report = Report {
        kind: "streets",
        input: args.input.display().to_string(),
        wall_s: start.elapsed().as_secs_f64(),
        peak_rss_mib: peak_rss_mib(),
        read_s,
        stats: table.stats,
        sizes,
        grid: grids,
    };
    if let Some(path) = &args.json {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(&report)?)
            .with_context(|| format!("write {}", path.display()))?;
    }
    eprintln!(
        "streets: {:.0} s, peak RSS {:.0} MiB",
        report.wall_s, report.peak_rss_mib
    );
    Ok(())
}

pub fn print_stats(s: &Stats) {
    eprintln!(
        "  streets: {} ways ({} vertices) → {} pieces ({} cut at admin 8, {} at {} km) → {} \
         streets ({} by postal town, {} by area, {} capped), {} parts ({} streets with \
         several, at most {}), {} stored points, {:.0} km, longest {:.1} km; {:.1} s",
        s.ways,
        s.way_vertices,
        s.pieces,
        s.cuts_area,
        s.cuts_length,
        PIECE_MAX_M / 1000.0,
        s.streets,
        s.by_postal_town,
        s.by_area,
        s.capped,
        s.parts,
        s.multi_part,
        s.max_parts,
        s.stored_points,
        s.length_km,
        s.longest_m / 1000.0,
        s.build_s
    );
    let towns: Vec<String> = s
        .town_by_source
        .iter()
        .map(|(k, v)| format!("{k:?} {v}"))
        .collect();
    eprintln!(
        "  streets: towns {}; {} without admin area, {} with alternate names, {} with refs",
        towns.join(", "),
        s.without_admin,
        s.with_alt_names,
        s.with_refs
    );
}

pub fn print_sizes(s: &encode::Sizes) {
    let mib = |b: u64| b as f64 / (1u64 << 20) as f64;
    eprintln!(
        "  streets: {} KiB blocks: SSTR {:.2} MiB ({:.2} raw, {} blocks), SGEO {:.2} MiB \
         ({:.2} raw, {} blocks), {:.1} + {:.1} bytes per street",
        s.block_kib,
        mib(s.sstr),
        mib(s.sstr_raw),
        s.sstr_blocks,
        mib(s.sgeo),
        mib(s.sgeo_raw),
        s.sgeo_blocks,
        s.sstr as f64 / s.streets.max(1) as f64,
        s.sgeo as f64 / s.streets.max(1) as f64
    );
}

#[cfg(test)]
mod tests;
