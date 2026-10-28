//! `route --addresses N`: how far random address points snap from the road a route would
//! start or end on (the snapping half of phase 3 criterion 1). Each address is snapped as
//! the app snaps a search result, with its own street first, and again without it, to the
//! nearest road; the worst are listed, so a cause such as a road the profile leaves out
//! can be looked up. Then the points in the 48 contiguous states and southern Canada are
//! paired in the order sampled and routed (the routing half of criterion 1).

use std::sync::Arc;

use anyhow::{Context, Result};
use omp_core::geo;
use omp_core::pack::Pack;
use omp_core::route::{RESTRICTED, Routing, Snap, Stop};
use omp_core::search::Search;

use super::set::quantile;
use crate::ch::check::Rng;

/// Snap distances counted, in metres.
const BANDS_M: [f64; 4] = [50.0, 200.0, 500.0, 1000.0];
/// Worst snaps listed.
const WORST: usize = 15;
/// Criterion 1's area, the 48 contiguous states and southern Canada, as a box in degrees:
/// west, south, east, north. It leaves out Alaska, Hawaii, Puerto Rico and the north.
const PAIR_BOX: [f64; 4] = [-125.0, 24.0, -52.0, 55.0];
/// Criterion 1: an end snapped farther than this, and not to its own street, misses.
const PAIR_SNAP_M: f64 = 50.0;

pub fn run(pack: &Arc<Pack>, r: &Routing, n: usize, seed: u64) -> Result<()> {
    let search = Search::open(pack.clone())?.context("the pack has no search sections")?;
    let addresses = search
        .addresses
        .as_ref()
        .context("the pack has no addresses")?;
    // One pass, keeping a uniform sample of `n` (reservoir sampling).
    let mut rng = Rng::new(seed);
    let mut sample = Vec::with_capacity(n);
    let mut seen = 0u64;
    addresses.for_each(|a| {
        seen += 1;
        if sample.len() < n {
            sample.push(a.clone());
        } else {
            let k = rng.below(seen) as usize;
            if k < n {
                sample[k] = a.clone();
            }
        }
    })?;
    println!("{seen} addresses, {} sampled (seed {seed})", sample.len());

    let restricted = |s: &Snap| -> Result<bool> {
        let seg = r.segment(s.segment)?;
        let flags = seg.forward.flags | seg.backward.map_or(0, |d| d.flags);
        Ok(flags & RESTRICTED != 0)
    };
    let (mut dists, mut nearest) = (Vec::new(), Vec::new());
    let (mut none, mut on_street, mut on_restricted, mut small) = (0, 0, 0, 0);
    let mut worst = Vec::new();
    // The stops to pair, in criterion 1's box.
    let mut ends = Vec::new();
    for a in &sample {
        let p = search.address_point(a)?;
        let stop = Stop {
            p,
            street: Some(a.street),
        };
        let [lon, lat] = geo::unit_to_lonlat(geo::to_unit(p));
        if (PAIR_BOX[0]..=PAIR_BOX[2]).contains(&lon) && (PAIR_BOX[1]..=PAIR_BOX[3]).contains(&lat)
        {
            ends.push(stop);
        }
        let snap = r.snap(&stop)?;
        let Some(s) = snap else {
            none += 1;
            continue;
        };
        let near = r.snap(&Stop::at(p))?.map_or(f64::NAN, |n| n.dist_m);
        on_street += usize::from(s.on_street);
        small += usize::from(s.small);
        on_restricted += usize::from(restricted(&s)?);
        dists.push(s.dist_m);
        nearest.push(near);
        worst.push((s.dist_m, near, p, a.street, s.on_street));
    }
    let k = dists.len().max(1) as f64;
    let pct = |c: usize| 100.0 * c as f64 / k;
    println!(
        "snapped {}, none within 50 km {none}; on its own street {:.1}%, on a restricted road {:.2}%, in a small component {:.2}%",
        dists.len(),
        pct(on_street),
        pct(on_restricted),
        pct(small)
    );
    for (what, v) in [
        ("as the app snaps", &dists),
        ("to the nearest road", &nearest),
    ] {
        let within: Vec<String> = BANDS_M
            .iter()
            .map(|&b| {
                format!(
                    "≤{b:.0} m {:.2}%",
                    pct(v.iter().filter(|&&d| d <= b).count())
                )
            })
            .collect();
        println!(
            "  {what}: p50 {:.0} m, p95 {:.0} m, p99 {:.0} m, max {:.0} m; {}",
            quantile(v, 0.5),
            quantile(v, 0.95),
            quantile(v, 0.99),
            quantile(v, 1.0),
            within.join(", ")
        );
    }
    worst.sort_by(|a, b| b.0.total_cmp(&a.0));
    println!("worst:");
    for (d, near, p, street, own) in worst.iter().take(WORST) {
        let name = search.streets.street(*street)?;
        let [lon, lat] = geo::unit_to_lonlat(geo::to_unit(*p));
        println!(
            "  {d:6.0} m ({}; nearest road {near:.0} m)  {lat:.5},{lon:.5}  {}, {}",
            if *own { "own street" } else { "another road" },
            name.name,
            name.town
        );
    }
    pairs(r, &ends)
}

/// Routes `ends` in pairs, and counts those with a route whose two ends snapped within
/// [`PAIR_SNAP_M`] or to their own street.
fn pairs(r: &Routing, ends: &[Stop]) -> Result<()> {
    let (mut routed, mut snapped, mut both) = (0, 0, 0);
    let mut missed = Vec::new();
    let mut ms = Vec::new();
    for pair in ends.as_chunks::<2>().0 {
        let t = std::time::Instant::now();
        let trip = r.trip(pair, 0)?;
        ms.push(t.elapsed().as_secs_f64() * 1e3);
        let ok_route = trip.legs[0].is_some();
        let ok_snaps = trip.snaps.iter().all(|s| {
            s.as_ref()
                .is_some_and(|s| s.dist_m <= PAIR_SNAP_M || s.on_street)
        });
        routed += usize::from(ok_route);
        snapped += usize::from(ok_snaps);
        both += usize::from(ok_route && ok_snaps);
        if !(ok_route && ok_snaps) {
            missed.push((pair, ok_route, trip.snaps.clone()));
        }
    }
    let n = ends.len() / 2;
    let pct = |c: usize| 100.0 * c as f64 / n.max(1) as f64;
    println!(
        "pairs in the box {PAIR_BOX:?}: {n} from {} points; routed {:.2}%, both ends within {PAIR_SNAP_M:.0} m or on their own street {:.2}%, both {:.2}% (criterion 1: 99.5%); trip p50 {:.1} ms, p95 {:.1} ms",
        ends.len(),
        pct(routed),
        pct(snapped),
        pct(both),
        quantile(&ms, 0.5),
        quantile(&ms, 0.95)
    );
    for (pair, ok_route, snaps) in missed {
        let at = |s: &Stop| {
            let [lon, lat] = geo::unit_to_lonlat(geo::to_unit(s.p));
            format!("{lat:.5},{lon:.5}")
        };
        let snap = |s: &Option<Snap>| {
            s.as_ref().map_or("no snap".into(), |s| {
                format!(
                    "{:.0} m{}{}",
                    s.dist_m,
                    if s.on_street { ", own street" } else { "" },
                    if s.small { ", small component" } else { "" }
                )
            })
        };
        println!(
            "  missed: {} ({}) to {} ({}){}",
            at(&pair[0]),
            snap(&snaps[0]),
            at(&pair[1]),
            snap(&snaps[1]),
            if ok_route { "" } else { ", no route" }
        );
    }
    Ok(())
}
