//! `inspect --readers`, the routing sections (phase 3 step 3.3): reads each through
//! `omp_core::route`, checks them against each other and the street table, compares routes
//! with Dijkstra over the base edges where the graph is small enough to hold, and times
//! queries warm and cold (each chunk file's pages dropped from the system's cache first).

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Result, ensure};
use omp_core::geo;
use omp_core::pack::Pack;
use omp_core::route::{AVOID, FERRY, Routing, SHORTCUT, Segment, TOLL, cell_key, cell_xy};

use super::readers::{Times, sample};
use crate::ch::check::Rng;
use crate::mib;

/// Above this many nodes, the base graph is not held for Dijkstra and only a sample of the
/// shortcuts is unpacked.
const FULL_CHECK_NODES: u32 = 10_000_000;

/// What a segment's direction must match: its start and end (plain), weight and flags.
#[derive(Clone, Copy)]
struct Dir {
    from: u32,
    to: u32,
    weight: u32,
    flags: u8,
}

fn dirs(s: &Segment) -> [Option<Dir>; 2] {
    let d = |from, to, x: Option<&omp_core::route::Direction>| {
        x.map(|x| Dir {
            from,
            to,
            weight: x.weight(),
            flags: x.flags,
        })
    };
    [
        d(s.a, s.b, Some(&s.forward)),
        d(s.b, s.a, s.backward.as_ref()),
    ]
}

fn length_m(pts: &[geo::P]) -> f64 {
    pts.windows(2)
        .map(|w| geo::dist(w[0], w[1]) * geo::metres_per_unit(w[0]))
        .sum()
}

/// Drops the pack's chunk files from the system's file cache, so the next reads are cold.
#[cfg(unix)]
#[allow(unsafe_code)]
fn evict(first: &Path, chunks: usize) -> Result<()> {
    use std::os::fd::AsRawFd;
    let dir = first.parent().unwrap_or(Path::new("."));
    let name = first
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_suffix(".omp.000"))
        .unwrap_or_default();
    for i in 0..chunks {
        let f = std::fs::File::open(omp_core::pack::chunk_path(dir, name, i as u32))?;
        // SAFETY: a valid descriptor; the call only advises the kernel.
        let r = unsafe { libc::posix_fadvise(f.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED) };
        ensure!(r == 0, "posix_fadvise failed: {r}");
    }
    Ok(())
}

#[cfg(not(unix))]
fn evict(_: &Path, _: usize) -> Result<()> {
    anyhow::bail!("cold reads are measured on Unix only")
}

pub fn run(first: &Path, pack: &Arc<Pack>, streets: u32, want: usize) -> Result<()> {
    let t = Instant::now();
    let Some(r) = Routing::open(pack.clone())? else {
        println!("\nrouting  none in this pack");
        return Ok(());
    };
    let h = r.header.clone();
    println!(
        "\nrouting  opened in {:.1} ms, holding {:.2} MiB: {} nodes ({} at the top, {:.1} MiB), \
         {} edges, {} segments, {} labels, {} nodes per block",
        t.elapsed().as_secs_f64() * 1e3,
        mib(r.heap_bytes() as u64),
        h.nodes,
        h.top_nodes,
        mib(h.top_bytes),
        h.edges,
        h.segments,
        h.labels,
        h.block_nodes
    );

    // Copies, and the plain node of each.
    let t = Instant::now();
    let mut plain_of: HashMap<u32, u32> = HashMap::new();
    let mut copy_leaves = Vec::new();
    r.for_each_copy(|p, c| {
        assert!(c.node != p, "node {p} is its own copy");
        assert!(
            plain_of.insert(c.node, p).is_none(),
            "copy {} twice",
            c.node
        );
        copy_leaves.push((p, c.leaves.clone()));
    })?;
    let plain = |v: u32| plain_of.get(&v).copied().unwrap_or(v);

    // Segments.
    let mut segs: Vec<[Option<Dir>; 2]> = Vec::with_capacity(h.segments as usize);
    let mut homes: Vec<u32> = Vec::with_capacity(h.segments as usize);
    let (mut named, mut labelled, mut points, mut off) = (0u64, 0u64, 0u64, 0u64);
    let mut by_class = [0u64; 20];
    r.for_each_segment(|i, s| {
        assert!(s.a != s.b, "segment {i} is a loop");
        assert!(s.street.is_none_or(|x| x < streets), "segment {i}: street");
        assert!(s.label.is_none_or(|l| l < h.labels), "segment {i}: label");
        assert!(
            s.class <= omp_core::route::FERRY_CLASS,
            "segment {i}: class"
        );
        for (d, end) in [(Some(&s.forward), s.b), (s.backward.as_ref(), s.a)] {
            if let Some(d) = d {
                assert!(
                    plain(d.head) == end,
                    "segment {i}: head {} of {end}",
                    d.head
                );
            }
        }
        // The stored length is the full way's, the points simplified to 2 m.
        let geo_m = length_m(&s.points);
        if (geo_m - s.metres).abs() > 10.0_f64.max(0.1 * s.metres) {
            off += 1;
        }
        named += u64::from(s.street.is_some());
        labelled += u64::from(s.label.is_some());
        points += s.points.len() as u64;
        by_class[s.class as usize] += 1;
        let (x, y) = cell_xy(s.points[0]);
        homes.push(cell_key(x, y));
        segs.push(dirs(s));
    })?;
    ensure!(
        segs.len() == h.segments as usize,
        "RGEO holds {} segments",
        segs.len()
    );
    for (p, leaves) in &copy_leaves {
        for &l in leaves {
            let d = segs[(l >> 1) as usize][(l & 1) as usize];
            ensure!(
                d.is_some_and(|d| d.from == *p),
                "a copy of {p} leaves by segment direction {l}, which does not start there"
            );
        }
    }
    println!(
        "  segments  {} ({points} points, {named} with a street, {labelled} with a label, \
         {off} whose length is off its points'), {} copies; read in {:.1} s",
        segs.len(),
        plain_of.len(),
        t.elapsed().as_secs_f64()
    );
    let classes: Vec<String> = by_class
        .iter()
        .enumerate()
        .filter(|(_, n)| **n > 0)
        .map(|(c, n)| {
            let name = omp_core::street::CLASSES.get(c).copied().unwrap_or("ferry");
            format!("{name} {n}")
        })
        .collect();
    println!("  classes   {}", classes.join(", "));

    // The grid: every segment is home in its first point's cell, once.
    let t = Instant::now();
    let (mut cells, mut cross, mut next) = (0u64, 0u64, 0u32);
    r.for_each_cell(|k, c| {
        cells += 1;
        cross += c.cross.len() as u64;
        assert!(c.home.start == next, "RGRD home runs are not consecutive");
        next = c.home.end;
        for s in c.home.clone() {
            assert!(
                homes[s as usize] == k,
                "segment {s} is not home in its cell"
            );
        }
        for &s in &c.cross {
            assert!(
                s < h.segments && homes[s as usize] != k,
                "cell crossing {s}"
            );
        }
    })?;
    ensure!(
        next == h.segments,
        "RGRD homes {next} of {} segments",
        h.segments
    );
    drop(homes);
    println!(
        "  grid      {cells} cells, {cross} crossings; read in {:.1} s",
        t.elapsed().as_secs_f64()
    );

    // The hierarchy: every base edge matches its segment's direction.
    let t = Instant::now();
    let full = h.nodes <= FULL_CHECK_NODES;
    let (mut base, mut shortcuts, mut small) = (0u64, Vec::new(), 0u64);
    let mut adj: Vec<Vec<(u32, u32, u8)>> = match full {
        true => vec![Vec::new(); h.nodes as usize],
        false => Vec::new(),
    };
    let mut check_base = |from: u32, to: u32, e: &omp_core::route::ChEdge| {
        let m = e.mid;
        let d = segs
            .get((m >> 1) as usize)
            .and_then(|d| d[(m & 1) as usize])
            .ok_or_else(|| anyhow::anyhow!("edge {from} -> {to}: no segment direction {m}"))?;
        ensure!(
            plain(from) == d.from
                && plain(to) == d.to
                && e.weight == d.weight
                && e.flags & AVOID == d.flags & AVOID,
            "edge {from} -> {to} does not match segment direction {m}"
        );
        if full {
            adj[from as usize].push((to, e.weight, e.flags));
        }
        Ok(())
    };
    for v in 0..h.nodes {
        let n = r.node(v)?;
        small += u64::from(n.small());
        let up = n.up().iter().map(|e| (v, e.other, e));
        for (from, to, e) in up.chain(n.down().iter().map(|e| (e.other, v, e))) {
            match e.flags & SHORTCUT {
                0 => check_base(from, to, e)?,
                _ => {
                    ensure!(e.mid < h.nodes, "shortcut {from} -> {to}: middle {}", e.mid);
                    shortcuts.push((from, to, *e));
                }
            }
        }
        base += (n.up().len() + n.down().len()) as u64;
        if !full && shortcuts.len() > 4 * want {
            shortcuts.truncate(4 * want);
        }
    }
    let nshort = shortcuts.len();
    let (mut legs, mut unpacked) = (Vec::new(), 0usize);
    for &(a, b, e) in shortcuts.iter().step_by(if full {
        1
    } else {
        (nshort / want.max(1)).max(1)
    }) {
        legs.clear();
        r.unpack(a, b, &e, &mut legs)?;
        let sum: u32 = legs
            .iter()
            .map(|&l| segs[(l >> 1) as usize][(l & 1) as usize].unwrap().weight)
            .sum();
        ensure!(
            sum == e.weight,
            "shortcut {a} -> {b} unpacks to weight {sum}"
        );
        unpacked += 1;
    }
    println!(
        "  hierarchy {base} edges ({} shortcuts seen, {unpacked} unpacked), {small} nodes in \
         small components; read in {:.1} s",
        nshort,
        t.elapsed().as_secs_f64()
    );
    drop(shortcuts);

    let with_exit = sample(h.nodes as usize, want)
        .map(|v| r.junction(v as u32))
        .collect::<std::io::Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .count();
    println!(
        "  exits     {} nodes with an exit number ({with_exit} in a sample of {want})",
        r.junctions()
    );

    // Queries: random plain nodes outside small components, long and short.
    let mut rng = Rng::new(7);
    let mut pick = |lo: u32| loop {
        let v = lo + rng.below(u64::from(h.nodes - lo)) as u32;
        if !plain_of.contains_key(&v) && !r.node(v).map(|n| n.small()).unwrap_or(true) {
            break v;
        }
    };
    let n_pairs = want.min(1000);
    let long: Vec<(u32, u32)> = (0..n_pairs).map(|_| (pick(0), pick(0))).collect();
    // Short: close in the Hilbert order that follows the top (in a pack that is mostly top,
    // anywhere).
    let rest = match h.top_nodes < h.nodes / 2 {
        true => h.top_nodes,
        false => 0,
    };
    let short: Vec<(u32, u32)> = (0..n_pairs)
        .map(|_| {
            let s = pick(rest);
            let lo = s.saturating_sub(20_000).max(rest);
            let mut t = pick(lo);
            while t.abs_diff(s) > 20_000 {
                t = pick(lo);
            }
            (s, t)
        })
        .collect();

    // A path must chain from s to t on segment directions that add up and avoid the flags.
    let valid = |s: u32, t: u32, p: &omp_core::route::Path, avoid: u8| {
        let mut at = s;
        let mut sum = 0;
        for &l in &p.legs {
            let Some(d) = segs[(l >> 1) as usize][(l & 1) as usize] else {
                return false;
            };
            if d.from != plain(at) || d.flags & avoid != 0 {
                return false;
            }
            sum += d.weight;
            at = d.to;
        }
        plain(at) == plain(t) && sum == p.weight
    };
    if full {
        let t0 = Instant::now();
        let (mut mismatches, mut bad, mut routed) = (0, 0, 0);
        let mut dist = vec![u32::MAX; h.nodes as usize];
        for &(s, t) in long.iter().take(100).chain(short.iter().take(100)) {
            for avoid in [0, TOLL, FERRY, AVOID] {
                let p = r.shortest(s, t, avoid)?;
                let d = dijkstra(&adj, s, t, avoid, &mut dist);
                if p.as_ref().map(|p| p.weight) != d {
                    mismatches += 1;
                }
                if let Some(p) = &p {
                    routed += 1;
                    bad += usize::from(!valid(s, t, p, avoid));
                }
            }
        }
        println!(
            "  routes    {routed} found, {mismatches} differ from Dijkstra, {bad} bad paths; \
             {:.1} s",
            t0.elapsed().as_secs_f64()
        );
        ensure!(mismatches == 0 && bad == 0, "routes differ from Dijkstra");
    }
    drop(adj);

    // Warm: each query twice, the second with the caches as the first left them.
    for (name, pairs) in [("long", &long), ("short", &short)] {
        let (mut once, mut again) = (Times::default(), Times::default());
        let mut settled = 0;
        for &(s, t) in pairs.iter() {
            let p = once.time(|| r.shortest(s, t, 0))?;
            again.time(|| r.shortest(s, t, 0))?;
            if let Some(p) = &p {
                settled += p.settled;
                ensure!(valid(s, t, p, 0), "bad path {s} -> {t}");
            }
        }
        once.line(&format!("{name} route, first"));
        again.line(&format!("{name} route, again"));
        println!(
            "  ({:.0} nodes settled per {name} route)",
            settled as f64 / pairs.len().max(1) as f64
        );
    }

    // Cold: a fresh reader and no file cache, without and with the top read first.
    let cold = want.min(20);
    for (name, pairs) in [("long", &long), ("short", &short)] {
        for warm_top in [false, true] {
            let (mut times, mut bytes, mut top) = (Times::default(), 0u64, 0u64);
            for &(s, t) in pairs.iter().take(cold) {
                if evict(first, pack.chunks()).is_err() {
                    return Ok(());
                }
                let fresh = Arc::new(Pack::open(first)?);
                let r = Routing::open(fresh)?.unwrap();
                times.time(|| -> Result<()> {
                    if warm_top {
                        top += r.warm_top()?;
                    }
                    r.shortest(s, t, 0)?;
                    Ok(())
                })?;
                bytes += r.reads().nodes;
            }
            times.line(&format!(
                "{name} route, cold{}",
                if warm_top { ", top first" } else { "" }
            ));
            println!(
                "  ({:.2} MiB of RCHE blocks read per route, and {:.1} MiB for the top first)",
                mib(bytes) / cold as f64,
                mib(top) / cold as f64
            );
        }
    }
    println!(
        "  routing readers hold {:.2} MiB after the queries",
        mib(r.heap_bytes() as u64)
    );
    Ok(())
}

/// Dijkstra over `adj` from `s` to `t` on edges without `avoid`; `dist` is all `u32::MAX`
/// and is left so.
fn dijkstra(
    adj: &[Vec<(u32, u32, u8)>],
    s: u32,
    t: u32,
    avoid: u8,
    dist: &mut [u32],
) -> Option<u32> {
    use std::cmp::Reverse;
    let mut heap = std::collections::BinaryHeap::from([Reverse((0u32, s))]);
    let mut touched = vec![s];
    dist[s as usize] = 0;
    let mut found = None;
    while let Some(Reverse((d, u))) = heap.pop() {
        if u == t {
            found = Some(d);
            break;
        }
        if dist[u as usize] < d {
            continue;
        }
        for &(v, w, f) in &adj[u as usize] {
            if f & avoid != 0 {
                continue;
            }
            let nd = d + w;
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
    found
}
