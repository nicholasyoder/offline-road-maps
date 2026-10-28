//! `route --alternatives` and `--alternatives-random` (phase 4 step 4.2): each leg's
//! alternatives with their time, the time ranked with turn costs, what they share and
//! their turns; and, on random pairs by length, how often one or two are found, their
//! stretch and sharing, and how long they take (criterion 2).

use std::time::Instant;

use anyhow::Result;
use omp_core::route::{AltParams, Alternatives, Names, Routing, Stop, Trip};

use super::check::random_point;
use super::set::{pair_near, quantile};
use super::{duration, latlon};
use crate::ch::check::Rng;
use crate::peak_rss_mib;

/// The alternatives of each routed leg of `trip`.
pub fn of_trip(r: &Routing, trip: &Trip, params: &AltParams) -> Result<Vec<Option<Alternatives>>> {
    let mut out = Vec::new();
    for (i, leg) in trip.legs.iter().enumerate() {
        let alts = match (leg, &trip.snaps[i], &trip.snaps[i + 1]) {
            (Some(l), Some(a), Some(b)) => {
                r.alternatives_until(a, b, l, trip.avoid, params, &|| false)?
            }
            _ => None,
        };
        out.push(alts);
    }
    Ok(out)
}

/// The trip with each leg's picked route.
pub fn picked(trip: &Trip, alts: &[Option<Alternatives>]) -> Trip {
    let mut t = trip.clone();
    for (leg, a) in t.legs.iter_mut().zip(alts) {
        if let Some(a) = a {
            *leg = Some(a.picked().leg.clone());
        }
    }
    t
}

/// "+4 min", "-30 s", "same".
fn delta(s: f64) -> String {
    match s.round() as i64 {
        0 => "same".into(),
        d if d > 0 => format!("+{}", duration(d as f64)),
        d => format!("-{}", duration(-d as f64)),
    }
}

pub fn print(r: &Routing, names: Option<&dyn Names>, alts: &[Option<Alternatives>]) -> Result<()> {
    for (i, a) in alts.iter().enumerate() {
        let Some(a) = a else { continue };
        println!(
            "leg {} alternatives: {} found; {} candidates, {} tested, {} nodes settled",
            i + 1,
            a.routes.len() - 1,
            a.candidates,
            a.tested,
            a.settled
        );
        let quickest = &a.routes[0];
        let legs: Vec<_> = a.routes.iter().map(|x| &x.leg).collect();
        for (k, x) in a.routes.iter().enumerate() {
            let others: Vec<_> = (0..legs.len())
                .filter(|&j| j != k)
                .map(|j| legs[j])
                .collect();
            let via = r
                .via_road(&x.leg, &others, names)?
                .map_or(String::new(), |road| format!("  via {}", road.text()));
            let what = match k {
                0 => "quickest".to_owned(),
                _ => format!(
                    "{} (x{:.2}), shares {:.0}%",
                    delta(x.leg.time_s - quickest.leg.time_s),
                    f64::from(x.leg.weight) / f64::from(quickest.leg.weight.max(1)),
                    100.0 * x.shared
                ),
            };
            println!(
                "  {}{}. {:>7.1} km {:>11}; ranked {:>11}; {} turns, {} U-turns; {what}{via}{}",
                if k == a.picked { "*" } else { " " },
                k + 1,
                x.leg.metres / 1000.0,
                duration(x.leg.time_s),
                duration(x.score_s()),
                x.turns.turns,
                x.turns.uturns,
                x.via
                    .map_or(String::new(), |p| format!(" (via node at {})", latlon(p)))
            );
        }
        if a.picked != 0 {
            let p = a.picked();
            println!(
                "  picked {}: {} slower on paper, {} fewer U-turns, {} fewer turns",
                a.picked + 1,
                duration(p.leg.time_s - quickest.leg.time_s),
                i64::from(quickest.turns.uturns) - i64::from(p.turns.uturns),
                i64::from(quickest.turns.turns) - i64::from(p.turns.turns),
            );
        }
    }
    Ok(())
}

/// What one random pair gave.
struct Sample {
    km: f64,
    found: usize,
    stretch: Vec<f64>,
    sharing: Vec<f64>,
    candidates: usize,
    quickest_ms: f64,
    alt_ms: f64,
    directions_ms: f64,
    picked: bool,
    uturns: [u32; 2],
}

/// Length bands of the quickest route, in km.
const BANDS: [(f64, f64); 5] = [
    (0.0, 10.0),
    (10.0, 50.0),
    (50.0, 200.0),
    (200.0, 1000.0),
    (1000.0, f64::INFINITY),
];

/// `n` random pairs, a quarter each 1–10 km, 10–50 km and 50–250 km apart and between
/// random points anywhere: the share with one and with two alternatives, their stretch and
/// sharing, the time they take, and how often turn costs pick another route. `out` gets
/// the pairs.
pub fn random(
    r: &Routing,
    names: Option<&dyn Names>,
    n: usize,
    avoid: u8,
    params: &AltParams,
    seed: u64,
    out: Option<&std::path::Path>,
) -> Result<()> {
    let mut rng = Rng::new(seed + 3);
    let mut pairs = Vec::with_capacity(n);
    let per = n / 4;
    for (lo, hi) in [(1_000, 10_000), (10_000, 50_000), (50_000, 250_000)] {
        let want = pairs.len() + per;
        while pairs.len() < want {
            if let Some(p) = pair_near(r, &mut rng, lo, hi)? {
                pairs.push(p);
            }
        }
    }
    while pairs.len() < n {
        pairs.push((random_point(r, &mut rng)?, random_point(r, &mut rng)?));
    }
    if let Some(out) = out {
        // For `tools/osrm/alternatives.py`.
        let mut text = String::from("# region\tid\tstops\tavoid\n");
        for (k, (p, q)) in pairs.iter().enumerate() {
            text += &format!("-\t{}\t{};{}\t-\n", k + 1, latlon(*p), latlon(*q));
        }
        std::fs::write(out, text)?;
        println!("wrote the pairs to {}", out.display());
    }
    println!("{n} random pairs, {params:?}");
    let mut samples = Vec::new();
    let (mut unrouted, mut cold) = (0, None);
    for (p, q) in pairs {
        let stops = [Stop::at(p), Stop::at(q)];
        let (Some(a), Some(b)) = (r.snap(&stops[0])?, r.snap(&stops[1])?) else {
            unrouted += 1;
            continue;
        };
        let t = Instant::now();
        let Some(leg) = r.leg_avoiding(&a, &b, avoid)? else {
            unrouted += 1;
            continue;
        };
        let quickest_ms = t.elapsed().as_secs_f64() * 1e3;
        let t = Instant::now();
        let alts = r
            .alternatives_until(&a, &b, &leg, avoid, params, &|| false)?
            .expect("never stopped");
        let alt_ms = t.elapsed().as_secs_f64() * 1e3;
        // Directions for every alternative, as the app shows them.
        let t = Instant::now();
        for x in &alts.routes[1..] {
            let trip = Trip {
                snaps: vec![Some(a.clone()), Some(b.clone())],
                legs: vec![Some(x.leg.clone())],
                avoid,
            };
            r.directions(&trip, &stops, names)?;
        }
        let directions_ms = t.elapsed().as_secs_f64() * 1e3;
        cold.get_or_insert(alt_ms + directions_ms);
        let opt = f64::from(leg.weight.max(1));
        samples.push(Sample {
            km: leg.metres / 1000.0,
            found: alts.routes.len() - 1,
            stretch: alts.routes[1..]
                .iter()
                .map(|x| f64::from(x.leg.weight) / opt)
                .collect(),
            sharing: alts.routes[1..].iter().map(|x| x.shared).collect(),
            candidates: alts.candidates,
            quickest_ms,
            alt_ms,
            directions_ms,
            picked: alts.picked != 0,
            uturns: [alts.routes[0].turns.uturns, alts.picked().turns.uturns],
        });
    }
    println!(
        "{} routed, {unrouted} with no route; the first pair took {:.1} ms with directions",
        samples.len(),
        cold.unwrap_or(0.0)
    );
    println!(
        "  {:<14} {:>5} {:>6} {:>6} {:>13} {:>13} {:>6} {:>8} {:>22} {:>7} {:>7} {:>13}",
        "km",
        "pairs",
        ">=1",
        "2",
        "stretch p50/max",
        "shares p50/max",
        "cands",
        "quick ms",
        "alts ms p50/p95/max",
        "dirs ms",
        "picked",
        "U-turn q/p"
    );
    let mut rows: Vec<(String, Vec<&Sample>)> = BANDS
        .iter()
        .map(|&(lo, hi)| {
            let name = match hi.is_finite() {
                true => format!("{lo:.0}-{hi:.0}"),
                false => format!("{lo:.0}+"),
            };
            let v = samples.iter().filter(|s| s.km >= lo && s.km < hi).collect();
            (name, v)
        })
        .collect();
    rows.push((
        "over 50".into(),
        samples.iter().filter(|s| s.km >= 50.0).collect(),
    ));
    rows.push(("all".into(), samples.iter().collect()));
    for (name, v) in rows {
        if v.is_empty() {
            continue;
        }
        let pct = |k: usize| 100.0 * k as f64 / v.len() as f64;
        let all = |f: fn(&Sample) -> &Vec<f64>| -> Vec<f64> {
            v.iter().flat_map(|s| f(s).iter().copied()).collect()
        };
        let (st, sh) = (all(|s| &s.stretch), all(|s| &s.sharing));
        let max = |x: &[f64]| x.iter().copied().fold(f64::NAN, f64::max);
        let col = |f: fn(&Sample) -> f64| -> Vec<f64> { v.iter().map(|s| f(s)).collect() };
        let alt = col(|s| s.alt_ms);
        println!(
            "  {:<14} {:>5} {:>5.1}% {:>5.1}% {:>6.3}/{:<6.3} {:>6.2}/{:<6.2} {:>6.0} {:>8.1} {:>7.1}/{:>6.1}/{:>7.1} {:>7.1} {:>6.1}% {:>5.1}/{:.1}%",
            name,
            v.len(),
            pct(v.iter().filter(|s| s.found >= 1).count()),
            pct(v.iter().filter(|s| s.found >= 2).count()),
            quantile(&st, 0.5),
            max(&st),
            quantile(&sh, 0.5),
            max(&sh),
            quantile(&col(|s| s.candidates as f64), 0.5),
            quantile(&col(|s| s.quickest_ms), 0.5),
            quantile(&alt, 0.5),
            quantile(&alt, 0.95),
            max(&alt),
            quantile(&col(|s| s.directions_ms), 0.5),
            pct(v.iter().filter(|s| s.picked).count()),
            pct(v.iter().filter(|s| s.uturns[0] > 0).count()),
            pct(v.iter().filter(|s| s.uturns[1] > 0).count()),
        );
    }
    println!("peak RSS {:.0} MiB", peak_rss_mib());
    Ok(())
}
