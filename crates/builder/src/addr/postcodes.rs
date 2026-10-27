//! Postcodes (phase 2 step 2.3, decision 5): one search entry per postcode at the centre of
//! the merged addresses that carry it, and the cost of showing a postcode with each address,
//! stored once per street with exceptions or once per address.
//!
//! `build` (step 2.4) computes the points per state and province and [`combine`]s them;
//! `addr-spike` writes its run's to `<samples>/postcodes.tsv`.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Context, Result};
use omp_core::varint::put;
use rayon::prelude::*;
use serde::Serialize;

use super::Rec;
use crate::measure::geom::{self, P};
use crate::mib;
use crate::street::Table;

/// A postcode as a search entry: the median of its addresses and the distance from there
/// that 90% of them lie within.
#[derive(Clone, Debug, PartialEq)]
pub struct Postcode {
    /// Display form ([`omp_core::normalize::postcode`]).
    pub code: String,
    pub addresses: u64,
    pub p: P,
    pub radius_m: f64,
}

#[derive(Serialize, Default)]
pub struct PostcodeReport {
    /// Merged addresses, and those with a postcode.
    pub addresses: u64,
    pub with_postcode: u64,
    /// Distinct postcodes: five-digit (US) and Canadian.
    pub codes_numeric: u64,
    pub codes_canadian: u64,
    /// Radius holding 90% of a postcode's addresses: `[p50, p90, max]` metres over postcodes.
    pub radius_m: [f64; 3],
    pub streets: u64,
    /// Streets with an attached address, and with an attached address that has a postcode.
    pub streets_with_addresses: u64,
    pub streets_with_postcode: u64,
    /// Streets whose addresses give more than one postcode.
    pub streets_several: u64,
    /// Attached addresses with a postcode, and those whose postcode is their street's
    /// commonest (shown right by one postcode per street).
    pub attached_with_postcode: u64,
    pub attached_majority: u64,
    /// Bytes, zstd 19 in 32 KiB blocks: the postcode table; one postcode per street (a
    /// varint per street record, 0 for none); the exceptions to it (`ordinal_delta code`, as
    /// `ASUF`); and the alternative of one postcode per attached address.
    pub table_bytes: u64,
    pub per_street_bytes: u64,
    pub exceptions: u64,
    pub exception_bytes: u64,
    pub per_address_bytes: u64,
}

fn compressed(raw: &[u8]) -> Result<u64> {
    raw.par_chunks(32 << 10)
        .map(|c| Ok(zstd::bulk::compress(c, 19)?.len() as u64))
        .sum()
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

/// Measures the postcodes of the merged addresses `reps` on `table` and writes
/// `<samples>/postcodes.tsv`. `rep_codes` is each merged address's postcode: its kept
/// record's, else one another record of its cluster gives.
pub(super) fn measure(
    table: &Table,
    recs: &[Rec],
    reps: &[u32],
    rep_codes: &[u32],
    codes: &[String],
    samples: &Path,
) -> Result<PostcodeReport> {
    let pts = points(recs, reps, rep_codes, codes);
    write_tsv(&samples.join("postcodes.tsv"), &pts)?;

    // Codes by their sorted position, as a pack would store them.
    let mut sorted: Vec<u32> = (1..codes.len() as u32).collect();
    sorted.sort_by(|&a, &b| codes[a as usize].cmp(&codes[b as usize]));
    let mut rank = vec![0u32; codes.len()];
    for (i, &c) in sorted.iter().enumerate() {
        rank[c as usize] = i as u32 + 1;
    }
    let mut table_raw = Vec::new();
    for &c in &sorted {
        let s = &codes[c as usize];
        put(&mut table_raw, s.len() as u64);
        table_raw.extend_from_slice(s.as_bytes());
    }

    // `(street, number, postcode rank)` of each attached address, in `ASTR` order.
    let mut placed: Vec<(u32, u32, u32)> = reps
        .par_iter()
        .zip(rep_codes)
        .filter_map(|(&r, &code)| {
            let rec = &recs[r as usize];
            let a = rec.att?;
            let spot = table.locate(a.way, rec.p)?;
            Some((spot.street, rec.num, rank[code as usize]))
        })
        .collect();
    placed.par_sort_unstable();

    let mut report = PostcodeReport {
        addresses: reps.len() as u64,
        with_postcode: pts.iter().map(|p| p.addresses).sum(),
        codes_numeric: pts.iter().filter(|p| !p.code.contains(' ')).count() as u64,
        codes_canadian: pts.iter().filter(|p| p.code.contains(' ')).count() as u64,
        streets: table.streets.len() as u64,
        ..Default::default()
    };
    let mut radii: Vec<f64> = pts.iter().map(|p| p.radius_m).collect();
    radii.sort_by(f64::total_cmp);
    if let Some(&max) = radii.last() {
        let q = |f: f64| radii[((radii.len() - 1) as f64 * f) as usize];
        report.radius_m = [q(0.5), q(0.9), max];
    }

    let mut majority = vec![0u32; table.streets.len()];
    for g in placed.chunk_by(|a, b| a.0 == b.0) {
        report.streets_with_addresses += 1;
        let mut counts: HashMap<u32, u32> = HashMap::new();
        for x in g.iter().filter(|x| x.2 != 0) {
            *counts.entry(x.2).or_default() += 1;
        }
        let Some((&code, _)) = counts.iter().max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0))) else {
            continue;
        };
        report.streets_with_postcode += 1;
        report.streets_several += u64::from(counts.len() > 1);
        majority[g[0].0 as usize] = code;
    }
    let (mut per_street, mut exceptions, mut per_address) = (Vec::new(), Vec::new(), Vec::new());
    for &m in &majority {
        put(&mut per_street, u64::from(m));
    }
    let mut last = 0;
    for (ordinal, &(street, _, code)) in placed.iter().enumerate() {
        put(&mut per_address, u64::from(code));
        if code == 0 {
            continue;
        }
        report.attached_with_postcode += 1;
        if code == majority[street as usize] {
            report.attached_majority += 1;
        } else {
            report.exceptions += 1;
            put(&mut exceptions, (ordinal - last) as u64);
            put(&mut exceptions, u64::from(code));
            last = ordinal;
        }
    }
    report.table_bytes = compressed(&table_raw)?;
    report.per_street_bytes = compressed(&per_street)?;
    report.exception_bytes = compressed(&exceptions)?;
    report.per_address_bytes = compressed(&per_address)?;
    eprintln!(
        "  postcodes: {} of {} addresses, {} codes ({} numeric, {} Canadian), radius p50/p90 \
         {:.0}/{:.0} m; {} of {} streets with addresses have one ({} several), the street's \
         commonest is right for {:.2}% of attached addresses; table {:.2} MiB, per street {:.2} \
         MiB + {} exceptions {:.2} MiB, per address {:.2} MiB",
        report.with_postcode,
        report.addresses,
        pts.len(),
        report.codes_numeric,
        report.codes_canadian,
        report.radius_m[0],
        report.radius_m[1],
        report.streets_with_postcode,
        report.streets_with_addresses,
        report.streets_several,
        100.0 * report.attached_majority as f64 / report.attached_with_postcode.max(1) as f64,
        mib(report.table_bytes),
        mib(report.per_street_bytes),
        report.exceptions,
        mib(report.exception_bytes),
        mib(report.per_address_bytes),
    );
    Ok(report)
}

/// `code addresses lat lon radius_m`, with a header.
pub fn write_tsv(path: &Path, pts: &[Postcode]) -> Result<()> {
    let mut text = String::from("code\taddresses\tlat\tlon\tradius_m\n");
    for p in pts {
        let (lat, lon) = geom::unproject(p.p);
        let _ = writeln!(
            text,
            "{}\t{}\t{lat:.7}\t{lon:.7}\t{:.0}",
            p.code, p.addresses, p.radius_m
        );
    }
    std::fs::write(path, text).with_context(|| format!("write {}", path.display()))
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
