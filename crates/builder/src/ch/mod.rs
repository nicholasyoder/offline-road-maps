//! The contraction hierarchy (phase 3 step 3.2) of the car graph ([`crate::graph`]).
//!
//! Nodes are contracted in rounds ([`contract`]). Each round takes the remaining nodes whose
//! priority is lower than every neighbour's and among the lowest 1% ([`Params::share`]; a
//! larger share gives more shortcuts), an independent set, and contracts them across
//! threads: the witness searches ([`witness`]) skip the whole set, so they see only nodes
//! that stay, and the shortcuts are added after. Then the neighbours' priorities are
//! computed again. The priority is the edge difference (shortcuts added minus arcs removed),
//! the contracted neighbours and the depth, each weighted ([`Params`]).
//!
//! The avoid flags ([`AVOID`]: tolls and ferries, decision 3) stay on the arcs: a shortcut
//! carries the union of its two arcs' flags, and only a witness with no flag the shortcut
//! lacks removes it. Parallel arcs between two nodes are kept while neither dominates the
//! other (shorter with fewer flags), so a toll road and the free road beside it both stay.
//! One hierarchy answers every combination of avoided flags exactly.
//!
//! [`check`] compares queries with Dijkstra on the graph and with `fast_paths`, and holds
//! `omp-builder ch`.

pub mod check;
#[cfg(test)]
mod tests;
mod witness;

use std::time::Instant;

use omp_core::profile::{FERRY, TOLL};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::graph::Csr;
use crate::peak_rss_mib;
use witness::{Search, Shortcut};

/// The flags a route can avoid.
pub const AVOID: u8 = TOLL | FERRY;
/// [`ChEdge::flags`]: a shortcut, whose `mid` is its middle node; otherwise `mid` is the
/// base edge's index in [`Csr::edges`].
pub const SHORTCUT: u8 = 0x40;
/// [`Arc::bits`]: the arc leaves the node whose list holds it, or enters it.
const OUT: u8 = 0x10;
const IN: u8 = 0x20;

/// An arc of the graph being contracted, kept in the lists of both its ends.
#[derive(Clone, Copy, Debug)]
pub struct Arc {
    other: u32,
    weight: u32,
    mid: u32,
    /// Avoid flags, [`SHORTCUT`], and [`OUT`] or [`IN`].
    bits: u8,
}

impl Arc {
    fn same_pair(&self, b: &Arc) -> bool {
        self.other == b.other && (self.bits ^ b.bits) & (OUT | IN) == 0
    }

    fn dominates(&self, b: &Arc) -> bool {
        self.weight <= b.weight && self.bits & AVOID & !b.bits == 0
    }
}

/// Adds `a` to `list` unless an arc between the same nodes in the same direction dominates
/// it, and removes those it dominates.
fn insert(list: &mut Vec<Arc>, a: Arc) {
    if list.iter().any(|b| b.same_pair(&a) && b.dominates(&a)) {
        return;
    }
    list.retain(|b| !(b.same_pair(&a) && a.dominates(b)));
    list.push(a);
}

/// The weights of the priority terms, the witness search limit and the round size. The
/// defaults were tuned on Ohio (see step 3.2 in `docs/phase3-plan.md`).
#[derive(Clone, Copy, Debug, Serialize)]
pub struct Params {
    pub edge_difference: i64,
    pub contracted_neighbours: i64,
    pub depth: i64,
    /// Nodes a witness search settles at most.
    pub settled: usize,
    /// The share of the remaining nodes, lowest priority first, a round may contract.
    pub share: f64,
    /// Keep the avoid flags; without them, the hierarchy has one metric (to measure the
    /// flags' cost).
    pub flags: bool,
}

impl Default for Params {
    fn default() -> Self {
        Params {
            edge_difference: 8,
            contracted_neighbours: 2,
            depth: 4,
            settled: 500,
            share: 0.01,
            flags: true,
        }
    }
}

/// An edge of the hierarchy, stored at its lower node.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChEdge {
    /// The higher node.
    pub other: u32,
    /// Tenths of a second, as [`crate::graph::Edge::weight`].
    pub weight: u32,
    /// The middle node of a shortcut, or the base edge.
    pub mid: u32,
    /// Avoid flags and [`SHORTCUT`].
    pub flags: u8,
}

/// Edges grouped by their lower node: `v`'s are `edges[first[v]..first[v + 1]]`.
#[derive(Default, Serialize, Deserialize)]
pub struct ChCsr {
    pub first: Vec<u32>,
    pub edges: Vec<ChEdge>,
}

impl ChCsr {
    pub fn at(&self, v: u32) -> &[ChEdge] {
        &self.edges[self.first[v as usize] as usize..self.first[v as usize + 1] as usize]
    }
}

#[derive(Serialize, Deserialize)]
pub struct Ch {
    /// Contraction order: `v` is the `rank[v]`-th node contracted.
    pub rank: Vec<u32>,
    /// Edges from a node up to higher ones (the forward search's), and edges into a node
    /// from higher ones (the backward search's), by the lower node.
    pub up: ChCsr,
    pub down: ChCsr,
    pub stats: Stats,
}

#[derive(Default, Serialize, Deserialize)]
pub struct Stats {
    pub nodes: u64,
    /// The graph's edges, and the arcs left of them when parallel ones are pruned.
    pub base_edges: u64,
    pub base_arcs: u64,
    pub up_edges: u64,
    pub down_edges: u64,
    pub shortcuts: u64,
    /// Edges with a toll or ferry flag, shortcuts among them.
    pub flagged_edges: u64,
    pub flagged_shortcuts: u64,
    pub rounds: u64,
    pub max_depth: u32,
    pub max_weight_ds: u32,
    /// Phase 0's estimate: 12 bytes per node and per edge.
    pub bytes_estimate: u64,
    pub contract_s: f64,
    pub peak_rss_mib: f64,
}

/// A node's tie-breaker, so neighbours with equal priority do not all wait on each other.
fn mix(v: u32) -> u32 {
    let mut x = v.wrapping_mul(0x9e37_79b9);
    x ^= x >> 16;
    x = x.wrapping_mul(0x85eb_ca6b);
    x ^ x >> 13
}

/// Nodes per chunk when lists are updated in parallel.
const CHUNK: usize = 1 << 14;

/// Contracts `g`.
pub fn contract(g: &Csr, p: &Params) -> Ch {
    let start = Instant::now();
    let n = g.nodes();
    let mask = if p.flags { AVOID } else { 0 };
    let mut lists = arcs(g, mask);
    let base_arcs = lists.par_iter().map(|l| l.len() as u64).sum::<u64>() / 2;
    eprintln!(
        "  ch: {n} nodes, {base_arcs} arcs, {:.1} s, peak RSS {:.0} MiB",
        start.elapsed().as_secs_f64(),
        peak_rss_mib()
    );

    let mut rank = vec![u32::MAX; n];
    let mut depth = vec![0u32; n];
    let mut neighbours = vec![0u32; n];
    let mut batch_mark = vec![false; n];
    let mut dirty = vec![false; n];
    let priority = |lists: &[Vec<Arc>], v: u32, depth: u32, neighbours: u32, s: &mut Search| {
        let mut out = Vec::new();
        witness::shortcuts(lists, v, p.settled, &|_| false, s, &mut out);
        // Each shortcut is an arc at both its ends, as each removed arc was.
        p.edge_difference * (out.len() as i64 - lists[v as usize].len() as i64)
            + p.contracted_neighbours * i64::from(neighbours)
            + p.depth * i64::from(depth)
    };
    let mut prio: Vec<i64> = (0..n as u32)
        .into_par_iter()
        .map_init(Search::default, |s, v| priority(&lists, v, 0, 0, s))
        .collect();
    let mut remaining: Vec<u32> = (0..n as u32).collect();
    let mut next_rank = 0u32;
    let mut stats = Stats {
        nodes: n as u64,
        base_edges: g.edges.len() as u64,
        base_arcs,
        ..Stats::default()
    };
    let mut report = Instant::now();
    let mut added = 0u64;
    while !remaining.is_empty() {
        let key = |v: u32| (prio[v as usize], mix(v), v);
        let mut ps: Vec<i64> = remaining.par_iter().map(|&v| prio[v as usize]).collect();
        let k = ((ps.len() as f64 * p.share) as usize).min(ps.len() - 1);
        let cut = *ps.select_nth_unstable(k).1;
        drop(ps);
        let batch: Vec<u32> = remaining
            .par_iter()
            .copied()
            .filter(|&v| {
                prio[v as usize] <= cut && lists[v as usize].iter().all(|a| key(v) < key(a.other))
            })
            .collect();
        for &v in &batch {
            batch_mark[v as usize] = true;
        }
        let skip = |x: u32| batch_mark[x as usize];
        let shortcuts: Vec<Shortcut> = batch
            .par_iter()
            .map_init(Search::default, |s, &v| {
                let mut out = Vec::new();
                witness::shortcuts(&lists, v, p.settled, &skip, s, &mut out);
                out
            })
            .flatten()
            .collect();
        let mut touched = Vec::new();
        for &v in &batch {
            rank[v as usize] = next_rank;
            next_rank += 1;
            for a in &lists[v as usize] {
                let o = a.other as usize;
                depth[o] = depth[o].max(depth[v as usize] + 1);
                neighbours[o] += 1;
                if !dirty[o] {
                    dirty[o] = true;
                    touched.push(a.other);
                }
            }
        }
        touched.par_sort_unstable();
        added += shortcuts.len() as u64;
        let mut adds: Vec<(u32, Arc)> = shortcuts.iter().flat_map(Shortcut::arcs).collect();
        drop(shortcuts);
        adds.par_sort_unstable_by_key(|(v, a)| (*v, a.other, a.bits, a.weight, a.mid));
        lists
            .par_chunks_mut(CHUNK)
            .enumerate()
            .for_each(|(c, chunk)| {
                let (lo, hi) = ((c * CHUNK) as u32, (c * CHUNK + chunk.len()) as u32);
                let range =
                    |v: &[u32]| v.partition_point(|&x| x < lo)..v.partition_point(|&x| x < hi);
                for &v in &touched[range(&touched)] {
                    chunk[(v - lo) as usize].retain(|a| !batch_mark[a.other as usize]);
                }
                let r =
                    adds.partition_point(|(v, _)| *v < lo)..adds.partition_point(|(v, _)| *v < hi);
                for &(v, a) in &adds[r] {
                    insert(&mut chunk[(v - lo) as usize], a);
                }
            });
        drop(adds);
        for &v in &batch {
            batch_mark[v as usize] = false;
        }
        remaining.retain(|&v| rank[v as usize] == u32::MAX);
        let new: Vec<i64> = touched
            .par_iter()
            .map_init(Search::default, |s, &v| {
                priority(&lists, v, depth[v as usize], neighbours[v as usize], s)
            })
            .collect();
        for (&v, p) in touched.iter().zip(new) {
            prio[v as usize] = p;
            dirty[v as usize] = false;
        }
        stats.rounds += 1;
        if report.elapsed().as_secs() >= 30 {
            report = Instant::now();
            eprintln!(
                "  ch: round {}, {} nodes left, {} shortcuts, {:.0} s, peak RSS {:.0} MiB",
                stats.rounds,
                remaining.len(),
                added,
                start.elapsed().as_secs_f64(),
                peak_rss_mib()
            );
        }
    }
    drop((prio, batch_mark, dirty, neighbours));
    stats.max_depth = depth.iter().copied().max().unwrap_or(0);
    drop(depth);

    let (up, down) = hierarchy(lists);
    let all = || up.edges.iter().chain(&down.edges);
    stats.up_edges = up.edges.len() as u64;
    stats.down_edges = down.edges.len() as u64;
    stats.shortcuts = all().filter(|e| e.flags & SHORTCUT != 0).count() as u64;
    stats.flagged_edges = all().filter(|e| e.flags & AVOID != 0).count() as u64;
    stats.flagged_shortcuts = all()
        .filter(|e| e.flags & AVOID != 0 && e.flags & SHORTCUT != 0)
        .count() as u64;
    stats.max_weight_ds = all().map(|e| e.weight).max().unwrap_or(0);
    stats.bytes_estimate = 12 * (stats.nodes + stats.up_edges + stats.down_edges);
    stats.contract_s = start.elapsed().as_secs_f64();
    stats.peak_rss_mib = peak_rss_mib();
    Ch {
        rank,
        up,
        down,
        stats,
    }
}

/// Each node's arcs, in and out, with the avoid flags in `mask`; loops left out and parallel
/// arcs pruned.
fn arcs(g: &Csr, mask: u8) -> Vec<Vec<Arc>> {
    let n = g.nodes();
    // The edges into each node, by a counting sort.
    let mut first = vec![0u32; n + 1];
    for e in &g.edges {
        first[e.to as usize + 1] += 1;
    }
    for i in 0..n {
        first[i + 1] += first[i];
    }
    let mut next = first.clone();
    let mut into = vec![(0u32, 0u32); g.edges.len()];
    for v in 0..n as u32 {
        for i in g.first[v as usize]..g.first[v as usize + 1] {
            let slot = &mut next[g.edges[i as usize].to as usize];
            into[*slot as usize] = (v, i);
            *slot += 1;
        }
    }
    drop(next);
    let arc = |other, i: u32, dir| {
        let e = &g.edges[i as usize];
        Arc {
            other,
            weight: e.weight(),
            mid: i,
            bits: e.flags & mask | dir,
        }
    };
    (0..n as u32)
        .into_par_iter()
        .map(|v| {
            let mut list = Vec::new();
            for i in g.first[v as usize]..g.first[v as usize + 1] {
                let to = g.edges[i as usize].to;
                if to != v {
                    insert(&mut list, arc(to, i, OUT));
                }
            }
            for &(from, i) in &into[first[v as usize] as usize..first[v as usize + 1] as usize] {
                if from != v {
                    insert(&mut list, arc(from, i, IN));
                }
            }
            list.shrink_to_fit();
            list
        })
        .collect()
}

/// The up and down edges from the contracted nodes' lists, which hold their arcs to the
/// nodes contracted after them.
fn hierarchy(lists: Vec<Vec<Arc>>) -> (ChCsr, ChCsr) {
    let csr = |dir: u8| {
        let mut first = Vec::with_capacity(lists.len() + 1);
        let mut edges = Vec::new();
        first.push(0);
        for list in &lists {
            edges.extend(list.iter().filter(|a| a.bits & dir != 0).map(|a| ChEdge {
                other: a.other,
                weight: a.weight,
                mid: a.mid,
                flags: a.bits & (AVOID | SHORTCUT),
            }));
            first.push(edges.len() as u32);
        }
        edges.shrink_to_fit();
        ChCsr { first, edges }
    };
    let up = csr(OUT);
    let down = csr(IN);
    (up, down)
}

pub fn print_stats(s: &Stats) {
    eprintln!(
        "  ch: {} nodes, {} base edges ({} arcs), {} up + {} down edges ({:.2} per base \
         edge), {} shortcuts; {} flagged edges ({} shortcuts); {} rounds, depth {}, longest \
         {:.0} s; about {:.0} MiB; {:.1} s, peak RSS {:.0} MiB",
        s.nodes,
        s.base_edges,
        s.base_arcs,
        s.up_edges,
        s.down_edges,
        (s.up_edges + s.down_edges) as f64 / s.base_edges.max(1) as f64,
        s.shortcuts,
        s.flagged_edges,
        s.flagged_shortcuts,
        s.rounds,
        s.max_depth,
        f64::from(s.max_weight_ds) / 10.0,
        crate::mib(s.bytes_estimate),
        s.contract_s,
        s.peak_rss_mib
    );
}
