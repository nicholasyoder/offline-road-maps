//! `route --check` (phase 3 criterion 2): random pairs of nodes routed through the reader
//! and by Dijkstra over the same reader's base edges, with each combination of avoid
//! flags; every path checked to chain, add up and avoid what it should, and, given the PBF
//! the pack was built from, against the node paths its turn restrictions forbid. Then
//! routes between random points, short and long, timed warm as the app would ask for
//! them, with the coast-to-coast routes of criterion 5 where the pack has them.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use anyhow::{Result, ensure};
use omp_core::geo::{self, P};
use omp_core::route::{FERRY_CLASS, Names, Routing, SHORTCUT, Stop, Trip};
use rayon::prelude::*;

use super::{RouteArgs, duration, flags_text};
use crate::build::readers::Times;
use crate::ch::check::{COMBINATIONS, Rng};
use crate::graph;
use crate::pbf::Pbf;
use crate::{mib, peak_rss_mib, routing};

/// The base edges of the hierarchy, out of each node.
struct Base {
    first: Vec<u64>,
    to: Vec<u32>,
    weight: Vec<u32>,
    flags: Vec<u8>,
}

impl Base {
    /// Reads every node's base edges: an up edge leaves its node, a down edge enters it.
    fn read(r: &Routing) -> Result<Base> {
        let n = r.header.nodes as usize;
        let mut first = vec![0u64; n + 1];
        for v in 0..n as u32 {
            let node = r.node(v)?;
            first[v as usize + 1] +=
                node.up().iter().filter(|e| e.flags & SHORTCUT == 0).count() as u64;
            for e in node.down().iter().filter(|e| e.flags & SHORTCUT == 0) {
                first[e.other as usize + 1] += 1;
            }
        }
        for i in 0..n {
            first[i + 1] += first[i];
        }
        let m = first[n] as usize;
        let (mut to, mut weight, mut flags) = (vec![0u32; m], vec![0u32; m], vec![0u8; m]);
        let mut at: Vec<u64> = first[..n].to_vec();
        let mut put = |from: u32, t: u32, w: u32, f: u8| {
            let i = at[from as usize] as usize;
            at[from as usize] += 1;
            (to[i], weight[i], flags[i]) = (t, w, f);
        };
        for v in 0..n as u32 {
            let node = r.node(v)?;
            for e in node.up().iter().filter(|e| e.flags & SHORTCUT == 0) {
                put(v, e.other, e.weight, e.flags);
            }
            for e in node.down().iter().filter(|e| e.flags & SHORTCUT == 0) {
                put(e.other, v, e.weight, e.flags);
            }
        }
        Ok(Base {
            first,
            to,
            weight,
            flags,
        })
    }

    /// Distances from `s` to each of `targets` on edges without `avoid`, stopping once all
    /// are settled; `dist` is all `u32::MAX` and is left so.
    fn dijkstra(&self, s: u32, targets: &[u32], avoid: u8, dist: &mut [u32]) -> Vec<Option<u32>> {
        let mut out = vec![None; targets.len()];
        let mut left = targets.len();
        let mut want: HashMap<u32, Vec<usize>> = HashMap::new();
        for (i, &t) in targets.iter().enumerate() {
            want.entry(t).or_default().push(i);
        }
        let mut heap = BinaryHeap::from([Reverse((0u32, s))]);
        let mut touched = vec![s];
        dist[s as usize] = 0;
        while let Some(Reverse((d, u))) = heap.pop() {
            if dist[u as usize] < d {
                continue;
            }
            if let Some(list) = want.remove(&u) {
                for i in list {
                    out[i] = Some(d);
                    left -= 1;
                }
                if left == 0 {
                    break;
                }
            }
            for i in self.first[u as usize] as usize..self.first[u as usize + 1] as usize {
                if self.flags[i] & avoid != 0 {
                    continue;
                }
                let (v, nd) = (self.to[i], d + self.weight[i]);
                if nd < dist[v as usize] {
                    if dist[v as usize] == u32::MAX {
                        touched.push(v);
                    }
                    dist[v as usize] = nd;
                    heap.push(Reverse((nd, v)));
                }
            }
        }
        for v in touched {
            dist[v as usize] = u32::MAX;
        }
        out
    }

    fn heap_bytes(&self) -> u64 {
        (self.first.len() * 8 + self.to.len() * 9) as u64
    }
}

/// The turn restrictions' forbidden node paths, and each pack segment's end nodes in the
/// graph's numbering, from the graph rebuilt from the PBF.
struct Restrictions {
    /// By their first two nodes.
    paths: HashMap<(u32, u32), Vec<Vec<u32>>>,
    /// Per pack segment, its `a` and `b` as base nodes of the graph.
    ends: Vec<[u32; 2]>,
}

impl Restrictions {
    fn load(pbf: &Path, r: &Routing) -> Result<Restrictions> {
        let t = Instant::now();
        let g = graph::make(&Pbf::open(pbf)?)?;
        ensure!(
            g.csr.nodes() == r.header.nodes as usize
                && g.segments.len() == r.header.segments as usize,
            "the pack was not built from this extract: {} nodes and {} segments, not {} and {}",
            g.csr.nodes(),
            g.segments.len(),
            r.header.nodes,
            r.header.segments
        );
        let (order, _, _) = routing::segment_order(&g);
        let edges = routing::segment_edges(&g);
        let ends: Vec<[u32; 2]> = order
            .iter()
            .map(|&old| {
                let e = edges[old as usize][0];
                [
                    routing::source(&g, e),
                    routing::plain(&g, g.csr.edges[e as usize].to),
                ]
            })
            .collect();
        // The pack's segments are the graph's, renumbered: their first points agree.
        for new in crate::build::readers::sample(order.len(), 1000) {
            let ours = g.points(order[new])[0];
            let theirs = r.segment(new as u32)?.points[0];
            ensure!(
                ours == theirs,
                "segment {new} is not the graph's segment {}: the pack was built from \
                 another extract",
                order[new]
            );
        }
        let mut paths: HashMap<(u32, u32), Vec<Vec<u32>>> = HashMap::new();
        for p in &g.forbidden {
            paths.entry((p[0], p[1])).or_default().push(p.clone());
        }
        eprintln!(
            "  restrictions: {} forbidden paths from the graph, rebuilt in {:.1} s",
            g.forbidden.len(),
            t.elapsed().as_secs_f64()
        );
        Ok(Restrictions { paths, ends })
    }

    /// The base nodes a path of segment directions passes.
    fn nodes(&self, legs: &[u32]) -> Vec<u32> {
        let mut out = Vec::with_capacity(legs.len() + 1);
        for &l in legs {
            let [a, b] = self.ends[(l >> 1) as usize];
            let (from, to) = match l & 1 {
                0 => (a, b),
                _ => (b, a),
            };
            if out.is_empty() {
                out.push(from);
            }
            out.push(to);
        }
        out
    }

    /// How many restricted approaches the path passes, and how many forbidden paths it
    /// drives.
    fn check(&self, legs: &[u32]) -> (u64, u64) {
        let seq = self.nodes(legs);
        let (mut approaches, mut broken) = (0, 0);
        for i in 0..seq.len().saturating_sub(1) {
            if let Some(list) = self.paths.get(&(seq[i], seq[i + 1])) {
                approaches += 1;
                broken += list.iter().filter(|p| seq[i..].starts_with(p)).count() as u64;
            }
        }
        (approaches, broken)
    }
}

#[derive(Default)]
struct Counts {
    routed: [AtomicU64; 4],
    unreachable: [AtomicU64; 4],
    mismatches: AtomicU64,
    bad: AtomicU64,
    approaches: AtomicU64,
    broken: AtomicU64,
    settled: AtomicU64,
    query_ns: AtomicU64,
}

pub fn run(
    first: &Path,
    r: &Routing,
    names: Option<&dyn Names>,
    n: usize,
    args: &RouteArgs,
) -> Result<()> {
    let start = Instant::now();
    let h = &r.header;
    println!(
        "route check  {}: {} nodes, {} segments",
        first.display(),
        h.nodes,
        h.segments
    );
    if n == 0 {
        return trips(r, names, args);
    }
    let restrictions = args
        .pbf
        .as_deref()
        .map(|p| Restrictions::load(p, r))
        .transpose()?;

    // Copies, and the plain node of each.
    let mut plain_of: HashMap<u32, u32> = HashMap::new();
    r.for_each_copy(|p, c| {
        plain_of.insert(c.node, p);
    })?;
    let plain = |v: u32| plain_of.get(&v).copied().unwrap_or(v);

    let t = Instant::now();
    let base = Base::read(r)?;
    println!(
        "  base edges  {} ({:.0} MiB), read in {:.1} s",
        base.to.len(),
        mib(base.heap_bytes()),
        t.elapsed().as_secs_f64()
    );

    // Random plain nodes outside small components: sources, each with its targets.
    let mut rng = Rng::new(args.seed);
    let mut pick = || loop {
        let v = rng.below(u64::from(h.nodes)) as u32;
        if !plain_of.contains_key(&v) && !r.node(v).map(|n| n.small()).unwrap_or(true) {
            break v;
        }
    };
    let per = args.targets.max(1);
    let groups: Vec<(u32, Vec<u32>)> = (0..n.div_ceil(per))
        .map(|g| {
            let s = pick();
            let k = per.min(n - g * per);
            (s, (0..k).map(|_| pick()).collect())
        })
        .collect();

    // A path must chain from s to t on segment directions that add up and avoid the flags.
    let valid = |s: u32, t: u32, p: &omp_core::route::Path, avoid: u8| -> Result<bool> {
        let (mut at, mut sum) = (s, 0);
        for &l in &p.legs {
            let seg = r.segment(l >> 1)?;
            let rev = l & 1 != 0;
            let Some(d) = seg.direction(rev) else {
                return Ok(false);
            };
            let (from, to) = match rev {
                false => (seg.a, seg.b),
                true => (seg.b, seg.a),
            };
            if from != plain(at) || d.flags & avoid != 0 {
                return Ok(false);
            }
            sum += d.weight();
            at = to;
        }
        Ok(plain(at) == plain(t) && sum == p.weight)
    };

    let t = Instant::now();
    let c = Counts::default();
    let done = AtomicU64::new(0);
    groups.par_iter().try_for_each_init(
        || vec![u32::MAX; h.nodes as usize],
        |dist, (s, targets)| -> Result<()> {
            for (k, &avoid) in COMBINATIONS.iter().enumerate() {
                let want = base.dijkstra(*s, targets, avoid, dist);
                for (&t, w) in targets.iter().zip(want) {
                    let q = Instant::now();
                    let p = r.shortest(*s, t, avoid)?;
                    c.query_ns
                        .fetch_add(q.elapsed().as_nanos() as u64, Ordering::Relaxed);
                    if p.as_ref().map(|p| p.weight) != w {
                        c.mismatches.fetch_add(1, Ordering::Relaxed);
                        eprintln!(
                            "  {s} -> {t} avoiding {}: hierarchy {:?}, Dijkstra {w:?}",
                            flags_text(avoid),
                            p.as_ref().map(|p| p.weight)
                        );
                    }
                    let Some(p) = p else {
                        c.unreachable[k].fetch_add(1, Ordering::Relaxed);
                        continue;
                    };
                    c.routed[k].fetch_add(1, Ordering::Relaxed);
                    c.settled.fetch_add(p.settled as u64, Ordering::Relaxed);
                    if !valid(*s, t, &p, avoid)? {
                        c.bad.fetch_add(1, Ordering::Relaxed);
                        eprintln!("  {s} -> {t} avoiding {}: bad path", flags_text(avoid));
                    }
                    if let Some(x) = &restrictions {
                        let (a, b) = x.check(&p.legs);
                        c.approaches.fetch_add(a, Ordering::Relaxed);
                        c.broken.fetch_add(b, Ordering::Relaxed);
                    }
                }
            }
            let k = done.fetch_add(1, Ordering::Relaxed) + 1;
            if k.is_multiple_of(100) {
                eprintln!(
                    "  {k} of {} sources, {:.0} s",
                    groups.len(),
                    t.elapsed().as_secs_f64()
                );
            }
            Ok(())
        },
    )?;
    let load = |a: &AtomicU64| a.load(Ordering::Relaxed);
    let routed: u64 = c.routed.iter().map(load).sum();
    let queries = (routed + c.unreachable.iter().map(load).sum::<u64>()).max(1);
    println!(
        "  pairs       {n} ({} starts × {per}) × 4 avoid combinations in {:.1} s",
        groups.len(),
        t.elapsed().as_secs_f64()
    );
    for (k, &avoid) in COMBINATIONS.iter().enumerate() {
        println!(
            "    avoiding {:<16} {} routed, {} unreachable",
            flags_text(avoid),
            load(&c.routed[k]),
            load(&c.unreachable[k])
        );
    }
    println!(
        "  exact       {} differ from Dijkstra, {} bad paths; {:.0} nodes settled per route, \
         {:.3} ms per query (16 threads at once)",
        load(&c.mismatches),
        load(&c.bad),
        load(&c.settled) as f64 / routed.max(1) as f64,
        load(&c.query_ns) as f64 / queries as f64 / 1e6
    );
    match &restrictions {
        Some(_) => println!(
            "  restrictions {} restricted approaches driven, {} forbidden paths driven",
            load(&c.approaches),
            load(&c.broken)
        ),
        None => println!("  restrictions not checked (give --pbf)"),
    }
    drop(base);
    ensure!(
        load(&c.mismatches) == 0 && load(&c.bad) == 0 && load(&c.broken) == 0,
        "the check failed"
    );

    trips(r, names, args)?;
    println!(
        "  readers hold {:.1} MiB; {:.1} MiB read; check {:.1} s, peak RSS {:.0} MiB",
        mib(r.heap_bytes() as u64),
        mib({
            let x = r.reads();
            x.nodes + x.segments + x.other
        }),
        start.elapsed().as_secs_f64(),
        peak_rss_mib()
    );
    Ok(())
}

/// A random point on a random segment outside small components and ferries.
pub(crate) fn random_point(r: &Routing, rng: &mut Rng) -> Result<P> {
    loop {
        let s = r.segment(rng.below(u64::from(r.header.segments)) as u32)?;
        if !s.small && s.class != FERRY_CLASS {
            return Ok(s.points[s.points.len() / 2]);
        }
    }
}

/// `p` moved `m` metres on bearing `rad` (east of north), on the map's plane.
pub(crate) fn offset(p: P, m: f64, rad: f64) -> P {
    let u = m / geo::metres_per_unit(p);
    let x = f64::from(p[0]) + u * rad.sin();
    let y = f64::from(p[1]) - u * rad.cos();
    [x.clamp(0.0, 4.29e9) as u32, y.clamp(0.0, 4.29e9) as u32]
}

/// Routes between points timed warm, with their directions (criterion 5): each asked
/// twice, the second with the caches as the first left them.
fn trips(r: &Routing, names: Option<&dyn Names>, args: &RouteArgs) -> Result<()> {
    let route = |stops: &[Stop]| -> std::io::Result<Trip> {
        let trip = r.trip(stops, 0)?;
        r.directions(&trip, stops, names)?;
        Ok(trip)
    };
    let mut rng = Rng::new(args.seed + 1);
    let n = args.trips;
    // Short: to a road 1 to 100 km away in a random direction.
    let mut short: Vec<(P, P)> = Vec::with_capacity(n);
    while short.len() < n {
        let p = random_point(r, &mut rng)?;
        let m = 1_000.0 + rng.below(99_000) as f64;
        let rad = rng.below(360) as f64 * std::f64::consts::PI / 180.0;
        let near = r.segments_near(offset(p, m, rad), 1_000.0)?;
        if near.is_empty() {
            continue;
        }
        let s = r.segment(near[rng.below(near.len() as u64) as usize])?;
        if !s.small && s.class != FERRY_CLASS {
            short.push((p, s.points[s.points.len() / 2]));
        }
    }
    let long: Vec<(P, P)> = (0..n)
        .map(|_| Ok((random_point(r, &mut rng)?, random_point(r, &mut rng)?)))
        .collect::<Result<_>>()?;
    for (name, pairs) in [("short", &short), ("long", &long)] {
        let (mut once, mut again) = (Times::default(), Times::default());
        let (mut routed, mut km, mut far, mut settled) = (0, 0.0, 0, 0);
        for &(a, b) in pairs.iter() {
            let stops = [Stop::at(a), Stop::at(b)];
            once.time(|| route(&stops))?;
            let trip = again.time(|| route(&stops))?;
            far += trip
                .snaps
                .iter()
                .flatten()
                .filter(|s| s.dist_m > omp_core::route::FAR_SNAP_M)
                .count();
            if let Some(Some(l)) = trip.legs.first() {
                routed += 1;
                km += l.metres / 1000.0;
                settled += l.settled;
            }
        }
        once.line(&format!("{name} trip, first"));
        again.line(&format!("{name} trip, again"));
        println!(
            "  ({routed} of {} routed, {:.0} km and {:.0} nodes settled on average; {far} \
             snaps over 500 m)",
            pairs.len(),
            km / routed.max(1) as f64,
            settled as f64 / routed.max(1) as f64
        );
    }
    // Criterion 5's coast-to-coast routes, where both ends are in the pack.
    let named = [
        (
            "Miami to Seattle",
            (25.7743, -80.1937),
            (47.6062, -122.3321),
        ),
        (
            "Halifax to Los Angeles",
            (44.6488, -63.5752),
            (34.0522, -118.2437),
        ),
        (
            "Toronto to Vancouver",
            (43.6532, -79.3832),
            (49.2827, -123.1207),
        ),
        (
            "Cincinnati to Cleveland",
            (39.1031, -84.5120),
            (41.4993, -81.6944),
        ),
        (
            "Montreal to Gaspe",
            (45.5017, -73.5673),
            (48.8316, -64.4869),
        ),
    ];
    for (name, a, b) in named {
        let stops = [
            Stop::at(geo::project(a.0, a.1)),
            Stop::at(geo::project(b.0, b.1)),
        ];
        let mut times = Times::default();
        let mut trip = r.trip(&stops, 0)?;
        if !trip.complete() || trip.snaps.iter().flatten().any(|s| s.dist_m > 5_000.0) {
            continue;
        }
        for _ in 0..5 {
            trip = times.time(|| route(&stops))?;
        }
        let l = trip.legs[0].as_ref().unwrap();
        times.line(&format!("{name}, again"));
        println!(
            "  ({:.0} km, {}, {} pieces, {} nodes settled)",
            l.metres / 1000.0,
            duration(l.time_s),
            l.pieces.len(),
            l.settled
        );
    }
    Ok(())
}
