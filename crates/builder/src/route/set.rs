//! The hand-written route set (phase 3 step 3.5, criterion 3) and the comparison with
//! OSRM (criterion 4), so both rerun without OSRM:
//!
//! - `route <pack> --set <tsv>` routes each line of the set for the pack's region and
//!   checks its expectations: roads used and not used, steps that must and must not
//!   appear, and a time range (`crates/core/tests/route/README.md`).
//! - `route <pack> --pairs N --out <tsv>` draws random pairs (a third in town, a third
//!   regional, a third anywhere) for `tools/osrm/fetch.py` to route through OSRM.
//! - `route <pack> --osrm <tsv>…` routes OSRM's saved answers' stops and compares time,
//!   distance, how much of the lines overlap and the number of steps; given two files
//!   (stock and without turn costs), it compares them with each other too.
//! - `route <pack> --near lat,lon` lists the turn restrictions and roundabouts near a
//!   point, to write lines of the set from.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Context, Result, bail};
use omp_core::geo::{self, P};
use omp_core::route::{FERRY, Kind, Names, Road, Routing, Step, Stop, TOLL, Trip, cell_xy};

use super::check::{offset, random_point};
use super::{distance, duration, flags_text, latlon, parse_avoid};
use crate::ch::check::Rng;

/// A road counts as used when the route drives this much of it.
const USED_M: f64 = 200.0;
/// A road counts as avoided when the route drives no more than this of it (crossing it,
/// or touching it at a junction).
const TOUCH_M: f64 = 50.0;
/// Two lines overlap where they are within this of each other.
const OVERLAP_M: f64 = 25.0;

/// "I-71", "I 71" and "i71" alike.
fn norm(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn parse_stops(s: &str) -> Result<Vec<P>> {
    s.split(';')
        .map(|p| {
            let (lat, lon) = p
                .split_once(',')
                .with_context(|| format!("bad stop {p:?}: lat,lon"))?;
            Ok(geo::project(lat.trim().parse()?, lon.trim().parse()?))
        })
        .collect()
}

fn stops_text(stops: &[P]) -> String {
    stops
        .iter()
        .map(|&p| latlon(p))
        .collect::<Vec<_>>()
        .join(";")
}

/// `text|!text|…`: each wanted (or, with `!`, unwanted).
fn parse_list(s: &str) -> Vec<(bool, String)> {
    match s.trim() {
        "-" | "" => Vec::new(),
        s => s
            .split('|')
            .map(|x| match x.strip_prefix('!') {
                Some(x) => (false, x.trim().to_owned()),
                None => (true, x.trim().to_owned()),
            })
            .collect(),
    }
}

/// One line of the route set.
struct Case {
    region: String,
    id: String,
    stops: Vec<P>,
    avoid: u8,
    roads: Vec<(bool, String)>,
    steps: Vec<(bool, String)>,
    minutes: Option<(f64, f64)>,
}

fn read_set(file: &Path) -> Result<Vec<Case>> {
    let text = std::fs::read_to_string(file).with_context(|| format!("read {}", file.display()))?;
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split('\t').collect();
        // The note may be left off.
        if f.len() < 7 {
            bail!("{}:{}: {} columns, want 8", file.display(), n + 1, f.len());
        }
        let minutes = match f[6].trim() {
            "-" => None,
            m => {
                let (a, b) = m.split_once('-').context("minutes: a-b")?;
                Some((a.parse()?, b.parse()?))
            }
        };
        let avoid = match f[3].trim() {
            "-" => 0,
            a => parse_avoid(&a.split(',').map(String::from).collect::<Vec<_>>())?,
        };
        out.push(Case {
            region: f[0].trim().to_owned(),
            id: f[1].trim().to_owned(),
            stops: parse_stops(f[2])?,
            avoid,
            roads: parse_list(f[4]),
            steps: parse_list(f[5]),
            minutes,
        });
    }
    Ok(out)
}

/// The pack's region as the set names it: its name in lowercase, hyphenated.
pub fn region_of(name: &str) -> String {
    name.to_lowercase().replace(' ', "-")
}

/// Metres driven on roads matching `want`: a route number equal to it, or a name holding
/// it.
fn metres_on(r: &Routing, names: Option<&dyn Names>, trip: &Trip, want: &str) -> Result<f64> {
    let w = norm(want);
    let mut m = 0.0;
    for leg in trip.legs.iter().flatten() {
        let roads = r.roads(leg, names)?;
        for (p, road) in leg.pieces.iter().zip(&roads) {
            if road.refs.iter().any(|x| norm(x) == w) || norm(&road.name).contains(&w) {
                m += p.metres;
            }
        }
    }
    Ok(m)
}

/// Metres of the trip on segment directions with `flag`.
fn metres_flagged(r: &Routing, trip: &Trip, flag: u8) -> Result<f64> {
    let mut m = 0.0;
    for leg in trip.legs.iter().flatten() {
        for p in &leg.pieces {
            let seg = r.segment(p.dir >> 1)?;
            if seg
                .direction(p.dir & 1 != 0)
                .is_some_and(|d| d.flags & flag != 0)
            {
                m += p.metres;
            }
        }
    }
    Ok(m)
}

/// Runs the route set's lines for `region` (every line for `north-america`).
pub fn run_set(
    r: &Routing,
    names: Option<&dyn Names>,
    file: &Path,
    region: &str,
    miles: bool,
    verbose: bool,
) -> Result<()> {
    let cases: Vec<Case> = read_set(file)?
        .into_iter()
        .filter(|c| region == "north-america" || c.region == region)
        .collect();
    println!(
        "route set {} on {region}: {} routes",
        file.display(),
        cases.len()
    );
    let (mut pass, mut n) = (0, 0);
    let mut by_region: HashMap<String, (usize, usize)> = HashMap::new();
    for c in &cases {
        let stops: Vec<Stop> = c.stops.iter().map(|&p| Stop::at(p)).collect();
        let trip = r.trip(&stops, c.avoid)?;
        let steps = r.directions(&trip, &stops, names)?;
        let mut fails: Vec<String> = Vec::new();
        if !trip.complete() {
            fails.push("not routed".into());
        }
        for (want, road) in &c.roads {
            // `a/b`: either.
            let mut m: f64 = 0.0;
            for alt in road.split('/') {
                m = m.max(match alt.trim() {
                    "toll" => metres_flagged(r, &trip, TOLL)?,
                    "ferry" => metres_flagged(r, &trip, FERRY)?,
                    x => metres_on(r, names, &trip, x)?,
                });
            }
            match want {
                true if m < USED_M => fails.push(format!("does not use {road} ({m:.0} m)")),
                false if m > TOUCH_M => fails.push(format!("uses {road} ({m:.0} m)")),
                _ => {}
            }
        }
        let texts: Vec<String> = steps.iter().map(|s| s.text().to_lowercase()).collect();
        for (want, text) in &c.steps {
            let t = text.to_lowercase();
            let found = texts.iter().any(|x| x.contains(&t));
            if found != *want {
                fails.push(match want {
                    true => format!("no step {text:?}"),
                    false => format!("a step {text:?}"),
                });
            }
        }
        let min = trip.time_s() / 60.0;
        if let Some((a, b)) = c.minutes
            && !(a..=b).contains(&min)
        {
            fails.push(format!("{min:.0} min, not {a}-{b}"));
        }
        n += 1;
        let e = by_region.entry(c.region.clone()).or_default();
        e.1 += 1;
        if fails.is_empty() {
            pass += 1;
            e.0 += 1;
        }
        println!(
            "  {:<5} {:<36} {:>7.1} km {:>11}{}{}",
            if fails.is_empty() { "pass" } else { "FAIL" },
            c.id,
            trip.metres() / 1000.0,
            duration(trip.time_s()),
            match trip.broken() {
                0 => String::new(),
                b => format!("  (breaks {})", flags_text(b)),
            },
            match fails.is_empty() {
                true => String::new(),
                false => format!("  {}", fails.join("; ")),
            }
        );
        if verbose || !fails.is_empty() {
            print_steps(&steps, miles, "          ");
        }
    }
    let mut regions: Vec<_> = by_region.into_iter().collect();
    regions.sort();
    let per: Vec<String> = regions
        .iter()
        .map(|(k, (p, n))| format!("{k} {p}/{n}"))
        .collect();
    println!(
        "  passed {pass} of {n} ({:.1}%; criterion 3: 95%): {}",
        100.0 * pass as f64 / n.max(1) as f64,
        per.join(", ")
    );
    Ok(())
}

pub fn print_steps(steps: &[Step], miles: bool, indent: &str) {
    for (i, s) in steps.iter().enumerate() {
        let dist = match s.pieces.is_empty() {
            true => String::new(),
            false => format!("{}, {}", distance(s.metres, miles), duration(s.time_s)),
        };
        println!("{indent}{:>3}. {:<72} {dist}", i + 1, s.text());
    }
}

/// Writes `n` random pairs for OSRM: a third 1–10 km apart, a third 10–100 km, a third
/// between random points anywhere in the pack.
pub fn write_pairs(r: &Routing, region: &str, n: usize, seed: u64, out: &Path) -> Result<()> {
    let mut rng = Rng::new(seed + 2);
    let mut text = String::from("# region\tid\tstops\tavoid\n");
    let per = n.div_ceil(3);
    for (class, lo, hi) in [("town", 1_000, 10_000), ("regional", 10_000, 100_000)] {
        let mut k = 0;
        while k < per {
            let p = random_point(r, &mut rng)?;
            let m = (lo + rng.below(hi - lo)) as f64;
            let rad = rng.below(360) as f64 * std::f64::consts::PI / 180.0;
            let near = r.segments_near(offset(p, m, rad), 500.0)?;
            if near.is_empty() {
                continue;
            }
            let s = r.segment(near[rng.below(near.len() as u64) as usize])?;
            if s.small || s.class == omp_core::route::FERRY_CLASS {
                continue;
            }
            let q = s.points[s.points.len() / 2];
            k += 1;
            writeln!(text, "{region}\t{class}-{k:04}\t{}\t-", stops_text(&[p, q]))?;
        }
    }
    for k in 1..=n - 2 * per {
        let (p, q) = (random_point(r, &mut rng)?, random_point(r, &mut rng)?);
        writeln!(text, "{region}\tany-{k:04}\t{}\t-", stops_text(&[p, q]))?;
    }
    std::fs::write(out, text)?;
    println!("wrote {n} pairs to {}", out.display());
    Ok(())
}

/// A route as compared: its time, length, steps, U-turns and line.
#[derive(Clone)]
struct Route {
    time_s: f64,
    metres: f64,
    steps: usize,
    uturns: usize,
    line: Vec<P>,
}

/// One answer from OSRM, saved by `tools/osrm/fetch.py`.
struct Answer {
    id: String,
    stops: Vec<P>,
    avoid: u8,
    /// `None` when OSRM found no route.
    route: Option<Route>,
}

fn read_answers(file: &Path) -> Result<Vec<Answer>> {
    let text = std::fs::read_to_string(file).with_context(|| format!("read {}", file.display()))?;
    let mut out = Vec::new();
    for line in text.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 7 {
            bail!("{}: {} columns, want 7", file.display(), f.len());
        }
        let avoid = match f[2] {
            "-" => 0,
            a => parse_avoid(&a.split(',').map(String::from).collect::<Vec<_>>())?,
        };
        let route = match f[3] {
            "-" => None,
            t => Some(Route {
                time_s: t.parse()?,
                metres: f[4].parse()?,
                steps: f[5].parse()?,
                // Answers saved before U-turns were counted have none.
                uturns: f.get(7).map_or(Ok(0), |u| u.parse())?,
                line: polyline6(f[6])?,
            }),
        };
        out.push(Answer {
            id: f[0].to_owned(),
            stops: parse_stops(f[1])?,
            avoid,
            route,
        });
    }
    Ok(out)
}

/// Google's encoded polyline at 10⁻⁶ degrees.
fn polyline6(s: &str) -> Result<Vec<P>> {
    let b = s.as_bytes();
    let (mut i, mut lat, mut lon) = (0, 0i64, 0i64);
    let mut out = Vec::new();
    let next = |i: &mut usize| -> Result<i64> {
        let (mut v, mut shift) = (0i64, 0);
        loop {
            let c = i64::from(*b.get(*i).context("bad polyline")?) - 63;
            *i += 1;
            v |= (c & 0x1f) << shift;
            shift += 5;
            if c < 0x20 {
                break;
            }
        }
        Ok(if v & 1 != 0 { !(v >> 1) } else { v >> 1 })
    };
    while i < b.len() {
        lat += next(&mut i)?;
        lon += next(&mut i)?;
        out.push(geo::project(lat as f64 * 1e-6, lon as f64 * 1e-6));
    }
    Ok(out)
}

/// Points along `line` about every `step` world units.
fn densify(line: &[P], step: f64) -> Vec<[f64; 2]> {
    let mut out = Vec::new();
    for w in line.windows(2) {
        let (a, b) = (w[0].map(f64::from), w[1].map(f64::from));
        let n = (geo::dist(w[0], w[1]) / step).ceil().max(1.0) as usize;
        for k in 0..n {
            let t = k as f64 / n as f64;
            out.push([a[0] + t * (b[0] - a[0]), a[1] + t * (b[1] - a[1])]);
        }
    }
    if let Some(&l) = line.last() {
        out.push(l.map(f64::from));
    }
    out
}

/// The share of `a`'s length within [`OVERLAP_M`] of `b`.
fn overlap(a: &[P], b: &[P]) -> f64 {
    let (Some(&p), false) = (a.first(), b.is_empty()) else {
        return 0.0;
    };
    let unit = OVERLAP_M / geo::metres_per_unit(p);
    let key = |q: [f64; 2]| ((q[0] / unit).floor() as i64, (q[1] / unit).floor() as i64);
    let mut cells: HashMap<(i64, i64), Vec<[f64; 2]>> = HashMap::new();
    for q in densify(b, unit / 2.0) {
        cells.entry(key(q)).or_default().push(q);
    }
    let pts = densify(a, unit / 2.0);
    let near = |q: [f64; 2]| {
        let (x, y) = key(q);
        (x - 1..=x + 1).any(|i| {
            (y - 1..=y + 1).any(|j| {
                cells
                    .get(&(i, j))
                    .is_some_and(|v| v.iter().any(|o| (o[0] - q[0]).hypot(o[1] - q[1]) <= unit))
            })
        })
    };
    pts.iter().filter(|&&q| near(q)).count() as f64 / pts.len().max(1) as f64
}

pub(super) fn quantile(v: &[f64], q: f64) -> f64 {
    let mut v = v.to_vec();
    v.sort_by(f64::total_cmp);
    v.get(((v.len() as f64 - 1.0) * q).round() as usize)
        .copied()
        .unwrap_or(f64::NAN)
}

/// Trip length classes for the turn-cost write-up.
fn class_of(km: f64) -> &'static str {
    match km {
        k if k < 10.0 => "in town (<10 km)",
        k if k < 100.0 => "regional (10-100 km)",
        _ => "highway (>100 km)",
    }
}

/// One route on both sides.
struct Pair {
    id: String,
    km: f64,
    a: Route,
    b: Route,
}

fn report(title: &str, pairs: &[Pair], worst: usize) {
    println!("  {title}: {} pairs routed by both", pairs.len());
    let mut classes: Vec<(&str, Vec<&Pair>)> = vec![("all", pairs.iter().collect())];
    for c in [
        "in town (<10 km)",
        "regional (10-100 km)",
        "highway (>100 km)",
    ] {
        classes.push((c, pairs.iter().filter(|p| class_of(p.km) == c).collect()));
    }
    println!(
        "    {:<22} {:>5} {:>7} {:>7} {:>7} {:>8} {:>8} {:>8} {:>8} {:>13}",
        "",
        "pairs",
        "time p50",
        "p10",
        "p90",
        "within15",
        "dist p50",
        "overlap",
        "steps",
        "U-turns a/b"
    );
    for (name, v) in &classes {
        if v.is_empty() {
            continue;
        }
        let t: Vec<f64> = v.iter().map(|p| p.a.time_s / p.b.time_s.max(1.0)).collect();
        let d: Vec<f64> = v.iter().map(|p| p.a.metres / p.b.metres.max(1.0)).collect();
        let o: Vec<f64> = v.iter().map(|p| overlap(&p.a.line, &p.b.line)).collect();
        let s: Vec<f64> = v
            .iter()
            .map(|p| p.a.steps as f64 / (p.b.steps as f64).max(1.0))
            .collect();
        let within = t.iter().filter(|&&x| (0.85..=1.15).contains(&x)).count();
        // Routes with a U-turn, on each side.
        let pct = |n: usize| 100.0 * n as f64 / v.len() as f64;
        let ua = pct(v.iter().filter(|p| p.a.uturns > 0).count());
        let ub = pct(v.iter().filter(|p| p.b.uturns > 0).count());
        println!(
            "    {:<22} {:>5} {:>7.3} {:>7.3} {:>7.3} {:>7.1}% {:>8.3} {:>7.1}% {:>8.2} {:>5.1}%/{:.1}%",
            name,
            v.len(),
            quantile(&t, 0.5),
            quantile(&t, 0.1),
            quantile(&t, 0.9),
            100.0 * within as f64 / v.len() as f64,
            quantile(&d, 0.5),
            100.0 * quantile(&o, 0.5),
            quantile(&s, 0.5),
            ua,
            ub,
        );
        let same = o.iter().filter(|&&x| x >= 0.9).count();
        println!(
            "    {:<22} {:>5} same roads (90% overlap) {:.1}%; mean time ratio {:.3}",
            "",
            "",
            100.0 * same as f64 / v.len() as f64,
            t.iter().sum::<f64>() / t.len() as f64
        );
    }
    if worst > 0 {
        let mut v: Vec<&Pair> = pairs.iter().collect();
        v.sort_by(|x, y| {
            let r = |p: &Pair| (p.a.time_s / p.b.time_s.max(1.0)).ln().abs();
            r(y).total_cmp(&r(x))
        });
        println!("    largest time differences:");
        for p in v.iter().take(worst) {
            println!(
                "      {:<14} {:>7.1} km  {} vs {} ({:.2}); overlap {:.0}%, steps {} vs {}",
                p.id,
                p.km,
                duration(p.a.time_s),
                duration(p.b.time_s),
                p.a.time_s / p.b.time_s.max(1.0),
                100.0 * overlap(&p.a.line, &p.b.line),
                p.a.steps,
                p.b.steps
            );
        }
    }
}

/// Compares our routes with OSRM's saved answers, and the answer files with each other.
pub fn compare(
    r: &Routing,
    names: Option<&dyn Names>,
    files: &[std::path::PathBuf],
    worst: usize,
) -> Result<()> {
    let sets: Vec<Vec<Answer>> = files
        .iter()
        .map(|f| read_answers(f))
        .collect::<Result<_>>()?;
    // Ours, once per pair.
    let mut ours: HashMap<String, Route> = HashMap::new();
    let mut unrouted = Vec::new();
    for a in &sets[0] {
        let stops: Vec<Stop> = a.stops.iter().map(|&p| Stop::at(p)).collect();
        let trip = r.trip(&stops, a.avoid)?;
        if !trip.complete() {
            unrouted.push(a.id.clone());
            continue;
        }
        let steps = r.directions(&trip, &stops, names)?;
        // Steps as OSRM counts them: a depart and an arrive per leg.
        let n = steps
            .iter()
            .filter(|s| !matches!(s.kind, Kind::Stop))
            .count()
            + trip.legs.len().saturating_sub(1) * 2;
        let line: Vec<P> = trip
            .legs
            .iter()
            .flatten()
            .flat_map(|l| l.points.iter().copied())
            .collect();
        let uturns = steps.iter().filter(|s| s.kind == Kind::UTurn).count();
        ours.insert(
            a.id.clone(),
            Route {
                time_s: trip.time_s(),
                metres: trip.metres(),
                steps: n,
                uturns,
                line,
            },
        );
    }
    for (f, set) in files.iter().zip(&sets) {
        let none: Vec<&str> = set
            .iter()
            .filter(|a| a.route.is_none())
            .map(|a| a.id.as_str())
            .collect();
        let pairs: Vec<Pair> = set
            .iter()
            .filter_map(|a| {
                let o = ours.get(&a.id)?;
                let b = a.route.clone()?;
                Some(Pair {
                    id: a.id.clone(),
                    km: b.metres / 1000.0,
                    a: o.clone(),
                    b,
                })
            })
            .collect();
        println!(
            "ours against {} ({} answers; OSRM found no route for {}, we for {})",
            f.display(),
            set.len(),
            none.len(),
            unrouted.len()
        );
        report("ours / OSRM", &pairs, worst);
    }
    if sets.len() == 2 {
        let other: HashMap<&str, &Answer> = sets[1].iter().map(|a| (a.id.as_str(), a)).collect();
        let pairs: Vec<Pair> = sets[0]
            .iter()
            .filter_map(|a| {
                let b = other.get(a.id.as_str())?.route.clone()?;
                let a2 = a.route.clone()?;
                Some(Pair {
                    id: a.id.clone(),
                    km: a2.metres / 1000.0,
                    a: a2,
                    b,
                })
            })
            .collect();
        println!("{} against {}", files[0].display(), files[1].display());
        report("first / second", &pairs, worst);
    }
    Ok(())
}

/// The point halfway along `pts`.
fn middle(pts: &[P]) -> P {
    let half = pts.windows(2).map(|w| geo::dist(w[0], w[1])).sum::<f64>() / 2.0;
    let mut run = 0.0;
    for w in pts.windows(2) {
        let l = geo::dist(w[0], w[1]);
        if run + l >= half && l > 0.0 {
            let t = (half - run) / l;
            let lerp = |a: u32, b: u32| (f64::from(a) + t * (f64::from(b) - f64::from(a))) as u32;
            return [lerp(w[0][0], w[1][0]), lerp(w[0][1], w[1][1])];
        }
        run += l;
    }
    pts[0]
}

/// Turn restrictions and roundabouts within `radius_m` of `p`, with points to route
/// between, for writing lines of the set.
pub fn near(r: &Routing, names: Option<&dyn Names>, p: P, radius_m: f64) -> Result<()> {
    let road = |seg: u32| -> Result<String> {
        let s = r.segment(seg)?;
        let road = match (s.street, names) {
            (Some(id), Some(n)) => n.street_road(id)?,
            _ => Road::default(),
        };
        Ok(match road.text() {
            t if t.is_empty() => "(unnamed)".into(),
            t => t,
        })
    };
    let segs = r.segments_near(p, radius_m)?;
    let mut ring_seen: HashSet<(u32, u32)> = HashSet::new();
    for &s in &segs {
        let seg = r.segment(s)?;
        let ways = [
            (false, seg.b, Some(seg.forward)),
            (true, seg.a, seg.backward),
        ];
        for (rev, end, d) in ways {
            let Some(d) = d.filter(|d| d.head != end) else {
                continue;
            };
            // An approach with its own copy: the ways out the copy has not.
            let Some(copy) = r.copies(end)?.into_iter().find(|c| c.node == d.head) else {
                continue;
            };
            let at = match rev {
                false => *seg.points.last().unwrap(),
                true => seg.points[0],
            };
            let (x, y) = cell_xy(at);
            for o in r.cell(x, y)?.segments() {
                let other = r.segment(o)?;
                for (orev, from, od) in [
                    (false, other.a, Some(other.forward)),
                    (true, other.b, other.backward),
                ] {
                    let dir = o << 1 | u32::from(orev);
                    if from != end || od.is_none() || o == s || copy.leaves.contains(&dir) {
                        continue;
                    }
                    println!(
                        "  no turn  {} -> {} at {}: from {} to {}",
                        road(s)?,
                        road(o)?,
                        latlon(at),
                        latlon(middle(&seg.points)),
                        latlon(middle(&other.points)),
                    );
                }
            }
        }
        if seg.roundabout {
            let (x, y) = cell_xy(seg.points[0]);
            if !ring_seen.insert((x >> 2, y >> 2)) {
                continue;
            }
            // The ring's nodes nearby, and the roads that meet them.
            let close = r.segments_near(seg.points[0], 150.0)?;
            let mut ring = HashSet::new();
            for &o in &close {
                let other = r.segment(o)?;
                if other.roundabout {
                    ring.extend([other.a, other.b]);
                }
            }
            let mut arms = Vec::new();
            for &o in &close {
                let other = r.segment(o)?;
                if !other.roundabout && (ring.contains(&other.a) || ring.contains(&other.b)) {
                    arms.push(format!("{} ({})", road(o)?, latlon(middle(&other.points))));
                }
            }
            arms.sort();
            arms.dedup();
            println!(
                "  roundabout at {}: {}",
                latlon(seg.points[0]),
                arms.join(", ")
            );
        }
    }
    Ok(())
}
