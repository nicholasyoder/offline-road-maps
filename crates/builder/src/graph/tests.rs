use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

use omp_core::profile::{self, FERRY, Restriction, TOLL};

use super::*;

/// Node `10 x + y` is at grid point `(x, y)`, 100 m apart.
pub(crate) fn pos(id: i64) -> P {
    let (x, y) = ((id / 10) as f64, (id % 10) as f64);
    let lat = 40.0 + y * 100.0 / 111_320.0;
    let lon = -80.0 + x * 100.0 / (111_320.0 * 40f64.to_radians().cos());
    geom::project(lat, lon)
}

pub(crate) const STREET: &[(&str, &str)] = &[("highway", "residential")];

/// A hand-built [`Input`].
#[derive(Default)]
pub(crate) struct In {
    pub(crate) ways: Vec<WayIn>,
    pub(crate) refs: Vec<i64>,
    nodes: HashMap<i64, profile::Node>,
    restrictions: Vec<RestrictionIn>,
    labels: Labels,
    pub(crate) junctions: Vec<(i64, String)>,
}

impl In {
    pub(crate) fn way(&mut self, id: i64, tags: &[(&str, &str)], nodes: &[i64]) -> &mut Self {
        let w = profile::way(tags).unwrap();
        let (class, roundabout, label) = way_tags(tags);
        let info = WayInfo {
            class,
            roundabout,
            label: self.labels.intern(label),
        };
        let start = self.refs.len();
        self.refs.extend(nodes);
        self.ways
            .push(WayIn::new(id, &w, info, start..self.refs.len()));
        self
    }

    pub(crate) fn node(&mut self, id: i64, tags: &[(&str, &str)]) -> &mut Self {
        self.nodes.insert(id, profile::node(tags).unwrap());
        self
    }

    pub(crate) fn restriction(
        &mut self,
        kind: Restriction,
        from: i64,
        via: Via,
        to: i64,
    ) -> &mut Self {
        self.restrictions.push(RestrictionIn {
            id: self.restrictions.len() as i64,
            kind,
            from: vec![from],
            via,
            to: vec![to],
        });
        self
    }

    pub(crate) fn build(&self) -> Graph {
        let mut ways = self.ways.clone();
        ways.sort_by_key(|w| w.id);
        build(Input {
            ways,
            refs: self.refs.clone(),
            coords: self.refs.iter().map(|&id| pos(id)).collect(),
            nodes: self.nodes.clone(),
            restrictions: self.restrictions.clone(),
            labels: Labels {
                list: self.labels.list.clone(),
                ..Labels::default()
            },
            junctions: self.junctions.clone(),
            read: ReadStats::default(),
        })
    }
}

/// The graph node at OSM node `osm` (its first, for a barrier).
fn id(g: &Graph, osm: i64) -> u32 {
    (0..g.base_nodes)
        .find(|&i| g.coords[i as usize] == pos(osm))
        .unwrap_or_else(|| panic!("no node {osm}"))
}

fn orig(g: &Graph, v: u32) -> u32 {
    match v < g.base_nodes {
        true => v,
        false => g.copy_of[(v - g.base_nodes) as usize],
    }
}

fn edge(g: &Graph, a: i64, b: i64) -> Edge {
    let (a, b) = (id(g, a), id(g, b));
    *g.csr
        .out(a)
        .iter()
        .find(|e| orig(g, e.to) == b)
        .expect("edge")
}

fn has_edge(g: &Graph, a: i64, b: i64) -> bool {
    let (a, b) = (id(g, a), id(g, b));
    g.csr.out(a).iter().any(|e| orig(g, e.to) == b)
}

/// Shortest weights from `src` to every node, by plain Dijkstra.
fn dijkstra(g: &Csr, src: u32) -> Vec<u32> {
    let mut dist = vec![u32::MAX; g.nodes()];
    let mut heap = BinaryHeap::from([Reverse((0, src))]);
    dist[src as usize] = 0;
    while let Some(Reverse((d, v))) = heap.pop() {
        if d > dist[v as usize] {
            continue;
        }
        for e in g.out(v) {
            let nd = d + e.weight();
            if nd < dist[e.to as usize] {
                dist[e.to as usize] = nd;
                heap.push(Reverse((nd, e.to)));
            }
        }
    }
    dist
}

/// Shortest weights to every base node, reaching any of its copies.
fn distances(g: &Graph, src: u32) -> Vec<u32> {
    let d = dijkstra(&g.csr, src);
    let mut out = vec![u32::MAX; g.base_nodes as usize];
    for (v, &d) in d.iter().enumerate() {
        let o = orig(g, v as u32) as usize;
        out[o] = out[o].min(d);
    }
    out
}

/// Shortest weights on `base` (a graph without restrictions) from `src`, never walking a
/// path in `forbidden`: Dijkstra over the last few nodes walked.
fn reference(base: &Csr, forbidden: &[Vec<u32>], src: u32) -> Vec<u32> {
    let keep = forbidden.iter().map(Vec::len).max().unwrap_or(1) - 1;
    let mut best: HashMap<Vec<u32>, u32> = HashMap::new();
    let mut out = vec![u32::MAX; base.nodes()];
    let mut heap = BinaryHeap::from([Reverse((0, vec![src]))]);
    while let Some(Reverse((d, hist))) = heap.pop() {
        if best.get(&hist).is_some_and(|&b| b <= d) {
            continue;
        }
        best.insert(hist.clone(), d);
        let v = *hist.last().unwrap();
        out[v as usize] = out[v as usize].min(d);
        for e in base.out(v) {
            let mut next = hist.clone();
            next.push(e.to);
            if forbidden.iter().any(|f| next.ends_with(f)) {
                continue;
            }
            let cut = next.len().saturating_sub(keep);
            heap.push(Reverse((d + e.weight(), next[cut..].to_vec())));
        }
    }
    out
}

#[test]
fn edges_between_intersections() {
    let g = In::default()
        .way(1, STREET, &[11, 21, 31])
        .way(2, STREET, &[21, 22])
        .build();
    assert_eq!((g.stats.nodes, g.stats.edges), (4, 6));
    let e = edge(&g, 11, 21);
    // 100 m at 25 km/h is 14.4 s.
    assert!((143..=146).contains(&e.time_ds), "{e:?}");
    let seg = g.segments[(e.seg >> 1) as usize];
    assert!((seg.metres - 100.0).abs() < 1.0);
    assert_eq!(g.ways[seg.way as usize], 1);
    // A way through a node that no other way uses is one edge.
    let g = In::default().way(1, STREET, &[11, 21, 31]).build();
    assert_eq!((g.stats.nodes, g.stats.edges), (2, 2));
    assert!((288..=292).contains(&edge(&g, 11, 31).time_ds));
    // A loop drive is cut at its middle node instead of left out.
    let g = In::default()
        .way(1, STREET, &[11, 21])
        .way(2, STREET, &[21, 31, 32, 22, 21])
        .build();
    assert_eq!((g.stats.nodes, g.stats.edges, g.stats.loops), (3, 6, 0));
    assert!(has_edge(&g, 21, 32) && has_edge(&g, 32, 21));
}

#[test]
fn oneway_edges() {
    let oneway = &[("highway", "residential"), ("oneway", "yes")];
    let g = In::default().way(1, oneway, &[11, 21]).build();
    assert!(has_edge(&g, 11, 21) && !has_edge(&g, 21, 11));
    let reverse = &[("highway", "residential"), ("oneway", "-1")];
    let g = In::default().way(1, reverse, &[11, 21]).build();
    assert!(!has_edge(&g, 11, 21) && has_edge(&g, 21, 11));
}

#[test]
fn signals_and_barriers_on_edges() {
    let plain = In::default().way(1, STREET, &[11, 15, 21]).build();
    let t = |g: &Graph| (edge(g, 11, 21).time_ds, edge(g, 21, 11).time_ds);
    let (f, b) = t(&plain);

    let g = In::default()
        .way(1, STREET, &[11, 15, 21])
        .node(15, &[("highway", "traffic_signals")])
        .build();
    assert_eq!(t(&g), (f + 20, b + 20));
    let g = In::default()
        .way(1, STREET, &[11, 15, 21])
        .node(
            15,
            &[("highway", "traffic_signals"), ("direction", "forward")],
        )
        .build();
    assert_eq!(t(&g), (f + 20, b));

    // At an intersection, the edges entering it pay; those leaving do not.
    let g = In::default()
        .way(1, STREET, &[11, 21, 31])
        .way(2, STREET, &[21, 22])
        .node(21, &[("highway", "traffic_signals")])
        .build();
    let base = In::default()
        .way(1, STREET, &[11, 21, 31])
        .way(2, STREET, &[21, 22])
        .build();
    for (a, b, extra) in [(11, 21, 20), (21, 11, 0), (22, 21, 20), (21, 31, 0)] {
        assert_eq!(edge(&g, a, b).time_ds, edge(&base, a, b).time_ds + extra);
    }

    let g = In::default()
        .way(1, STREET, &[11, 15, 21])
        .node(15, &[("barrier", "toll_booth")])
        .build();
    for (a, b) in [(11, 21), (21, 11)] {
        let e = edge(&g, a, b);
        assert_eq!(e.flags & TOLL, TOLL);
    }
    assert_eq!(t(&g), (f + 100, b + 100));
}

#[test]
fn blocking_barriers_cut_the_road() {
    let g = In::default()
        .way(1, STREET, &[11, 21, 31])
        .way(2, STREET, &[21, 22])
        .node(21, &[("barrier", "bollard")])
        .build();
    // 11, 31, 22 and three ends at the bollard.
    assert_eq!((g.stats.nodes, g.stats.barrier_nodes), (6, 2));
    let d = distances(&g, id(&g, 11));
    for n in [31, 22] {
        assert_eq!(d[id(&g, n) as usize], u32::MAX);
    }
    // The road up to the bollard stays.
    assert!(d[id(&g, 21) as usize] < u32::MAX);
}

#[test]
fn ferries() {
    let ferry = &[("route", "ferry"), ("duration", "PT20M")];
    let g = In::default()
        .way(1, STREET, &[11, 21])
        .way(2, STREET, &[51, 61])
        .way(3, ferry, &[21, 51])
        .way(4, &[("route", "ferry")], &[61, 71])
        .build();
    assert_eq!(g.stats.ferries_unconnected, 1);
    let e = edge(&g, 21, 51);
    assert_eq!(e.flags, FERRY);
    assert_eq!(e.time_ds, 18_000);

    // A crossing in two ways that meet at sea is kept; a branch off it to nowhere is not.
    let plain = &[("route", "ferry")];
    let g = In::default()
        .way(1, STREET, &[11, 21])
        .way(2, STREET, &[51, 61])
        .way(3, plain, &[21, 31, 41])
        .way(4, plain, &[41, 51])
        .way(5, plain, &[31, 33])
        .build();
    assert_eq!(g.stats.ferries_unconnected, 1);
    assert!(has_edge(&g, 21, 41) && has_edge(&g, 41, 51));
}

#[test]
fn restricted_roads_cost_to_enter() {
    let dest = &[("highway", "residential"), ("access", "destination")];
    let g = In::default()
        .way(1, STREET, &[11, 21, 31])
        .way(2, dest, &[21, 22, 23])
        .way(3, dest, &[22, 32])
        .build();
    let e = edge(&g, 21, 22);
    assert_eq!(e.flags, RESTRICTED | ENTRY);
    assert_eq!(e.weight(), e.time_ds + profile::RESTRICTED_PENALTY_S * 10);
    assert_eq!(edge(&g, 22, 23).flags, RESTRICTED);
    assert_eq!(edge(&g, 22, 21).flags, RESTRICTED);
    assert_eq!(edge(&g, 21, 11).flags, 0);

    // A private road is restricted like a destination one, and so is the road through a
    // private gate: a gated street of plain residential roads.
    let private = &[("highway", "residential"), ("access", "private")];
    let g = In::default()
        .way(1, STREET, &[11, 21, 31])
        .way(2, private, &[21, 22])
        .way(3, STREET, &[31, 32, 33])
        .node(32, &[("barrier", "gate"), ("access", "private")])
        .build();
    assert_eq!(edge(&g, 21, 22).flags, RESTRICTED | ENTRY);
    assert_eq!(edge(&g, 31, 33).flags, RESTRICTED | ENTRY);
    assert_eq!(edge(&g, 33, 31).flags, RESTRICTED);
    assert_eq!(edge(&g, 21, 31).flags, 0);
}

/// A 3 × 3 grid of streets, one way per block, with a one-way street out to 41.
pub(crate) fn grid() -> In {
    let mut i = In::default();
    let mut id = 100;
    for x in 1..=3 {
        for y in 1..=3 {
            let n = 10 * x + y;
            if x < 3 {
                i.way(id, STREET, &[n, n + 10]);
                id += 1;
            }
            if y < 3 {
                i.way(id, STREET, &[n, n + 1]);
                id += 1;
            }
        }
    }
    i.way(
        200,
        &[("highway", "residential"), ("oneway", "yes")],
        &[31, 41],
    );
    i
}

/// The ID of the grid's way between nodes `a` and `b`.
pub(crate) fn way_id(i: &In, a: i64, b: i64) -> i64 {
    i.ways
        .iter()
        .find(|w| {
            let r = &i.refs[w.refs.clone()];
            r == [a, b] || r == [b, a]
        })
        .unwrap()
        .id
}

#[test]
fn restrictions_match_a_search_over_paths() {
    use Restriction::*;
    let base_in = grid();
    let base = base_in.build();
    let mut i = grid();
    let w = |a, b| way_id(&base_in, a, b);
    i.restriction(No, w(11, 21), Via::Node(21), w(21, 22))
        .restriction(Only, w(12, 22), Via::Node(22), w(22, 32))
        .restriction(No, w(13, 23), Via::Ways(vec![w(23, 33)]), w(33, 32))
        .restriction(No, w(21, 22), Via::Node(22), w(22, 23))
        // From a one-way street leading away from the via node: never reached.
        .restriction(No, 200, Via::Node(31), w(31, 32))
        // Not in the graph.
        .restriction(No, 999, Via::Node(21), w(21, 22))
        // The from way does not touch the via node.
        .restriction(No, w(11, 21), Via::Node(33), w(33, 32));
    let g = i.build();
    let s = &g.stats.restrictions;
    assert_eq!(
        (
            s.for_cars,
            s.applied,
            s.via_ways,
            s.missing_members,
            s.broken
        ),
        (7, 5, 1, 1, 1)
    );
    // Copies of 21, 22 (twice), 23 and 33; the one-way's copy of 31 is never reached.
    assert_eq!(s.nodes_added, 5);

    let ids = |p: &[i64]| p.iter().map(|&n| id(&base, n)).collect::<Vec<_>>();
    let forbidden: Vec<Vec<u32>> = [
        &[11, 21, 22][..],
        &[12, 22, 21],
        &[12, 22, 23],
        &[12, 22, 12],
        &[13, 23, 33, 32],
        &[21, 22, 23],
        &[41, 31, 32],
    ]
    .iter()
    .map(|p| ids(p))
    .collect();
    // The same node in `g`, found by its location.
    let in_g = |v: u32| {
        (0..g.base_nodes)
            .find(|&x| g.coords[x as usize] == base.coords[v as usize])
            .unwrap()
    };
    for src in 0..base.base_nodes {
        let want = reference(&base.csr, &forbidden, src);
        let got = distances(&g, in_g(src));
        for (v, &w) in want.iter().enumerate() {
            assert_eq!(got[in_g(v as u32) as usize], w, "from {src} to {v}");
        }
    }
    // Coming from 11, 21 is a copy without the turn to 22.
    let copy = edge(&g, 11, 21).to;
    assert!(copy >= g.base_nodes);
    let outs: Vec<u32> = g.csr.out(copy).iter().map(|e| orig(&g, e.to)).collect();
    assert!(outs.contains(&id(&g, 31)) && !outs.contains(&id(&g, 22)));
}

#[test]
fn small_components() {
    let e = |to| Edge {
        to,
        time_ds: 1,
        seg: 0,
        flags: 0,
    };
    // 0 ⇄ 1 ⇄ 2, and 3 → 0 with no way back.
    let g = Csr::from_edges(
        4,
        vec![(0, e(1)), (1, e(0)), (1, e(2)), (2, e(1)), (3, e(0))],
    );
    let (small, st) = scc::mark_small(&g, 2);
    assert_eq!(small, [false, false, false, true]);
    assert_eq!(
        (st.count, st.largest, st.small, st.small_nodes),
        (2, 3, 1, 1)
    );
    assert_eq!(st.by_size, [1, 1, 0, 0, 0]);
}
