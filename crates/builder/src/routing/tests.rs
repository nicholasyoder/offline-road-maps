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

/// With a U-turn penalty, a leg between the middles of any two segments weighs what a
/// search over the graph's states finds: from the head of either direction of the first,
/// to any state with an edge along a direction of the second, with that edge's penalty
/// (the `RCPY` U-turn bit for copies, `RGEO`'s [`omp_core::route::UTURN`] for plain nodes).
#[test]
fn legs_pay_the_uturn_penalty_to_reach_a_stop() {
    // A one-way street into 22 that may not turn to 23: a U-turn at 21 or 32 is quicker
    // than going round the block.
    let mut i = network();
    i.way(
        400,
        &[("highway", "residential"), ("oneway", "yes")],
        &[52, 22],
    );
    let to = way_id(&i, 22, 23);
    i.restriction(Restriction::No, 400, Via::Node(22), to);
    i.uturn_s = 1;
    let g = i.build();
    assert!(g.stats.restrictions.uturn_edges > 0);
    let (r, o, ..) = reader(&g);
    assert_eq!(r.uturn_penalty_s(), 1);
    let (seg_order, seg_id, _) = segment_order(&g);
    let edges = segment_edges(&g);
    let snap = |old: usize| omp_core::route::Snap {
        segment: seg_id[old],
        along: 0.5,
        point: [0, 0],
        dist_m: 0.0,
        on_street: false,
        small: false,
    };
    let half = |t: u32| (f64::from(t) * 0.5).round() as u32;
    let n = g.csr.nodes() as u32;
    let mut dist = vec![u32::MAX; n as usize];
    let (mut legs, mut paid) = (0, 0);
    for (a, starts) in edges.iter().enumerate() {
        for b in 0..seg_order.len() {
            if a == b || g.way_info[g.segments[a].way as usize].class == FERRY_CLASS {
                continue;
            }
            let mut want: Option<u32> = None;
            for &ea in starts.iter().filter(|&&e| e != u32::MAX) {
                let ea = &g.csr.edges[ea as usize];
                for c in 0..n {
                    for eb in g.csr.out(c).iter().filter(|e| e.seg >> 1 == b as u32) {
                        let Some(d) = dijkstra(&g.csr, ea.to, c, 0, &mut dist) else {
                            continue;
                        };
                        let w = half(ea.time_ds) + d + half(eb.time_ds) + u32::from(eb.uturn_ds);
                        want = Some(want.map_or(w, |x| x.min(w)));
                    }
                }
            }
            let got = r.leg_between(&snap(a), &snap(b), 0).unwrap();
            assert_eq!(got.as_ref().map(|l| l.weight), want, "{a} -> {b}");
            let Some(leg) = got else { continue };
            legs += 1;
            // The time shown leaves the penalty out.
            paid += usize::from(f64::from(leg.weight) > leg.time_s * 10.0 + 1.0);
        }
    }
    assert!(legs > 100, "{legs}");
    // Some legs are quickest with a U-turn (round the banned turns at 22).
    assert!(paid > 0);

    // Copies list the way back as a U-turn, and so do the plain nodes' directions.
    let mut copy_uturns = 0;
    r.for_each_copy(|_, c| {
        assert!(c.uturns.iter().all(|d| c.leaves.contains(d)));
        copy_uturns += c.uturns.len();
    })
    .unwrap();
    assert!(copy_uturns > 0);
    let mut plain_uturns = 0;
    r.for_each_segment(|_, s| {
        for d in [Some(&s.forward), s.backward.as_ref()]
            .into_iter()
            .flatten()
        {
            plain_uturns += usize::from(d.flags & omp_core::route::UTURN != 0);
        }
    })
    .unwrap();
    assert!(plain_uturns > 0);
    let _ = o;
}

/// The matrix between stops holds each leg's weight, with every avoid option: across the
/// grid, along one segment both ways, round the banned turn, and out of the one-way
/// pocket only by ferry. The optimized order's legs add up to it.
#[test]
fn matrix_holds_the_legs_weights() {
    use omp_core::route::{Ends, order::NONE};
    let g = network().build();
    let (r, ..) = reader(&g);
    let stops = [
        stop(11, 12, 0.5),
        stop(32, 33, 0.5),
        stop(21, 22, 0.2),
        stop(21, 22, 0.8),
        stop(12, 22, 0.5),
        stop(22, 23, 0.5),
        stop(31, 41, 0.8),
        stop(31, 41, 0.2),
        stop(43, 44, 0.5),
        stop(13, 43, 0.3),
    ];
    let snaps: Vec<_> = stops.iter().map(|s| r.snap(s).unwrap().unwrap()).collect();
    for avoid in [0, TOLL, FERRY, AVOID] {
        let (m, settled) = r.matrix_until(&snaps, avoid, &|| false).unwrap().unwrap();
        assert!(settled > 0);
        for i in 0..stops.len() {
            for j in (0..stops.len()).filter(|&j| j != i) {
                let leg = r.leg_avoiding(&snaps[i], &snaps[j], avoid).unwrap();
                assert_eq!(m.get(i, j), leg.map_or(NONE, |l| l.weight), "{i} -> {j}");
            }
        }
    }
    assert_eq!(r.matrix_until(&snaps, 0, &|| true).unwrap(), None);

    for ends in [
        Ends::default(),
        Ends {
            keep_first: true,
            keep_last: true,
            round_trip: false,
        },
        Ends {
            round_trip: true,
            ..Ends::default()
        },
    ] {
        let o = r
            .optimize_until(&stops, FERRY, ends, &|| false)
            .unwrap()
            .unwrap();
        let mut sorted = o.order.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..stops.len()).collect::<Vec<_>>());
        assert!(o.after.unwrap() <= o.before.unwrap());
        let mut ordered: Vec<Stop> = o.order.iter().map(|&i| stops[i]).collect();
        if ends.round_trip {
            ordered.push(stops[o.order[0]]);
        }
        let trip = r.trip(&ordered, FERRY).unwrap();
        let sum: u64 = trip
            .legs
            .iter()
            .map(|l| u64::from(l.as_ref().unwrap().weight))
            .sum();
        assert_eq!(Some(sum), o.after, "{ends:?}");
    }
    // A stop with no road is an error.
    let lost = [stops[0], Stop::at(omp_core::geo::project(10.0, -30.0))];
    assert!(
        r.optimize_until(&lost, 0, Ends::default(), &|| false)
            .is_err()
    );
}

/// Two roads between the same ends: a street along the south (y = 1) and a faster
/// primary road along the north (y = 4), joined at both ends, with a street across the
/// middle that only makes detours, and dead ends off both.
fn ladder() -> Graph {
    let primary = &[("highway", "primary"), ("name", "North Rd")];
    let mut i = In::default();
    i.way(1, STREET, &[11, 21, 31, 41, 51, 61])
        .way(2, primary, &[14, 24, 34, 44, 54, 64])
        .way(3, STREET, &[11, 12, 13, 14])
        .way(4, STREET, &[61, 62, 63, 64])
        .way(5, STREET, &[31, 32, 33, 34]);
    for (k, (a, b)) in [(21, 22), (41, 42), (51, 52), (24, 25), (44, 45)]
        .into_iter()
        .enumerate()
    {
        i.way(10 + k as i64, STREET, &[a, b]);
    }
    i.build()
}

/// The ladder's alternative runs along the north road, and every route found is
/// admissible: within the stretch, sharing no more than allowed, without a loop.
#[test]
fn alternatives_take_other_roads_and_are_admissible() {
    use omp_core::route::AltParams;
    let g = ladder();
    let (r, ..) = reader(&g);
    let (a, b) = (stop(11, 12, 0.5), stop(61, 62, 0.5));
    let (sa, sb) = (r.snap(&a).unwrap().unwrap(), r.snap(&b).unwrap().unwrap());
    let quickest = r.leg_avoiding(&sa, &sb, 0).unwrap().unwrap();
    // 600 m of street: 86 s, against 1 km with the primary road's 500 m: 100 s.
    assert!((quickest.metres - 600.0).abs() < 2.0, "{}", quickest.metres);
    for stall in [false, true] {
        let p = AltParams {
            stall,
            ..AltParams::default()
        };
        let alts = r
            .alternatives_until(&sa, &sb, &quickest, 0, &p, &|| false)
            .unwrap()
            .unwrap();
        assert_eq!(alts.routes[0].leg, quickest);
        assert_eq!(alts.routes.len(), 2, "stall {stall}");
        let alt = &alts.routes[1].leg;
        assert!((alt.metres - 1000.0).abs() < 2.0, "{}", alt.metres);
        let north = omp_core::geo::dist(alt.points[alt.points.len() / 2], pos(34));
        assert!(north * omp_core::geo::metres_per_unit(pos(34)) < 260.0);
        assert!(alt.weight > quickest.weight);
        assert!(f64::from(alt.weight) <= 1.25 * f64::from(quickest.weight));
        assert!(alts.routes[1].shared <= 0.8);
        assert!(alts.routes[1].via.is_some());
        // The line runs from snap to snap.
        assert_eq!(alt.points[0], sa.point);
        assert_eq!(*alt.points.last().unwrap(), sb.point);
        let sum: f64 = alt.pieces.iter().map(|p| p.time_s).sum();
        assert!((sum - alt.time_s).abs() < 1e-6);
        // A straight route along the street needs no turns: it is the one picked.
        assert_eq!(alts.picked, 0);
        assert!(alts.routes[1].turns.turns >= 2);
    }
    // Stopped, and without a graph along one segment.
    let p = AltParams::default();
    let alts_ab = r
        .alternatives_until(&sa, &sb, &quickest, 0, &p, &|| false)
        .unwrap()
        .unwrap();
    assert_eq!(
        r.alternatives_until(&sa, &sb, &quickest, 0, &p, &|| true)
            .unwrap(),
        None
    );
    let (c, d) = (
        r.snap(&stop(21, 31, 0.2)).unwrap().unwrap(),
        r.snap(&stop(21, 31, 0.8)).unwrap().unwrap(),
    );
    let along = r.leg_avoiding(&c, &d, 0).unwrap().unwrap();
    let alts = r
        .alternatives_until(&c, &d, &along, 0, &p, &|| false)
        .unwrap()
        .unwrap();
    assert_eq!(alts.routes.len(), 1);

    // Directions with each piece's road (step 4.3): the same steps, and the roads `roads`
    // gives; the alternative's via road is from them as from `via_road`. Stopped: `None`.
    let trip = omp_core::route::Trip {
        snaps: vec![Some(sa.clone()), Some(sb.clone())],
        legs: vec![Some(alts_ab.routes[1].leg.clone())],
        avoid: 0,
    };
    let (steps, roads) = r
        .directions_roads_until(&trip, &[a, b], None, &|| false)
        .unwrap()
        .unwrap();
    assert_eq!(steps, r.directions(&trip, &[a, b], None).unwrap());
    let alt = &alts_ab.routes[1].leg;
    assert_eq!(roads, [r.roads(alt, None).unwrap()]);
    assert_eq!(
        omp_core::route::via_road_of(alt, &roads[0], &[&quickest]),
        r.via_road(alt, &[&quickest], None).unwrap()
    );
    assert!(alt.restricted().is_empty());
    assert_eq!(
        r.directions_until(&trip, &[a, b], None, &|| true).unwrap(),
        None
    );

    // Between every pair of points on the grid network, with every avoid option.
    let g = network().build();
    let (r, ..) = reader(&g);
    let stops = [
        stop(11, 12, 0.5),
        stop(32, 33, 0.5),
        stop(21, 22, 0.2),
        stop(12, 22, 0.5),
        stop(22, 23, 0.5),
        stop(31, 41, 0.8),
        stop(43, 44, 0.5),
        stop(13, 43, 0.3),
    ];
    let snaps: Vec<_> = stops.iter().map(|s| r.snap(s).unwrap().unwrap()).collect();
    let mut found = 0;
    for avoid in [0, TOLL, FERRY, AVOID] {
        for i in 0..snaps.len() {
            for j in (0..snaps.len()).filter(|&j| j != i) {
                let Some(q) = r.leg_avoiding(&snaps[i], &snaps[j], avoid).unwrap() else {
                    continue;
                };
                let alts = r
                    .alternatives_until(&snaps[i], &snaps[j], &q, avoid, &p, &|| false)
                    .unwrap()
                    .unwrap();
                found += alts.routes.len() - 1;
                let best = alts
                    .routes
                    .iter()
                    .map(|x| x.score_s())
                    .fold(f64::INFINITY, f64::min);
                assert_eq!(alts.picked().score_s(), best);
                for x in &alts.routes[1..] {
                    let l = &x.leg;
                    assert!(f64::from(l.weight) <= 1.25 * f64::from(q.weight));
                    assert!(x.shared <= 0.8);
                    assert_eq!(l.broken, 0);
                    assert_eq!(l.uses & avoid & !q.broken, 0, "{i} -> {j}");
                    let mut segs: Vec<u32> = l.pieces.iter().map(|p| p.dir >> 1).collect();
                    segs.sort_unstable();
                    let n = segs.len();
                    segs.dedup();
                    assert_eq!(segs.len(), n, "a loop, {i} -> {j}");
                }
            }
        }
    }
    assert!(found > 0);
}
