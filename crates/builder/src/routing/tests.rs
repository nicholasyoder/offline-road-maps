use std::sync::Arc;

use omp_core::pack::{self, Info, Pack, Writer};
use omp_core::profile::Restriction;
use omp_core::route::{AVOID, FERRY, FERRY_CLASS, Names, Road, Routing, Stop, TOLL, cell_xy};

use super::*;
use crate::ch::check::dijkstra;
use crate::graph::Via;
use crate::graph::tests::{In, STREET, grid, pos, way_id};
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

/// The routing sections of `g` written to a pack and opened: the reader, the layout's
/// order and sizes, and the pack's directory.
fn reader(g: &Graph) -> (Routing, Order, Sizes, tempfile::TempDir) {
    reader_with(g, WayStreets(vec![(302, 0, 7), (302, 2, 8)]))
}

/// [`reader`] with the ways' streets given.
fn reader_with(g: &Graph, streets: WayStreets) -> (Routing, Order, Sizes, tempfile::TempDir) {
    let h = ch::contract(&g.csr, &ch::Params::default());
    let p = Params {
        block_nodes: 4,
        top_mib: 0.0001,
    };
    let (s, sizes, o, _) = sections(g, &h, &streets, &p).unwrap();
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
    (r, o, sizes, dir)
}

#[test]
fn sections_read_back_through_the_app_reader() {
    let g = network().build();
    assert!(!g.copy_of.is_empty(), "the restriction makes copies");
    let (r, o, sizes, _dir) = reader(&g);
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
        .find(|&v| g.coords[v as usize] == pos(13))
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

/// A point `t` of the way from node `a` to node `b`, moved `east_m` metres east.
fn between(a: i64, b: i64, t: f64, east_m: f64) -> omp_core::geo::P {
    let (p, q) = (pos(a), pos(b));
    let lerp = |x: u32, y: u32| (f64::from(x) + t * (f64::from(y) - f64::from(x))).round() as u32;
    let mut out = [lerp(p[0], q[0]), lerp(p[1], q[1])];
    out[0] += (east_m / omp_core::geo::metres_per_unit(out)) as u32;
    out
}

fn stop(a: i64, b: i64, t: f64) -> Stop {
    Stop::at(between(a, b, t, 0.0))
}

/// The segment under the snap of a point of the way from `a` to `b`.
fn segment_at(r: &Routing, a: i64, b: i64) -> u32 {
    r.snap(&stop(a, b, 0.5)).unwrap().unwrap().segment
}

#[test]
fn snaps_to_the_nearest_road_or_the_stops_own_street() {
    let g = network().build();
    let (r, ..) = reader(&g);
    let s = r
        .snap(&Stop::at(between(21, 22, 0.25, 10.0)))
        .unwrap()
        .unwrap();
    assert!((s.dist_m - 10.0).abs() < 0.5, "{}", s.dist_m);
    let seg = r.segment(s.segment).unwrap();
    assert!((s.along - 0.25).abs() < 0.01 || (s.along - 0.75).abs() < 0.01);
    assert_eq!(seg.points.len(), 2);
    assert!(!s.on_street);

    // Beside 32-33, but asking for Long Rd's street: its own street wins.
    let p = between(32, 33, 0.5, -20.0);
    let near = r.snap(&Stop::at(p)).unwrap().unwrap();
    assert_ne!(r.segment(near.segment).unwrap().street, Some(7));
    let own = r.snap(&Stop { p, street: Some(7) }).unwrap().unwrap();
    assert!(own.on_street && own.dist_m > near.dist_m);
    assert_eq!(r.segment(own.segment).unwrap().street, Some(7));

    // Nothing within reach, and never a ferry.
    let far = omp_core::geo::project(10.0, -30.0);
    assert_eq!(r.snap(&Stop::at(far)).unwrap(), None);
    let on_ferry = r.snap(&stop(41, 43, 0.5)).unwrap().unwrap();
    assert_ne!(r.segment(on_ferry.segment).unwrap().class, FERRY_CLASS);
}

#[test]
fn legs_start_and_end_partway_along_segments() {
    let g = network().build();
    let (r, ..) = reader(&g);
    // Along one two-way street, both ways, without the graph.
    for (from, to) in [(0.2, 0.8), (0.8, 0.2)] {
        let t = r.trip(&[stop(21, 22, from), stop(21, 22, to)], 0).unwrap();
        let l = t.legs[0].as_ref().unwrap();
        assert_eq!(l.pieces.len(), 1);
        assert!((l.metres - 60.0).abs() < 1.0, "{}", l.metres);
        assert_eq!(l.points.len(), 2);
    }
    // Across the grid: the line runs from snap to snap, and the pieces add up.
    let t = r.trip(&[stop(11, 12, 0.5), stop(32, 33, 0.5)], 0).unwrap();
    assert!(t.complete());
    let l = t.legs[0].as_ref().unwrap();
    assert_eq!(l.points[0], t.snaps[0].as_ref().unwrap().point);
    assert_eq!(
        *l.points.last().unwrap(),
        t.snaps[1].as_ref().unwrap().point
    );
    // Half a block, two blocks across, and half a block: 300 m, or more.
    assert!(l.metres >= 299.0 && l.metres < 500.0, "{}", l.metres);
    let sum: f64 = l.pieces.iter().map(|p| p.time_s).sum();
    assert!((sum - l.time_s).abs() < 1e-6);
    assert!((f64::from(l.weight) / 10.0 - l.time_s).abs() < 1.0);
    for w in l.pieces.windows(2) {
        assert_eq!(w[0].points.end - 1, w[1].points.start);
    }
    assert_eq!(l.pieces.last().unwrap().points.end, l.points.len());
}

#[test]
fn legs_honour_restrictions_one_ways_and_avoid_options() {
    let g = network().build();
    let (r, ..) = reader(&g);
    // No turn from 12-22 onto 22-23: the leg goes round instead.
    let banned = segment_at(&r, 22, 23);
    let t = r.trip(&[stop(12, 22, 0.5), stop(22, 23, 0.5)], 0).unwrap();
    let l = t.legs[0].as_ref().unwrap();
    assert!(l.pieces.len() > 2, "{:?}", l.pieces);
    assert_ne!(l.pieces[1].dir >> 1, banned);
    // Back to 12, on to 13 and 23: 300 m, against 100 m through the banned turn.
    assert!(l.metres > 250.0, "{}", l.metres);
    // The turn the other way is allowed: 50 m to 22, 50 m on.
    let t = r.trip(&[stop(12, 22, 0.5), stop(22, 32, 0.5)], 0).unwrap();
    assert!((t.legs[0].as_ref().unwrap().metres - 100.0).abs() < 1.0);

    // Back along the one-way 31-41: out of 41 only by the ferry, so avoiding ferries
    // breaks the option, and says so.
    let back = [stop(31, 41, 0.8), stop(31, 41, 0.2)];
    let t = r.trip(&back, FERRY).unwrap();
    let l = t.legs[0].as_ref().unwrap();
    assert_eq!((l.uses & FERRY, l.broken), (FERRY, FERRY));
    assert_eq!(t.broken(), FERRY);
    assert!(
        r.leg_between(
            t.snaps[0].as_ref().unwrap(),
            t.snaps[1].as_ref().unwrap(),
            FERRY
        )
        .unwrap()
        .is_none()
    );
    // Forward along it, nothing to break.
    let t = r
        .trip(&[stop(31, 41, 0.2), stop(31, 41, 0.8)], AVOID)
        .unwrap();
    assert_eq!(t.legs[0].as_ref().unwrap().broken, 0);

    // Three stops, two legs; a stop with no road leaves its legs out.
    let stops = [stop(11, 12, 0.5), stop(22, 32, 0.5), stop(13, 23, 0.5)];
    let t = r.trip(&stops, 0).unwrap();
    assert_eq!(t.legs.len(), 2);
    assert!(t.complete());
    let total: f64 = t.legs.iter().flatten().map(|l| l.metres).sum();
    assert_eq!(total, t.metres());
    let lost = [
        stops[0],
        Stop::at(omp_core::geo::project(10.0, -30.0)),
        stops[2],
    ];
    let t = r.trip(&lost, 0).unwrap();
    assert!(!t.complete() && t.legs.iter().all(Option::is_none));

    // Stopped by its caller, at once or after the first stop's snap, a trip gives up;
    // never stopped, it is the plain trip.
    assert_eq!(r.trip_until(&stops, 0, &|| true).unwrap(), None);
    let asked = std::cell::Cell::new(0);
    let later = || {
        asked.set(asked.get() + 1);
        asked.get() > 1
    };
    assert_eq!(r.trip_until(&stops, 0, &later).unwrap(), None);
    let whole = r.trip_until(&stops, 0, &|| false).unwrap();
    assert_eq!(whole, Some(r.trip(&stops, 0).unwrap()));
}

/// Street names for directions, without a street table.
struct TestNames(std::collections::HashMap<u32, Road>);

impl Names for TestNames {
    fn street_road(&self, id: u32) -> std::io::Result<Road> {
        Ok(self.0.get(&id).cloned().unwrap_or_default())
    }
}

#[test]
fn directions_name_turns_exits_roundabouts_and_ferries() {
    let mut i = network();
    // A roundabout of four one-way quarters, driven anticlockwise, with a road off each
    // corner.
    let ring = &[("highway", "residential"), ("junction", "roundabout")];
    for (id, a, b) in [(400, 72, 83), (401, 83, 74), (402, 74, 63), (403, 63, 72)] {
        i.way(id, ring, &[a, b]);
    }
    for (id, a, b) in [(410, 71, 72), (411, 83, 93), (412, 74, 75), (413, 63, 53)] {
        i.way(id, STREET, &[a, b]);
    }
    // A one-way motorway east with exit 12A off to the right, onto Main St.
    let motorway = &[("highway", "motorway"), ("ref", "I 70")];
    i.way(420, motorway, &[18, 28])
        .way(421, motorway, &[28, 38]);
    i.way(
        422,
        &[("highway", "motorway_link"), ("destination", "Columbus")],
        &[28, 37],
    );
    i.way(423, STREET, &[37, 47]);
    i.junctions.push((28, "12A".into()));
    let g = i.build();
    let (first, oak) = (way_id(&i, 11, 21), way_id(&i, 21, 22));
    let mut ways = vec![
        (first, 0, 1),
        (oak, 0, 2),
        (302, 0, 7),
        (412, 0, 3),
        (423, 0, 4),
        (420, 0, 5),
        (421, 0, 5),
    ];
    ways.sort();
    let (r, ..) = reader_with(&g, WayStreets(ways));
    let road = |name: &str, refs: &[&str]| Road {
        name: name.into(),
        refs: refs.iter().map(|x| x.to_string()).collect(),
    };
    let names = TestNames(
        [
            (1, road("First St", &[])),
            (2, road("Oak St", &[])),
            (3, road("North Rd", &[])),
            (4, road("Main St", &[])),
            (5, road("", &["I 70"])),
            (7, road("Long Rd", &[])),
        ]
        .into(),
    );
    let texts = |stops: &[Stop]| -> Vec<String> {
        let t = r.trip(stops, 0).unwrap();
        assert!(t.complete());
        let steps = r.directions(&t, stops, Some(&names)).unwrap();
        // The steps cover the trip, in order.
        let m: f64 = steps.iter().map(|s| s.metres).sum();
        assert!((m - t.metres()).abs() < 0.01);
        assert!(steps.windows(2).all(|w| w[0].at_m <= w[1].at_m));
        steps.iter().map(|s| s.text()).collect()
    };

    // East along First St, then north: a left turn.
    assert_eq!(
        texts(&[stop(11, 21, 0.5), stop(21, 22, 0.5)]),
        [
            "Head east on First St",
            "Turn left onto Oak St",
            "Arrive at your destination"
        ]
    );
    // From the south arm round to the north one: past the east arm, the 2nd exit.
    assert_eq!(
        texts(&[stop(71, 72, 0.5), stop(74, 75, 0.5)]),
        [
            "Head north",
            "At the roundabout, take the 2nd exit onto North Rd",
            "Arrive at your destination"
        ]
    );
    // Off the motorway at its exit.
    assert_eq!(
        texts(&[stop(18, 28, 0.5), stop(37, 47, 0.5)]),
        [
            "Head east on I-70",
            "Take exit 12A onto Main St toward Columbus",
            "Arrive at your destination"
        ]
    );
    // Along the one-way 31-41 and across the ferry to Long Rd; a stop off to the side.
    let t = texts(&[stop(31, 41, 0.5), Stop::at(between(43, 44, 0.5, 20.0))]);
    assert_eq!(t[1], "Take the ferry (Island Ferry)");
    assert_eq!(t[2], "Leave the ferry onto Long Rd");
    assert_eq!(
        t.last().unwrap(),
        "Arrive at your destination, on the right"
    );
}
