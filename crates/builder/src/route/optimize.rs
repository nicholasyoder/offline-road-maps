//! `route --optimize` and `--optimize-random` (phase 4 step 4.1): the stops' quickest
//! order, how long the matrix and the order took, and whether the legs routed in that
//! order add up to the matrix's total (criterion 1).

use std::time::Instant;

use anyhow::{Context, Result, bail};
use omp_core::route::order::{self, NONE};
use omp_core::route::{Ends, Optimized, Routing, Stop, Trip};

use super::check::random_point;
use super::duration;
use crate::ch::check::Rng;
use crate::peak_rss_mib;

/// A matrix weight as a time.
fn weight(w: Option<u64>) -> String {
    w.map_or("no route".into(), |w| duration(w as f64 / 10.0))
}

/// The new order, with the stops' names, and the times.
pub fn print(o: &Optimized, names: &[String]) {
    println!("optimized order:");
    for (k, &i) in o.order.iter().enumerate() {
        println!("  {:>3}. stop {}  {}", k + 1, i + 1, names[i]);
    }
    summary(o);
}

fn summary(o: &Optimized) {
    let saved = match (o.before, o.after) {
        (Some(b), Some(a)) => format!(", saves {}", duration(b.saturating_sub(a) as f64 / 10.0)),
        _ => String::new(),
    };
    println!(
        "before {}, after {}{saved}; matrix {:.1} ms ({} nodes settled), order {:.1} ms",
        weight(o.before),
        weight(o.after),
        o.matrix_time.as_secs_f64() * 1e3,
        o.settled,
        o.order_time.as_secs_f64() * 1e3,
    );
}

/// Whether the legs of `trip`, routed in the optimized order, add up to the matrix.
pub fn check_legs(o: &Optimized, trip: &Trip) -> bool {
    let sum: Option<u64> = trip
        .legs
        .iter()
        .map(|l| l.as_ref().map(|l| u64::from(l.weight)))
        .sum();
    let ok = sum == o.after;
    match ok {
        true => println!("legs add up to the matrix: {}", weight(sum)),
        false => println!(
            "legs DO NOT add up to the matrix: {:?} against {:?}",
            sum, o.after
        ),
    }
    ok
}

/// `n` random stops: optimized cold, then warm, against 100 random restarts, and the legs
/// routed. Stops some other stop has no route to or from (islands, pockets) are replaced
/// by new ones, so the times add up.
pub fn random(r: &Routing, n: usize, avoid: u8, ends: Ends, seed: u64) -> Result<()> {
    if n < 2 {
        bail!("give at least two stops");
    }
    let mut rng = Rng::new(seed);
    let mut stops = (0..n)
        .map(|_| Ok(Stop::at(random_point(r, &mut rng)?)))
        .collect::<Result<Vec<_>>>()?;
    println!("{n} random stops, {ends:?}");
    // Replace the stops with no route to or from most others, then the worst left.
    let mut replaced = 0;
    for pass in 0.. {
        let t = Instant::now();
        let snaps = stops
            .iter()
            .map(|s| r.snap(s)?.context("a random stop with no road"))
            .collect::<Result<Vec<_>>>()?;
        let (m, settled) = r.matrix_until(&snaps, avoid, &|| false)?.unwrap();
        if pass == 0 {
            println!(
                "cold matrix: {:.1} ms ({settled} nodes settled)",
                t.elapsed().as_secs_f64() * 1e3
            );
        }
        let missing: Vec<usize> = (0..n)
            .map(|i| {
                (0..n)
                    .filter(|&j| m.get(i, j) == NONE || m.get(j, i) == NONE)
                    .count()
            })
            .collect();
        let mut bad: Vec<usize> = (0..n).filter(|&i| missing[i] > n / 2).collect();
        let worst = (0..n).max_by_key(|&i| missing[i]).unwrap();
        if bad.is_empty() && missing[worst] > 0 {
            bad.push(worst);
        }
        if bad.is_empty() {
            break;
        }
        for i in bad {
            stops[i] = Stop::at(random_point(r, &mut rng)?);
            replaced += 1;
        }
    }
    if replaced > 0 {
        println!("{replaced} stops replaced: no route to or from them");
    }
    let o = r
        .optimize_until(&stops, avoid, ends, &|| false)?
        .expect("never stopped");
    print!("warm: ");
    summary(&o);
    let t = Instant::now();
    let best = order::restarts(&o.matrix, ends, 100, seed);
    let best_w = o.matrix.total(&best, ends.round_trip);
    let ratio = match (o.after, best_w) {
        (Some(a), Some(b)) if b > 0 => format!("{:.4}", a as f64 / b as f64),
        _ => "-".into(),
    };
    println!(
        "100 random restarts: {} in {:.1} ms; the order is {ratio} of it",
        weight(best_w),
        t.elapsed().as_secs_f64() * 1e3
    );
    let mut ordered: Vec<Stop> = o.order.iter().map(|&i| stops[i]).collect();
    if ends.round_trip {
        ordered.push(ordered[0]);
    }
    let t = Instant::now();
    let trip = r.trip(&ordered, avoid)?;
    println!(
        "legs routed in {:.1} ms; peak RSS {:.0} MiB",
        t.elapsed().as_secs_f64() * 1e3,
        peak_rss_mib()
    );
    if !check_legs(&o, &trip) {
        bail!("the legs do not add up to the matrix");
    }
    Ok(())
}
