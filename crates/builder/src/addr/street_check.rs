//! The street table (step 2.2) built from the spike's streets and addresses, and a check
//! that it puts addresses where the spike did: each sampled address is placed on the table
//! by its way and position, then decoded from `SSTR` and `SGEO` at that offset.

use std::fmt::Write as _;
use std::path::Path;

use anyhow::Result;
use rayon::prelude::*;
use serde::Serialize;

use super::streets::Streets;
use super::{Rec, sample};
use crate::admin::Admin;
use crate::measure::geom::{self, P};
use crate::street::{self, Along, Table, encode};

/// Addresses checked per run.
const SAMPLE: usize = 200_000;

#[derive(Serialize)]
pub struct StreetReport {
    pub stats: street::Stats,
    pub sizes: encode::Sizes,
    pub check: Check,
}

#[derive(Serialize, Default)]
pub struct Check {
    pub sampled: u64,
    /// Not placed: the way is in no street (cannot happen) or the table has no piece.
    pub missing: u64,
    /// Metres between the spike's position and the table's, at exact offsets and with both
    /// offsets rounded to 5 m: `[p50, p95, p99, max]`.
    pub exact_m: [f64; 4],
    pub quantized_m: [f64; 4],
    /// Share within 5 m.
    pub exact_within_5m: f64,
    pub quantized_within_5m: f64,
    /// Largest change in the distance from the street, metres.
    pub lateral_max_m: f64,
    /// Share whose spike offset, decoded along the spike's component (its ways laid end to
    /// end), lands more than 5 m from the spike's own point: an offset at the end of one
    /// way is also the start of the next.
    pub spike_decode_off: f64,
    /// Of those more than 5 m apart (exact), the share where the table's point is nearer the
    /// address than the spike's: the table projects onto the street around the way's ends,
    /// the spike onto that way only.
    pub beyond_5m_nearer: f64,
    /// Of those more than 5 m apart, the share more than 25 m from the street, where the
    /// nearest point on a curve or a cul-de-sac moves with small changes in the line.
    pub beyond_5m_far: f64,
    /// Share on the same side of the street as in the spike (each side relative to its own
    /// direction of travel, compared along the street).
    pub same_side: f64,
}

/// One sampled address: metres apart at exact and 5 m offsets, the change in distance
/// from the street, whether it is on the same side, whether the spike's own decode is more
/// than 5 m off, whether the table's point is nearer the address than the spike's, and
/// the spike's distance from the street.
type Placed = (f64, f64, f64, bool, bool, bool, f64);

fn quantiles(mut v: Vec<f64>) -> [f64; 4] {
    if v.is_empty() {
        return [0.0; 4];
    }
    v.sort_by(f64::total_cmp);
    let q = |f: f64| v[((v.len() - 1) as f64 * f) as usize];
    [q(0.5), q(0.95), q(0.99), v[v.len() - 1]]
}

/// Builds the table with the postal town votes of the attached records, encodes it and
/// checks a sample of the merged addresses (`reps`). Returns the table too, for the
/// postcode measurement.
pub fn run(
    admin: &Admin,
    streets: &Streets,
    recs: &[Rec],
    reps: &[u32],
    cities: &[String],
    samples: &Path,
) -> Result<(StreetReport, Table)> {
    let votes: Vec<(u32, P, u32)> = recs
        .iter()
        .filter(|r| r.city != 0)
        .filter_map(|r| r.att.map(|a| (a.way, r.p, r.city)))
        .collect();
    let mut table = Table::build(&streets.raw, admin, &votes, cities);
    drop(votes);
    street::print_stats(&table.stats);
    let (sstr, sgeo, sizes) = encode::sections(&table, encode::BLOCK)?;
    street::print_sizes(&sizes);
    let decoded = encode::decode(&sstr, &sgeo)?;

    let attached: Vec<u32> = reps
        .iter()
        .copied()
        .filter(|&r| recs[r as usize].att.is_some())
        .collect();
    let picked = sample(attached.len(), SAMPLE, 11);
    let results: Vec<Option<Placed>> = picked
        .par_iter()
        .map(|&k| {
            let r = &recs[attached[k] as usize];
            let a = r.att.unwrap();
            let spot = table.locate(a.way, r.p)?;
            let (_, parts) = &decoded[spot.street as usize];
            let along = Along::of(parts);
            let mpu = geom::metres_per_unit(r.p);
            let d = |a: P, b: P| {
                (f64::from(a[0]) - f64::from(b[0])).hypot(f64::from(a[1]) - f64::from(b[1])) * mpu
            };
            let five = |m: f64| (m / 5.0).round() * 5.0;
            // The spike's point: the foot on its way, where the attach put it.
            let on_way = streets.project(a.way, r.p).along_m;
            let spike = streets.point_on_way(a.way, on_way);
            let exact = d(spike, street::point_at(parts, &along, spot.offset_m));
            let quantized = d(
                streets.point_on_way(a.way, on_way + five(a.offset_m) - a.offset_m),
                street::point_at(parts, &along, five(spot.offset_m)),
            );
            // The spike's own decode, its ways laid end to end.
            let spike_decode = d(spike, streets.point_along(a.comp, a.offset_m));
            // Whether the two directions of travel agree here.
            let dir = |p: P, q: P| {
                [
                    f64::from(q[0]) - f64::from(p[0]),
                    f64::from(q[1]) - f64::from(p[1]),
                ]
            };
            let u = dir(
                streets.point_along(a.comp, (a.offset_m - 2.0).max(0.0)),
                streets.point_along(a.comp, a.offset_m + 2.0),
            );
            let v = dir(
                street::point_at(parts, &along, (spot.offset_m - 2.0).max(0.0)),
                street::point_at(parts, &along, spot.offset_m + 2.0),
            );
            let same_way = u[0] * v[0] + u[1] * v[1] >= 0.0;
            let same_side = (a.left == spot.left) == same_way;
            Some((
                exact,
                quantized,
                (spot.dist_m - a.dist_m).abs(),
                same_side,
                spike_decode > 5.0,
                spot.dist_m < a.dist_m - 1.0,
                a.dist_m,
            ))
        })
        .collect();
    // The worst placed, for `streets-check.tsv`.
    let mut worst: Vec<(f64, usize)> = results
        .iter()
        .enumerate()
        .filter_map(|(i, r)| r.map(|x| (x.0, i)))
        .collect();
    worst.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut text = String::from(
        "error_m\tway\tway_name\tstreet\tstreet_name\tparts\tspike_offset_m\toffset_m\tdist_m\tspike_dist_m\taddress\tspike_at\ttable_at\n",
    );
    let lat_lon = |p: P| {
        let (lat, lon) = geom::unproject(p);
        format!("{lat:.6},{lon:.6}")
    };
    for &(e, i) in worst.iter().take(100) {
        let r = &recs[attached[picked[i]] as usize];
        let a = r.att.unwrap();
        let spot = table.locate(a.way, r.p).unwrap();
        let (rec, parts) = &decoded[spot.street as usize];
        let along = Along::of(parts);
        let _ = writeln!(
            text,
            "{e:.1}\t{}\t{}\t{}\t{}\t{}\t{:.1}\t{:.1}\t{:.1}\t{:.1}\t{}\t{}\t{}",
            streets.raw[a.way as usize].id,
            streets.name(a.way),
            spot.street,
            rec.name,
            parts.len(),
            a.offset_m,
            spot.offset_m,
            spot.dist_m,
            a.dist_m,
            lat_lon(r.p),
            lat_lon(streets.point_on_way(a.way, streets.project(a.way, r.p).along_m)),
            lat_lon(street::point_at(parts, &along, spot.offset_m)),
        );
    }
    std::fs::create_dir_all(samples)?;
    std::fs::write(samples.join("streets-check.tsv"), text)?;
    let found: Vec<Placed> = results.iter().flatten().copied().collect();
    let share = |f: &dyn Fn(&Placed) -> bool| {
        found.iter().filter(|x| f(x)).count() as f64 / found.len().max(1) as f64
    };
    let check = Check {
        sampled: picked.len() as u64,
        missing: (results.len() - found.len()) as u64,
        exact_m: quantiles(found.iter().map(|x| x.0).collect()),
        quantized_m: quantiles(found.iter().map(|x| x.1).collect()),
        exact_within_5m: share(&|x| x.0 <= 5.0),
        quantized_within_5m: share(&|x| x.1 <= 5.0),
        lateral_max_m: found.iter().map(|x| x.2).fold(0.0, f64::max),
        same_side: share(&|x| x.3),
        spike_decode_off: share(&|x| x.4),
        beyond_5m_nearer: {
            let far: Vec<&Placed> = found.iter().filter(|x| x.0 > 5.0).collect();
            far.iter().filter(|x| x.5).count() as f64 / far.len().max(1) as f64
        },
        beyond_5m_far: {
            let far: Vec<&Placed> = found.iter().filter(|x| x.0 > 5.0).collect();
            far.iter().filter(|x| x.6 > 25.0).count() as f64 / far.len().max(1) as f64
        },
    };
    eprintln!(
        "  streets check: {} addresses, {} missing; exact p50/p95/p99/max {:.2}/{:.2}/{:.2}/{:.2} m \
         ({:.4} within 5 m), at 5 m offsets {:.2}/{:.2}/{:.2}/{:.2} m ({:.4}), lateral max {:.2} m, same side {:.4}; spike's own decode off by > 5 m {:.4}; beyond 5 m: nearer {:.4}, over 25 m from the street {:.4}",
        check.sampled,
        check.missing,
        check.exact_m[0],
        check.exact_m[1],
        check.exact_m[2],
        check.exact_m[3],
        check.exact_within_5m,
        check.quantized_m[0],
        check.quantized_m[1],
        check.quantized_m[2],
        check.quantized_m[3],
        check.quantized_within_5m,
        check.lateral_max_m,
        check.same_side,
        check.spike_decode_off,
        check.beyond_5m_nearer,
        check.beyond_5m_far
    );
    let report = StreetReport {
        stats: std::mem::take(&mut table.stats),
        sizes,
        check,
    };
    Ok((report, table))
}
