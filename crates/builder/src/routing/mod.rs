//! The routing sections of the pack (phase 3 step 3.3, `docs/format.md`): the car graph
//! ([`crate::graph`]) and its contraction hierarchy ([`crate::ch`]), renumbered for cold
//! reads, with the segments' geometry and names, the grid that finds them, the node copies
//! of turn restrictions and the exit numbers. `omp_core::route` reads them.
//!
//! **Node order.** The top of the hierarchy comes first, highest first, as many nodes as
//! fit in about [`Params::top_mib`], so one read at the start of `RCHE` brings them into the
//! cache; the rest follow a Hilbert
//! curve of their position, so a local search and the unpacking of a route read nearby
//! blocks. Segments are ordered by the Hilbert key of their first point's z14 cell, so each
//! cell's own segments are one run of IDs (`RGRD`).
//!
//! [`layout`] compares this layout with fixed-width records and with the middle nodes kept
//! apart, by the pages a route touches.

pub mod layout;
#[cfg(test)]
mod tests;

use std::time::Instant;

use anyhow::{Result, ensure};
use omp_core::pack::Writer;
use omp_core::profile;
use omp_core::route::{self as core, GRID_ZOOM, Header, SHORTCUT};
use omp_core::tile::hilbert;
use omp_core::varint::{put, put_str, zigzag};
use rayon::prelude::*;
use serde::Serialize;

use crate::ch::{self, AVOID, Ch};
use crate::geom::P;
use crate::graph::{self, Graph};
use crate::street::WayStreets;
use crate::street::grid::segment_cells;

/// zstd level of the compressed routing blocks, as the street sections'.
const LEVEL: i32 = 19;
/// Uncompressed bytes per block of the zstd sections, about.
pub const BLOCK: usize = 16 << 10;

/// The layout's parameters, chosen in step 3.3 from the pages queries touch (see
/// `docs/phase3-plan.md`).
#[derive(Clone, Copy, Debug, Serialize)]
pub struct Params {
    /// Nodes per `RCHE` block.
    pub block_nodes: u32,
    /// The top of the hierarchy, stored first: about this many MiB of `RCHE`.
    pub top_mib: f64,
}

impl Default for Params {
    fn default() -> Self {
        Params {
            block_nodes: 32,
            top_mib: 32.0,
        }
    }
}

/// A first guess at how many of the highest nodes fit in `mib` of `RCHE`, at 2 bytes per
/// node and 5 per edge; [`order_with_top`] corrects it.
fn top_guess(h: &Ch, mib: f64) -> u32 {
    let n = h.rank.len();
    let mut by_rank = vec![0u32; n];
    for (v, &r) in h.rank.iter().enumerate() {
        by_rank[r as usize] = v as u32;
    }
    let budget = (mib * f64::from(1 << 20)) as u64;
    let mut bytes = 0u64;
    let mut top = 0;
    for &v in by_rank.iter().rev() {
        bytes += 2 + 5 * (h.up.at(v).len() + h.down.at(v).len()) as u64;
        if bytes > budget {
            break;
        }
        top += 1;
    }
    top
}

#[derive(Serialize, Default, Clone)]
pub struct Sizes {
    pub nodes: u64,
    pub edges: u64,
    pub segments: u64,
    pub labels: u64,
    pub cells: u64,
    pub cross_pairs: u64,
    pub copies: u64,
    pub junctions: u64,
    pub segment_points: u64,
    pub segments_with_street: u64,
    pub segments_with_label: u64,
    pub top_nodes: u64,
    pub top_bytes: u64,
    /// Section bytes, index included (padding at chunk boundaries not).
    pub rhdr: u64,
    pub rche: u64,
    pub rgeo: u64,
    pub rgeo_raw: u64,
    pub rnam: u64,
    pub rgrd: u64,
    pub rcpy: u64,
    pub rjct: u64,
    pub encode_s: f64,
}

impl Sizes {
    pub fn total(&self) -> u64 {
        self.rhdr + self.rche + self.rgeo + self.rnam + self.rgrd + self.rcpy + self.rjct
    }
}

/// Blocks of a blocked section, `(first, bytes)`.
type Blocks = Vec<(u32, Vec<u8>)>;

/// A blocked section: its count and its blocks, `(first, bytes)`.
pub struct BlockedSection {
    pub id: [u8; 4],
    pub count: u32,
    pub blocks: Blocks,
}

impl BlockedSection {
    fn bytes(&self) -> u64 {
        8 + self
            .blocks
            .iter()
            .map(|(_, b)| 12 + b.len() as u64)
            .sum::<u64>()
    }
}

pub struct Sections {
    pub rhdr: Vec<u8>,
    /// `RCHE`'s blocks of nodes.
    pub rche: Vec<Vec<u8>>,
    pub nodes: u32,
    pub blocked: Vec<BlockedSection>,
}

/// `RCHE`'s bytes before its blocks: `nodes:u32 nblocks:u32 offset:u32 × (nblocks + 1)`.
fn rche_head_len(blocks: usize) -> u64 {
    8 + 4 * (blocks as u64 + 1)
}

impl Sections {
    /// Writes them: `RHDR` contiguous, the rest spanning chunks.
    pub fn write(&self, w: &mut Writer) -> Result<Vec<omp_core::pack::Section>> {
        let mut out = vec![w.section(core::RHDR, &self.rhdr)?];
        // RCHE: each block runs to the next one's offset, padding included.
        let head_len = rche_head_len(self.rche.len());
        let lens = std::iter::once(head_len).chain(self.rche.iter().map(|b| b.len() as u64));
        let offsets = w.spanning_offsets(lens);
        let end = offsets.last().unwrap() + self.rche.last().map_or(0, |b| b.len() as u64);
        let mut head = Vec::with_capacity(head_len as usize);
        head.extend_from_slice(&self.nodes.to_le_bytes());
        head.extend_from_slice(&(self.rche.len() as u32).to_le_bytes());
        for &o in offsets[1..].iter().chain([&end]) {
            let o = u32::try_from(o).map_err(|_| anyhow::anyhow!("RCHE is over 4 GiB"))?;
            head.extend_from_slice(&o.to_le_bytes());
        }
        w.begin_spanning(core::RCHE);
        w.record(&head)?;
        for b in &self.rche {
            w.record(b)?;
        }
        out.push(w.end_spanning());
        for b in &self.blocked {
            out.push(core::write_spanning(w, b.id, b.count, &b.blocks)?);
        }
        Ok(out)
    }
}

/// The new node order: `order[new] = old`, and its inverse.
pub struct Order {
    pub order: Vec<u32>,
    pub id: Vec<u32>,
    pub top: u32,
}

/// The top `top` nodes by contraction rank, then the rest, each along a Hilbert curve of
/// their position (which keeps the edges' target deltas small).
pub fn node_order(g: &Graph, rank: &[u32], top: u32) -> Order {
    let n = rank.len() as u32;
    let top = top.min(n);
    let key = |v: u32| {
        let p = g.coords[v as usize];
        (
            rank[v as usize] < n - top,
            hilbert(24, p[0] >> 8, p[1] >> 8),
        )
    };
    let mut order: Vec<u32> = (0..n).collect();
    order.par_sort_unstable_by_key(|&v| (key(v), v));
    let mut id = vec![0u32; n as usize];
    for (new, &old) in order.iter().enumerate() {
        id[old as usize] = new as u32;
    }
    Order { order, id, top }
}

/// The node order with a top of about [`Params::top_mib`]: from [`top_guess`], scaled a few
/// times by what the top's records take once encoded (twice the guess on North America,
/// whose top edges span a continent).
fn order_with_top(g: &Graph, h: &Ch, seg_id: &[u32], p: &Params) -> Order {
    let n = h.rank.len() as u32;
    let budget = p.top_mib * f64::from(1 << 20);
    let mut o = node_order(g, &h.rank, top_guess(h, p.top_mib));
    for _ in 0..4 {
        let blocks = o.top.div_ceil(p.block_nodes);
        let bytes: u64 = (0..blocks)
            .into_par_iter()
            .map(|b| {
                let (mut out, mut seg) = (Vec::new(), 0u32);
                let first = b * p.block_nodes;
                for new in first..(first + p.block_nodes).min(n) {
                    encode_node(g, h, &o, seg_id, new, Some(&mut seg), &mut out);
                }
                out.len() as u64
            })
            .sum();
        let ratio = budget / bytes.max(1) as f64;
        let next = ((f64::from(o.top) * ratio) as u32).min(n);
        if (0.9..=1.0).contains(&ratio) || next == o.top || o.top == n && ratio > 1.0 {
            break;
        }
        o = node_order(g, &h.rank, next);
    }
    o
}

/// Hierarchy edge flags as `RCHE` stores them.
fn ch_flags(f: u8) -> u8 {
    f & AVOID | if f & ch::SHORTCUT != 0 { SHORTCUT } else { 0 }
}

/// One node's `RCHE` record. Each edge carries its middle node (a shortcut's, as a delta
/// from the node) or its segment direction (a base edge's, as a delta from the block's
/// previous one, `seg`), so unpacking reads only the middle nodes. Without `seg`, it leaves
/// them out, as a layout with a separate array of them would ([`layout`]).
fn encode_node(
    g: &Graph,
    h: &Ch,
    o: &Order,
    seg_id: &[u32],
    new: u32,
    mut seg: Option<&mut u32>,
    out: &mut Vec<u8>,
) {
    let v = o.order[new as usize];
    let lists = [h.up.at(v), h.down.at(v)];
    put(
        out,
        (lists[0].len() as u64) << 1 | u64::from(g.small[v as usize]),
    );
    put(out, lists[1].len() as u64);
    for list in lists {
        let mut edges: Vec<(u32, &ch::ChEdge)> =
            list.iter().map(|e| (o.id[e.other as usize], e)).collect();
        edges.sort_unstable_by_key(|(t, e)| (*t, e.weight, e.flags));
        let mut prev = new;
        for (t, e) in edges {
            put(out, zigzag(t as i32 - prev as i32));
            put(out, u64::from(e.weight) << 3 | u64::from(ch_flags(e.flags)));
            prev = t;
            let Some(seg) = seg.as_deref_mut() else {
                continue;
            };
            match e.flags & ch::SHORTCUT {
                0 => {
                    let s = g.csr.edges[e.mid as usize].seg;
                    let m = seg_id[(s >> 1) as usize] << 1 | (s & 1);
                    put(out, zigzag(m as i32 - *seg as i32));
                    *seg = m;
                }
                _ => put(out, zigzag(o.id[e.mid as usize] as i32 - new as i32)),
            }
        }
    }
}

/// Each node's record length, as written and with the middle nodes left out (for
/// [`layout`]).
pub struct Lens {
    pub apart: Vec<u32>,
    pub inline: Vec<u32>,
}

/// `RCHE`'s blocks, its edge count and each node's record length.
fn hierarchy(
    g: &Graph,
    h: &Ch,
    o: &Order,
    seg_id: &[u32],
    block_nodes: u32,
) -> (Vec<Vec<u8>>, u64, Lens) {
    let n = o.order.len() as u32;
    let starts: Vec<u32> = (0..n).step_by(block_nodes as usize).collect();
    let blocks: Vec<(Vec<u8>, Vec<u32>, Vec<u32>)> = starts
        .par_iter()
        .map(|&first| {
            let (mut out, mut inline, mut apart) = (Vec::new(), Vec::new(), Vec::new());
            let (mut scratch, mut seg) = (Vec::new(), 0u32);
            for new in first..(first + block_nodes).min(n) {
                let before = out.len();
                encode_node(g, h, o, seg_id, new, Some(&mut seg), &mut out);
                inline.push((out.len() - before) as u32);
                scratch.clear();
                encode_node(g, h, o, seg_id, new, None, &mut scratch);
                apart.push(scratch.len() as u32);
            }
            (out, inline, apart)
        })
        .collect();
    let mut lens = Lens {
        apart: Vec::with_capacity(n as usize),
        inline: Vec::with_capacity(n as usize),
    };
    let mut list = Vec::with_capacity(blocks.len());
    for (b, inline, apart) in blocks {
        list.push(b);
        lens.inline.extend(inline);
        lens.apart.extend(apart);
    }
    let edges = o
        .order
        .iter()
        .map(|&v| (h.up.at(v).len() + h.down.at(v).len()) as u64)
        .sum();
    (list, edges, lens)
}

/// Per segment, the indices in `g.csr.edges` of its forward and backward edges out of the
/// plain node at its start (`u32::MAX` for none). Every edge out of a plain node is kept by
/// the restrictions, so each direction has one.
pub(crate) fn segment_edges(g: &Graph) -> Vec<[u32; 2]> {
    let mut out = vec![[u32::MAX; 2]; g.segments.len()];
    for v in 0..g.base_nodes {
        let first = g.csr.first[v as usize];
        for (i, e) in g.csr.out(v).iter().enumerate() {
            out[(e.seg >> 1) as usize][(e.seg & 1) as usize] = first + i as u32;
        }
    }
    out
}

/// The node a CSR edge leaves.
pub(crate) fn source(g: &Graph, edge: u32) -> u32 {
    g.csr.first.partition_point(|&f| f <= edge) as u32 - 1
}

pub(crate) fn plain(g: &Graph, v: u32) -> u32 {
    match v < g.base_nodes {
        true => v,
        false => g.copy_of[(v - g.base_nodes) as usize],
    }
}

/// Segments in their new order: by the Hilbert key of their first point's grid cell, then
/// along a finer curve. Returns `order[new] = old`, the inverse, and each one's home key.
pub(crate) fn segment_order(g: &Graph) -> (Vec<u32>, Vec<u32>, Vec<u32>) {
    let s = 32 - u32::from(GRID_ZOOM);
    let keys: Vec<(u32, u64)> = (0..g.segments.len() as u32)
        .into_par_iter()
        .map(|i| {
            let p = g.points(i)[0];
            (
                core::cell_key(p[0] >> s, p[1] >> s),
                hilbert(24, p[0] >> 8, p[1] >> 8),
            )
        })
        .collect();
    let mut order: Vec<u32> = (0..keys.len() as u32).collect();
    order.par_sort_unstable_by_key(|&i| (keys[i as usize], i));
    let mut id = vec![0u32; order.len()];
    for (new, &old) in order.iter().enumerate() {
        id[old as usize] = new as u32;
    }
    let home = order.iter().map(|&i| keys[i as usize].0).collect();
    (order, id, home)
}

fn direction(g: &Graph, o: &Order, edge: u32, end: u32, out: &mut Vec<u8>) {
    let e = &g.csr.edges[edge as usize];
    put(out, u64::from(e.time_ds));
    let uturn = match e.uturn_ds {
        0 => 0,
        _ => core::UTURN,
    };
    put(out, u64::from(e.flags | uturn));
    let head = o.id[e.to as usize];
    put(
        out,
        match head == end {
            true => 0,
            false => u64::from(head) + 1,
        },
    );
}

/// Cuts records into blocks of about [`BLOCK`] bytes and compresses them. `records` are
/// `(first key of a block starting here, may a block start here)`; `enc` appends record `i`
/// and is told when a block starts, to reset its cursors.
fn zstd_blocks(
    n: usize,
    key: impl Fn(usize) -> u32,
    can_start: impl Fn(usize) -> bool,
    mut enc: impl FnMut(usize, bool, &mut Vec<u8>),
) -> Result<(Blocks, u64)> {
    let mut raw = Vec::new();
    let mut cur = Vec::new();
    let mut first = 0u32;
    for i in 0..n {
        let start = i == 0 || cur.len() >= BLOCK && can_start(i);
        if start && i > 0 {
            raw.push((first, std::mem::take(&mut cur)));
        }
        if start {
            first = key(i);
        }
        enc(i, start, &mut cur);
    }
    if !cur.is_empty() {
        raw.push((first, cur));
    }
    let bytes = raw.iter().map(|(_, b)| b.len() as u64).sum();
    let blocks = raw
        .into_par_iter()
        .map(|(f, b)| Ok((f, zstd::bulk::compress(&b, LEVEL)?)))
        .collect::<Result<_>>()?;
    Ok((blocks, bytes))
}

/// The routing sections of `g` and its hierarchy `h`. `streets` names the segments.
pub fn sections(
    g: &Graph,
    h: &Ch,
    streets: &WayStreets,
    p: &Params,
) -> Result<(Sections, Sizes, Order, Lens)> {
    let start = Instant::now();
    let n = g.csr.nodes() as u32;
    ensure!(
        h.rank.len() == n as usize,
        "the hierarchy is not of this graph"
    );
    let (seg_order, seg_id, home) = segment_order(g);
    let o = order_with_top(g, h, &seg_id, p);

    let (rche, edges, lens) = hierarchy(g, h, &o, &seg_id, p.block_nodes);
    let top_blocks = (o.top.div_ceil(p.block_nodes) as usize).min(rche.len());
    let top_bytes = rche[..top_blocks].iter().map(|b| b.len() as u64).sum();

    // RGEO.
    let ends = segment_edges(g);
    let mut sizes = Sizes::default();
    let (mut prev_a, mut cur) = (0u32, [0i64; 2]);
    let (rgeo_blocks, rgeo_raw) = zstd_blocks(
        seg_order.len(),
        |i| i as u32,
        |_| true,
        |i, start, out| {
            if start {
                (prev_a, cur) = (0, [0, 0]);
            }
            let old = seg_order[i] as usize;
            let [fwd, bwd] = ends[old];
            let from = source(g, fwd);
            let fe = &g.csr.edges[fwd as usize];
            let (a, b) = (o.id[from as usize], o.id[plain(g, fe.to) as usize]);
            let seg = &g.segments[old];
            let info = &g.way_info[seg.way as usize];
            let street = streets.street(g.ways[seg.way as usize], seg.vertex);
            let pts = g.points(old as u32);
            sizes.segment_points += pts.len() as u64;
            sizes.segments_with_street += u64::from(street.is_some());
            sizes.segments_with_label += u64::from(info.label != 0);
            put(out, zigzag(a as i32 - prev_a as i32));
            put(out, zigzag(b as i32 - a as i32));
            prev_a = a;
            let bits = u64::from(info.class) << 3
                | u64::from(info.roundabout) << 2
                | u64::from(g.small[from as usize]) << 1
                | u64::from(bwd == u32::MAX);
            put(out, bits);
            put(out, street.map_or(0, |s| u64::from(s) + 1));
            put(out, u64::from(info.label));
            put(out, (f64::from(seg.metres) * 10.0).round() as u64);
            direction(g, &o, fwd, b, out);
            if bwd != u32::MAX {
                direction(g, &o, bwd, a, out);
            }
            put(out, pts.len() as u64);
            for q in &pts {
                let q = [
                    i64::from(q[0] >> core::SHIFT),
                    i64::from(q[1] >> core::SHIFT),
                ];
                put(out, zigzag((q[0] - cur[0]) as i32));
                put(out, zigzag((q[1] - cur[1]) as i32));
                cur = q;
            }
        },
    )?;
    let rgeo = BlockedSection {
        id: core::RGEO,
        count: seg_order.len() as u32,
        blocks: rgeo_blocks,
    };

    // RNAM: label 0 is the empty one, so a segment's label field is its index (0 = none).
    let labels = &g.labels;
    let (rnam_blocks, _) = zstd_blocks(
        labels.len(),
        |i| i as u32,
        |_| true,
        |i, _, out| {
            for f in labels[i].fields() {
                put_str(out, f);
            }
        },
    )?;
    let rnam = BlockedSection {
        id: core::RNAM,
        count: labels.len() as u32,
        blocks: rnam_blocks,
    };

    let (rgrd, cells, cross_pairs) = grid(g, &seg_order, &home)?;
    let (rcpy, copies) = copies(g, &o, &seg_id)?;
    let (rjct, junctions) = junctions(g, &o)?;

    let mut settings = serde_json::json!({
        "simplify_m": graph::SIMPLIFY_M,
        "maxspeed_factor": profile::MAXSPEED_FACTOR,
        "signal_s": profile::SIGNAL_S,
        "ferry_kmh": profile::FERRY_KMH,
        "ferry_boarding_s": profile::FERRY_BOARDING_S,
        "restricted_penalty_s": profile::RESTRICTED_PENALTY_S,
        "small_component": graph::SMALL_COMPONENT,
    })
    .as_object()
    .cloned()
    .unwrap_or_default();
    if g.stats.uturn_penalty_s > 0 {
        settings.insert(core::UTURN_SETTING.into(), g.stats.uturn_penalty_s.into());
    }
    let header = Header {
        nodes: n,
        edges: edges as u32,
        segments: seg_order.len() as u32,
        labels: labels.len() as u32,
        block_nodes: p.block_nodes,
        top_nodes: o.top,
        top_bytes,
        settings,
    };
    let rhdr = header.encode()?;
    sizes = Sizes {
        nodes: u64::from(n),
        edges,
        segments: seg_order.len() as u64,
        labels: labels.len() as u64,
        cells,
        cross_pairs,
        copies,
        junctions,
        top_nodes: u64::from(o.top),
        top_bytes,
        rhdr: rhdr.len() as u64,
        rche: rche_head_len(rche.len()) + rche.iter().map(|b| b.len() as u64).sum::<u64>(),
        rgeo: rgeo.bytes(),
        rgeo_raw,
        rnam: rnam.bytes(),
        rgrd: rgrd.bytes(),
        rcpy: rcpy.bytes(),
        rjct: rjct.bytes(),
        encode_s: start.elapsed().as_secs_f64(),
        ..sizes
    };
    let s = Sections {
        rhdr,
        rche,
        nodes: n,
        blocked: vec![rgeo, rnam, rgrd, rcpy, rjct],
    };
    Ok((s, sizes, o, lens))
}

/// `RGRD`: per cell, its own segments (a run of IDs) and the others that pass through it.
fn grid(g: &Graph, seg_order: &[u32], home: &[u32]) -> Result<(BlockedSection, u64, u64)> {
    let mut pairs: Vec<(u32, u32)> = seg_order
        .par_iter()
        .enumerate()
        .flat_map_iter(|(new, &old)| {
            let pts: Vec<P> = g.points(old);
            let mut c = Vec::new();
            for w in pts.windows(2) {
                segment_cells(w[0], w[1], GRID_ZOOM, &mut c);
            }
            c.sort_unstable();
            c.dedup();
            let h = home[new];
            c.into_iter()
                .map(|(x, y)| core::cell_key(x, y))
                .filter(move |&k| k != h)
                .map(move |k| (k, new as u32))
        })
        .collect();
    pairs.par_sort_unstable();
    pairs.dedup();
    let cross_pairs = pairs.len() as u64;
    // Cells: (key, first home, home count, crossing segments).
    let mut cells: Vec<(u32, u32, u32, Vec<u32>)> = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < home.len() || j < pairs.len() {
        let k = match (home.get(i), pairs.get(j)) {
            (Some(&h), Some(p)) => h.min(p.0),
            (Some(&h), None) => h,
            (None, Some(p)) => p.0,
            (None, None) => unreachable!(),
        };
        let first = i as u32;
        while home.get(i) == Some(&k) {
            i += 1;
        }
        let mut cross = Vec::new();
        while let Some(&(_, s)) = pairs.get(j).filter(|p| p.0 == k) {
            cross.push(s);
            j += 1;
        }
        cells.push((k, first, i as u32 - first, cross));
    }
    let (mut prev_key, mut prev_cross) = (0u32, 0u32);
    let (blocks, _) = zstd_blocks(
        cells.len(),
        |i| cells[i].0,
        |_| true,
        |i, start, out| {
            let (k, first, nhome, cross) = &cells[i];
            if start {
                put(out, u64::from(*first));
                (prev_key, prev_cross) = (*k, 0);
            }
            put(out, u64::from(k - prev_key));
            prev_key = *k;
            put(out, u64::from(*nhome));
            put(out, cross.len() as u64);
            if let Some(&c0) = cross.first() {
                put(out, zigzag(c0 as i32 - prev_cross as i32));
                prev_cross = c0;
                for w in cross.windows(2) {
                    put(out, u64::from(w[1] - w[0]));
                }
            }
        },
    )?;
    let n = cells.len() as u64;
    Ok((
        BlockedSection {
            id: core::RGRD,
            count: n as u32,
            blocks,
        },
        n,
        cross_pairs,
    ))
}

/// `RCPY`: per plain node, its copies and the segment directions each may leave by. With a
/// U-turn penalty, each direction is `dir << 1 | uturn`.
fn copies(g: &Graph, o: &Order, seg_id: &[u32]) -> Result<(BlockedSection, u64)> {
    let uturns = g.stats.uturn_penalty_s > 0;
    let mut list: Vec<(u32, u32, Vec<u32>)> = g
        .copy_of
        .par_iter()
        .enumerate()
        .map(|(i, &p)| {
            let c = g.base_nodes + i as u32;
            let mut leaves: Vec<u32> = g
                .csr
                .out(c)
                .iter()
                .map(|e| {
                    let dir = seg_id[(e.seg >> 1) as usize] << 1 | (e.seg & 1);
                    match uturns {
                        true => dir << 1 | u32::from(e.uturn_ds > 0),
                        false => dir,
                    }
                })
                .collect();
            leaves.sort_unstable();
            leaves.dedup();
            (o.id[p as usize], o.id[c as usize], leaves)
        })
        .collect();
    list.par_sort_unstable();
    let mut prev = 0u32;
    let (blocks, _) = zstd_blocks(
        list.len(),
        |i| list[i].0,
        |i| list[i].0 != list[i - 1].0,
        |i, start, out| {
            let (p, c, leaves) = &list[i];
            if start {
                prev = *p;
            }
            put(out, u64::from(p - prev));
            prev = *p;
            put(out, zigzag(*c as i32 - *p as i32));
            put(out, leaves.len() as u64);
            let mut last = 0;
            for (k, &l) in leaves.iter().enumerate() {
                put(out, u64::from(if k == 0 { l } else { l - last }));
                last = l;
            }
        },
    )?;
    let n = list.len() as u64;
    Ok((
        BlockedSection {
            id: core::RCPY,
            count: n as u32,
            blocks,
        },
        n,
    ))
}

/// `RJCT`: the exit number at each junction node and its copies.
fn junctions(g: &Graph, o: &Order) -> Result<(BlockedSection, u64)> {
    let mut copies: Vec<(u32, u32)> = g
        .copy_of
        .iter()
        .enumerate()
        .map(|(i, &p)| (p, g.base_nodes + i as u32))
        .collect();
    copies.sort_unstable();
    let mut list: Vec<(u32, &str)> = Vec::new();
    for (v, r) in &g.junctions {
        list.push((o.id[*v as usize], r));
        let from = copies.partition_point(|c| c.0 < *v);
        for &(_, c) in copies[from..].iter().take_while(|c| c.0 == *v) {
            list.push((o.id[c as usize], r));
        }
    }
    list.sort_unstable();
    let mut prev = 0u32;
    let (blocks, _) = zstd_blocks(
        list.len(),
        |i| list[i].0,
        |_| true,
        |i, start, out| {
            let (v, r) = list[i];
            if start {
                prev = v;
            }
            put(out, u64::from(v - prev));
            prev = v;
            put_str(out, r);
        },
    )?;
    let n = list.len() as u64;
    Ok((
        BlockedSection {
            id: core::RJCT,
            count: n as u32,
            blocks,
        },
        n,
    ))
}

pub fn print_sizes(s: &Sizes) {
    let m = crate::mib;
    eprintln!(
        "  routing: {} nodes ({} at the top, {:.1} MiB), {} edges, {} segments ({} points, {} \
         with a street, {} with a label), {} labels, {} cells ({} crossings), {} copies, {} \
         exit numbers; {:.1} s",
        s.nodes,
        s.top_nodes,
        m(s.top_bytes),
        s.edges,
        s.segments,
        s.segment_points,
        s.segments_with_street,
        s.segments_with_label,
        s.labels,
        s.cells,
        s.cross_pairs,
        s.copies,
        s.junctions,
        s.encode_s
    );
    eprintln!(
        "  routing: RCHE {:.1} MiB, RGEO {:.1} ({:.1} raw), RNAM {:.2}, RGRD {:.2}, \
         RCPY {:.2}, RJCT {:.2}, RHDR {:.3}; {:.1} MiB in all",
        m(s.rche),
        m(s.rgeo),
        m(s.rgeo_raw),
        m(s.rnam),
        m(s.rgrd),
        m(s.rcpy),
        m(s.rjct),
        m(s.rhdr),
        m(s.total())
    );
}
