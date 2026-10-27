//! Car road graph and an off-the-shelf contraction hierarchy (`fast_paths`), for size numbers only.

use std::collections::HashMap;
use std::time::Instant;

use anyhow::Result;
use fast_paths::InputGraph;
use omp_core::whitelist::Whitelist;
use serde::Serialize;

use crate::geom::{self, P};
use crate::pbf::Pbf;
use crate::pbf::features::{self, Road};
use crate::pbf::nodes::NodeSet;

#[derive(Serialize)]
pub struct RoutingStats {
    pub road_ways: u64,
    pub routable_ways: u64,
    /// Intersections and dead ends (way endpoints).
    pub nodes: u64,
    /// Directed edges between graph nodes (a two-way segment counts twice).
    pub edges: u64,
    pub oneway_segments: u64,
    pub restriction_relations: u64,
    /// The contraction hierarchy; `None` when only the graph was counted.
    #[serde(flatten)]
    pub ch: Option<ChStats>,
}

#[derive(Serialize)]
pub struct ChStats {
    pub ch_prepare_s: f64,
    pub ch_edges: u64,
    pub ch_shortcuts: u64,
    /// `fast_paths` in-memory layout: 5 × u32 per CH edge, 3 × u32 per node.
    pub ch_bytes_fast_paths: u64,
    /// A compact layout: target, weight and middle node (u32 each) per CH edge, plus one u32
    /// first-edge offset and 8 bytes of coordinates per node.
    pub ch_bytes_compact: u64,
}

/// Default car speeds in km/h by road class name, used when `maxspeed` is missing.
fn default_speed(class: &str) -> f64 {
    match class {
        "motorway" => 105.0,
        "trunk" => 85.0,
        "primary" => 65.0,
        "secondary" => 55.0,
        "tertiary" => 45.0,
        c if c.ends_with("_link") => 45.0,
        "unclassified" | "residential" => 35.0,
        "living_street" => 10.0,
        _ => 15.0,
    }
}

fn metres(a: P, b: P) -> f64 {
    let dx = f64::from(a[0]) - f64::from(b[0]);
    let dy = f64::from(a[1]) - f64::from(b[1]);
    (dx * dx + dy * dy).sqrt() * geom::metres_per_unit(a)
}

fn routable(roads: &[Road]) -> Vec<&Road> {
    roads
        .iter()
        .filter(|r| r.routable && r.refs.len() >= 2)
        .collect()
}

pub fn build(roads: &[Road], class_names: &[String], restrictions: u64) -> RoutingStats {
    let routable = routable(roads);
    // A node is in the graph if two ways share it, a way passes it twice, or a way ends there.
    let mut uses: HashMap<i64, u32> = HashMap::new();
    for r in &routable {
        for id in &r.refs {
            *uses.entry(*id).or_default() += 1;
        }
        *uses.entry(r.refs[0]).or_default() += 1;
        *uses.entry(*r.refs.last().unwrap()).or_default() += 1;
    }
    let mut ids: HashMap<i64, usize> = HashMap::new();
    let mut graph = InputGraph::new();
    let (mut edges, mut oneway_segments) = (0u64, 0u64);
    for r in &routable {
        let speed = r
            .maxspeed_kmh
            .unwrap_or_else(|| default_speed(&class_names[r.class as usize]))
            .max(5.0);
        let mut from = r.refs[0];
        let mut dist = 0.0;
        for i in 1..r.refs.len() {
            dist += metres(r.coords[i - 1], r.coords[i]);
            let id = r.refs[i];
            if uses[&id] < 2 {
                continue;
            }
            let n = ids.len();
            let a = *ids.entry(from).or_insert(n);
            let n = ids.len();
            let b = *ids.entry(id).or_insert(n);
            // Weight: travel time in tenths of a second; fast_paths ignores zero-weight edges.
            let w = ((dist / (speed / 3.6)) * 10.0).round().max(1.0) as usize;
            if a != b {
                match r.oneway {
                    1 => graph.add_edge(a, b, w),
                    -1 => graph.add_edge(b, a, w),
                    _ => graph.add_edge_bidir(a, b, w),
                };
                if r.oneway == 0 {
                    edges += 2;
                } else {
                    edges += 1;
                    oneway_segments += 1;
                }
            }
            from = id;
            dist = 0.0;
        }
    }
    graph.freeze();
    let start = Instant::now();
    let fast = fast_paths::prepare(&graph);
    let ch_prepare_s = start.elapsed().as_secs_f64();
    let nodes = fast.get_num_nodes() as u64;
    let ch_edges = (fast.get_num_out_edges() + fast.get_num_in_edges()) as u64;
    RoutingStats {
        road_ways: roads.len() as u64,
        routable_ways: routable.len() as u64,
        nodes,
        edges,
        oneway_segments,
        restriction_relations: restrictions,
        ch: Some(ChStats {
            ch_prepare_s,
            ch_edges,
            ch_shortcuts: ch_edges.saturating_sub(graph.get_num_edges() as u64),
            ch_bytes_fast_paths: ch_edges * 20 + nodes * 12,
            ch_bytes_compact: ch_edges * 12 + nodes * 12,
        }),
    }
}

/// Counts the car routing graph like [`build`] does, without keeping the roads:
/// two more passes over the ways, one marking nodes that are intersections or way ends and
/// one counting the edges between them. For a continent, where the roads do not fit next to
/// everything else.
pub fn count_graph(pbf: &Pbf, wl: &Whitelist, restrictions: u64) -> Result<RoutingStats> {
    let roads = |f: &(dyn Fn(&[i64], i8, bool) + Sync)| features::road_ways(pbf, wl, f);
    let routable = |refs: &[i64], ok: bool| ok && refs.len() >= 2;
    let (seen, graph_nodes) = (NodeSet::new(), NodeSet::new());
    let [road_ways, routable_ways, edges, oneway_segments] =
        [(); 4].map(|_| std::sync::atomic::AtomicU64::new(0));
    let add = |n: &std::sync::atomic::AtomicU64, v: u64| {
        n.fetch_add(v, std::sync::atomic::Ordering::Relaxed);
    };
    roads(&|refs, _, ok| {
        add(&road_ways, 1);
        if !routable(refs, ok) {
            return;
        }
        add(&routable_ways, 1);
        for &id in refs {
            if seen.insert(id) {
                graph_nodes.insert(id);
            }
        }
        graph_nodes.insert(refs[0]);
        graph_nodes.insert(refs[refs.len() - 1]);
    })?;
    drop(seen);
    roads(&|refs, oneway, ok| {
        if !routable(refs, ok) {
            return;
        }
        let mut from = refs[0];
        for &id in &refs[1..] {
            if !graph_nodes.contains(id) {
                continue;
            }
            if from != id {
                if oneway == 0 {
                    add(&edges, 2);
                } else {
                    add(&edges, 1);
                    add(&oneway_segments, 1);
                }
            }
            from = id;
        }
    })?;
    Ok(RoutingStats {
        road_ways: road_ways.into_inner(),
        routable_ways: routable_ways.into_inner(),
        nodes: graph_nodes.len(),
        edges: edges.into_inner(),
        oneway_segments: oneway_segments.into_inner(),
        restriction_relations: restrictions,
        ch: None,
    })
}
