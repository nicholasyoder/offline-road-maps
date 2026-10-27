use std::sync::Arc;

use omp_core::pack::{self, Info, Pack, Writer};
use omp_core::profile::Restriction;
use omp_core::route::{AVOID, FERRY, FERRY_CLASS, Routing, TOLL, cell_xy};

use super::*;
use crate::ch::check::dijkstra;
use crate::graph::Via;
use crate::graph::tests::{In, grid, way_id};
use crate::street::WayStreets;

/// The grid of `graph::tests`, with a toll motorway, a ferry, a long way with inner nodes,
/// a turn restriction and an exit number.
fn network() -> In {
    let mut i = grid();
    let motorway = &[
        ("highway", "motorway"),
        ("oneway", "no"),
        ("toll", "yes"),
        ("ref", "I 71"),
        ("destination", "Columbus"),
    ];
    i.way(300, motorway, &[13, 43]);
    i.way(
        301,
        &[("route", "ferry"), ("name", "Island Ferry")],
        &[41, 43],
    );
    i.way(
        302,
        &[("highway", "residential"), ("name", "Long Rd")],
        &[43, 44, 45, 33],
    );
    let (from, to) = (way_id(&i, 12, 22), way_id(&i, 22, 23));
    i.restriction(Restriction::No, from, Via::Node(22), to);
    i.junctions.push((13, "12A".into()));
    i
}

fn plain(g: &Graph, v: u32) -> u32 {
    match v < g.base_nodes {
        true => v,
        false => g.copy_of[(v - g.base_nodes) as usize],
    }
}

#[test]
fn sections_read_back_through_the_app_reader() {
    let g = network().build();
    assert!(!g.copy_of.is_empty(), "the restriction makes copies");
    let h = ch::contract(&g.csr, &ch::Params::default());
    let streets = WayStreets(vec![(302, 0, 7), (302, 2, 8)]);
    let p = Params {
        block_nodes: 4,
        top_mib: 0.0001,
    };
    let (s, sizes, o, _) = sections(&g, &h, &streets, &p).unwrap();
    assert!(o.top > 0 && o.top < g.csr.nodes() as u32, "top {}", o.top);

    // Small chunks and a section in front, so the routing records cross chunk boundaries.
    let dir = tempfile::tempdir().unwrap();
    let mut w = Writer::create(dir.path(), "t", 2 * pack::PAGE).unwrap();
    w.section(*b"FILL", &[7; 5000]).unwrap();
    s.write(&mut w).unwrap();
    let info = Info {
        pack_id: [1; 16],
        bbox: [0; 4],
        build_time: 0,
        name: "Test".into(),
        snapshot: String::new(),
    };
    let paths = w.finish(&info).unwrap();
    assert!(paths.len() > 2);
    let r = Routing::open(Arc::new(Pack::open(&paths[0]).unwrap()))
        .unwrap()
        .unwrap();
    let n = g.csr.nodes() as u32;
    assert_eq!(r.header.nodes, n);
    assert_eq!(r.header.segments as usize, g.segments.len());
    assert_eq!(u64::from(r.header.edges), sizes.edges);

    // Segments: the graph's, renumbered, with their geometry, street, label and class.
    let mut ours = Vec::new();
    for v in 0..g.base_nodes {
        for e in g.csr.out(v) {
            if e.seg & 1 == 0 {
                let pts = g.points(e.seg >> 1);
                ours.push((o.id[v as usize], o.id[plain(&g, e.to) as usize], pts));
            }
        }
    }
    let mut theirs = Vec::new();
    let (mut named, mut ferry, mut signed) = (false, false, false);
    r.for_each_segment(|i, seg| {
        theirs.push((seg.a, seg.b, seg.points.clone()));
        let (x, y) = cell_xy(seg.points[0]);
        let cell = r.cell(x, y).unwrap();
        assert!(cell.home.contains(&i), "segment {i} not home in its cell");
        named |= seg.street == Some(7);
        if seg.class == FERRY_CLASS {
            ferry = true;
            assert_eq!(r.label(seg.label.unwrap()).unwrap().name, "Island Ferry");
            assert_eq!(seg.forward.flags & FERRY, FERRY);
        }
        if let Some(l) = seg.label.map(|l| r.label(l).unwrap())
            && l.destination == "Columbus"
        {
            signed = true;
            assert_eq!(l.refs, "I 71");
            assert_eq!(seg.forward.flags & TOLL, TOLL);
        }
    })
    .unwrap();
    ours.sort();
    theirs.sort();
    assert_eq!(ours, theirs);
    assert!(named && ferry && signed);

    // Exit numbers on the node and its copies; copies and the ways out of them.
    let exit = (0..g.base_nodes)
        .find(|&v| g.coords[v as usize] == crate::graph::tests::pos(13))
        .unwrap();
    assert_eq!(
        r.junction(o.id[exit as usize]).unwrap().as_deref(),
        Some("12A")
    );
    let mut copies = 0;
    for (k, &p) in g.copy_of.iter().enumerate() {
        let c = g.base_nodes + k as u32;
        let list = r.copies(o.id[p as usize]).unwrap();
        let found = list.iter().find(|x| x.node == o.id[c as usize]).unwrap();
        assert_eq!(found.leaves.len(), g.csr.out(c).len());
        copies += 1;
    }
    assert_eq!(copies, sizes.copies);

    // Every route through the reader matches Dijkstra on the graph, and its segment
    // directions chain from one end to the other.
    let plain_new = |x: u32| o.id[plain(&g, o.order[x as usize]) as usize];
    let mut dist = vec![u32::MAX; n as usize];
    let mut routed = 0;
    for s in 0..n {
        for t in 0..n {
            for avoid in [0, TOLL, FERRY, AVOID] {
                let want = dijkstra(&g.csr, s, t, avoid, &mut dist);
                let got = r
                    .shortest(o.id[s as usize], o.id[t as usize], avoid)
                    .unwrap();
                assert_eq!(
                    got.as_ref().map(|p| p.weight),
                    want,
                    "{s} -> {t} avoiding {avoid}"
                );
                let Some(path) = got else { continue };
                routed += 1;
                let (mut at, mut sum) = (o.id[s as usize], 0);
                for &l in &path.legs {
                    let seg = r.segment(l >> 1).unwrap();
                    let d = seg.direction(l & 1 != 0).unwrap();
                    let (from, to) = match l & 1 {
                        0 => (seg.a, seg.b),
                        _ => (seg.b, seg.a),
                    };
                    assert_eq!(from, plain_new(at));
                    assert_eq!(d.flags & avoid, 0);
                    sum += d.weight();
                    at = to;
                }
                assert_eq!(plain_new(at), plain_new(o.id[t as usize]));
                assert_eq!(sum, path.weight);
            }
        }
    }
    assert!(routed > 4 * n as usize);
}
