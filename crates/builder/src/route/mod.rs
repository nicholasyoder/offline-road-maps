//! `route` (phase 3 step 3.4): a thin wrapper over `omp-core`'s routing, so routes can be
//! tried and checked without the app, as `search` and `here` are. It prints each stop's
//! snap and each leg's distance and time, and `--geojson` writes the line. `route --check`
//! ([`check`]) runs random pairs against Dijkstra over the same reader's base edges,
//! checks the paths against the turn restrictions, and times routes.

pub mod check;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use clap::Args;
use omp_core::find::Request;
use omp_core::geo::{self, P};
use omp_core::pack::Pack;
use omp_core::route::{AVOID, FAR_SNAP_M, FERRY, FERRY_CLASS, Leg, Routing, Snap, Stop, TOLL};
use omp_core::search::Search;
use omp_core::street::{CLASSES, Streets};
use serde_json::json;

use crate::build::first_chunk;
use crate::find::parse_point;

#[derive(Args)]
pub struct RouteArgs {
    /// Pack directory or its `.omp.000` file.
    pack: PathBuf,

    /// The stops, in order: `lat,lon`, or text to search for (its first result, which
    /// snaps to its own street).
    stops: Vec<String>,

    /// What to avoid: `tolls`, `ferries`, or both, comma-separated.
    #[arg(long, value_delimiter = ',')]
    avoid: Vec<String>,

    /// Write the stops, snaps and legs as GeoJSON.
    #[arg(long)]
    geojson: Option<PathBuf>,

    /// Print every piece of each leg: its segment, road and distance.
    #[arg(long)]
    pieces: bool,

    /// Check this many random pairs of nodes against Dijkstra (criterion 2), and time
    /// routes between random points.
    #[arg(long)]
    check: Option<usize>,

    /// With `--check`, the OSM extract the pack was built from: its graph is rebuilt to
    /// check every path against the turn restrictions.
    #[arg(long)]
    pbf: Option<PathBuf>,

    /// With `--check`, the pairs per start node: one Dijkstra serves them all.
    #[arg(long, default_value_t = 10)]
    targets: usize,

    /// With `--check`, routes between random points to time, short and long each.
    #[arg(long, default_value_t = 1000)]
    trips: usize,

    #[arg(long, default_value_t = 7)]
    seed: u64,

    /// MiB of decoded hierarchy blocks to keep [default: the reader's].
    #[arg(long)]
    node_cache: Option<usize>,

    /// MiB of decoded segment blocks to keep [default: the reader's].
    #[arg(long)]
    segment_cache: Option<usize>,
}

/// Opens the pack's routing reader, with the cache sizes asked for.
fn open(args: &RouteArgs) -> Result<(PathBuf, Arc<Pack>, Routing)> {
    let pack = &args.pack;
    let first = first_chunk(pack)?;
    let pack = Arc::new(Pack::open(&first).with_context(|| format!("open {}", first.display()))?);
    let Some(mut r) = Routing::open(pack.clone())? else {
        bail!("the pack has no routing sections (format 0.{})", pack.minor);
    };
    if let Some(m) = args.node_cache {
        r = r.with_node_cache(m << 20);
    }
    if let Some(m) = args.segment_cache {
        r = r.with_segment_cache(m << 20);
    }
    Ok((first, pack, r))
}

fn parse_avoid(list: &[String]) -> Result<u8> {
    let mut avoid = 0;
    for a in list {
        avoid |= match a.trim() {
            "tolls" | "toll" => TOLL,
            "ferries" | "ferry" => FERRY,
            "" => 0,
            x => bail!("can't avoid {x:?}: tolls or ferries"),
        };
    }
    Ok(avoid)
}

pub(crate) fn flags_text(f: u8) -> String {
    let mut v = Vec::new();
    if f & TOLL != 0 {
        v.push("tolls");
    }
    if f & FERRY != 0 {
        v.push("ferries");
    }
    match v.is_empty() {
        true => "none".into(),
        false => v.join(", "),
    }
}

pub(crate) fn duration(s: f64) -> String {
    let m = (s / 60.0).round() as u64;
    match m {
        0 => format!("{s:.0} s"),
        m if m < 60 => format!("{m} min"),
        m => format!("{} h {:02} min", m / 60, m % 60),
    }
}

/// A segment's road, as the directions will name it: its street's name and refs, else its
/// label's, else its class.
fn road(r: &Routing, streets: Option<&Streets>, segment: u32) -> Result<String> {
    let s = r.segment(segment)?;
    let mut parts = Vec::new();
    if let (Some(id), Some(st)) = (s.street, streets) {
        let st = st.street(id)?;
        parts.extend(st.refs.iter().cloned());
        if !st.name.is_empty() {
            parts.push(st.name);
        }
    }
    if parts.is_empty()
        && let Some(l) = s.label
    {
        let l = r.label(l)?;
        parts.extend(
            l.refs
                .split(';')
                .filter(|x| !x.is_empty())
                .map(String::from),
        );
        if !l.name.is_empty() {
            parts.push(l.name);
        }
    }
    let class = match s.class {
        FERRY_CLASS => "ferry",
        c => CLASSES.get(c as usize).copied().unwrap_or("road"),
    };
    Ok(match parts.is_empty() {
        true => format!("unnamed {class}"),
        false => format!("{} ({class})", parts.join(" / ")),
    })
}

fn latlon(p: P) -> String {
    let (lat, lon) = geo::unproject(p);
    format!("{lat:.5},{lon:.5}")
}

fn line_json(pts: &[P]) -> serde_json::Value {
    let coords: Vec<_> = pts
        .iter()
        .map(|&p| {
            let (lat, lon) = geo::unproject(p);
            json!([lon, lat])
        })
        .collect();
    json!({"type": "LineString", "coordinates": coords})
}

fn point_json(p: P) -> serde_json::Value {
    let (lat, lon) = geo::unproject(p);
    json!({"type": "Point", "coordinates": [lon, lat]})
}

pub fn run(args: RouteArgs) -> Result<()> {
    let (first, pack, r) = open(&args)?;
    if let Some(n) = args.check {
        return check::run(&first, &r, n, &args);
    }
    if args.stops.len() < 2 {
        bail!("give at least two stops, or --check");
    }
    let avoid = parse_avoid(&args.avoid)?;
    let streets = Streets::open(pack.clone())?;
    // Stops given as text are searched for, as the app's search box would.
    let mut search: Option<Search> = None;
    let mut stops = Vec::new();
    for text in &args.stops {
        if let Ok(p) = parse_point(text) {
            stops.push((Stop::at(p), text.clone()));
            continue;
        }
        if search.is_none() {
            search = Some(Search::open(pack.clone())?.context("the pack has no search sections")?);
        }
        let hits = search.as_ref().unwrap().find(&Request {
            limit: 1,
            ..Request::new(text)
        })?;
        let h = hits
            .into_iter()
            .next()
            .with_context(|| format!("no search result for {text:?}"))?;
        let name = match h.context.is_empty() {
            true => h.name,
            false => format!("{}, {}", h.name, h.context),
        };
        stops.push((
            Stop {
                p: h.p,
                street: h.street,
            },
            name,
        ));
    }

    let t = Instant::now();
    let only: Vec<Stop> = stops.iter().map(|s| s.0).collect();
    let trip = r.trip(&only, avoid)?;
    let ms = t.elapsed().as_secs_f64() * 1e3;

    for (i, ((stop, name), snap)) in stops.iter().zip(&trip.snaps).enumerate() {
        match snap {
            Some(s) => println!(
                "stop {}  {name} ({}): snapped {:.0} m onto {}{}{}",
                i + 1,
                latlon(stop.p),
                s.dist_m,
                road(&r, streets.as_ref(), s.segment)?,
                if s.on_street { ", its own street" } else { "" },
                if s.dist_m > FAR_SNAP_M {
                    "  far from any road"
                } else {
                    ""
                }
            ),
            None => println!(
                "stop {}  {name} ({}): no road within reach",
                i + 1,
                latlon(stop.p)
            ),
        }
    }
    for (i, leg) in trip.legs.iter().enumerate() {
        match leg {
            Some(l) => print_leg(&r, streets.as_ref(), i, l, avoid, args.pieces)?,
            None => match (&trip.snaps[i], &trip.snaps[i + 1]) {
                (Some(_), Some(_)) => println!(
                    "leg {}   no route: the stops are not joined by roads (another landmass, \
                     or a pocket with no way out)",
                    i + 1
                ),
                _ => println!("leg {}   not routed: a stop has no road", i + 1),
            },
        }
    }
    let settled: usize = trip.legs.iter().flatten().map(|l| l.settled).sum();
    println!(
        "total   {:.1} km, {}{}; {ms:.1} ms, {settled} nodes settled",
        trip.metres() / 1000.0,
        duration(trip.time_s()),
        match trip.complete() {
            true => "",
            false => " (incomplete)",
        }
    );
    if let Some(path) = &args.geojson {
        write_geojson(path, &stops, &trip)?;
    }
    Ok(())
}

fn print_leg(
    r: &Routing,
    streets: Option<&Streets>,
    i: usize,
    l: &Leg,
    avoid: u8,
    pieces: bool,
) -> Result<()> {
    let mut notes = Vec::new();
    if l.uses != 0 {
        notes.push(format!("uses {}", flags_text(l.uses)));
    }
    if l.broken != 0 {
        notes.push(format!(
            "no route avoids {}, so it uses them",
            flags_text(l.broken)
        ));
    } else if avoid != 0 {
        notes.push(format!("avoids {}", flags_text(avoid & AVOID)));
    }
    println!(
        "leg {}   {:.1} km, {} ({} pieces, {} nodes settled){}",
        i + 1,
        l.metres / 1000.0,
        duration(l.time_s),
        l.pieces.len(),
        l.settled,
        match notes.is_empty() {
            true => String::new(),
            false => format!("; {}", notes.join("; ")),
        }
    );
    if pieces {
        // Runs of pieces on the same road, as one line each.
        let mut run: Option<(String, f64, f64)> = None;
        let flush = |run: &mut Option<(String, f64, f64)>| {
            if let Some((name, m, s)) = run.take() {
                println!("        {:>7.2} km  {:>8}  {name}", m / 1000.0, duration(s));
            }
        };
        for p in &l.pieces {
            let name = road(r, streets, p.dir >> 1)?;
            match &mut run {
                Some((n, m, s)) if *n == name => {
                    *m += p.metres;
                    *s += p.time_s;
                }
                _ => {
                    flush(&mut run);
                    run = Some((name, p.metres, p.time_s));
                }
            }
        }
        flush(&mut run);
    }
    Ok(())
}

fn write_geojson(
    path: &Path,
    stops: &[(Stop, String)],
    trip: &omp_core::route::Trip,
) -> Result<()> {
    let mut features = Vec::new();
    for (i, ((stop, name), snap)) in stops.iter().zip(&trip.snaps).enumerate() {
        features.push(json!({
            "type": "Feature",
            "geometry": point_json(stop.p),
            "properties": {"stop": i + 1, "name": name},
        }));
        if let Some(Snap { point, dist_m, .. }) = snap {
            features.push(json!({
                "type": "Feature",
                "geometry": line_json(&[stop.p, *point]),
                "properties": {"snap": i + 1, "dist_m": dist_m},
            }));
        }
    }
    for (i, leg) in trip.legs.iter().enumerate() {
        if let Some(l) = leg {
            features.push(json!({
                "type": "Feature",
                "geometry": line_json(&l.points),
                "properties": {
                    "leg": i + 1,
                    "metres": l.metres,
                    "time_s": l.time_s,
                    "uses": flags_text(l.uses),
                    "broken": flags_text(l.broken),
                },
            }));
        }
    }
    let v = json!({"type": "FeatureCollection", "features": features});
    std::fs::write(path, serde_json::to_string(&v)?)?;
    println!("wrote {}", path.display());
    Ok(())
}
