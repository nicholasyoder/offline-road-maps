//! `omp-builder build`: cuts a PBF into a V4 tile pyramid and writes it as a pack with
//! `META`, `SCHM`, `TDIC`, `TDAT` and `TIDX` (see `docs/format.md`).
//!
//! The pipeline is `measure`'s: needed-nodes store, whitelist, ring assembly, z6 split
//! tiles. `--bands n` cuts the pyramid in `n` passes, freeing the features in between, and
//! every band appends its V2 payloads to one temp store. Dictionaries are trained once from
//! all of it, then tiles are compressed and written in zoom, then Hilbert order.
//!
//! Admin areas are read first (`SADM`, `SAPL`), and every place and POI is given its chain
//! of admin areas; for now only the coverage is reported.

pub mod inspect;

use std::fs::File;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use anyhow::{Context, Result, bail, ensure};
use clap::Args;
use omp_core::meta::{self, Meta, Source};
use omp_core::pack::{self, DEFAULT_CHUNK_CAPACITY, Info, Writer};
use omp_core::schema::{self, Schema};
use omp_core::tile::hilbert;
use omp_core::tiles::{FrameCompressor, LEVEL, TileWriter};
use omp_core::whitelist::Whitelist;
use rayon::prelude::*;
use xxhash_rust::xxh3::{Xxh3, xxh3_128};

use crate::admin::{self, Admin, encode as admin_encode};
use crate::fetch::manifest::{FileEntry, Manifest};
use crate::measure::compress;
use crate::measure::osm;
use crate::measure::peak_rss_mib;
use crate::measure::poly::Poly;
use crate::measure::tiles::{self, Band, Entry, Store};
use crate::measure::water;

/// Tiles compressed in parallel before they are written in order.
const BATCH_TILES: usize = 16_384;

#[derive(Args)]
pub struct BuildArgs {
    /// OSM extract (.osm.pbf).
    input: PathBuf,

    /// Pack directory. Chunk files are named after it: `<out>/<dir name>.omp.000`, …
    #[arg(long)]
    out: PathBuf,

    /// Region name shown in the app [default: from the input file name, e.g. `Ohio`].
    #[arg(long)]
    name: Option<String>,

    /// Tag whitelist [default: the one built into omp-core].
    #[arg(long)]
    whitelist: Option<PathBuf>,

    /// osmdata.openstreetmap.de water polygons [default: <work dir>/inputs/osmdata/water-polygons-split-4326.zip, skipped if missing].
    #[arg(long)]
    water: Option<PathBuf>,

    /// Keep only the ocean pieces that meet this boundary (`.poly`), as in `measure`.
    #[arg(long)]
    clip_poly: Option<PathBuf>,

    /// Cut the pyramid in this many passes (z6 columns `x % n`), freeing the features in
    /// between. Each pass reads the whole PBF; North America needs 3 on a 62 GB machine.
    #[arg(long, default_value_t = 1)]
    bands: u32,

    /// The `fetch` manifest, for `META`'s sources [default: <work dir>/inputs/manifest.toml].
    #[arg(long)]
    manifest: Option<PathBuf>,

    /// Directory of state and province `.poly` files (`US-OH.poly`, `CA-QC.poly`, from `fetch
    /// --sources poly`), to split the route coverage report by [default: <work dir>/inputs/poly].
    #[arg(long)]
    route_regions: Option<PathBuf>,

    /// Directory for the temporary tile store [default: <work dir>/tmp].
    #[arg(long)]
    temp: Option<PathBuf>,

    #[arg(long, default_value_t = LEVEL)]
    zstd_level: i32,

    /// Build time 0 and a pack ID derived from the contents, so the same inputs and options
    /// give a byte-identical pack.
    #[arg(long)]
    deterministic: bool,

    /// Payload bytes per chunk file (a multiple of 4096); for tests.
    #[arg(long, hide = true, default_value_t = DEFAULT_CHUNK_CAPACITY)]
    chunk_capacity: u64,
}

/// `ohio-261001.osm.pbf` → `Ohio`, `north-america-261001.osm.pbf` → `North America`.
fn region_name(input: &Path) -> String {
    let file = input.file_name().unwrap_or_default().to_string_lossy();
    let stem = file.split('.').next().unwrap_or_default();
    let stem = match stem.rsplit_once('-') {
        Some((head, date)) if date.len() == 6 && date.bytes().all(|b| b.is_ascii_digit()) => head,
        _ => stem,
    };
    stem.split(['-', '_'])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            c.next()
                .map(|f| f.to_uppercase().chain(c).collect::<String>())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The manifest entry for `file`, matched by canonical path.
fn manifest_entry<'m>(manifest: &'m Manifest, dir: &Path, file: &Path) -> Option<&'m FileEntry> {
    let want = file.canonicalize().ok()?;
    manifest
        .files
        .iter()
        .find(|e| dir.join(&e.path).canonicalize().ok().as_ref() == Some(&want))
}

fn source(e: &FileEntry) -> Source {
    Source {
        name: match &e.source_id {
            Some(id) => format!("{} {id}", e.source),
            None => e.source.clone(),
        },
        url: e.url.clone(),
        date: e.downloaded.clone(),
        sha256: e.sha256.clone(),
        license: e.license.name.clone(),
        license_url: e.license.url.clone(),
        attribution: e.license.attribution.clone(),
    }
}

/// `osm 2026-10-01` from a Geofabrik file name's `-YYMMDD`, else the download date.
fn snapshot(input: &Path, entry: Option<&FileEntry>) -> String {
    let file = input.file_name().unwrap_or_default().to_string_lossy();
    let stem = file.split('.').next().unwrap_or_default();
    if let Some((_, d)) = stem.rsplit_once('-')
        && d.len() == 6
        && d.bytes().all(|b| b.is_ascii_digit())
    {
        return format!("osm 20{}-{}-{}", &d[0..2], &d[2..4], &d[4..6]);
    }
    match entry {
        Some(e) => format!("osm {}", e.downloaded.get(..10).unwrap_or(&e.downloaded)),
        None => "osm".into(),
    }
}

/// One line per distinct attribution, with its license.
fn attribution(sources: &[Source]) -> String {
    let mut lines: Vec<String> = Vec::new();
    for s in sources {
        let line = match &s.attribution {
            Some(a) => format!("{a}, {}", s.license),
            None => s.license.clone(),
        };
        if !lines.contains(&line) {
            lines.push(line);
        }
    }
    lines.join("\n")
}

fn log_stage(name: &str, t: &mut Instant) {
    eprintln!(
        "  {name}: {:.1} s, peak RSS {:.0} MiB",
        t.elapsed().as_secs_f64(),
        peak_rss_mib()
    );
    *t = Instant::now();
}

/// Returns a [`FrameCompressor`] to its pool when a rayon split is done with it.
struct Pooled<'a> {
    pool: &'a Mutex<Vec<FrameCompressor>>,
    fc: Option<FrameCompressor>,
}

impl Drop for Pooled<'_> {
    fn drop(&mut self) {
        if let Some(fc) = self.fc.take() {
            self.pool.lock().unwrap().push(fc);
        }
    }
}

/// Per-zoom totals printed at the end.
#[derive(Default, Clone, Copy)]
struct ZoomTotals {
    tiles: u64,
    frames: u64,
    raw: u64,
    bytes: u64,
}

pub fn run(args: BuildArgs, work_dir: &Path) -> Result<()> {
    let start = Instant::now();
    ensure!(
        (1..=64).contains(&args.bands),
        "--bands must be between 1 and 64"
    );
    let wl_text = match &args.whitelist {
        Some(p) => std::fs::read_to_string(p).with_context(|| format!("read {}", p.display()))?,
        None => omp_core::whitelist::DEFAULT.to_owned(),
    };
    let wl = Whitelist::parse(&wl_text).context("parse whitelist")?;
    let name = args
        .name
        .clone()
        .unwrap_or_else(|| region_name(&args.input));
    let file_name = args
        .out
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty())
        .context("--out needs a directory name")?
        .to_owned();
    let temp = args.temp.clone().unwrap_or_else(|| work_dir.join("tmp"));
    std::fs::create_dir_all(&temp)?;
    std::fs::create_dir_all(&args.out).with_context(|| format!("create {}", args.out.display()))?;
    remove_old_chunks(&args.out, &file_name)?;

    // Provenance, from the fetch manifest.
    let manifest_path = args
        .manifest
        .clone()
        .unwrap_or_else(|| work_dir.join("inputs/manifest.toml"));
    let manifest = Manifest::load_or_default(&manifest_path)?;
    let manifest_dir = manifest_path.parent().unwrap_or(Path::new("."));
    let osm_entry = manifest_entry(&manifest, manifest_dir, &args.input);
    let mut sources = match osm_entry {
        Some(e) => vec![source(e)],
        None => {
            eprintln!(
                "  warning: {} is not in {}; META lists it without a hash",
                args.input.display(),
                manifest_path.display()
            );
            vec![Source {
                name: "osm".into(),
                url: args
                    .input
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into(),
                license: "ODbL-1.0".into(),
                attribution: Some("© OpenStreetMap contributors".into()),
                ..Source::default()
            }]
        }
    };
    let mut snap = snapshot(&args.input, osm_entry);

    let bounds = osm::header_bounds(&args.input)?
        .context("the PBF header has no bounding box; the pack needs one")?;
    let water_zip = args
        .water
        .clone()
        .unwrap_or_else(|| work_dir.join("inputs/osmdata/water-polygons-split-4326.zip"));
    let water_zip = water_zip.exists().then_some(water_zip);
    match &water_zip {
        Some(z) => {
            if let Some(e) = manifest_entry(&manifest, manifest_dir, z) {
                sources.push(source(e));
                snap.push_str(&format!(
                    "; water {}",
                    e.downloaded.get(..10).unwrap_or(&e.downloaded)
                ));
            }
        }
        None => eprintln!("  water: no water polygons, oceans left out"),
    }
    let clip = args.clip_poly.as_deref().map(Poly::read).transpose()?;

    let regions = load_regions(
        &args
            .route_regions
            .clone()
            .unwrap_or_else(|| work_dir.join("inputs/poly")),
    )?;

    eprintln!("build {name}: {}", args.input.display());
    let mut t = Instant::now();
    let admin = admin::load(&args.input)?;
    let mut chains = ChainStats::default();
    log_stage("admin", &mut t);
    let store_path = temp.join(format!("{file_name}-build.v2"));
    let mut store = Store::create(&store_path)?;
    let mut cut = tiles::Stats::default();
    for k in 0..args.bands {
        let band = Band { k, n: args.bands };
        if args.bands > 1 {
            eprintln!(" band {k}/{}", args.bands);
        }
        let mut data = osm::read(
            &args.input,
            &wl,
            osm::Strategy::Sparse,
            false,
            band,
            &regions,
        )?;
        data.roads = Vec::new();
        log_stage("read", &mut t);
        if k == 0 {
            print_routes(&data.stats.routes);
        }
        if let (Some(zip), Some(bbox)) = (&water_zip, data.bbox) {
            let layer = wl.layer_index("water").context("no water layer")?;
            let class = wl.layers[layer]
                .classes
                .iter()
                .position(|c| c.external && c.name == "ocean")
                .context("no external ocean class in the water layer")?;
            let (feats, _, _) = water::read(zip, bbox, clip.as_ref(), layer as u8, class as u16)?;
            let minzoom = tiles::minzooms(&wl);
            data.features.extend(feats.into_iter().filter(|f| {
                band.keeps(f.geom.bbox(), minzoom[f.layer as usize][f.class as usize])
            }));
            log_stage("water", &mut t);
        }
        chains.add(&data.features, &wl, &admin);
        cut.merge(tiles::cut_pyramid(
            &data.features,
            &wl,
            &mut store,
            band,
            false,
        ));
        drop(data);
        log_stage("tiles", &mut t);
    }
    let (mut entries, store_bytes) = store.finish()?;
    let store_file =
        File::open(&store_path).with_context(|| format!("open {}", store_path.display()))?;

    let layer_names: Vec<String> = wl.layers.iter().map(|l| l.name.clone()).collect();
    let (dicts, _) = compress::train(&store_file, &entries, &layer_names);
    log_stage("dictionaries", &mut t);

    let mut settings = serde_json::Map::new();
    settings.insert("whitelist_version".into(), wl.version.into());
    settings.insert("encoding".into(), "V4".into());
    settings.insert("zstd_level".into(), args.zstd_level.into());
    settings.insert("max_zoom".into(), tiles::MAX_ZOOM.into());
    settings.insert("admin_tolerance_m".into(), admin::PACK_TOLERANCE_M.into());
    settings.insert("input".into(), file_label(&args.input).into());
    if let Some(z) = &water_zip {
        settings.insert("water".into(), file_label(z).into());
    }
    if let Some(p) = &args.clip_poly {
        settings.insert("clip_poly".into(), file_label(p).into());
    }
    let build_time = match args.deterministic {
        true => 0,
        false => jiff::Timestamp::now().as_second(),
    };
    let meta = Meta {
        region: name.clone(),
        bbox: bounds,
        build_time,
        builder_version: env!("CARGO_PKG_VERSION").into(),
        format_version: format!("{}.{}", pack::MAJOR, pack::MINOR),
        attribution: attribution(&sources),
        sources,
        settings,
    };

    let mut w = Writer::create(&args.out, &file_name, args.chunk_capacity)?;
    let mut sections = vec![
        w.section(meta::ID, &meta.encode()?)?,
        w.section(schema::ID, &Schema::from_whitelist(&wl).encode()?)?,
    ];

    // Zoom, then Hilbert order; layers in order within a tile.
    entries.par_sort_unstable_by_key(|e| (e.z, hilbert(e.z, e.x, e.y), e.layer));
    let tiles: Vec<Range<usize>> = {
        let mut v = Vec::new();
        let mut at = 0;
        for g in entries.chunk_by(|a, b| (a.z, a.x, a.y) == (b.z, b.x, b.y)) {
            v.push(at..at + g.len());
            at += g.len();
        }
        v
    };
    let pool: Mutex<Vec<FrameCompressor>> = Mutex::new(Vec::new());
    let mut zooms = [ZoomTotals::default(); tiles::MAX_ZOOM as usize + 1];
    let mut tw = TileWriter::begin(&mut w, &dicts)?;
    for batch in tiles.chunks(BATCH_TILES) {
        let framed: Vec<Vec<(u8, Vec<u8>)>> = batch
            .par_iter()
            .map_init(
                || Pooled {
                    pool: &pool,
                    fc: pool.lock().unwrap().pop(),
                },
                |p, r| -> Result<Vec<(u8, Vec<u8>)>> {
                    if p.fc.is_none() {
                        p.fc = Some(FrameCompressor::new(&dicts, args.zstd_level)?);
                    }
                    let fc = p.fc.as_mut().unwrap();
                    entries[r.clone()]
                        .iter()
                        .map(|e: &Entry| {
                            let raw = compress::read(&store_file, e);
                            Ok((e.layer, fc.compress(e.layer, e.z, &raw)?))
                        })
                        .collect()
                },
            )
            .collect::<Result<_>>()?;
        for (r, frames) in batch.iter().zip(&framed) {
            let e = &entries[r.start];
            let refs: Vec<(u8, &[u8])> = frames.iter().map(|(l, f)| (*l, &f[..])).collect();
            tw.add(e.z, e.x, e.y, &refs)?;
            let zt = &mut zooms[e.z as usize];
            zt.tiles += 1;
            zt.frames += frames.len() as u64;
            zt.raw += entries[r.clone()]
                .iter()
                .map(|e| u64::from(e.len))
                .sum::<u64>();
            zt.bytes += frames.iter().map(|(_, f)| f.len() as u64).sum::<u64>();
        }
    }
    let (tdat, tidx) = tw.finish()?;
    sections.extend([tdat, tidx]);
    let (sadm, sapl, admin_sizes) = admin_encode::sections(&admin)?;
    sections.push(w.section(admin_encode::SADM, &sadm)?);
    sections.push(w.section(admin_encode::SAPL, &sapl)?);
    drop(admin);
    drop(store_file);
    let _ = std::fs::remove_file(&store_path);
    log_stage("compress", &mut t);

    let pack_id = match args.deterministic {
        true => {
            let mut h = Xxh3::new();
            for s in &sections {
                h.update(&s.id);
                h.update(&s.checksum.to_le_bytes());
            }
            h.digest128()
        }
        false => {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let seed = format!("{nanos} {} {}", std::process::id(), args.out.display());
            xxh3_128(seed.as_bytes())
        }
    };
    let e7 = |d: f64| (d * 1e7).round() as i32;
    let info = Info {
        pack_id: pack_id.to_le_bytes(),
        bbox: bounds.map(e7),
        build_time,
        name: name.clone(),
        snapshot: snap,
    };
    let paths = w.finish(&info)?;
    log_stage("write", &mut t);

    let total: u64 = paths
        .iter()
        .map(|p| std::fs::metadata(p).map(|m| m.len()))
        .sum::<std::io::Result<u64>>()?;
    eprintln!("  zoom      tiles     frames       raw MiB    tiles MiB");
    let mut sum = ZoomTotals::default();
    for (z, zt) in zooms.iter().enumerate() {
        eprintln!(
            "  z{z:<3} {:>10} {:>10} {:>13.1} {:>12.1}",
            zt.tiles,
            zt.frames,
            mib(zt.raw),
            mib(zt.bytes)
        );
        sum.tiles += zt.tiles;
        sum.frames += zt.frames;
        sum.raw += zt.raw;
        sum.bytes += zt.bytes;
    }
    eprintln!(
        "  all  {:>10} {:>10} {:>13.1} {:>12.1}",
        sum.tiles,
        sum.frames,
        mib(sum.raw),
        mib(sum.bytes)
    );
    print_left_out(&cut, &wl);
    eprintln!(
        "  admin: {} areas, SADM {:.2} MiB, SAPL {:.2} MiB ({} blocks)",
        admin_sizes.areas,
        mib(admin_sizes.sadm),
        mib(admin_sizes.sapl),
        admin_sizes.sapl_blocks
    );
    chains.print();
    eprintln!(
        "wrote {} in {} chunk(s), {:.1} MiB (tiles {} B, TIDX {} B, TDIC {} dictionaries); \
         {:.0} s, peak RSS {:.0} MiB, temp store {:.1} MiB",
        args.out.join(format!("{file_name}.omp.000")).display(),
        paths.len(),
        mib(total),
        tdat.len,
        tidx.len,
        dicts.0.values().filter(|d| !d.is_empty()).count(),
        start.elapsed().as_secs_f64(),
        peak_rss_mib(),
        mib(store_bytes)
    );
    Ok(())
}

/// Admin chains of places and POIs, per layer: `[features, in no area, without a level 4
/// area, without level 6, without level 8, in a city, town or village]`.
#[derive(Default)]
struct ChainStats {
    seen: std::collections::HashSet<(u8, i64)>,
    layers: std::collections::BTreeMap<String, [u64; 6]>,
}

impl ChainStats {
    /// Counts the point features of the `places` and `pois` layers, each once over all bands.
    fn add(&mut self, features: &[osm::Feature], wl: &Whitelist, admin: &Admin) {
        let layers: Vec<u8> = ["places", "pois"]
            .iter()
            .filter_map(|l| wl.layer_index(l).map(|i| i as u8))
            .collect();
        let points: Vec<(u8, crate::measure::geom::P)> = features
            .iter()
            .filter(|f| layers.contains(&f.layer) && self.seen.insert((f.layer, f.id)))
            .filter_map(|f| match f.geom {
                crate::measure::geom::Geometry::Point(p) => Some((f.layer, p)),
                _ => None,
            })
            .collect();
        let counted: Vec<(u8, [u64; 6])> = points
            .par_iter()
            .map(|&(layer, p)| {
                let chain = admin.chain(p);
                let missing = |l| u64::from(admin.at_level(&chain, l).is_none());
                let town = chain.iter().any(|&a| admin.towns[a as usize]);
                (
                    layer,
                    [
                        1,
                        u64::from(chain.is_empty()),
                        missing(4),
                        missing(6),
                        missing(8),
                        u64::from(town),
                    ],
                )
            })
            .collect();
        for (layer, c) in counted {
            let row = self
                .layers
                .entry(wl.layers[layer as usize].name.clone())
                .or_default();
            for (r, v) in row.iter_mut().zip(c) {
                *r += v;
            }
        }
    }

    fn print(&self) {
        for (layer, c) in &self.layers {
            eprintln!(
                "  admin chains, {layer}: {} features, {} in no area, {} without level 4, \
                 {} without level 6, {} without level 8, {} in a city, town or village",
                c[0], c[1], c[2], c[3], c[4], c[5]
            );
        }
    }
}

/// Tiles left out (one polygon covering them), per zoom and per class.
fn print_left_out(cut: &tiles::Stats, wl: &Whitelist) {
    use std::collections::BTreeMap;
    let mut zooms: BTreeMap<u8, u64> = BTreeMap::new();
    let mut classes: BTreeMap<(u8, u16), (u64, u64)> = BTreeMap::new();
    for (&(z, l, c), &(t, b)) in &cut.left_out {
        *zooms.entry(z).or_default() += t;
        let e = classes.entry((l, c)).or_default();
        e.0 += t;
        e.1 += b;
    }
    let per_zoom: Vec<String> = zooms.iter().map(|(z, t)| format!("z{z} {t}")).collect();
    eprintln!(
        "  left out: {} tiles covered by one polygon ({})",
        zooms.values().sum::<u64>(),
        per_zoom.join(", ")
    );
    let mut classes: Vec<_> = classes.into_iter().collect();
    classes.sort_by_key(|c| std::cmp::Reverse(c.1.0));
    for ((l, c), (t, b)) in classes {
        let layer = &wl.layers[l as usize];
        eprintln!(
            "    {}/{}: {t} tiles, {:.1} MiB raw",
            layer.name,
            layer.classes[c as usize].name,
            mib(b)
        );
    }
}

/// The `XX-YY.poly` files in `dir`, sorted by name; none if `dir` does not exist.
fn load_regions(dir: &Path) -> Result<Vec<(String, Poly)>> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Ok(Vec::new());
    };
    let mut paths: Vec<(String, PathBuf)> = rd
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter_map(|p| {
            let stem = p.file_name()?.to_str()?.strip_suffix(".poly")?.to_owned();
            let b = stem.as_bytes();
            let code = b.len() == 5
                && b[2] == b'-'
                && b[..2].iter().chain(&b[3..]).all(u8::is_ascii_uppercase);
            code.then_some((stem, p))
        })
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|(name, p)| Ok((name, Poly::read(&p)?)))
        .collect()
}

/// Route relation coverage per country and class and per region, and the commonest networks.
fn print_routes(r: &osm::RouteStats) {
    use std::collections::BTreeMap;
    // The last count is the ways with a real shield, out of all.
    let line = |name: &str, c: [u64; 4]| {
        let all: u64 = c[..3].iter().sum();
        let pct = |n: u64| 100.0 * n as f64 / all.max(1) as f64;
        eprintln!(
            "    {name:<20} {all:>9} {:>8.1}% {:>8.1}% {:>8.1}% {:>8.1}%",
            pct(c[0]),
            pct(c[1]),
            pct(c[2]),
            pct(c[3])
        );
    };
    let add = |t: &mut [u64; 4], c: &[u64; 4]| {
        for i in 0..4 {
            t[i] += c[i];
        }
    };
    eprintln!("  routes: {} road route relations", r.relations);
    eprintln!("    ways                      all  relation  ref only   neither    shield");
    // Country → class → counts; the class order is the whitelist's, roughly by importance.
    let order = ["motorway", "trunk", "primary", "secondary", "tertiary"];
    let mut countries: BTreeMap<&str, BTreeMap<usize, [u64; 4]>> = BTreeMap::new();
    for (region, classes) in &r.coverage {
        let country = region.split('-').next().unwrap_or(region);
        for (class, c) in classes {
            let i = order.iter().position(|o| o == class).unwrap_or(order.len());
            add(
                countries.entry(country).or_default().entry(i).or_default(),
                c,
            );
        }
    }
    for (country, classes) in &countries {
        let mut total = [0; 4];
        for (i, c) in classes {
            add(&mut total, c);
            line(
                &format!("{country} {}", order.get(*i).unwrap_or(&"other")),
                *c,
            );
        }
        line(&format!("{country} all"), total);
    }
    if r.coverage.len() > countries.len() {
        for (region, classes) in &r.coverage {
            let mut total = [0; 4];
            for c in classes.values() {
                add(&mut total, c);
            }
            line(region, total);
        }
    }
    let mut nets: Vec<(&String, &u64)> = r.networks.iter().collect();
    nets.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    let top: Vec<String> = nets
        .iter()
        .take(40)
        .map(|(n, c)| format!("{n} {c}"))
        .collect();
    eprintln!(
        "    {} networks; ways per network: {}",
        nets.len(),
        top.join(", ")
    );
}

fn mib(b: u64) -> f64 {
    b as f64 / (1u64 << 20) as f64
}

fn file_label(p: &Path) -> String {
    p.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

/// Deletes `<dir>/<name>.omp.NNN` left by an earlier build, so a smaller pack does not leave
/// stale chunks behind.
fn remove_old_chunks(dir: &Path, name: &str) -> Result<()> {
    let prefix = format!("{name}.omp.");
    for e in std::fs::read_dir(dir)? {
        let e = e?;
        let file = e.file_name();
        let Some(rest) = file.to_str().and_then(|f| f.strip_prefix(&prefix)) else {
            continue;
        };
        if rest.len() == 3 && rest.bytes().all(|b| b.is_ascii_digit()) {
            std::fs::remove_file(e.path())?;
        }
    }
    Ok(())
}

/// The first chunk of the pack at `path`: the `.omp.000` file itself, or the only one in a
/// directory.
pub fn first_chunk(path: &Path) -> Result<PathBuf> {
    if !path.is_dir() {
        return Ok(path.to_owned());
    }
    let mut found: Vec<PathBuf> = std::fs::read_dir(path)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.to_str().is_some_and(|s| s.ends_with(".omp.000")))
        .collect();
    found.sort();
    match found.len() {
        1 => Ok(found.remove(0)),
        0 => bail!("no .omp.000 file in {}", path.display()),
        _ => bail!(
            "{} holds several packs; name one of the .omp.000 files",
            path.display()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_from_file_names() {
        assert_eq!(region_name(Path::new("x/ohio-261001.osm.pbf")), "Ohio");
        assert_eq!(
            region_name(Path::new("north-america-261001.osm.pbf")),
            "North America"
        );
        assert_eq!(region_name(Path::new("delaware.osm.pbf")), "Delaware");
        assert_eq!(
            snapshot(Path::new("ohio-261001.osm.pbf"), None),
            "osm 2026-10-01"
        );
    }

    #[test]
    fn attribution_lists_each_once() {
        let s = |a: Option<&str>, l: &str| Source {
            attribution: a.map(Into::into),
            license: l.into(),
            ..Source::default()
        };
        let a = attribution(&[
            s(Some("© OpenStreetMap contributors"), "ODbL-1.0"),
            s(Some("© OpenStreetMap contributors"), "ODbL-1.0"),
            s(None, "CC0"),
        ]);
        assert_eq!(a, "© OpenStreetMap contributors, ODbL-1.0\nCC0");
    }
}
