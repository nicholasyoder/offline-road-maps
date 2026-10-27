//! Postcodes (phase 2 step 2.3, decision 5): one search entry per postcode at the centre of
//! the merged addresses that carry it. `build` computes the points per state and province
//! and [`combine`]s them.

use std::collections::HashMap;

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use super::pipeline::Rec;
use crate::geom::{self, P};

/// A postcode as a search entry: the median of its addresses and the distance from there
/// that 90% of them lie within.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Postcode {
    /// Display form ([`omp_core::normalize::postcode`]).
    pub code: String,
    pub addresses: u64,
    pub p: P,
    pub radius_m: f64,
}

/// Median of `v` (sorted in place).
fn median(v: &mut [u32]) -> u32 {
    v.sort_unstable();
    v[v.len() / 2]
}

/// The postcode entries of the merged addresses `reps`, whose postcodes are `rep_codes`,
/// sorted by code.
pub(super) fn points(
    recs: &[Rec],
    reps: &[u32],
    rep_codes: &[u32],
    codes: &[String],
) -> Vec<Postcode> {
    let mut by_code: Vec<(u32, P)> = reps
        .iter()
        .zip(rep_codes)
        .filter(|(_, c)| **c != 0)
        .map(|(&r, &c)| (c, recs[r as usize].p))
        .collect();
    by_code.par_sort_unstable_by_key(|x| x.0);
    let groups: Vec<&[(u32, P)]> = by_code.chunk_by(|a, b| a.0 == b.0).collect();
    let mut out: Vec<Postcode> = groups
        .par_iter()
        .map(|g| {
            let mut xs: Vec<u32> = g.iter().map(|x| x.1[0]).collect();
            let mut ys: Vec<u32> = g.iter().map(|x| x.1[1]).collect();
            let p = [median(&mut xs), median(&mut ys)];
            let mpu = geom::metres_per_unit(p);
            let mut d: Vec<f64> = g
                .iter()
                .map(|x| {
                    (f64::from(x.1[0]) - f64::from(p[0])).hypot(f64::from(x.1[1]) - f64::from(p[1]))
                        * mpu
                })
                .collect();
            d.sort_by(f64::total_cmp);
            Postcode {
                code: codes[g[0].0 as usize].clone(),
                addresses: g.len() as u64,
                p,
                radius_m: d[((d.len() - 1) as f64 * 0.9) as usize],
            }
        })
        .collect();
    out.sort_by(|a, b| a.code.cmp(&b.code));
    out
}

/// One entry per postcode from the rows of several runs (one per state or province). A
/// postcode in several (one crossing a state line) is merged: the mean of their points
/// weighted by addresses, and a radius that covers each.
pub fn combine(rows: Vec<Postcode>) -> Vec<Postcode> {
    let mut by_code: HashMap<String, Vec<Postcode>> = HashMap::new();
    for p in rows {
        by_code.entry(p.code.clone()).or_default().push(p);
    }
    let mut out: Vec<Postcode> = by_code.into_values().map(merge).collect();
    out.sort_by(|a, b| a.code.cmp(&b.code));
    out
}

/// Rows further than this from a postcode's largest row are dropped as errors (a Quebec
/// address carrying a Massachusetts ZIP).
const MERGE_MAX_M: f64 = 50_000.0;

fn merge(mut rows: Vec<Postcode>) -> Postcode {
    rows.sort_by(|a, b| b.addresses.cmp(&a.addresses).then(a.p.cmp(&b.p)));
    let main = rows[0].p;
    let mpu = geom::metres_per_unit(main);
    let dist = |a: P, b: P| {
        (f64::from(a[0]) - f64::from(b[0])).hypot(f64::from(a[1]) - f64::from(b[1])) * mpu
    };
    rows.retain(|r| dist(r.p, main) <= MERGE_MAX_M);
    if rows.len() == 1 {
        return rows.pop().unwrap();
    }
    let n: u64 = rows.iter().map(|r| r.addresses).sum();
    let w = |f: &dyn Fn(&Postcode) -> u32| {
        let s: f64 = rows
            .iter()
            .map(|r| f64::from(f(r)) * r.addresses as f64)
            .sum();
        (s / n.max(1) as f64).round() as u32
    };
    let p = [w(&|r| r.p[0]), w(&|r| r.p[1])];
    let radius_m = rows
        .iter()
        .map(|r| dist(r.p, p) + r.radius_m)
        .fold(0.0, f64::max);
    Postcode {
        code: rows[0].code.clone(),
        addresses: n,
        p,
        radius_m,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_combine_across_state_lines() {
        let pc = |code: &str, n, lat, lon, r| Postcode {
            code: code.into(),
            addresses: n,
            p: geom::project(lat, lon),
            radius_m: r,
        };
        let got = combine(vec![
            pc("45999", 10, 39.2, -84.8, 300.0),
            pc("45201", 30, 39.1, -84.5, 800.0),
            pc("45999", 10, 39.2, -84.82, 300.0),
            // A stray row far from the others is dropped.
            pc("45201", 2, 42.0, -71.0, 10.0),
        ]);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].code, "45201");
        assert_eq!((got[0].addresses, got[0].radius_m), (30, 800.0));
        let m = &got[1];
        assert_eq!((m.code.as_str(), m.addresses), ("45999", 20));
        let (lat, lon) = geom::unproject(m.p);
        assert!(
            (lat - 39.2).abs() < 1e-4 && (lon + 84.81).abs() < 1e-4,
            "{lat},{lon}"
        );
        // Each half is about 860 m from the middle, plus its own 300 m.
        assert!((1_100.0..1_250.0).contains(&m.radius_m), "{}", m.radius_m);
    }
}
