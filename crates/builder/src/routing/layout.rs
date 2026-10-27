//! Which layout of the hierarchy reads least from a cold USB stick (step 3.3), by the 4 KiB
//! pages a route touches: in the search, and in unpacking its shortcuts, which needs each
//! edge's middle node or segment and each middle node's edges. Three layouts:
//!
//! - **fixed**: 8 bytes per node (first edge and the split between up and down edges) and 8
//!   per edge (target, 24-bit weight and flags), with `RCHM` apart;
//! - **varint**: blocks of varint-coded nodes, with the middle nodes apart in `RCHM`, 4
//!   bytes per edge;
//! - **inline**: the same blocks with each edge's middle node or segment in its record, as
//!   written ([`super::hierarchy`]), so unpacking reads only the middle nodes' blocks.
//!
//! Pages in the top of the hierarchy count apart, since one read at the start brings them
//! into the cache. The queries are the builder's ([`crate::ch::check::query`]).

use std::collections::HashSet;

use rayon::prelude::*;
use serde::Serialize;

use super::{Lens, Order};
use crate::ch::check::{Rng, halves, query};
use crate::ch::{Ch, SHORTCUT};
use crate::graph::Graph;

const PAGE: u64 = 4096;
/// Where `RCHM` starts in the page numbering, apart from `RCHE`'s pages.
const MIDS_AT: u64 = 1 << 50;
/// `RCHE` block sizes compared, in nodes.
pub const BLOCK_NODES: [u32; 4] = [16, 32, 64, 128];
/// Top sizes whose share of the settled nodes is reported.
pub const TOPS: [u32; 4] = [1 << 14, 1 << 16, 1 << 18, 1 << 20];

/// Pages per route, mean: in the search, in all, and in all outside the top.
#[derive(Serialize, Default, Clone)]
pub struct Pages {
    pub search: f64,
    pub all: f64,
    pub outside_top: f64,
    /// Bytes of `RCHE` (and `RCHM`, where it is apart) and of the top.
    pub bytes: u64,
    pub top_bytes: u64,
}

#[derive(Serialize, Default, Clone)]
pub struct Set {
    pub name: String,
    pub queries: u64,
    pub settled_mean: f64,
    /// Hierarchy edges unpacked and middle nodes visited per route.
    pub unpacked_mean: f64,
    /// The share of settled nodes in the top `TOPS[i]` nodes.
    pub in_top: Vec<f64>,
    pub fixed: Pages,
    /// Per [`BLOCK_NODES`].
    pub varint: Vec<Pages>,
    pub inline: Vec<Pages>,
}

/// Page counts of one route: search, all, all outside the top.
type Count = [u64; 3];

/// Adds the pages of `[a, b)`.
fn add(seen: &mut HashSet<u64>, a: u64, b: u64) {
    for p in a / PAGE..=(b.max(a + 1) - 1) / PAGE {
        seen.insert(p);
    }
}

/// Block starts for `lens` in blocks of `b` nodes, after `RCHE`'s index, and the end.
fn starts(lens: &[u32], b: usize) -> Vec<u64> {
    let nblocks = lens.len().div_ceil(b);
    let mut at = super::rche_head_len(nblocks);
    let mut v = Vec::with_capacity(nblocks + 1);
    for c in lens.chunks(b) {
        v.push(at);
        at += c.iter().map(|&l| u64::from(l)).sum::<u64>();
    }
    v.push(at);
    v
}

/// A layout: the byte ranges a node's edges take, and where `RCHM` entry `i` is, if apart.
trait Layout: Sync {
    fn node(&self, u: usize, out: &mut HashSet<u64>);
    fn mid(&self, index: u64) -> Option<u64>;
    /// Where the top ends, in `RCHE`.
    fn top(&self) -> u64;
    fn in_top(&self, page: u64) -> bool {
        page < MIDS_AT / PAGE && page * PAGE < self.top()
    }
}

struct Fixed<'a> {
    first: &'a [u64],
    edges_at: u64,
    top: usize,
}

impl Layout for Fixed<'_> {
    fn node(&self, u: usize, out: &mut HashSet<u64>) {
        add(out, 8 * u as u64, 8 * u as u64 + 16);
        let e = self.edges_at;
        add(out, e + 8 * self.first[u], e + 8 * self.first[u + 1]);
    }
    fn mid(&self, index: u64) -> Option<u64> {
        Some(MIDS_AT + 4 * index)
    }
    fn top(&self) -> u64 {
        self.edges_at + 8 * self.first[self.top]
    }
    fn in_top(&self, page: u64) -> bool {
        match page * PAGE < self.edges_at {
            true => page * PAGE < 8 * self.top as u64,
            false => page * PAGE < self.top(),
        }
    }
}

struct Blocks<'a> {
    starts: &'a [u64],
    b: usize,
    top: usize,
    apart: bool,
}

impl Layout for Blocks<'_> {
    fn node(&self, u: usize, out: &mut HashSet<u64>) {
        add(out, self.starts[u / self.b], self.starts[u / self.b + 1]);
    }
    fn mid(&self, index: u64) -> Option<u64> {
        self.apart.then_some(MIDS_AT + 4 * index)
    }
    fn top(&self) -> u64 {
        self.starts[self.top.div_ceil(self.b).min(self.starts.len() - 1)]
    }
}

/// The pages of one route in `l`: the search's nodes, then the unpacked edges' `RCHM`
/// entries and the middle nodes.
fn route_pages(
    l: &dyn Layout,
    visited: &[usize],
    unpacked: &[(usize, u64)],
    middles: &[usize],
    seen: &mut HashSet<u64>,
) -> Count {
    seen.clear();
    for &u in visited {
        l.node(u, seen);
    }
    let search = seen.len() as u64;
    for &(_, index) in unpacked {
        if let Some(at) = l.mid(index) {
            add(seen, at, at + 4);
        }
    }
    for &m in middles {
        l.node(m, seen);
    }
    let outside = seen.iter().filter(|&&p| !l.in_top(p)).count() as u64;
    [search, seen.len() as u64, outside]
}

/// Compares the layouts on `pairs`.
pub fn compare(g: &Graph, h: &Ch, o: &Order, lens: &Lens, pairs: &[(u32, u32)], name: &str) -> Set {
    let n = o.order.len();
    let mut first = Vec::with_capacity(n + 1);
    first.push(0u64);
    for &v in &o.order {
        let k = (h.up.at(v).len() + h.down.at(v).len()) as u64;
        first.push(first.last().unwrap() + k);
    }
    let node_bytes = 8 * (n as u64 + 1);
    let edges_at = node_bytes.div_ceil(PAGE) * PAGE;
    let top = o.top as usize;
    let fixed = Fixed {
        first: &first,
        edges_at,
        top,
    };
    let plain: Vec<Vec<u64>> = BLOCK_NODES
        .iter()
        .map(|&b| starts(&lens.apart, b as usize))
        .collect();
    let inline: Vec<Vec<u64>> = BLOCK_NODES
        .iter()
        .map(|&b| starts(&lens.inline, b as usize))
        .collect();
    let layouts: Vec<Blocks> = [(&plain, true), (&inline, false)]
        .into_iter()
        .flat_map(|(s, apart)| {
            (0..4).map(move |k| Blocks {
                starts: &s[k],
                b: BLOCK_NODES[k] as usize,
                top,
                apart,
            })
        })
        .collect();

    struct One {
        settled: u64,
        unpacked: u64,
        in_top: [u64; 4],
        fixed: Count,
        blocks: Vec<Count>,
    }
    let rank_cut = TOPS.map(|t| (n as u32).saturating_sub(t));
    let new = |v: u32| o.id[v as usize] as usize;
    let ones: Vec<One> = pairs
        .par_iter()
        .map_init(HashSet::new, |seen, &(s, t)| {
            let r = query(h, &g.csr, s, t, 0)?;
            let visited: Vec<usize> = r.visited.iter().map(|&v| new(v)).collect();
            let mut in_top = [0; 4];
            for &v in &r.visited {
                for (k, &cut) in rank_cut.iter().enumerate() {
                    in_top[k] += u64::from(h.rank[v as usize] >= cut);
                }
            }
            // Unpacking: each edge's owner (its lower node) and RCHM index, and the middle
            // nodes.
            let (mut unpacked, mut middles) = (Vec::new(), Vec::new());
            let mut stack: Vec<_> = r.path.clone();
            while let Some((a, b, e)) = stack.pop() {
                let owner = match h.rank[a as usize] < h.rank[b as usize] {
                    true => a,
                    false => b,
                };
                let k =
                    h.up.at(owner)
                        .iter()
                        .chain(h.down.at(owner))
                        .position(|x| *x == e)
                        .unwrap_or(0);
                unpacked.push((new(owner), first[new(owner)] + k as u64));
                if e.flags & SHORTCUT != 0 {
                    middles.push(new(e.mid));
                    let (d, u) = halves(h, a, b, &e);
                    stack.push((a, e.mid, d));
                    stack.push((e.mid, b, u));
                }
            }
            Some(One {
                settled: r.settled as u64,
                unpacked: unpacked.len() as u64,
                in_top,
                fixed: route_pages(&fixed, &visited, &unpacked, &middles, seen),
                blocks: layouts
                    .iter()
                    .map(|l| route_pages(l, &visited, &unpacked, &middles, seen))
                    .collect(),
            })
        })
        .flatten()
        .collect();
    let q = ones.len().max(1) as f64;
    let mean = |f: &dyn Fn(&One) -> u64| ones.iter().map(f).sum::<u64>() as f64 / q;
    let settled: u64 = ones.iter().map(|x| x.settled).sum();
    let mids = 4 * first[n];
    let pages = |k: Option<usize>, bytes: u64, top_bytes: u64| {
        let c = |x: &One| match k {
            None => x.fixed,
            Some(k) => x.blocks[k],
        };
        Pages {
            search: mean(&|x| c(x)[0]),
            all: mean(&|x| c(x)[1]),
            outside_top: mean(&|x| c(x)[2]),
            bytes,
            top_bytes,
        }
    };
    Set {
        name: name.to_owned(),
        queries: ones.len() as u64,
        settled_mean: settled as f64 / q,
        unpacked_mean: mean(&|x| x.unpacked),
        in_top: (0..4)
            .map(|k| ones.iter().map(|x| x.in_top[k]).sum::<u64>() as f64 / settled.max(1) as f64)
            .collect(),
        fixed: pages(None, edges_at + 8 * first[n] + mids, fixed.top()),
        varint: (0..4)
            .map(|k| {
                let l = &layouts[k];
                pages(Some(k), l.starts[n.div_ceil(l.b)] + mids, l.top())
            })
            .collect(),
        inline: (0..4)
            .map(|k| {
                let l = &layouts[4 + k];
                pages(Some(4 + k), l.starts[n.div_ceil(l.b)], l.top())
            })
            .collect(),
    }
}

/// Short pairs: nodes close in the Hilbert order outside the top (tens of km apart), not in
/// small components. In a pack that is mostly top, any nodes.
pub fn short_pairs(g: &Graph, o: &Order, count: usize, seed: u64) -> Vec<(u32, u32)> {
    let n = o.order.len() as u64;
    let lo = match u64::from(o.top) < n / 2 {
        true => u64::from(o.top),
        false => 0,
    };
    let mut rng = Rng::new(seed);
    let mut out = Vec::with_capacity(count);
    if n <= lo + 2 {
        return out;
    }
    while out.len() < count {
        let i = lo + rng.below(n - lo);
        let j = (i + rng.below(40_000))
            .saturating_sub(20_000)
            .clamp(lo, n - 1);
        let (s, t) = (o.order[i as usize], o.order[j as usize]);
        if !g.small[s as usize] && !g.small[t as usize] && s != t {
            out.push((s, t));
        }
    }
    out
}

pub fn print(set: &Set) {
    let m = crate::mib;
    eprintln!(
        "  layout {}: {} queries, {:.0} nodes settled, {:.0} edges unpacked; in the top {:?} \
         nodes: {:.1?} %",
        set.name,
        set.queries,
        set.settled_mean,
        set.unpacked_mean,
        TOPS,
        set.in_top.iter().map(|x| x * 100.0).collect::<Vec<_>>()
    );
    let line = |what: &str, p: &Pages| {
        eprintln!(
            "  layout {}: {what:<22} pages: search {:.0}, route {:.0}, outside the top {:.0} \
             ({:.1} MiB, top {:.1} MiB)",
            set.name,
            p.search,
            p.all,
            p.outside_top,
            m(p.bytes),
            m(p.top_bytes)
        );
    };
    line("fixed + RCHM", &set.fixed);
    for (b, p) in BLOCK_NODES.iter().zip(&set.varint) {
        line(&format!("varint {b} + RCHM"), p);
    }
    for (b, p) in BLOCK_NODES.iter().zip(&set.inline) {
        line(&format!("inline {b}"), p);
    }
}
