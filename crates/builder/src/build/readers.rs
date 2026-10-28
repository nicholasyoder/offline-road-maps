//! `inspect --readers`: reads a pack's search, street, address, admin and grid sections
//! through `omp-core`'s readers, checks that they agree with each other, and times warm
//! lookups (phase 2 step 2.5); then the routing sections ([`super::route_readers`]).

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use omp_core::addr::{Address, Loose, Range};
use omp_core::geo;
use omp_core::grid;
use omp_core::pack::Pack;
use omp_core::search::{Found, Kind, NearAddress, Search};

use crate::mib;

/// Every `n / want`-th index of `0..n`, about `want` of them.
pub(crate) fn sample(n: usize, want: usize) -> impl Iterator<Item = usize> {
    let step = (n / want.max(1)).max(1);
    // An odd offset, so the sample does not line up with block starts.
    (step / 3..n).step_by(step).take(want)
}

#[derive(Default)]
pub(crate) struct Times(Vec<Duration>);

impl Times {
    pub(crate) fn time<T>(&mut self, f: impl FnOnce() -> T) -> T {
        let t = Instant::now();
        let v = f();
        self.0.push(t.elapsed());
        v
    }

    pub(crate) fn line(&mut self, what: &str) {
        self.0.sort_unstable();
        let q = |p: f64| {
            self.0
                .get(((self.0.len() as f64 - 1.0) * p).round() as usize)
                .map_or(0.0, |d| d.as_secs_f64() * 1e3)
        };
        println!(
            "  {what:<28} {:>6} calls, p50 {:.3} ms, p95 {:.3} ms, max {:.3} ms",
            self.0.len(),
            q(0.5),
            q(0.95),
            q(1.0)
        );
    }
}

fn number(a: &NearAddress) -> (u32, &str) {
    match a {
        NearAddress::Street(a) => (a.number, &a.suffix),
        NearAddress::Loose(l) => (l.number, &l.suffix),
    }
}

/// Metres from `p` to the nearest edge of `rings`.
fn edge_m(rings: &[Vec<geo::P>], p: geo::P) -> f64 {
    let mut best = f64::INFINITY;
    for r in rings {
        for i in 0..r.len() {
            let g = omp_core::street::Geometry::new(vec![vec![r[i], r[(i + 1) % r.len()]]]);
            best = best.min(g.project(p).dist_m);
        }
    }
    best
}

pub fn run(first: &Path, want: usize) -> Result<()> {
    let pack = Arc::new(Pack::open(first).with_context(|| format!("open {}", first.display()))?);
    let t = Instant::now();
    let Some(s) = Search::open(pack.clone())? else {
        bail!("the pack has no search sections (format 0.{})", pack.minor);
    };
    println!(
        "\nreaders  opened in {:.1} ms, holding {:.2} MiB",
        t.elapsed().as_secs_f64() * 1e3,
        mib(s.heap_bytes() as u64)
    );

    // Whole sections, in order.
    let t = Instant::now();
    let nareas = s.admin.areas().len() as u32;
    let (mut streets, mut parts, mut points) = (0u64, 0u64, 0u64);
    let mut street_sample: Vec<(u32, geo::P)> = Vec::new();
    let every = (s.streets.count() as usize / want.max(1)).max(1);
    s.streets.for_each(|id, rec, g| {
        streets += 1;
        parts += g.parts.len() as u64;
        points += g.parts.iter().map(|p| p.len() as u64).sum::<u64>();
        assert!(
            rec.admin.is_none_or(|a| a < nareas),
            "street {id}: admin {:?}",
            rec.admin
        );
        assert!(
            (g.length_m() - rec.length_m as f64).abs() <= 1.0,
            "street {id}: {} m stored, {} m measured",
            rec.length_m,
            g.length_m()
        );
        if id as usize % every == every / 2 {
            street_sample.push((id, g.point_at(g.length_m() / 2.0)));
        }
    })?;
    println!(
        "  streets   {streets} ({parts} parts, {points} points), read in {:.1} s",
        t.elapsed().as_secs_f64()
    );
    let t = Instant::now();
    let (mut cells, mut pairs, mut loose_cells) = (0u64, 0u64, 0u64);
    s.grid.for_each(|_, c| {
        cells += 1;
        pairs += c.streets.len() as u64;
        loose_cells += u64::from(c.loose);
    })?;
    println!(
        "  grid      {cells} z{} cells ({loose_cells} with unattached addresses), {pairs} \
         cell-street pairs, read in {:.1} s",
        grid::ZOOM,
        t.elapsed().as_secs_f64()
    );
    let (mut addresses, mut loose, mut ranges) = (Vec::new(), Vec::new(), Vec::new());
    if let Some(a) = &s.addresses {
        let t = Instant::now();
        let [na, nl, nr, nc] = a.counts();
        // Every attached address is kept only in the sample, to bound memory.
        let (mut n, mut prev) = (0u64, (0u32, 0u32));
        let stride = (na as usize / want.max(1)).max(1) as u64;
        a.for_each(|x: &Address| {
            assert!(
                x.street < s.streets.count(),
                "address on street {}",
                x.street
            );
            assert!((x.street, x.number) >= prev, "addresses out of order");
            assert!(x.postcode.is_none_or(|c| c < nc), "bad postcode");
            prev = (x.street, x.number);
            if n % stride == stride / 3 {
                addresses.push(x.clone());
            }
            n += 1;
        })?;
        ensure!(n == u64::from(na), "ASTR holds {n} addresses, not {na}");
        let mut m = 0u64;
        let stride = (nl as usize / want.max(1)).max(1) as u64;
        a.for_each_loose(|l: &Loose| {
            if m % stride == stride / 3 {
                loose.push(l.clone());
            }
            m += 1;
        })?;
        ensure!(m == u64::from(nl), "ALSE holds {m} addresses, not {nl}");
        let mut k = 0u64;
        let stride = (nr as usize / want.max(1)).max(1) as u64;
        a.for_each_range(|r: &Range| {
            assert!(r.street < s.streets.count(), "range on street {}", r.street);
            if k % stride == stride / 3 {
                ranges.push(*r);
            }
            k += 1;
        })?;
        ensure!(k == u64::from(nr), "ARNG holds {k} ranges, not {nr}");
        println!(
            "  addresses {na} on streets, {nl} unattached, {nr} ranges, {nc} postcodes, read \
             in {:.1} s",
            t.elapsed().as_secs_f64()
        );
    }
    let ne = s.index.count();
    println!(
        "  search    {ne} entries, {} towns, {} tokens",
        s.index.towns().len(),
        s.index.tokens().len()
    );

    // Lookups: each sample is looked up twice, first with the readers' caches as they are
    // (the pack is in the OS page cache after the passes above), then again.
    let (mut f_entry, mut t_entry, mut t_post) =
        (Times::default(), Times::default(), Times::default());
    let mut checked_postings = 0;
    for id in sample(ne as usize, want).map(|i| i as u32) {
        let e = f_entry.time(|| s.index.entry(id))?;
        t_entry.time(|| s.index.entry(id))?;
        assert!(e.admin.is_none_or(|a| a < nareas), "entry {id}: admin");
        assert!(
            e.town.is_none_or(|t| (t as usize) < s.index.towns().len()),
            "entry {id}: town"
        );
        if e.kind == Kind::Street {
            let st = s.streets.street(e.reference as u32)?;
            ensure!(
                st.name == e.name || e.name.is_empty(),
                "entry {id}: street name"
            );
        }
        let tokens = match e.kind {
            Kind::Postcode => vec![omp_core::normalize::postcode_token(&e.name)],
            _ => omp_core::normalize::search_tokens(&e.name),
        };
        for tok in tokens {
            let p = t_post
                .time(|| s.index.postings(&tok))?
                .with_context(|| format!("entry {id}: token {tok:?} not indexed"))?;
            // Generic tokens list only some of their entries.
            if p.stored == p.total {
                ensure!(
                    p.ids.binary_search(&id).is_ok(),
                    "entry {id} not posted to {tok:?}"
                );
                checked_postings += 1;
            }
        }
    }
    f_entry.line("entry, first");
    t_entry.line("entry, again");
    t_post.line("postings of a token");
    println!("  ({checked_postings} postings lists hold their entry)");

    // The builder gave each street the deepest area holding its middle, from the full
    // polygons; the chain here comes from the stored ones, simplified to 50 m.
    let (mut t_chain, mut f_chain) = (Times::default(), Times::default());
    let (mut chained, mut differ, mut far) = (0, 0, 0);
    for &(id, p) in &street_sample {
        f_chain.time(|| s.admin.chain(p))?;
        let chain = t_chain.time(|| s.admin.chain(p))?;
        chained += usize::from(!chain.is_empty());
        let rec = s.streets.street(id)?;
        let Some(a) = rec.admin.filter(|a| !chain.contains(a)) else {
            continue;
        };
        differ += 1;
        let edge = edge_m(&s.admin.polygon(a)?, p);
        if edge > 60.0 {
            far += 1;
            let deepest = chain
                .last()
                .map(|&d| s.admin.areas()[d as usize].name.clone());
            println!(
                "  note: street {id} {:?} is in {:?} for the builder, {edge:.0} m from its \
                 edge; the chain ends at {deepest:?}",
                rec.name,
                s.admin.areas()[a as usize].name
            );
        }
    }
    f_chain.line("admin chain, first");
    t_chain.line("admin chain, again");
    println!(
        "  ({chained} of {} street middles are in an area; {differ} chains miss the street's \
         area, {far} of them more than 60 m inside it)",
        street_sample.len()
    );

    let (mut t_near_streets, mut f_near_streets) = (Times::default(), Times::default());
    for &(id, p) in &street_sample {
        f_near_streets.time(|| s.streets_near(p, 100.0))?;
        let near = t_near_streets.time(|| s.streets_near(p, 100.0))?;
        ensure!(
            near.iter().any(|(n, spot)| *n == id && spot.dist_m < 1.0),
            "street {id} is not near its own middle"
        );
    }
    f_near_streets.line("streets within 100 m, first");
    t_near_streets.line("streets within 100 m, again");

    let (mut t_locate, mut t_near) = (Times::default(), Times::default());
    let (mut f_locate, mut f_near) = (Times::default(), Times::default());
    let mut not_first = 0;
    for a in &addresses {
        let p = s.address_point(a)?;
        f_locate.time(|| s.locate(a.street, a.number, &a.suffix))?;
        let found = t_locate.time(|| s.locate(a.street, a.number, &a.suffix))?;
        let hit = found
            .iter()
            .any(|l| l.found == Found::Point(a.clone()) && l.p == p);
        ensure!(
            hit,
            "{} {:?} on street {} not located",
            a.number,
            a.suffix,
            a.street
        );
        f_near.time(|| s.addresses_near(p, 100.0))?;
        let near = t_near.time(|| s.addresses_near(p, 100.0))?;
        ensure!(
            near.iter()
                .any(|n| n.address == NearAddress::Street(a.clone())),
            "{} on street {} is not near its own point",
            a.number,
            a.street
        );
        // Another address can share the point (a unit, a suffix, a duplicate).
        if near
            .first()
            .is_some_and(|n| number(&n.address) != (a.number, a.suffix.as_str()))
        {
            not_first += 1;
        }
    }
    if !addresses.is_empty() {
        f_locate.line("locate a number, first");
        t_locate.line("locate a number, again");
        f_near.line("addresses within 100 m, first");
        t_near.line("addresses within 100 m, again");
        println!(
            "  ({} of {} sampled addresses have another address nearer or at their point)",
            not_first,
            addresses.len()
        );
    }
    for l in &loose {
        let (x, y) = grid::cell_xy(l.p);
        ensure!(
            s.grid.cell(x, y)?.loose,
            "the grid misses an unattached address"
        );
        let near = s.addresses_near(l.p, 5.0)?;
        ensure!(
            near.iter()
                .any(|n| n.address == NearAddress::Loose(l.clone())),
            "unattached {} {} is not near its point",
            l.number,
            l.street
        );
    }
    let mut interpolated = 0;
    for r in &ranges {
        let mid = (r.from + r.to) / 2;
        let n = if r.parity == 1 {
            mid | 1
        } else if r.parity == 2 {
            mid & !1
        } else {
            mid
        };
        if !r.holds(n) {
            continue;
        }
        let found = s.locate(r.street, n, "")?;
        // A point address with that number wins over the range.
        let ok = found.iter().any(|l| match &l.found {
            Found::Range(q) => q == r && l.approx,
            Found::Point(_) => true,
        });
        ensure!(ok, "{n} on street {} not found in its range", r.street);
        interpolated += usize::from(found.iter().any(|l| matches!(l.found, Found::Range(_))));
    }
    println!(
        "  checked {} streets, {} addresses, {} unattached, {} ranges ({interpolated} by \
         interpolation)",
        street_sample.len(),
        addresses.len(),
        loose.len(),
        ranges.len()
    );
    println!(
        "  readers hold {:.2} MiB after the lookups (search {:.2}, admin {:.2}, streets {:.2}, \
         addresses {:.2}, grid {:.2}); {:.1} MiB read into their caches",
        mib(s.heap_bytes() as u64),
        mib(s.index.heap_bytes() as u64),
        mib(s.admin.heap_bytes() as u64),
        mib(s.streets.heap_bytes() as u64),
        mib(s.addresses.as_ref().map_or(0, |a| a.heap_bytes()) as u64),
        mib(s.grid.heap_bytes() as u64),
        mib(s.bytes_read())
    );
    super::route_readers::run(first, &pack, s.streets.count(), want)
}
