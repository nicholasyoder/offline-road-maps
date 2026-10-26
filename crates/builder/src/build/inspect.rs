//! `omp-builder inspect` and `verify`: thin wrappers over `omp-core`'s pack and tile readers.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use clap::Args;
use omp_core::meta::{self, Meta};
use omp_core::pack::Pack;
use omp_core::schema::{self, Schema};
use omp_core::tile::{Feature, Geom, Layer, MAX_ZOOM, hilbert_xy};
use omp_core::tiles::TileReader;

use super::first_chunk;

#[derive(Args)]
pub struct InspectArgs {
    /// Pack directory or its `.omp.000` file.
    pack: PathBuf,

    /// Decode one tile, `z/x/y`, and list its layers and first features.
    #[arg(long)]
    tile: Option<String>,

    /// Read the whole tile index: bytes per layer and zoom, and the largest tile per zoom.
    #[arg(long)]
    stats: bool,

    /// Decode every tile of one zoom and list one layer's features, `layer/z` (e.g.
    /// `labels/8`), one per line with its tile and position.
    #[arg(long)]
    features: Option<String>,

    /// Summarize the search sections and list the entries each search token of these words
    /// is posted to (all of them, so `""` gives the summary only).
    #[arg(long)]
    search: Option<String>,

    /// Read the search, street, address, admin and grid sections through the app's
    /// readers, check them against each other on about this many samples of each, and time
    /// warm lookups.
    #[arg(long, num_args = 0..=1, default_missing_value = "2000")]
    readers: Option<usize>,
}

#[derive(Args)]
pub struct VerifyArgs {
    /// Pack directory or its `.omp.000` file.
    pack: PathBuf,
}

fn mib(b: u64) -> f64 {
    b as f64 / (1u64 << 20) as f64
}

/// `inspect --search`: entry counts, then the entries posted to each token of `query`.
fn search(pack: &Pack, schema: Option<&Schema>, query: &str) -> Result<()> {
    use crate::search::Kind;
    use crate::search::encode::{SENT, SFST, SPST, STWN, decode_entries, decode_towns, lookup};
    let read = |id: [u8; 4]| -> Result<Vec<u8>> {
        let s = pack
            .section(id)
            .with_context(|| format!("no {} section", String::from_utf8_lossy(&id)))?;
        Ok(pack.read_section(s)?)
    };
    let entries = decode_entries(&read(SENT)?)?;
    let towns = decode_towns(&read(STWN)?)?;
    let (sfst, spst) = (read(SFST)?, read(SPST)?);
    let mut kinds: BTreeMap<Kind, u64> = BTreeMap::new();
    for e in &entries {
        *kinds.entry(e.kind).or_default() += 1;
    }
    let tokens = fst::Map::new(&sfst[..])?.len();
    println!(
        "\nsearch   {} entries ({kinds:?}), {} towns, {tokens} tokens",
        entries.len(),
        towns.len()
    );
    let layer_class = |layer: &str, class: u16| {
        schema
            .and_then(|s| s.layers.iter().find(|l| l.name == layer))
            .and_then(|l| l.classes.get(class as usize))
            .map_or_else(|| class.to_string(), |c| c.name.clone())
    };
    for token in omp_core::normalize::search_tokens(query) {
        let Some((ids, total)) = lookup(&sfst, &spst, &token)? else {
            println!("  {token}: not indexed");
            continue;
        };
        println!("  {token}: {} posted of {total}", ids.len());
        for &id in ids.iter().take(20) {
            let e = &entries[id as usize];
            let class = match e.kind {
                Kind::Admin => format!("level {}", e.class),
                Kind::Place => layer_class("places", e.class),
                Kind::Street => crate::addr::osm::HIGHWAYS
                    .get(e.class as usize)
                    .map_or_else(|| "other".into(), |h| (*h).to_owned()),
                Kind::Poi => layer_class("pois", e.class),
                Kind::Postcode => ["numeric", "canadian"][usize::from(e.class.min(1))].into(),
                Kind::Feature => layer_class("labels", e.class),
                Kind::AddressStreet => "from addresses".into(),
            };
            let (lat, lon) = crate::measure::geom::unproject(e.p);
            let town = match e.town {
                0 => String::new(),
                t => format!(", {}", towns[t as usize - 1]),
            };
            let alts = match e.alts.is_empty() {
                true => String::new(),
                false => format!(" (also {})", e.alts.join("; ")),
            };
            println!(
                "    {id:>8} {:?}/{class} {}{town}{alts} at {lat:.5},{lon:.5}, importance {}, admin {:?}, ref {}",
                e.kind, e.name, e.importance, e.admin, e.reference
            );
        }
    }
    Ok(())
}

fn time(secs: i64) -> String {
    jiff::Timestamp::from_second(secs).map_or_else(|_| secs.to_string(), |t| t.to_string())
}

pub fn inspect(args: InspectArgs) -> Result<()> {
    let first = first_chunk(&args.pack)?;
    let pack = Pack::open(&first).with_context(|| format!("open {}", first.display()))?;
    let i = &pack.info;
    let deg = |v: i32| f64::from(v) * 1e-7;
    println!("pack     {}", first.display());
    println!(
        "format   0.{}, id {}, {} chunk(s) of {:.1} MiB",
        pack.minor,
        hex::encode(i.pack_id),
        pack.chunks(),
        mib(pack.capacity)
    );
    println!("region   {} ({})", i.name, i.snapshot);
    println!(
        "bbox     W {:.4} S {:.4} E {:.4} N {:.4}",
        deg(i.bbox[0]),
        deg(i.bbox[1]),
        deg(i.bbox[2]),
        deg(i.bbox[3])
    );
    println!("built    {}", time(i.build_time));
    println!("\nsection  flags        offset           bytes  xxh3");
    for s in &pack.sections {
        println!(
            "{}     {:>5} {:>13} {:>15}  {:016x}",
            String::from_utf8_lossy(&s.id),
            s.flags,
            s.offset,
            s.len,
            s.checksum
        );
    }

    if let Some(s) = pack.section(meta::ID) {
        let m = Meta::decode(&pack.read_section(s)?)?;
        println!(
            "\nMETA     builder {}, format {}, {} source(s)",
            m.builder_version,
            m.format_version,
            m.sources.len()
        );
        for s in m.sources.iter().take(10) {
            println!("  {} ({}) {}", s.name, s.license, s.url);
        }
        if m.sources.len() > 10 {
            println!("  … {} more", m.sources.len() - 10);
        }
        println!("  settings {}", serde_json::Value::Object(m.settings));
        for line in m.attribution.lines() {
            println!("  attribution: {line}");
        }
    }
    let schema = match pack.section(schema::ID) {
        Some(s) => Some(Schema::decode(&pack.read_section(s)?)?),
        None => None,
    };
    if let Some(s) = &schema {
        println!("\nSCHM     whitelist v{}", s.whitelist_version);
        for (id, l) in s.layers.iter().enumerate() {
            let cap = l
                .maxzoom
                .map_or_else(String::new, |m| format!(", maxzoom {m}"));
            println!(
                "  {id:>2} {:<12} {:?}, {} classes{cap}, props: {}",
                l.name,
                l.geometry,
                l.classes.len(),
                l.props.join(" ")
            );
        }
    }
    let layer_name = |id: u8| {
        schema
            .as_ref()
            .and_then(|s| s.layers.get(id as usize))
            .map_or_else(|| format!("layer {id}"), |l| l.name.clone())
    };

    if let Some(q) = &args.search {
        search(&pack, schema.as_ref(), q)?;
    }

    if let Some(n) = args.readers {
        super::readers::run(&first, n)?;
    }

    let tiles = TileReader::open(pack)?;
    println!("\nzoom      tiles");
    let mut all = 0;
    for z in 0..=MAX_ZOOM {
        let n = tiles.tile_count(z)?;
        all += n;
        println!("z{z:<3} {n:>10}");
    }
    println!("all  {all:>10}");

    if args.stats {
        // (zoom, layer) → (frames, bytes); zoom → (largest tile bytes, key).
        let mut cells: BTreeMap<(u8, u8), (u64, u64)> = BTreeMap::new();
        let mut largest: BTreeMap<u8, (u64, u64)> = BTreeMap::new();
        for z in 0..=MAX_ZOOM {
            tiles.for_each_tile(z, |key, loc| {
                for &(l, n) in &loc.layers {
                    let c = cells.entry((z, l)).or_default();
                    c.0 += 1;
                    c.1 += u64::from(n);
                }
                let big = largest.entry(z).or_default();
                if loc.len() > big.0 {
                    *big = (loc.len(), key);
                }
            })?;
        }
        let layers: Vec<u8> = {
            let mut v: Vec<u8> = cells.keys().map(|k| k.1).collect();
            v.sort_unstable();
            v.dedup();
            v
        };
        print!("\nMiB  ");
        for &l in &layers {
            print!("{:>11}", layer_name(l));
        }
        println!("{:>11}  largest tile", "all");
        let mut col_totals = vec![0u64; layers.len()];
        for z in 0..=MAX_ZOOM {
            let Some(&(big, key)) = largest.get(&z) else {
                continue;
            };
            print!("z{z:<3}");
            let mut row = 0;
            for (c, &l) in layers.iter().enumerate() {
                let b = cells.get(&(z, l)).map_or(0, |c| c.1);
                col_totals[c] += b;
                row += b;
                print!("{:>11.1}", mib(b));
            }
            let (x, y) = hilbert_xy(z, key);
            println!(
                "{:>11.1}  {z}/{x}/{y} {:.0} KiB",
                mib(row),
                big as f64 / 1024.0
            );
        }
        print!("all ");
        for b in &col_totals {
            print!("{:>11.2}", mib(*b));
        }
        println!("{:>11.2}", mib(col_totals.iter().sum()));
    }

    if let Some(spec) = &args.features {
        let (name, z) = spec.rsplit_once('/').context("--features wants layer/z")?;
        let z: u8 = z.parse()?;
        let s = schema.as_ref().context("the pack has no SCHM")?;
        let id = s
            .layers
            .iter()
            .position(|l| l.name == name)
            .with_context(|| format!("no layer {name:?}"))? as u8;
        let mut keys = Vec::new();
        tiles.for_each_tile(z, |key, loc| {
            if loc.layers.iter().any(|&(l, _)| l == id) {
                keys.push(key);
            }
        })?;
        let mut n = 0;
        for key in keys {
            let (x, y) = hilbert_xy(z, key);
            let Some(t) = tiles.tile(z, x, y)? else {
                continue;
            };
            for (lid, layer) in &t.layers {
                if *lid == id {
                    for f in &layer.features {
                        println!("{z}/{x}/{y} {}", describe(&s.layers[id as usize], layer, f));
                        n += 1;
                    }
                }
            }
        }
        println!("{n} features");
    }

    if let Some(spec) = &args.tile {
        let parts: Vec<&str> = spec.split('/').collect();
        let [z, x, y] = parts[..] else {
            bail!("--tile wants z/x/y");
        };
        let (z, x, y): (u8, u32, u32) = (z.parse()?, x.parse()?, y.parse()?);
        let Some(t) = tiles.tile(z, x, y)? else {
            println!("\ntile {z}/{x}/{y}: not in the pack");
            return Ok(());
        };
        println!("\ntile {z}/{x}/{y}: {} bytes compressed", t.bytes);
        for (id, layer) in &t.layers {
            let sl = schema.as_ref().and_then(|s| s.layers.get(*id as usize));
            println!(
                "  {}: {} features, {} vertices",
                layer_name(*id),
                layer.features.len(),
                layer.vertices()
            );
            for f in layer.features.iter().take(5) {
                match sl {
                    Some(sl) => println!("    {}", describe(sl, layer, f)),
                    None => println!("    class {}", f.class),
                }
            }
            if layer.features.len() > 5 {
                println!("    …");
            }
        }
    }
    Ok(())
}

/// `class shape key=value …` for one decoded feature.
fn describe(sl: &schema::Layer, layer: &Layer, f: &Feature) -> String {
    let class = sl
        .classes
        .get(f.class as usize)
        .map_or_else(|| f.class.to_string(), |c| c.name.clone());
    let props: Vec<String> = layer
        .props(f)
        .iter()
        .map(|&(k, _)| {
            let key = sl
                .props
                .get(k as usize)
                .map_or_else(|| k.to_string(), Clone::clone);
            format!("{key}={}", layer.prop(f, k).unwrap_or_default())
        })
        .collect();
    let shape = match layer.geom(f) {
        Geom::Point(p) => format!("point {},{}", p[0], p[1]),
        Geom::Lines(l) => format!("{} line(s)", l.len()),
        Geom::Polygon(r) if r.iter().map(Vec::len).sum::<usize>() <= 16 => format!("{r:?}"),
        Geom::Polygon(r) => format!("{} ring(s)", r.len()),
    };
    format!("{class} {shape} {}", props.join(" "))
}

pub fn verify(args: VerifyArgs) -> Result<()> {
    let first = first_chunk(&args.pack)?;
    let pack = Pack::open(&first).with_context(|| format!("open {}", first.display()))?;
    let start = Instant::now();
    let mut last = 0;
    let bad = pack.verify_with(|done, total| {
        let pct = done * 100 / total.max(1);
        if pct >= last + 10 || (done == total && pct > last) {
            last = pct;
            eprint!("\r  {pct}%");
            let _ = std::io::stderr().flush();
        }
    })?;
    eprintln!();
    let total: u64 = pack.sections.iter().map(|s| s.len).sum();
    let secs = start.elapsed().as_secs_f64();
    if bad.is_empty() {
        println!(
            "OK: {} sections, {:.1} MiB in {secs:.1} s ({:.0} MiB/s)",
            pack.sections.len(),
            mib(total),
            mib(total) / secs.max(1e-9)
        );
        Ok(())
    } else {
        let ids: Vec<String> = bad
            .iter()
            .map(|id| String::from_utf8_lossy(id).into_owned())
            .collect();
        bail!("checksum mismatch in {}", ids.join(", "))
    }
}
