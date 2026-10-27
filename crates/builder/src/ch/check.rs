//! Checks of the hierarchy: queries ([`query`]) against Dijkstra on the graph for random
//! pairs and every combination of avoided flags, and against `fast_paths` without them; and
//! `omp-builder ch`, which contracts a saved graph and runs the checks.
//!
//! The query here is the plain bidirectional search, enough for the checks and the search
//! space counts; step 3.4 writes the app's in `omp-core`.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use anyhow::{Context, Result, ensure};
use clap::Args;
use rayon::prelude::*;
use serde::Serialize;

use super::witness::NodeMap;
use super::{AVOID, Ch, ChEdge, Params, SHORTCUT, Stats};
use crate::graph::{self, Csr, Graph};
use crate::pbf::Pbf;
use crate::peak_rss_mib;

/// The four combinations of tolls and ferries avoided.
pub const COMBINATIONS: [u8; 4] = [0, omp_core::profile::TOLL, omp_core::profile::FERRY, AVOID];

/// A route found in the hierarchy: its weight, the nodes it settled, and its base edges.
pub struct Route {
    pub weight: u32,
    pub settled: usize,
    /// The nodes settled, in order.
    pub visited: Vec<u32>,
    /// The hierarchy edges between the nodes of the route, before unpacking.
    pub path: Vec<(u32, u32, ChEdge)>,
    pub edges: Vec<u32>,
}

/// One side of a query: distances, and the edge each node was reached by.
#[derive(Default)]
struct Side {
    dist: NodeMap<(u32, u32, ChEdge)>,
    heap: BinaryHeap<Reverse<(u32, u32)>>,
}

impl Side {
    fn start(&mut self, s: u32) {
        let none = ChEdge {
            other: u32::MAX,
            weight: 0,
            mid: 0,
            flags: 0,
        };
        self.dist.insert(s, (0, u32::MAX, none));
        self.heap.push(Reverse((0, s)));
    }

    fn min(&self) -> u32 {
        self.heap.peek().map_or(u32::MAX, |Reverse((d, _))| *d)
    }

    fn dist(&self, v: u32) -> Option<u32> {
        self.dist.get(&v).map(|d| d.0)
    }

    /// Settles the next node, relaxing its edges in `edges` that avoid `avoid`.
    fn step(&mut self, edges: &super::ChCsr, avoid: u8) -> Option<u32> {
        let Reverse((d, u)) = self.heap.pop()?;
        if self.dist[&u].0 < d {
            return Some(u32::MAX);
        }
        for e in edges.at(u) {
            if e.flags & avoid != 0 {
                continue;
            }
            let nd = d + e.weight;
            let old = self.dist.entry(e.other).or_insert((u32::MAX, 0, *e));
            if nd < old.0 {
                *old = (nd, u, *e);
                self.heap.push(Reverse((nd, e.other)));
            }
        }
        Some(u)
    }
}

/// The shortest route from `s` to `t` avoiding the flags in `avoid`, or `None`.
pub fn query(ch: &Ch, g: &Csr, s: u32, t: u32, avoid: u8) -> Option<Route> {
    let (mut f, mut b) = (Side::default(), Side::default());
    f.start(s);
    b.start(t);
    let (mut best, mut meet, mut settled) = (u32::MAX, u32::MAX, 0);
    let mut visited = Vec::new();
    while f.min().min(b.min()) < best {
        let (side, other, edges) = match f.min() <= b.min() {
            true => (&mut f, &b, &ch.up),
            false => (&mut b, &f, &ch.down),
        };
        let Some(u) = side.step(edges, avoid) else {
            break;
        };
        if u == u32::MAX {
            continue;
        }
        settled += 1;
        visited.push(u);
        if let (Some(a), Some(c)) = (side.dist(u), other.dist(u))
            && a + c < best
        {
            (best, meet) = (a + c, u);
        }
    }
    if best == u32::MAX {
        return None;
    }
    // Up from s to the meeting node, then down to t: each node's edge, unpacked.
    let mut edges = Vec::new();
    let mut v = meet;
    let mut up = Vec::new();
    while v != s {
        let (_, from, e) = f.dist[&v];
        up.push((from, v, e));
        v = from;
    }
    let mut path: Vec<(u32, u32, ChEdge)> = up.into_iter().rev().collect();
    let mut v = meet;
    while v != t {
        let (_, from, e) = b.dist[&v];
        path.push((v, from, e));
        v = from;
    }
    for (from, to, e) in &path {
        unpack(ch, g, *from, *to, e, &mut edges);
    }
    Some(Route {
        weight: best,
        settled,
        visited,
        path,
        edges,
    })
}

/// The two halves of shortcut `e` from `a` to `b`, at its middle node: a down edge from
/// `a` and an up edge to `b`.
pub fn halves(ch: &Ch, a: u32, b: u32, e: &ChEdge) -> (ChEdge, ChEdge) {
    let m = e.mid;
    for d in ch.down.at(m).iter().filter(|d| d.other == a) {
        for u in ch.up.at(m).iter().filter(|u| u.other == b) {
            if d.weight + u.weight == e.weight && (d.flags | u.flags) & AVOID == e.flags & AVOID {
                return (*d, *u);
            }
        }
    }
    panic!("shortcut {a} -> {b} through {m} has no halves");
}

/// Appends the base edges of `e`, an edge of the hierarchy from `a` to `b`.
fn unpack(ch: &Ch, g: &Csr, a: u32, b: u32, e: &ChEdge, out: &mut Vec<u32>) {
    if e.flags & SHORTCUT == 0 {
        debug_assert_eq!(g.edges[e.mid as usize].to, b);
        out.push(e.mid);
        return;
    }
    // The middle node is below both ends: `a` to it is one of its down edges, it to `b` one
    // of its up edges, with this weight and flags between them.
    let (d, u) = halves(ch, a, b, e);
    unpack(ch, g, a, e.mid, &d, out);
    unpack(ch, g, e.mid, b, &u, out);
}

/// Dijkstra on the graph from `s` to `t`, on edges that avoid `avoid`; `dist` is all
/// `u32::MAX` and is left so.
pub fn dijkstra(g: &Csr, s: u32, t: u32, avoid: u8, dist: &mut [u32]) -> Option<u32> {
    let mut heap = BinaryHeap::new();
    let mut touched = vec![s];
    dist[s as usize] = 0;
    heap.push(Reverse((0u32, s)));
    let mut found = None;
    while let Some(Reverse((d, u))) = heap.pop() {
        if u == t {
            found = Some(d);
            break;
        }
        if dist[u as usize] < d {
            continue;
        }
        for e in g.out(u) {
            if e.flags & avoid != 0 {
                continue;
            }
            let nd = d + e.weight();
            if nd < dist[e.to as usize] {
                if dist[e.to as usize] == u32::MAX {
                    touched.push(e.to);
                }
                dist[e.to as usize] = nd;
                heap.push(Reverse((nd, e.to)));
            }
        }
    }
    for v in touched {
        dist[v as usize] = u32::MAX;
    }
    found
}

/// A small random number generator (SplitMix64), so checks are repeatable from a seed.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed)
    }

    pub fn below(&mut self, n: u64) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        (z ^ (z >> 31)) % n
    }
}

/// Random pairs of nodes outside the small components.
pub fn pairs(g: &Graph, count: usize, seed: u64) -> Vec<(u32, u32)> {
    let big: Vec<u32> = (0..g.small.len() as u32)
        .filter(|&v| !g.small[v as usize])
        .collect();
    let mut rng = Rng::new(seed);
    let mut pick = || big[rng.below(big.len() as u64) as usize];
    (0..count).map(|_| (pick(), pick())).collect()
}

#[derive(Default, Serialize)]
pub struct CheckStats {
    pub pairs: u64,
    /// Queries per combination of avoided flags, in [`COMBINATIONS`] order.
    pub routed: [u64; 4],
    pub unreachable: [u64; 4],
    pub mismatches: u64,
    /// Unpacked routes whose edges do not chain from `s` to `t`, or that use an avoided
    /// flag, or whose weights do not add up.
    pub bad_paths: u64,
    /// Nodes the hierarchy's queries settled, mean and largest.
    pub settled_mean: f64,
    pub settled_max: u64,
    pub ch_query_ms_mean: f64,
    pub dijkstra_ms_mean: f64,
    /// Routes where `fast_paths` (without flags) gave another weight, of those compared.
    pub fast_paths_compared: u64,
    pub fast_paths_mismatches: u64,
    pub fast_paths_prepare_s: f64,
    pub fast_paths_edges: u64,
    pub check_s: f64,
}

/// Whether `edges` are a route from `s` to `t` of weight `w` that avoids `avoid`.
fn valid(g: &Csr, edges: &[u32], s: u32, t: u32, w: u32, avoid: u8) -> bool {
    let mut at = s;
    let mut sum = 0u32;
    for &i in edges {
        let range = g.first[at as usize]..g.first[at as usize + 1];
        if !range.contains(&i) {
            return false;
        }
        let e = &g.edges[i as usize];
        if e.flags & avoid != 0 {
            return false;
        }
        sum += e.weight();
        at = e.to;
    }
    at == t && sum == w
}

/// Checks `pairs` with every combination of avoided flags (only none without flags).
pub fn check(ch: &Ch, g: &Csr, pairs: &[(u32, u32)], flags: bool) -> CheckStats {
    let start = Instant::now();
    let combos: &[u8] = if flags {
        &COMBINATIONS
    } else {
        &COMBINATIONS[..1]
    };
    let n = g.nodes();
    let counts: [[AtomicU64; 4]; 2] = Default::default();
    let [mismatches, bad, settled_sum, settled_max, ch_ns, dij_ns] =
        [(); 6].map(|_| AtomicU64::new(0));
    let done = AtomicU64::new(0);
    pairs.par_iter().for_each_init(
        || vec![u32::MAX; n],
        |dist, &(s, t)| {
            for (k, &avoid) in combos.iter().enumerate() {
                let t0 = Instant::now();
                let r = query(ch, g, s, t, avoid);
                let t1 = Instant::now();
                let d = dijkstra(g, s, t, avoid, dist);
                dij_ns.fetch_add(t1.elapsed().as_nanos() as u64, Ordering::Relaxed);
                ch_ns.fetch_add((t1 - t0).as_nanos() as u64, Ordering::Relaxed);
                let side = usize::from(r.is_none());
                counts[side][k].fetch_add(1, Ordering::Relaxed);
                if r.as_ref().map(|r| r.weight) != d {
                    mismatches.fetch_add(1, Ordering::Relaxed);
                    eprintln!(
                        "  ch check: {s} -> {t} avoiding {avoid}: hierarchy {:?}, Dijkstra {d:?}",
                        r.as_ref().map(|r| r.weight)
                    );
                }
                if let Some(r) = r {
                    if !valid(g, &r.edges, s, t, r.weight, avoid) {
                        bad.fetch_add(1, Ordering::Relaxed);
                    }
                    settled_sum.fetch_add(r.settled as u64, Ordering::Relaxed);
                    settled_max.fetch_max(r.settled as u64, Ordering::Relaxed);
                }
            }
            let k = done.fetch_add(1, Ordering::Relaxed) + 1;
            if k.is_multiple_of(1000) {
                eprintln!(
                    "  ch check: {k} of {} pairs, {:.0} s",
                    pairs.len(),
                    start.elapsed().as_secs_f64()
                );
            }
        },
    );
    let routed = counts[0].each_ref().map(|c| c.load(Ordering::Relaxed));
    let queries = (pairs.len() * combos.len()).max(1) as f64;
    CheckStats {
        pairs: pairs.len() as u64,
        routed,
        unreachable: counts[1].each_ref().map(|c| c.load(Ordering::Relaxed)),
        mismatches: mismatches.into_inner(),
        bad_paths: bad.into_inner(),
        settled_mean: settled_sum.into_inner() as f64 / routed.iter().sum::<u64>().max(1) as f64,
        settled_max: settled_max.into_inner(),
        ch_query_ms_mean: ch_ns.into_inner() as f64 / queries / 1e6,
        dijkstra_ms_mean: dij_ns.into_inner() as f64 / queries / 1e6,
        check_s: start.elapsed().as_secs_f64(),
        ..CheckStats::default()
    }
}

/// Compares the hierarchy with `fast_paths` on the same graph, without flags.
pub fn check_fast_paths(ch: &Ch, g: &Csr, pairs: &[(u32, u32)], st: &mut CheckStats) {
    let mut input = fast_paths::InputGraph::new();
    for v in 0..g.nodes() as u32 {
        for e in g.out(v) {
            if e.to != v {
                input.add_edge(v as usize, e.to as usize, e.weight() as usize);
            }
        }
    }
    input.freeze();
    let t = Instant::now();
    let fast = fast_paths::prepare(&input);
    st.fast_paths_prepare_s = t.elapsed().as_secs_f64();
    st.fast_paths_edges = (fast.get_num_out_edges() + fast.get_num_in_edges()) as u64;
    let mismatches = AtomicU64::new(0);
    pairs.par_iter().for_each_init(
        || fast_paths::create_calculator(&fast),
        |calc, &(s, t)| {
            let theirs = calc
                .calc_path(&fast, s as usize, t as usize)
                .map(|p| p.get_weight() as u32);
            let ours = query(ch, g, s, t, 0).map(|r| r.weight);
            if theirs != ours {
                mismatches.fetch_add(1, Ordering::Relaxed);
            }
        },
    );
    st.fast_paths_compared = pairs.len() as u64;
    st.fast_paths_mismatches = mismatches.into_inner();
}

pub fn print_check(c: &CheckStats) {
    eprintln!(
        "  ch check: {} pairs; routed {:?}, unreachable {:?} (none, tolls, ferries, both \
         avoided); {} mismatches, {} bad paths; settled mean {:.0}, max {}; query {:.3} ms, \
         Dijkstra {:.1} ms; {:.1} s",
        c.pairs,
        c.routed,
        c.unreachable,
        c.mismatches,
        c.bad_paths,
        c.settled_mean,
        c.settled_max,
        c.ch_query_ms_mean,
        c.dijkstra_ms_mean,
        c.check_s
    );
    if c.fast_paths_compared > 0 {
        eprintln!(
            "  ch check: fast_paths: {} CH edges, prepared in {:.1} s; {} of {} routes differ",
            c.fast_paths_edges,
            c.fast_paths_prepare_s,
            c.fast_paths_mismatches,
            c.fast_paths_compared
        );
    }
}

#[derive(Args)]
pub struct ChArgs {
    /// A graph saved by `graph --save`, or an OSM extract (.osm.pbf) to build it from.
    input: PathBuf,

    /// Contract without the avoid flags, to measure what they cost.
    #[arg(long)]
    no_flags: bool,

    /// Random pairs to check against Dijkstra, with every combination of avoided flags.
    #[arg(long, default_value_t = 0)]
    check: usize,

    /// Also check against `fast_paths` (without flags).
    #[arg(long)]
    fast_paths: bool,

    #[arg(long, default_value_t = 1)]
    seed: u64,

    /// Priority weights: edge difference, contracted neighbours, depth.
    #[arg(long, value_delimiter = ',')]
    priority: Option<Vec<i64>>,

    /// Nodes a witness search settles at most.
    #[arg(long)]
    settled: Option<usize>,

    /// The share of the remaining nodes, lowest priority first, a round may contract.
    #[arg(long)]
    share: Option<f64>,

    /// Encode the routing sections (without street names) and compare layouts on this many
    /// long and short random pairs.
    #[arg(long)]
    layout: Option<usize>,

    /// Nodes per `RCHE` block and MiB at the top, for `--layout`.
    #[arg(long, value_delimiter = ',')]
    layout_params: Option<Vec<f64>>,

    /// Write the counts as JSON.
    #[arg(long)]
    json: Option<PathBuf>,
}

#[derive(Serialize)]
struct Report<'a> {
    kind: &'static str,
    input: String,
    params: Params,
    wall_s: f64,
    peak_rss_mib: f64,
    stats: &'a Stats,
    check: Option<CheckStats>,
}

/// `omp-builder ch`: contracts a graph and checks the hierarchy.
pub fn run(args: ChArgs) -> Result<()> {
    let start = Instant::now();
    let is_pbf = args.input.to_string_lossy().ends_with(".pbf");
    let g: Graph = match is_pbf {
        true => {
            let g = graph::make(&Pbf::open(&args.input)?)?;
            graph::print_stats(&g.stats);
            g
        }
        false => {
            let bytes = std::fs::read(&args.input)
                .with_context(|| format!("read {}", args.input.display()))?;
            postcard::from_bytes(&bytes).context("not a saved graph")?
        }
    };
    eprintln!(
        "  graph: {} nodes, {} edges, loaded in {:.1} s",
        g.csr.nodes(),
        g.csr.edges.len(),
        start.elapsed().as_secs_f64()
    );
    let mut params = Params {
        flags: !args.no_flags,
        ..Params::default()
    };
    if let Some(w) = &args.priority {
        ensure!(w.len() == 3, "--priority takes three weights");
        (
            params.edge_difference,
            params.contracted_neighbours,
            params.depth,
        ) = (w[0], w[1], w[2]);
    }
    if let Some(s) = args.settled {
        params.settled = s;
    }
    if let Some(s) = args.share {
        params.share = s;
    }
    let ch = super::contract(&g.csr, &params);
    super::print_stats(&ch.stats);
    let check = match args.check {
        0 => None,
        k => {
            let pairs = pairs(&g, k, args.seed);
            let mut c = check(&ch, &g.csr, &pairs, params.flags);
            if args.fast_paths {
                check_fast_paths(&ch, &g.csr, &pairs, &mut c);
            }
            print_check(&c);
            Some(c)
        }
    };
    if let Some(k) = args.layout {
        let mut p = crate::routing::Params::default();
        if let Some(v) = &args.layout_params {
            ensure!(v.len() == 2, "--layout-params takes two numbers");
            (p.block_nodes, p.top_mib) = (v[0] as u32, v[1]);
        }
        let streets = crate::street::WayStreets::default();
        let (_, sizes, order, lens) = crate::routing::sections(&g, &ch, &streets, &p)?;
        crate::routing::print_sizes(&sizes);
        use crate::routing::layout;
        let sets = [
            ("long", pairs(&g, k, args.seed)),
            ("short", layout::short_pairs(&g, &order, k, args.seed + 1)),
        ];
        for (name, p) in &sets {
            layout::print(&layout::compare(&g, &ch, &order, &lens, p, name));
        }
    }
    let report = Report {
        kind: "ch",
        input: args.input.display().to_string(),
        params,
        wall_s: start.elapsed().as_secs_f64(),
        peak_rss_mib: peak_rss_mib(),
        stats: &ch.stats,
        check,
    };
    if let Some(path) = &args.json {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(&report)?)?;
    }
    eprintln!(
        "ch: {:.1} s, peak RSS {:.0} MiB",
        start.elapsed().as_secs_f64(),
        peak_rss_mib()
    );
    if let Some(c) = &report.check {
        ensure!(
            c.mismatches == 0 && c.bad_paths == 0 && c.fast_paths_mismatches == 0,
            "the hierarchy disagrees with Dijkstra or fast_paths"
        );
    }
    Ok(())
}
