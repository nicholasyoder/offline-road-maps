//! Witness searches: whether contracting a node needs a shortcut between two of its
//! neighbours, or a path that avoids the node is as short.
//!
//! A witness only counts when its avoid flags are a subset of the shortcut's (label-
//! constrained contraction, Rice and Tsotras 2010), so the search for a shortcut with flags
//! `F` uses only arcs whose flags are in `F`. Then for any set of avoided flags, the
//! hierarchy without the flagged arcs keeps every shortest path of the graph without them.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::hash::{BuildHasherDefault, Hasher};

use super::{AVOID, Arc, IN, OUT, SHORTCUT};

/// A hasher for node IDs: one multiply, enough for keys that are already well spread.
#[derive(Default)]
pub struct NodeHasher(u64);

impl Hasher for NodeHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 << 8 | u64::from(b)).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        }
    }

    fn write_u32(&mut self, n: u32) {
        self.0 = u64::from(n).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    }
}

pub type NodeMap<V> = HashMap<u32, V, BuildHasherDefault<NodeHasher>>;

/// A shortcut a contraction adds: from `from` to `to` through the contracted node.
#[derive(Clone, Copy, Debug)]
pub struct Shortcut {
    pub from: u32,
    pub to: u32,
    pub weight: u32,
    /// Avoid flags.
    pub flags: u8,
    pub mid: u32,
}

impl Shortcut {
    /// Its arc at `from` and at `to`.
    pub fn arcs(&self) -> [(u32, Arc); 2] {
        let arc = |other, dir| Arc {
            other,
            weight: self.weight,
            mid: self.mid,
            bits: self.flags | SHORTCUT | dir,
        };
        [
            (self.from, arc(self.to, OUT)),
            (self.to, arc(self.from, IN)),
        ]
    }
}

/// A search's state, kept between searches to reuse its memory.
#[derive(Default)]
pub struct Search {
    dist: NodeMap<u32>,
    heap: BinaryHeap<Reverse<(u32, u32)>>,
}

impl Search {
    /// Distances from `s` over out-arcs whose flags are in `allowed`, not through `skip`,
    /// up to `limit`, settling at most `settled` nodes. Unsettled nodes keep their
    /// tentative distances, which are still lengths of paths.
    fn run(
        &mut self,
        lists: &[Vec<Arc>],
        s: u32,
        allowed: u8,
        limit: u32,
        settled: usize,
        skip: &dyn Fn(u32) -> bool,
    ) {
        self.dist.clear();
        self.heap.clear();
        self.dist.insert(s, 0);
        self.heap.push(Reverse((0, s)));
        let mut done = 0;
        while let Some(Reverse((d, u))) = self.heap.pop() {
            if d > limit || done >= settled {
                break;
            }
            if self.dist[&u] < d {
                continue;
            }
            done += 1;
            for a in &lists[u as usize] {
                if a.bits & OUT == 0 || a.bits & AVOID & !allowed != 0 || skip(a.other) {
                    continue;
                }
                let nd = d.saturating_add(a.weight);
                let old = self.dist.entry(a.other).or_insert(u32::MAX);
                if nd < *old {
                    *old = nd;
                    self.heap.push(Reverse((nd, a.other)));
                }
            }
        }
    }

    fn dist(&self, v: u32) -> u32 {
        self.dist.get(&v).copied().unwrap_or(u32::MAX)
    }
}

/// The shortcuts contracting `v` needs, given the nodes `skip` that are being contracted
/// with it; added to `out`.
pub fn shortcuts(
    lists: &[Vec<Arc>],
    v: u32,
    settled: usize,
    skip: &dyn Fn(u32) -> bool,
    search: &mut Search,
    out: &mut Vec<Shortcut>,
) {
    let list = &lists[v as usize];
    let skip_v = |x: u32| x == v || skip(x);
    let mut cands: Vec<Shortcut> = Vec::new();
    let mut sources: Vec<u32> = list
        .iter()
        .filter(|a| a.bits & IN != 0)
        .map(|a| a.other)
        .collect();
    sources.sort_unstable();
    sources.dedup();
    for u in sources {
        cands.clear();
        for i in list.iter().filter(|a| a.bits & IN != 0 && a.other == u) {
            for o in list.iter().filter(|a| a.bits & OUT != 0 && a.other != u) {
                let c = Shortcut {
                    from: u,
                    to: o.other,
                    weight: i.weight.saturating_add(o.weight),
                    flags: (i.bits | o.bits) & AVOID,
                    mid: v,
                };
                // Keep only candidates no other candidate to the same node dominates.
                if cands.iter().any(|d| d.to == c.to && dominates(d, &c)) {
                    continue;
                }
                cands.retain(|d| !(d.to == c.to && dominates(&c, d)));
                cands.push(c);
            }
        }
        let mut sets: Vec<u8> = cands.iter().map(|c| c.flags).collect();
        sets.sort_unstable();
        sets.dedup();
        for f in sets {
            let limit = cands
                .iter()
                .filter(|c| c.flags == f)
                .map(|c| c.weight)
                .max()
                .unwrap_or(0);
            search.run(lists, u, f, limit, settled, &skip_v);
            out.extend(
                cands
                    .iter()
                    .filter(|c| c.flags == f && search.dist(c.to) > c.weight),
            );
        }
    }
}

/// `a` is as short as `b` with no flag `b` lacks, so a route that may use `b` may use `a`.
pub fn dominates(a: &Shortcut, b: &Shortcut) -> bool {
    a.weight <= b.weight && a.flags & !b.flags == 0
}
