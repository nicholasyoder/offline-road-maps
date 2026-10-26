//! `search` and `here` (phase 2 step 2.6): thin wrappers over `omp-core`'s search engine,
//! so it can be tried and checked without the app. `search --check` runs a query set from
//! `crates/core/tests/search/`; `search --sample` draws random addresses for one.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use clap::Args;
use omp_core::find::{Hit, HitKind, Request, Session, number_text};
use omp_core::geo::{self, P};
use omp_core::normalize::{loose_key, street_key};
use omp_core::pack::Pack;
use omp_core::search::{Kind, Search};
use serde_json::json;

use crate::build::first_chunk;

#[derive(Args)]
pub struct SearchArgs {
    /// Pack directory or its `.omp.000` file.
    pack: PathBuf,

    /// What to search for.
    query: Option<String>,

    /// The map view's centre, `lat,lon`.
    #[arg(long)]
    near: Option<String>,

    /// The map view's zoom.
    #[arg(long, default_value_t = 5.0)]
    zoom: f64,

    #[arg(long, default_value_t = 10)]
    limit: usize,

    /// Treat the last word as still being typed (with `--check`, in every query).
    #[arg(long)]
    prefix: bool,

    /// Print the hits as JSON.
    #[arg(long)]
    json: bool,

    /// Run query sets (`.tsv`, see `crates/core/tests/search/README.md`) and report their
    /// pass rates and timings.
    #[arg(long, num_args = 1..)]
    check: Vec<PathBuf>,

    /// With `--check`, the region whose lines to run [default: the pack's name, lowercase].
    #[arg(long)]
    region: Option<String>,

    /// With `--check`, print every failure.
    #[arg(long)]
    verbose: bool,

    /// With `--check`, run every search level of each address query (phase 2 step 2.7b)
    /// and report what each finds and how long it takes; with `--fresh`, also each level
    /// alone in a new session.
    #[arg(long)]
    levels: bool,

    #[arg(long)]
    fresh: bool,

    /// With `--check`, search each query down to this level, as the app does after a
    /// pause [default: 0, as each keystroke].
    #[arg(long, default_value_t = 0)]
    level: usize,

    /// With `--check`, type the address queries another way, as the last part being
    /// typed: `state` (the state's name in full), `state:N` (its first N letters),
    /// `nostate`, or `town:N` (the town's first N letters, no state).
    #[arg(long)]
    typed: Option<String>,

    /// Draw this many random addresses from the pack and write them as an address query
    /// set to `--out`.
    #[arg(long)]
    sample: Option<usize>,

    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(Args)]
pub struct HereArgs {
    /// Pack directory or its `.omp.000` file.
    pack: PathBuf,

    /// The point, `lat,lon`.
    point: String,

    /// How far to look for an address and a street, in metres.
    #[arg(long, default_value_t = 100.0)]
    radius: f64,

    /// How far to look for places, POIs and labels, in metres.
    #[arg(long, default_value_t = 100.0)]
    entries: f64,

    /// Print the answer as JSON.
    #[arg(long)]
    json: bool,
}

fn open(pack: &Path) -> Result<(Arc<Pack>, Search)> {
    let first = first_chunk(pack)?;
    let pack = Arc::new(Pack::open(&first).with_context(|| format!("open {}", first.display()))?);
    let Some(s) = Search::open(pack.clone())? else {
        bail!("the pack has no search sections (format 0.{})", pack.minor);
    };
    Ok((pack, s))
}

fn parse_point(s: &str) -> Result<P> {
    let (lat, lon) = s
        .split_once(',')
        .with_context(|| format!("expected lat,lon, not {s:?}"))?;
    Ok(geo::project(lat.trim().parse()?, lon.trim().parse()?))
}

fn kind_name(k: HitKind) -> &'static str {
    match k {
        HitKind::Address => "address",
        HitKind::Entry(Kind::Admin) => "admin",
        HitKind::Entry(Kind::Place) => "place",
        HitKind::Entry(Kind::Street) => "street",
        HitKind::Entry(Kind::Poi) => "poi",
        HitKind::Entry(Kind::Postcode) => "postcode",
        HitKind::Entry(Kind::Feature) => "feature",
        HitKind::Entry(Kind::AddressStreet) => "addrstreet",
    }
}

fn hit_json(h: &Hit) -> serde_json::Value {
    let (lat, lon) = geo::unproject(h.p);
    json!({
        "kind": kind_name(h.kind),
        "class": h.class,
        "name": h.name,
        "context": h.context,
        "lat": lat,
        "lon": lon,
        "radius_m": h.radius_m,
        "approx": h.approx,
        "number_missing": h.number_missing,
        "postcode": h.postcode,
        "units": h.units,
        "entry": h.entry,
        "street": h.street,
        "score": h.score,
    })
}

fn hit_line(h: &Hit) -> String {
    let (lat, lon) = geo::unproject(h.p);
    let mut s = format!("{:<10} {}", kind_name(h.kind), h.name);
    if !h.context.is_empty() {
        write!(s, ", {}", h.context).unwrap();
    }
    if let Some(c) = &h.postcode {
        write!(s, " {c}").unwrap();
    }
    write!(s, "  {lat:.5},{lon:.5}  score {:.1}", h.score).unwrap();
    if h.approx {
        s.push_str("  approximate");
    }
    if h.number_missing {
        s.push_str("  number not found");
    }
    if !h.units.is_empty() {
        write!(s, "  units: {}", h.units.join(", ")).unwrap();
    }
    s
}

pub fn search(args: SearchArgs) -> Result<()> {
    let (pack, s) = open(&args.pack)?;
    if let Some(n) = args.sample {
        let out = args.out.context("--sample needs --out")?;
        return sample(&s, n, &out);
    }
    if !args.check.is_empty() {
        let region = args
            .region
            .clone()
            .unwrap_or_else(|| pack.info.name.to_lowercase());
        if args.levels || args.typed.is_some() {
            return check_levels(
                &s,
                &args.check,
                args.typed.as_deref(),
                args.fresh,
                args.verbose,
            );
        }
        return check(
            &s,
            &args.check,
            &region,
            args.prefix,
            args.level,
            args.verbose,
        );
    }
    let Some(text) = &args.query else {
        bail!("give a query, --check or --sample");
    };
    let req = Request {
        text,
        near: args.near.as_deref().map(parse_point).transpose()?,
        zoom: args.zoom,
        limit: args.limit,
        prefix: args.prefix,
    };
    let t = Instant::now();
    let hits = s.find(&req)?;
    let ms = t.elapsed().as_secs_f64() * 1e3;
    if args.json {
        let v: Vec<_> = hits.iter().map(hit_json).collect();
        println!("{}", serde_json::to_string_pretty(&v)?);
    } else {
        for (i, h) in hits.iter().enumerate() {
            println!("{:>2}. {}", i + 1, hit_line(h));
        }
        if hits.is_empty() {
            println!("no results");
        }
        println!("{ms:.1} ms");
    }
    Ok(())
}

pub fn here(args: HereArgs) -> Result<()> {
    let (_, s) = open(&args.pack)?;
    let p = parse_point(&args.point)?;
    let t = Instant::now();
    let h = s.here(p, args.radius, args.entries)?;
    let ms = t.elapsed().as_secs_f64() * 1e3;
    let areas = s.admin.areas();
    let chain: Vec<String> = h
        .chain
        .iter()
        .rev()
        .map(|&a| areas[a as usize].name.clone())
        .collect();
    let (lat, lon) = geo::unproject(p);
    if args.json {
        let v = json!({
            "lat": lat,
            "lon": lon,
            "address": h.address.as_ref().map(|a| json!({
                "label": a.label,
                "dist_m": a.dist_m,
                "approx": a.approx,
                "postcode": a.postcode,
                "units": a.units,
            })),
            "street": h.street.as_ref().map(|(id, st, spot)| json!({
                "id": id,
                "name": st.name,
                "town": st.town,
                "dist_m": spot.dist_m,
            })),
            "nearby": h.nearby.iter().map(|(id, e, d)| json!({
                "entry": id,
                "name": e.name,
                "dist_m": d,
            })).collect::<Vec<_>>(),
            "chain": chain,
        });
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    println!("point    {lat:.6},{lon:.6}");
    match &h.address {
        Some(a) => println!(
            "address  {}{}{}, {:.0} m{}",
            a.label,
            a.postcode
                .as_ref()
                .map_or(String::new(), |c| format!(" {c}")),
            if a.units.is_empty() {
                String::new()
            } else {
                format!(" (units: {})", a.units.join(", "))
            },
            a.dist_m,
            if a.approx { ", approximate" } else { "" }
        ),
        None => println!("address  none within {} m", args.radius),
    }
    match &h.street {
        Some((_, st, spot)) => println!("street   {}, {} ({:.0} m)", st.name, st.town, spot.dist_m),
        None => println!("street   none within {} m", args.radius),
    }
    for (_, e, d) in &h.nearby {
        println!("nearby   {} ({:?}, {d:.0} m)", e.name, e.kind);
    }
    println!("where    {}", chain.join(" > "));
    println!("{ms:.1} ms");
    Ok(())
}

/// A small deterministic generator (xorshift64*), so a sample can be drawn again.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    /// `k` distinct sorted indices below `n`.
    fn pick(&mut self, n: u64, k: usize) -> Vec<u64> {
        let mut v: Vec<u64> = Vec::new();
        while v.len() < k.min(n as usize) {
            let x = self.next() % n;
            if !v.contains(&x) {
                v.push(x);
            }
        }
        v.sort_unstable();
        v
    }
}

/// The admin chain's town (an admin area of level 7–10) and state code at `p`.
fn town_state(s: &Search, p: P) -> Result<(String, String)> {
    let chain = s.admin.chain(p)?;
    let areas = s.admin.areas();
    let mut town = String::new();
    let mut state = String::new();
    for &a in &chain {
        let area = &areas[a as usize];
        if area.level == 4 {
            state = area.abbr.rsplit('-').next().unwrap_or("").to_owned();
            if state.is_empty() {
                state = area.name.clone();
            }
        }
        if (7..=10).contains(&area.level) && (town.is_empty() || area.town) {
            town = area.name.clone();
        }
    }
    Ok((town, state))
}

/// Writes `n` random addresses of the pack as `query lat lon approx` lines: "number
/// street, town, state", with the street's town (or, for an unattached address, the town of
/// the street entry of its name nearby, else its admin town) and the state's code.
fn sample(s: &Search, n: usize, out: &Path) -> Result<()> {
    let Some(a) = &s.addresses else {
        bail!("the pack has no addresses");
    };
    let [na, nl, _, _] = a.counts();
    let total = u64::from(na) + u64::from(nl);
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15 ^ total);
    let picks = rng.pick(total, n);
    let (attached, loose): (Vec<u64>, Vec<u64>) = picks.iter().partition(|&&i| i < u64::from(na));
    let mut lines: Vec<(u64, String)> = Vec::new();
    let mut i = 0u64;
    let mut k = 0usize;
    let mut found = Vec::new();
    a.for_each(|x| {
        if k < attached.len() && attached[k] == i {
            found.push(x.clone());
            k += 1;
        }
        i += 1;
    })?;
    for x in found {
        let p = s.address_point(&x)?;
        let st = s.streets.street(x.street)?;
        let (admin_town, state) = town_state(s, p)?;
        let town = if st.town.is_empty() {
            admin_town
        } else {
            st.town.clone()
        };
        lines.push((
            attached[lines.len()],
            line(
                &number_text(x.number, &x.suffix),
                &st.name,
                &town,
                &state,
                p,
                x.approx,
            ),
        ));
    }
    let mut i = 0u64;
    let mut k = 0usize;
    let mut found = Vec::new();
    a.for_each_loose(|l| {
        if k < loose.len() && loose[k] - u64::from(na) == i {
            found.push(l.clone());
            k += 1;
        }
        i += 1;
    })?;
    for (j, l) in found.into_iter().enumerate() {
        let key = loose_key(&street_key(&l.street));
        let (admin_town, state) = town_state(s, l.p)?;
        let mut town = admin_town;
        for (_, e) in s.index.entries_in(&[geo::bbox_around(l.p, 1_500.0)])? {
            if matches!(e.kind, Kind::Street | Kind::AddressStreet)
                && loose_key(&street_key(&e.name)) == key
                && let Some(t) = e
                    .town
                    .and_then(|t| s.index.town(t))
                    .filter(|t| !t.is_empty())
            {
                town = t.to_owned();
                break;
            }
        }
        lines.push((
            loose[j],
            line(
                &number_text(l.number, &l.suffix),
                &l.street,
                &town,
                &state,
                l.p,
                l.approx,
            ),
        ));
    }
    lines.sort();
    let mut text = String::from("# query\tlat\tlon\tapprox\n");
    for (_, l) in lines {
        text.push_str(&l);
    }
    std::fs::write(out, text).with_context(|| format!("write {}", out.display()))?;
    println!("wrote {n} addresses to {}", out.display());
    Ok(())
}

fn line(number: &str, street: &str, town: &str, state: &str, p: P, approx: bool) -> String {
    let (lat, lon) = geo::unproject(p);
    let mut q = format!("{number} {street}");
    for part in [town, state] {
        if !part.is_empty() {
            q.push_str(", ");
            q.push_str(part);
        }
    }
    format!("{q}\t{lat:.6}\t{lon:.6}\t{}\n", u8::from(approx))
}

/// One line of a hand-written query set.
struct Case {
    category: String,
    query: String,
    near: Option<P>,
    zoom: f64,
    /// Kinds accepted, or empty for "no results".
    kinds: Vec<String>,
    name: Option<String>,
    at: Option<(P, f64)>,
}

/// Where a case's expected hit is: 1 for first, up to 5, or not.
fn rank_of(c: &Case, hits: &[Hit]) -> Option<usize> {
    hits.iter().take(5).position(|h| {
        c.kinds.iter().any(|k| k == "any" || k == kind_name(h.kind))
            && c.name
                .as_ref()
                .is_none_or(|n| h.name.to_lowercase().contains(&n.to_lowercase()))
            && c.at
                .is_none_or(|(p, m)| geo::dist(p, h.p) * geo::metres_per_unit(p) <= m)
    })
}

fn quantile(v: &mut [f64], q: f64) -> f64 {
    v.sort_by(f64::total_cmp);
    v.get(((v.len() as f64 - 1.0) * q).round() as usize)
        .copied()
        .unwrap_or(0.0)
}

#[derive(Default)]
struct Tally {
    n: usize,
    first: usize,
    top5: usize,
}

/// Runs query sets: address files (`query lat lon approx`) and hand-written ones
/// (`region category query near zoom kinds name lat,lon within_m`).
fn check(
    s: &Search,
    files: &[PathBuf],
    region: &str,
    prefix: bool,
    level: usize,
    verbose: bool,
) -> Result<()> {
    let find = |req: &Request| -> Result<Vec<Hit>> {
        let mut session = s.start(req);
        let mut hits = Vec::new();
        for d in 0..=level.min(Session::LEVELS - 1) {
            hits = session.level(d)?.unwrap_or_default();
        }
        Ok(hits)
    };
    let mut times = Vec::new();
    let mut slowest: Vec<(f64, String)> = Vec::new();
    for file in files {
        let text =
            std::fs::read_to_string(file).with_context(|| format!("read {}", file.display()))?;
        let rows: Vec<Vec<&str>> = text
            .lines()
            .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
            .map(|l| l.split('\t').collect())
            .collect();
        let addresses = rows.first().is_some_and(|r| r.len() == 4);
        println!("{}", file.display());
        if addresses {
            let (mut pass, mut same, mut here_ok, mut n) = (0, 0, 0, 0);
            let mut here_times = Vec::new();
            let mut fails: Vec<String> = Vec::new();
            for r in &rows {
                let want = geo::project(r[1].parse()?, r[2].parse()?);
                let req = Request {
                    prefix,
                    ..Request::new(r[0])
                };
                let t = Instant::now();
                let hits = find(&req)?;
                times.push(t.elapsed().as_secs_f64() * 1e3);
                slowest.push((*times.last().unwrap(), r[0].to_owned()));
                n += 1;
                let ok = hits.first().is_some_and(|h| {
                    let d = geo::dist(want, h.p) * geo::metres_per_unit(want);
                    h.kind == HitKind::Address && (d <= 10.0 || h.approx && d <= 100.0)
                });
                // The same address at another of its points: the store keeps two points
                // of one address when its sources put them over 250 m apart.
                let label = |t: &str| loose_key(&street_key(t.split(',').next().unwrap_or("")));
                let dup = !ok
                    && hits.first().is_some_and(|h| {
                        let d = geo::dist(want, h.p) * geo::metres_per_unit(want);
                        h.kind == HitKind::Address && label(&h.name) == label(r[0]) && d <= 3_000.0
                    });
                same += usize::from(ok || dup);
                // What's here at the point: that address, or one within 30 m on the same
                // street (phase 2 exit criterion 2).
                let t = Instant::now();
                let here = s.here(want, 100.0, 0.0)?;
                here_times.push(t.elapsed().as_secs_f64() * 1e3);
                let street_of = |t: &str| {
                    let first = t.split(',').next().unwrap_or("");
                    let rest = first.split_once(' ').map_or("", |x| x.1);
                    loose_key(&street_key(rest))
                };
                here_ok += usize::from(here.address.as_ref().is_some_and(|a| {
                    label(&a.label) == label(r[0])
                        || a.dist_m <= 30.0 && street_of(&a.label) == street_of(r[0])
                }));
                if ok {
                    pass += 1;
                } else {
                    let got = hits.first().map_or("nothing".into(), |h| {
                        let d = geo::dist(want, h.p) * geo::metres_per_unit(want);
                        format!("{} ({d:.0} m)", hit_line(h))
                    });
                    let tag = if dup { "DUP " } else { "FAIL" };
                    fails.push(format!("  {tag} {}\n       got {got}", r[0]));
                }
            }
            println!(
                "  addresses: {pass}/{n} first at its point ({:.1}%), {same} the same address \
                 ({:.1}%)",
                100.0 * pass as f64 / n.max(1) as f64,
                100.0 * same as f64 / n.max(1) as f64
            );
            println!(
                "  what's here: {here_ok}/{n} ({:.1}%), p50 {:.1} ms, p95 {:.1} ms",
                100.0 * here_ok as f64 / n.max(1) as f64,
                quantile(&mut here_times, 0.5),
                quantile(&mut here_times, 0.95)
            );
            if verbose {
                for f in &fails {
                    println!("{f}");
                }
            }
            continue;
        }
        let mut cases = Vec::new();
        for r in &rows {
            if r.len() < 6 {
                bail!("{}: short line {r:?}", file.display());
            }
            if r[0] != region {
                continue;
            }
            let at = match (r.get(7), r.get(8)) {
                (Some(p), Some(m)) if *p != "-" => Some((parse_point(p)?, m.parse()?)),
                _ => None,
            };
            cases.push(Case {
                category: r[1].to_owned(),
                query: r[2].to_owned(),
                near: (r[3] != "-").then(|| parse_point(r[3])).transpose()?,
                zoom: r[4].parse()?,
                kinds: if r[5] == "none" {
                    Vec::new()
                } else {
                    r[5].split('|').map(str::to_owned).collect()
                },
                name: r.get(6).filter(|n| **n != "-").map(|n| (*n).to_owned()),
                at,
            });
        }
        let mut by_cat: Vec<(String, Tally)> = Vec::new();
        let mut all = Tally::default();
        for c in &cases {
            let req = Request {
                near: c.near,
                zoom: c.zoom,
                prefix,
                ..Request::new(&c.query)
            };
            let t = Instant::now();
            let hits = find(&req)?;
            times.push(t.elapsed().as_secs_f64() * 1e3);
            slowest.push((*times.last().unwrap(), c.query.clone()));
            let rank = if c.kinds.is_empty() {
                hits.is_empty().then_some(0)
            } else {
                rank_of(c, &hits)
            };
            let tally = match by_cat.iter_mut().find(|(k, _)| *k == c.category) {
                Some((_, t)) => t,
                None => {
                    by_cat.push((c.category.clone(), Tally::default()));
                    &mut by_cat.last_mut().unwrap().1
                }
            };
            for t in [&mut *tally, &mut all] {
                t.n += 1;
                t.first += usize::from(rank == Some(0));
                t.top5 += usize::from(rank.is_some());
            }
            if verbose && rank != Some(0) {
                println!(
                    "  {} [{}] {:?}: wanted {} {}",
                    if rank.is_some() { "TOP5" } else { "FAIL" },
                    c.category,
                    c.query,
                    if c.kinds.is_empty() {
                        "nothing".into()
                    } else {
                        c.kinds.join("|")
                    },
                    c.name.as_deref().unwrap_or("")
                );
                for (i, h) in hits.iter().take(5).enumerate() {
                    println!("       {}. {}", i + 1, hit_line(h));
                }
            }
        }
        for (cat, t) in &by_cat {
            println!(
                "  {cat:<12} {:>3}/{:<3} first, {:>3} top 5",
                t.first, t.n, t.top5
            );
        }
        println!(
            "  all          {}/{} first ({:.1}%), {} top 5 ({:.1}%)",
            all.first,
            all.n,
            100.0 * all.first as f64 / all.n.max(1) as f64,
            all.top5,
            100.0 * all.top5 as f64 / all.n.max(1) as f64
        );
    }
    let n = times.len();
    if verbose {
        slowest.sort_by(|a, b| b.0.total_cmp(&a.0));
        for (ms, q) in slowest.iter().take(8) {
            println!("  slow {ms:.1} ms  {q}");
        }
    }
    println!(
        "{n} queries: p50 {:.1} ms, p95 {:.1} ms, max {:.1} ms; readers hold {:.1} MiB",
        quantile(&mut times, 0.5),
        quantile(&mut times, 0.95),
        quantile(&mut times, 1.0),
        s.heap_bytes() as f64 / (1u64 << 20) as f64
    );
    Ok(())
}

/// An address query ("number street, town, state") typed another way (`--typed`).
fn retype(s: &Search, query: &str, typed: &str) -> Result<String> {
    let parts: Vec<&str> = query.split(", ").collect();
    let [street, town, state] = parts[..] else {
        return Ok(query.to_owned());
    };
    let full = || -> String {
        let areas = s.admin.areas();
        areas
            .iter()
            .take_while(|a| a.level <= 4)
            .find(|a| a.level == 4 && a.abbr.rsplit('-').next() == Some(state))
            .map_or(state.to_owned(), |a| a.name.clone())
    };
    let cut = |t: &str, n: usize| t.chars().take(n).collect::<String>();
    Ok(match typed.split_once(':') {
        None if typed == "state" => format!("{street}, {town}, {}", full()),
        None if typed == "nostate" => format!("{street}, {town}"),
        Some(("state", n)) => format!("{street}, {town}, {}", cut(&full(), n.parse()?)),
        Some(("town", n)) => format!("{street}, {}", cut(town, n.parse()?)),
        _ => bail!("--typed: expected state, state:N, nostate or town:N, not {typed:?}"),
    })
}

/// Runs address query sets level by level, typed (the last word a prefix), with the whole
/// continent in view: per level, how many are first at their point or among the 8
/// suggestions, and the level's time.
fn check_levels(
    s: &Search,
    files: &[PathBuf],
    typed: Option<&str>,
    fresh: bool,
    verbose: bool,
) -> Result<()> {
    let levels = Session::LEVELS;
    let mut first = vec![0usize; levels];
    let mut top = vec![0usize; levels];
    // First at level 0, not at this one.
    let mut lost = vec![0usize; levels];
    let mut times: Vec<Vec<f64>> = vec![Vec::new(); levels];
    let mut alone: Vec<Vec<f64>> = vec![Vec::new(); levels];
    let mut total: Vec<f64> = Vec::new();
    // From the first level past 0 to the one that put the address first, when one did.
    let mut deeper: Vec<f64> = Vec::new();
    let mut peak = 0usize;
    let mut n = 0usize;
    for file in files {
        let text =
            std::fs::read_to_string(file).with_context(|| format!("read {}", file.display()))?;
        for l in text
            .lines()
            .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        {
            let r: Vec<&str> = l.split('\t').collect();
            if r.len() != 4 {
                bail!("{}: not an address query set", file.display());
            }
            let want = geo::project(r[1].parse()?, r[2].parse()?);
            let query = match typed {
                Some(t) => retype(s, r[0], t)?,
                None => r[0].to_owned(),
            };
            let req = Request {
                prefix: true,
                limit: 8,
                ..Request::new(&query)
            };
            let at = |h: &Hit| {
                let d = geo::dist(want, h.p) * geo::metres_per_unit(want);
                h.kind == HitKind::Address && (d <= 10.0 || h.approx && d <= 100.0)
            };
            n += 1;
            let mut session = s.start(&req);
            let mut sum = 0.0;
            let mut found_at = None;
            let mut first0 = false;
            for d in 0..levels {
                let t = Instant::now();
                let hits = session.level(d)?.unwrap_or_default();
                let ms = t.elapsed().as_secs_f64() * 1e3;
                sum += ms;
                times[d].push(ms);
                if hits.first().is_some_and(at) {
                    first[d] += 1;
                    if found_at.is_none() && d > 0 {
                        deeper.push(sum - times[0].last().copied().unwrap_or(0.0));
                    }
                    found_at.get_or_insert(d);
                    first0 |= d == 0;
                } else if first0 {
                    lost[d] += 1;
                    if verbose {
                        let got = hits.first().map_or("nothing".into(), hit_line);
                        println!("  LOST at level {d}: {query}\n       got {got}");
                    }
                }
                top[d] += usize::from(hits.iter().any(at));
                if verbose && d + 1 == levels && !hits.first().is_some_and(at) {
                    let got = hits.first().map_or("nothing".into(), hit_line);
                    println!("  FAIL {query}\n       got {got}");
                }
            }
            total.push(sum);
            peak = peak.max(session.heap_bytes());
            if fresh {
                for (d, v) in alone.iter_mut().enumerate() {
                    let t = Instant::now();
                    s.start(&req).level(d)?;
                    v.push(t.elapsed().as_secs_f64() * 1e3);
                }
            }
            if verbose && let Some(d) = found_at.filter(|&d| d > 0) {
                println!("  level {d}: {query}");
            }
        }
    }
    let pct = |k: usize| 100.0 * k as f64 / n.max(1) as f64;
    println!(
        "{n} addresses{}",
        typed.map_or(String::new(), |t| format!(", typed {t}"))
    );
    for d in 0..levels {
        let mut line = format!(
            "  level {d}: first {:>5.1}%, in 8 {:>5.1}%, lost {}; ms p50 {:.1}, p95 {:.1}, max {:.1}",
            pct(first[d]),
            pct(top[d]),
            lost[d],
            quantile(&mut times[d], 0.5),
            quantile(&mut times[d], 0.95),
            quantile(&mut times[d], 1.0)
        );
        if fresh {
            write!(
                line,
                "; alone p50 {:.1}, p95 {:.1}, max {:.1}",
                quantile(&mut alone[d], 0.5),
                quantile(&mut alone[d], 0.95),
                quantile(&mut alone[d], 1.0)
            )?;
        }
        println!("{line}");
    }
    println!(
        "  found by a deeper level: {}, after the pause ms p50 {:.1}, p95 {:.1}, max {:.1}",
        deeper.len(),
        quantile(&mut deeper, 0.5),
        quantile(&mut deeper, 0.95),
        quantile(&mut deeper, 1.0)
    );
    println!(
        "  all levels: ms p50 {:.1}, p95 {:.1}, max {:.1}; session caches peak {:.1} MiB; \
         readers hold {:.1} MiB",
        quantile(&mut total, 0.5),
        quantile(&mut total, 0.95),
        quantile(&mut total, 1.0),
        peak as f64 / (1u64 << 20) as f64,
        s.heap_bytes() as f64 / (1u64 << 20) as f64
    );
    Ok(())
}
