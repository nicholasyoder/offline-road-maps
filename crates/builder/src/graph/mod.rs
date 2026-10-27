//! The car routing graph (phase 3 step 3.1): the roads and ferries of a PBF that the car
//! profile ([`omp_core::profile`]) lets a car use, cut into edges between intersections,
//! with turn restrictions applied by splitting nodes ([`restrict`]) and the small strongly
//! connected components marked ([`scc`]).
//!
//! [`read`] reads the PBF in three passes (restrictions, ways, nodes) into an [`Input`], and
//! [`build`] makes the [`Graph`] from it, so tests can build an input by hand.
//!
//! Signals, gates and toll booths that are not intersections are not nodes of the graph:
//! their delay and toll flag go on the edges through them, counted where an edge enters the
//! node, so a route pays each once. Blocking barriers (bollards) cut the road: each road
//! leading to one ends at its own copy of the node.

mod read;
mod restrict;
mod scc;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::ops::Range;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::Result;
use clap::Args;
use omp_core::profile::{self, FERRY, TOLL};
use serde::{Deserialize, Serialize};

use crate::geom::{self, P};
use crate::pbf::Pbf;
use crate::pbf::nodes::{NodeRanks, NodeSet};
use crate::{mib, peak_rss_mib};
pub use read::{ReadStats, read};
pub use restrict::RestrictionStats;
pub use scc::ComponentStats;

/// Edge flag: a road a car may use only to start or end a route on it.
pub const RESTRICTED: u8 = 4;
/// Edge flag: a [`RESTRICTED`] edge leaving a node that a road open to all also touches, so
/// a route entering the restricted roads there pays [`profile::RESTRICTED_PENALTY_S`].
pub const ENTRY: u8 = 8;

/// Nodes in a strongly connected component smaller than this are marked small, so snapping
/// prefers the large network (OSRM's `small_component_size`).
pub const SMALL_COMPONENT: u32 = 1000;

/// How a car may use one direction of a way: km/h (0 = not at all) and edge flags.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Dir {
    pub kmh: f32,
    pub flags: u8,
}

/// A way the car profile keeps.
#[derive(Clone)]
pub struct WayIn {
    pub id: i64,
    /// Along the way's nodes, and against them.
    pub dirs: [Dir; 2],
    /// A ferry's crossing time from its `duration`.
    pub duration_s: Option<f32>,
    pub ferry: bool,
    /// Its nodes in [`Input::refs`] and [`Input::coords`].
    pub refs: Range<usize>,
}

impl WayIn {
    pub fn new(id: i64, way: &profile::Way, refs: Range<usize>) -> Self {
        let dir = |d: Option<profile::Direction>| {
            d.map_or(Dir::default(), |d| Dir {
                kmh: d.kmh as f32,
                flags: d.flags | if d.restricted { RESTRICTED } else { 0 },
            })
        };
        WayIn {
            id,
            dirs: [dir(way.forward), dir(way.backward)],
            duration_s: way.duration_s.map(|d| d as f32),
            ferry: way.ferry,
            refs,
        }
    }
}

/// A restriction's `via` members.
#[derive(Clone, Debug, PartialEq)]
pub enum Via {
    Node(i64),
    /// Via ways in the relation's member order.
    Ways(Vec<i64>),
}

/// A `type=restriction` relation that applies to cars.
#[derive(Clone, Debug)]
pub struct RestrictionIn {
    pub id: i64,
    pub kind: profile::Restriction,
    pub from: Vec<i64>,
    pub via: Via,
    pub to: Vec<i64>,
}

/// What [`read`] gives [`build`].
pub struct Input {
    /// Sorted by ID.
    pub ways: Vec<WayIn>,
    /// The ways' node IDs, and their locations.
    pub refs: Vec<i64>,
    pub coords: Vec<P>,
    /// The nodes on those ways that the profile does something with.
    pub nodes: HashMap<i64, profile::Node>,
    pub restrictions: Vec<RestrictionIn>,
    pub read: ReadStats,
}

/// A directed edge, stored with its source node's edges.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Edge {
    pub to: u32,
    /// Travel time in tenths of a second, with the delays of the signals and barriers it
    /// enters.
    pub time_ds: u32,
    pub metres: f32,
    /// Index into [`Graph::ways`].
    pub way: u32,
    /// [`TOLL`], [`FERRY`], [`RESTRICTED`], [`ENTRY`].
    pub flags: u8,
}

impl Edge {
    /// What the route minimizes: the time, plus the penalty for entering restricted roads.
    pub fn weight(&self) -> u32 {
        self.time_ds
            + match self.flags & ENTRY {
                0 => 0,
                _ => profile::RESTRICTED_PENALTY_S * 10,
            }
    }
}

/// Edges grouped by source node: node `v`'s are `edges[first[v]..first[v + 1]]`.
#[derive(Default, Serialize, Deserialize)]
pub struct Csr {
    pub first: Vec<u32>,
    pub edges: Vec<Edge>,
}

impl Csr {
    pub fn nodes(&self) -> usize {
        self.first.len().saturating_sub(1)
    }

    pub fn out(&self, v: u32) -> &[Edge] {
        &self.edges[self.first[v as usize] as usize..self.first[v as usize + 1] as usize]
    }

    /// From edges in any order, with their source nodes.
    fn from_edges(nodes: usize, list: Vec<(u32, Edge)>) -> Csr {
        let mut first = vec![0u32; nodes + 1];
        for (from, _) in &list {
            first[*from as usize + 1] += 1;
        }
        for i in 0..nodes {
            first[i + 1] += first[i];
        }
        let mut next = first.clone();
        let blank = Edge {
            to: 0,
            time_ds: 0,
            metres: 0.0,
            way: 0,
            flags: 0,
        };
        let mut edges = vec![blank; list.len()];
        for (from, e) in list {
            let slot = &mut next[from as usize];
            edges[*slot as usize] = e;
            *slot += 1;
        }
        Csr { first, edges }
    }
}

#[derive(Serialize, Deserialize)]
pub struct Graph {
    pub csr: Csr,
    /// Per node.
    pub coords: Vec<P>,
    /// In a strongly connected component of fewer than [`SMALL_COMPONENT`] nodes.
    pub small: Vec<bool>,
    /// Nodes `0..base_nodes` are intersections, way ends and barrier ends; the rest are
    /// copies made by turn restrictions, of the nodes in `copy_of`.
    pub base_nodes: u32,
    pub copy_of: Vec<u32>,
    /// OSM way IDs, by [`Edge::way`].
    pub ways: Vec<i64>,
    pub stats: Stats,
}

#[derive(Default, Serialize, Deserialize)]
pub struct Stats {
    pub read: ReadStats,
    /// Ferries left out because one end meets no road.
    pub ferries_unconnected: u64,
    pub ways: u64,
    /// Before restrictions: intersections and way ends, then the extra ends at blocking
    /// barriers.
    pub base_nodes: u64,
    pub barrier_nodes: u64,
    pub base_edges: u64,
    /// Edges that would start and end at the same node: a node repeated in a way.
    pub loops: u64,
    pub nodes: u64,
    pub edges: u64,
    pub toll_edges: u64,
    pub ferry_edges: u64,
    pub restricted_edges: u64,
    pub entry_edges: u64,
    /// The largest [`Edge::weight`], in tenths of a second.
    pub max_weight_ds: u32,
    pub restrictions: RestrictionStats,
    pub components: ComponentStats,
    pub read_s: f64,
    pub build_s: f64,
    pub restrict_s: f64,
    pub components_s: f64,
    pub peak_rss_mib: f64,
}

/// Reads the PBF and builds its graph.
pub fn make(pbf: &Pbf) -> Result<Graph> {
    let t = Instant::now();
    let input = read(pbf)?;
    let read_s = t.elapsed().as_secs_f64();
    eprintln!(
        "  graph read: {} ways, {} node refs, {read_s:.1} s, peak RSS {:.0} MiB",
        input.ways.len(),
        input.refs.len(),
        peak_rss_mib()
    );
    let mut g = build(input);
    g.stats.read_s = read_s;
    g.stats.peak_rss_mib = peak_rss_mib();
    Ok(g)
}

/// Seconds to travel `metres` of `w` in direction `d`, of the way's `total` metres.
fn travel_s(w: &WayIn, d: Dir, metres: f64, total: f64) -> f64 {
    let share = if total > 0.0 { metres / total } else { 1.0 };
    let run = match w.duration_s {
        Some(s) => f64::from(s) * share,
        None => metres / (f64::from(d.kmh) / 3.6),
    };
    match w.ferry {
        true => run + profile::FERRY_BOARDING_S * share,
        false => run,
    }
}

fn metres(a: P, b: P) -> f64 {
    let dx = f64::from(a[0]) - f64::from(b[0]);
    let dy = f64::from(a[1]) - f64::from(b[1]);
    (dx * dx + dy * dy).sqrt() * geom::metres_per_unit(a)
}

pub fn build(mut input: Input) -> Graph {
    let start = Instant::now();
    let mut stats = Stats {
        read: std::mem::take(&mut input.read),
        ..Stats::default()
    };
    let refs = |w: &WayIn| &input.refs[w.refs.clone()];

    // Ferries must meet a road at both ends.
    let ends: HashSet<i64> = input
        .ways
        .iter()
        .filter(|w| w.ferry)
        .flat_map(|w| [refs(w)[0], *refs(w).last().unwrap()])
        .collect();
    let met: HashSet<i64> = input
        .ways
        .iter()
        .filter(|w| !w.ferry)
        .flat_map(|w| refs(w).iter().copied().filter(|id| ends.contains(id)))
        .collect();
    let before = input.ways.len();
    let ways = std::mem::take(&mut input.ways);
    let ways: Vec<WayIn> = ways
        .into_iter()
        .filter(|w| {
            !w.ferry || (met.contains(&refs(w)[0]) && met.contains(refs(w).last().unwrap()))
        })
        .collect();
    stats.ferries_unconnected = (before - ways.len()) as u64;
    stats.ways = ways.len() as u64;

    // Graph nodes: nodes on two ways (or twice on one), way ends and blocking barriers.
    let (seen, at) = (NodeSet::new(), NodeSet::new());
    for w in &ways {
        let r = refs(w);
        for &id in r {
            if seen.insert(id) {
                at.insert(id);
            }
        }
        at.insert(r[0]);
        at.insert(r[r.len() - 1]);
    }
    for (&id, n) in &input.nodes {
        if n.block && seen.contains(id) {
            at.insert(id);
        }
    }
    // A stretch that leaves a node and comes back to it with no graph node between (a loop
    // drive) gets its middle node too, so it is not left out as a loop.
    for w in &ways {
        let r = refs(w);
        let mut i = 0;
        for k in 1..r.len() {
            if at.contains(r[k]) {
                if r[k] == r[i] && k > i + 1 {
                    at.insert(r[(i + k) / 2]);
                }
                i = k;
            }
        }
    }
    drop(seen);
    let ranks = NodeRanks::new(at);
    let mut coords = vec![[0u32; 2]; ranks.len() as usize];

    // Edges, way by way. A blocking barrier's first edge uses its node; later ones get a copy.
    let members: HashSet<i64> = input
        .restrictions
        .iter()
        .flat_map(|r| {
            let via = match &r.via {
                Via::Node(_) => &[][..],
                Via::Ways(w) => &w[..],
            };
            r.from.iter().chain(via).chain(&r.to).copied()
        })
        .collect();
    let mut seqs: HashMap<i64, Vec<u32>> = HashMap::new();
    let mut blocked_used: HashSet<u32> = HashSet::new();
    let mut list: Vec<(u32, Edge)> = Vec::new();
    for (wi, w) in ways.iter().enumerate() {
        let r = refs(w);
        let pts = &input.coords[w.refs.clone()];
        let total: f64 = pts.windows(2).map(|p| metres(p[0], p[1])).sum();
        let node = |k: usize| input.nodes.get(&r[k]);
        let mut id_at = |k: usize, coords: &mut Vec<P>| -> u32 {
            let rank = ranks.rank(r[k]).expect("graph node") as u32;
            coords[rank as usize] = pts[k];
            if node(k).is_some_and(|n| n.block) && !blocked_used.insert(rank) {
                coords.push(pts[k]);
                stats.barrier_nodes += 1;
                return coords.len() as u32 - 1;
            }
            rank
        };
        let mut seq = Vec::new();
        let (mut i, mut a, mut len) = (0, id_at(0, &mut coords), 0.0);
        for k in 1..r.len() {
            len += metres(pts[k - 1], pts[k]);
            if ranks.rank(r[k]).is_none() {
                continue;
            }
            let b = id_at(k, &mut coords);
            seq.push(a);
            if a == b {
                stats.loops += 1;
            } else {
                // Forward enters nodes i+1..=k; backward enters k-1..=i.
                for (fwd, from, to, nodes) in [(true, a, b, i + 1..k + 1), (false, b, a, i..k)] {
                    let d = w.dirs[usize::from(!fwd)];
                    if d.kmh <= 0.0 {
                        continue;
                    }
                    let (mut delay, mut flags) = (0, d.flags);
                    for n in nodes.filter_map(node) {
                        if !n.block
                            && if fwd {
                                n.applies.forward()
                            } else {
                                n.applies.backward()
                            }
                        {
                            delay += n.delay_s;
                            if n.toll {
                                flags |= TOLL;
                            }
                        }
                    }
                    let s = travel_s(w, d, len, total) + f64::from(delay);
                    let e = Edge {
                        to,
                        time_ds: ((s * 10.0).round() as u32).max(1),
                        metres: len as f32,
                        way: wi as u32,
                        flags,
                    };
                    list.push((from, e));
                }
            }
            (i, len) = (k, 0.0);
            a = match k + 1 < r.len() {
                true => id_at(k, &mut coords),
                false => b,
            };
        }
        seq.push(a);
        if members.contains(&w.id) {
            seqs.insert(w.id, seq);
        }
    }
    let n = coords.len();
    stats.base_nodes = n as u64;
    stats.base_edges = list.len() as u64;

    // Entering restricted roads from open ones costs the penalty.
    let mut open = vec![false; n];
    for (from, e) in &list {
        if e.flags & RESTRICTED == 0 {
            open[*from as usize] = true;
            open[e.to as usize] = true;
        }
    }
    for (from, e) in &mut list {
        if e.flags & RESTRICTED != 0 && open[*from as usize] {
            e.flags |= ENTRY;
        }
    }
    drop(open);
    let base = Csr::from_edges(n, list);
    stats.build_s = start.elapsed().as_secs_f64();

    let t = Instant::now();
    let way_ids: Vec<i64> = ways.iter().map(|w| w.id).collect();
    drop(ways);
    let via_rank = |id: i64| ranks.rank(id).map(|r| r as u32);
    let (csr, copy_of) = restrict::apply(
        base,
        &seqs,
        via_rank,
        &input.restrictions,
        &mut stats.restrictions,
    );
    drop(ranks);
    for &c in &copy_of {
        coords.push(coords[c as usize]);
    }
    stats.restrict_s = t.elapsed().as_secs_f64();

    let t = Instant::now();
    let (small, components) = scc::mark_small(&csr, SMALL_COMPONENT);
    stats.components = components;
    stats.components_s = t.elapsed().as_secs_f64();

    stats.nodes = csr.nodes() as u64;
    stats.edges = csr.edges.len() as u64;
    let count = |f: u8| csr.edges.iter().filter(|e| e.flags & f != 0).count() as u64;
    stats.toll_edges = count(TOLL);
    stats.ferry_edges = count(FERRY);
    stats.restricted_edges = count(RESTRICTED);
    stats.entry_edges = count(ENTRY);
    stats.max_weight_ds = csr.edges.iter().map(Edge::weight).max().unwrap_or(0);
    Graph {
        csr,
        coords,
        small,
        base_nodes: n as u32,
        copy_of,
        ways: way_ids,
        stats,
    }
}

pub fn print_stats(s: &Stats) {
    let r = &s.read;
    eprintln!(
        "  graph: {} road and ferry ways read, {} kept ({} ferries, {} left out for meeting \
         no road); dropped: access {}, closed {}, size {}, speed {}; {} node refs missing",
        r.ways_read,
        s.ways,
        r.ferries.saturating_sub(s.ferries_unconnected),
        s.ferries_unconnected,
        r.dropped_access,
        r.dropped_closed,
        r.dropped_size,
        r.dropped_speed,
        r.missing_node_refs
    );
    eprintln!(
        "  graph: nodes: {} signals, {} passable barriers, {} toll booths, {} blocking \
         barriers ({} ends added)",
        r.signals, r.passable_barriers, r.toll_booths, r.blocking_barriers, s.barrier_nodes
    );
    let x = &s.restrictions;
    eprintln!(
        "  graph: restrictions: {} relations, {} for cars, {} applied ({} via ways), {} \
         missing members, {} broken; {} forbidden paths, {} nodes added",
        r.restriction_relations,
        x.for_cars,
        x.applied,
        x.via_ways,
        x.missing_members,
        x.broken,
        x.forbidden_paths,
        x.nodes_added
    );
    let c = &s.components;
    eprintln!(
        "  graph: {} nodes ({} before restrictions), {} edges ({} toll, {} ferry, {} \
         restricted, {} entering restricted), longest {:.0} s; {} loops left out",
        s.nodes,
        s.base_nodes,
        s.edges,
        s.toll_edges,
        s.ferry_edges,
        s.restricted_edges,
        s.entry_edges,
        f64::from(s.max_weight_ds) / 10.0,
        s.loops
    );
    eprintln!(
        "  graph: {} components, largest {} nodes; {} nodes in {} small ones; sizes 1 / \
         2–9 / 10–99 / 100–999 / 1000+: {:?}",
        c.count, c.largest, c.small_nodes, c.small, c.by_size
    );
    eprintln!(
        "  graph: read {:.1} s, edges {:.1} s, restrictions {:.1} s, components {:.1} s, \
         peak RSS {:.0} MiB",
        s.read_s, s.build_s, s.restrict_s, s.components_s, s.peak_rss_mib
    );
}

#[derive(Args)]
pub struct GraphArgs {
    /// OSM extract (.osm.pbf).
    input: PathBuf,

    /// Write the counts as JSON.
    #[arg(long)]
    json: Option<PathBuf>,

    /// Save the graph (postcard), for rerunning the next stage alone.
    #[arg(long)]
    save: Option<PathBuf>,

    /// Write the IDs of the ways in the graph, one per line, to compare with OSRM's.
    #[arg(long)]
    way_ids: Option<PathBuf>,
}

#[derive(Serialize)]
struct Report<'a> {
    kind: &'static str,
    input: String,
    wall_s: f64,
    stats: &'a Stats,
}

/// `omp-builder graph`: the car routing graph of a PBF, without the rest of `build`.
pub fn run(args: GraphArgs) -> Result<()> {
    let start = Instant::now();
    let pbf = Pbf::open(&args.input)?;
    let g = make(&pbf)?;
    print_stats(&g.stats);
    if let Some(path) = &args.way_ids {
        let mut w = std::io::BufWriter::new(std::fs::File::create(path)?);
        for id in &g.ways {
            writeln!(w, "{id}")?;
        }
        w.flush()?;
    }
    if let Some(path) = &args.save {
        let t = Instant::now();
        let mut w = std::io::BufWriter::new(std::fs::File::create(path)?);
        postcard::to_io(&g, &mut w)?;
        w.flush()?;
        eprintln!(
            "  saved {} ({:.1} MiB) in {:.1} s",
            path.display(),
            mib(std::fs::metadata(path)?.len()),
            t.elapsed().as_secs_f64()
        );
    }
    let report = Report {
        kind: "graph",
        input: args.input.display().to_string(),
        wall_s: start.elapsed().as_secs_f64(),
        stats: &g.stats,
    };
    if let Some(path) = &args.json {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(&report)?)?;
    }
    eprintln!(
        "graph: {:.1} s, peak RSS {:.0} MiB",
        start.elapsed().as_secs_f64(),
        peak_rss_mib()
    );
    Ok(())
}
