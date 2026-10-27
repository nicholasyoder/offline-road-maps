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
//!
//! After the bands, so their memory is free: the addresses of `--addresses` merged per state
//! and province ([`addr::merge`], step 2.4), then the street table (`SSTR`, `SGEO`), split
//! where the postal town of its addresses changes, at admin 8 boundaries where it has none,
//! and at the length cap. The addresses are placed on it and written as `ASTR`, `ASUF`,
//! `AUNI`, `ALSE`, `ARNG` and `APCD` ([`addr::store`]).
//!
//! The spatial grid (`SGRD`) lists the streets and unattached addresses in each z14 cell.
//!
//! Search (`SENT`, `STWN`, `SFST`, `SPST`) takes its streets from the table before it is
//! dropped, its places, POIs and labels from the bands, and its postcodes and the streets
//! known only from their addresses from the merge.

pub mod inspect;
mod readers;

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

use crate::addr::{self, Input, SourceOpts, merge, postcodes, store};
use crate::admin::{self, Admin, encode as admin_encode};
use crate::fetch::manifest::{FileEntry, Manifest};
use crate::fetch::regions::Subdivision;
use crate::measure::compress;
use crate::measure::osm;
use crate::measure::peak_rss_mib;
use crate::measure::poly::Poly;
use crate::measure::tiles::{self, Band, Entry, Store};
use crate::measure::water;
use crate::mib;
use crate::search::{self, encode as search_encode};
use crate::street::{self, encode as street_encode, grid};

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

    /// States or provinces whose addresses to merge, as ISO codes (`US-OH`, `CA-QC`), or
    /// `all` for every US state and Canadian province; none if not given. Needs their files
    /// from `fetch` and `<inputs>/poly/<ISO>.poly`.
    #[arg(long = "addresses")]
    addresses: Vec<String>,

    /// Address sources to read.
    #[arg(
        long,
        value_enum,
        value_delimiter = ',',
        default_value = "osm,openaddresses,nar,tiger"
    )]
    address_sources: Vec<Input>,

    /// Skip address files whose license name contains this text, ignoring case.
    #[arg(long)]
    exclude_license: Vec<String>,

    /// Uncompressed KiB per address block; the first is written, the others only measured.
    #[arg(long, hide = true, value_delimiter = ',', default_value = "16")]
    address_block_kib: Vec<usize>,

    /// Uncompressed KiB per `SENT` block; the first is written, the others only measured.
    #[arg(long, hide = true, value_delimiter = ',', default_value = "16")]
    search_block_kib: Vec<usize>,

    /// Zooms of the spatial grid (`SGRD`); the first is written and must be 14, the others
    /// only measured.
    #[arg(long, hide = true, value_delimiter = ',', default_value = "14")]
    grid_zoom: Vec<u8>,

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
    ensure!(
        args.grid_zoom.first() == Some(&omp_core::grid::ZOOM)
            && args.grid_zoom.iter().all(|&z| (1..=16).contains(&z)),
        "--grid-zoom must start with {} and stay within 1–16",
        omp_core::grid::ZOOM
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
    let staging = staging_dir(&args.out)?;

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

    let subs = Subdivision::parse_list(&args.addresses)?;
    let address_opts = merge::Options {
        subdivisions: subs,
        sources: SourceOpts {
            sources: args.address_sources.clone(),
            exclude_license: args.exclude_license.clone(),
        },
        inputs: manifest_dir.to_owned(),
        attach_m: merge::ATTACH_M,
        dedup_m: merge::DEDUP_M,
    };

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
    let mut search = search::Builder::new();
    let mut collected = search::Collector::new(&wl);
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
        collected.add(&data.features);
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

    // Addresses, the street table they vote the postal towns of, and the address sections.
    let (ways, merged) = if address_opts.subdivisions.is_empty() {
        (addr::osm::read_streets(&args.input)?, None)
    } else {
        let osm = addr::osm::read(&args.input)?;
        log_stage("addresses read", &mut t);
        let (m, ways) = merge::merge(&address_opts, &admin, osm, &args.input)?;
        log_stage("addresses merged", &mut t);
        (ways, Some(m))
    };
    let mut table = Some(match &merged {
        Some(m) => street::Table::build(&ways, &admin, &m.votes, &m.cities.list),
        None => street::Table::build(&ways, &admin, &[], &[]),
    });
    drop(ways);
    let (street_sections, sgrd) = {
        let table = table.as_ref().unwrap();
        street::print_stats(&table.stats);
        search.add_streets(table, &admin);
        let loose: Vec<_> = merged
            .as_ref()
            .map(|m| m.unattached.iter().map(|u| u.p).collect())
            .unwrap_or_default();
        let mut sgrd = None;
        for &z in &args.grid_zoom {
            let (bytes, sizes) = grid::section(table, &loose, z, grid::BLOCK)?;
            grid::print_sizes(&sizes);
            sgrd.get_or_insert(bytes);
        }
        let sgrd = sgrd.unwrap();
        (street_encode::sections(table, street_encode::BLOCK)?, sgrd)
    };
    log_stage("streets", &mut t);
    let address_block = args
        .address_block_kib
        .first()
        .map_or(store::BLOCK, |k| k << 10);
    let mut address_rows = Vec::new();
    let (address_sections, postcodes, address_streets) = match merged {
        Some(mut m) => {
            m.votes = Vec::new();
            let attached = std::mem::take(&mut m.attached);
            let (inp, dropped) = store::place(table.as_ref().unwrap(), attached, &m);
            // The table and the attached records are not needed once placed.
            table = None;
            let (secs, mut sizes) = store::sections(&inp, address_block)?;
            sizes.ranges_dropped = dropped;
            print_address_sizes(&sizes);
            for &kib in args.address_block_kib.iter().skip(1) {
                print_address_sizes(&store::sections(&inp, kib << 10)?.1);
            }
            drop(inp);
            let streets = merge::address_streets(&m, &admin);
            let pcs = postcodes::combine(std::mem::take(&mut m.postcodes));
            for f in &m.files {
                match manifest.files.iter().find(|e| e.path == *f) {
                    Some(e) => sources.push(source(e)),
                    None => eprintln!("  warning: {f} is not in the manifest"),
                }
            }
            print_address_rows(&m.rows, &streets, pcs.len());
            address_rows = std::mem::take(&mut m.rows);
            (Some((secs, sizes)), pcs, streets)
        }
        None => (None, Vec::new(), Vec::new()),
    };
    drop(table);
    log_stage("addresses", &mut t);
    let index = search.finish(
        &admin,
        &wl,
        collected,
        &args.input,
        &postcodes,
        &address_streets,
    )?;
    drop(address_streets);
    search::print_stats(&index.stats);
    let block_kib = args
        .search_block_kib
        .first()
        .map_or(search_encode::BLOCK, |k| k << 10);
    let (search_sections, search_sizes) = search_encode::sections(&index, block_kib)?;
    print_search_sizes(&search_sizes);
    for &kib in args.search_block_kib.iter().skip(1) {
        print_search_sizes(&search_encode::sections(&index, kib << 10)?.1);
    }
    drop(index);
    log_stage("search", &mut t);
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
    settings.insert("street_tolerance_m".into(), street::SIMPLIFY_M.into());
    settings.insert("street_max_m".into(), street::STREET_MAX_M.into());
    settings.insert("grid_zoom".into(), omp_core::grid::ZOOM.into());
    settings.insert("search_stop_min".into(), search::STOP_MIN.into());
    settings.insert("search_block_kib".into(), (block_kib >> 10).into());
    settings.insert("postcodes".into(), postcodes.len().into());
    if let Some((_, sizes)) = &address_sections {
        settings.insert("addresses".into(), (sizes.addresses + sizes.points).into());
        settings.insert("address_block_kib".into(), (address_block >> 10).into());
        settings.insert("attach_m".into(), merge::ATTACH_M.into());
        settings.insert("dedup_m".into(), merge::DEDUP_M.into());
        let isos: Vec<String> = address_rows.iter().map(|r| r.iso.clone()).collect();
        settings.insert("address_subdivisions".into(), isos.join(",").into());
        if !args.exclude_license.is_empty() {
            settings.insert(
                "exclude_license".into(),
                args.exclude_license.join(",").into(),
            );
        }
    }
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

    let mut w = Writer::create(&staging, &file_name, args.chunk_capacity)?;
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
    let (sstr, sgeo, street_sizes) = street_sections;
    sections.push(w.section(street_encode::SSTR, &sstr)?);
    sections.push(w.section(street_encode::SGEO, &sgeo)?);
    sections.push(w.section(grid::SGRD, &sgrd)?);
    drop((sstr, sgeo, sgrd));
    let search_encode::Sections {
        sent,
        stwn,
        sfst,
        spst,
    } = search_sections;
    sections.push(w.section(search_encode::SENT, &sent)?);
    sections.push(w.section(search_encode::STWN, &stwn)?);
    sections.push(w.section(search_encode::SFST, &sfst)?);
    sections.push(w.section(search_encode::SPST, &spst)?);
    drop((sent, stwn, sfst, spst));
    let address_sizes = match address_sections {
        Some((secs, sizes)) => {
            for (id, bytes) in secs.list() {
                sections.push(w.section(id, &bytes)?);
            }
            Some(sizes)
        }
        None => None,
    };
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
    let paths = replace_pack(&staging, &args.out, &file_name, &paths)?;
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
    street::print_sizes(&street_sizes);
    print_search_sizes(&search_sizes);
    if let Some(s) = &address_sizes {
        print_address_sizes(s);
    }
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

fn print_search_sizes(s: &search_encode::Sizes) {
    let kinds: Vec<String> = s
        .raw_by_kind
        .iter()
        .map(|(k, b)| format!("{k:?} {:.2}", mib(*b)))
        .collect();
    eprintln!(
        "  search: {} KiB blocks: SENT {:.2} MiB ({:.2} raw: {}; street names {:.2}; {} \
         blocks), {:.2} MiB without street names; STWN {:.2} MiB, SFST {:.2} MiB, SPST {:.2} \
         MiB; {:.1} bytes per entry in all",
        s.block_kib,
        mib(s.sent),
        mib(s.sent_raw),
        kinds.join(", "),
        mib(s.street_names_raw),
        s.sent_blocks,
        mib(s.sent_without_street_names),
        mib(s.stwn),
        mib(s.sfst),
        mib(s.spst),
        (s.sent + s.stwn + s.sfst + s.spst) as f64 / s.entries.max(1) as f64
    );
}

fn print_address_sizes(s: &store::Sizes) {
    eprintln!(
        "  addresses: {} KiB blocks: {} on {} streets, {} unattached, {} ranges ({} dropped), \
         {} codes, {} exceptions; ASTR {:.2} MiB ({:.2} raw, {} blocks, {:.0} µs to decode \
         one), ASUF {:.2}, AUNI {:.2}, ALSE {:.2} ({} blocks), ARNG {:.2}, APCD {:.2}; {:.2} \
         MiB, {:.2} bytes per address",
        s.block_kib,
        s.addresses,
        s.streets_with_addresses,
        s.points,
        s.ranges,
        s.ranges_dropped,
        s.codes,
        s.exceptions,
        mib(s.astr),
        mib(s.astr_raw),
        s.astr_blocks,
        s.decode_us_per_block,
        mib(s.asuf),
        mib(s.auni),
        mib(s.alse),
        s.alse_blocks,
        mib(s.arng),
        mib(s.apcd),
        mib(s.total()),
        (s.total() - s.arng) as f64 / (s.addresses + s.points).max(1) as f64
    );
}

/// Totals of the per-subdivision merge lines.
fn print_address_rows(rows: &[merge::SubRow], streets: &[search::AddressStreet], postcodes: usize) {
    let sum = |f: &dyn Fn(&merge::SubRow) -> u64| rows.iter().map(f).sum::<u64>();
    let (records, addresses, attached) = (
        sum(&|r| r.records),
        sum(&|r| r.addresses),
        sum(&|r| r.attached),
    );
    let by: Vec<u64> = (0..4).map(|k| sum(&|r| r.by[k])).collect();
    eprintln!(
        "  addresses: {} subdivisions, {records} records ({} strict, {} loose, {} alias, {} \
         fuzzy, {} unattached) → {addresses} addresses, {attached} attached ({:.2}%), {} \
         approximate, {} units, {} of {} ranges kept; {} streets known only from {} \
         addresses, {postcodes} postcodes",
        rows.len(),
        by[0],
        by[1],
        by[2],
        by[3],
        sum(&|r| r.unattached_records),
        100.0 * attached as f64 / addresses.max(1) as f64,
        sum(&|r| r.approximate),
        sum(&|r| r.units),
        sum(&|r| r.fallback_ranges),
        sum(&|r| r.ranges),
        streets.len(),
        streets.iter().map(|s| s.addresses).sum::<u64>(),
    );
}

fn file_label(p: &Path) -> String {
    p.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

/// `<out>/.building`, emptied: the new pack is written there and moved into `<out>` only
/// once it is complete, so a build that fails leaves the old pack as it was. A failed build's
/// leftovers are cleared here, by the next one.
fn staging_dir(out: &Path) -> Result<PathBuf> {
    let dir = out.join(".building");
    match std::fs::remove_dir_all(&dir) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            return Err(e).with_context(|| format!("clear {}", dir.display()));
        }
        _ => {}
    }
    std::fs::create_dir(&dir).with_context(|| format!("create {}", dir.display()))?;
    Ok(dir)
}

/// Moves the finished chunks `new` from `staging` into `out`, over the old pack's, then deletes
/// the old pack's extra chunks, so a smaller pack does not leave stale ones behind. Returns
/// the chunks' new paths.
fn replace_pack(staging: &Path, out: &Path, name: &str, new: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut moved = Vec::with_capacity(new.len());
    for p in new {
        let to = out.join(p.file_name().context("chunk without a file name")?);
        std::fs::rename(p, &to)
            .with_context(|| format!("move {} to {}", p.display(), to.display()))?;
        moved.push(to);
    }
    let prefix = format!("{name}.omp.");
    for e in std::fs::read_dir(out)? {
        let e = e?;
        let file = e.file_name();
        let Some(rest) = file.to_str().and_then(|f| f.strip_prefix(&prefix)) else {
            continue;
        };
        let index = Some(rest)
            .filter(|r| r.len() == 3)
            .and_then(|r| r.parse::<usize>().ok());
        if index.is_some_and(|i| i >= new.len()) {
            std::fs::remove_file(e.path())?;
        }
    }
    std::fs::remove_dir(staging).with_context(|| format!("remove {}", staging.display()))?;
    Ok(moved)
}

/// The first chunk of the pack at `path`: the `.omp.000` file itself, or the only one in a
/// directory.
pub fn first_chunk(path: &Path) -> Result<PathBuf> {
    if !path.is_dir() {
        return Ok(path.to_owned());
    }
    let mut found = pack::first_chunks(path);
    match found.len() {
        1 => Ok(found.remove(0)),
        0 => bail!("no .omp.000 file in {}", path.display()),
        _ => bail!(
            "{} holds several packs; name one of the .omp.000 files",
            path.display()
        ),
    }
}

/// A one-chunk pack of `sections` in `dir`, opened, for tests of the app's readers against
/// the builder's encoders.
#[cfg(test)]
pub fn test_pack(dir: &Path, sections: &[([u8; 4], &[u8])]) -> std::sync::Arc<pack::Pack> {
    let mut w = Writer::create(dir, "t", DEFAULT_CHUNK_CAPACITY).unwrap();
    for (id, b) in sections {
        w.section(*id, b).unwrap();
    }
    let info = Info {
        pack_id: [1; 16],
        bbox: [0; 4],
        build_time: 0,
        name: "Test".into(),
        snapshot: String::new(),
    };
    let paths = w.finish(&info).unwrap();
    std::sync::Arc::new(pack::Pack::open(&paths[0]).unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_build_keeps_the_old_pack() {
        let out = tempfile::tempdir().unwrap();
        let out = out.path();
        let build = |chunks: usize| {
            let staging = staging_dir(out).unwrap();
            let mut w = Writer::create(&staging, "p", pack::PAGE).unwrap();
            for i in 0..chunks {
                w.section([b'A', b'B', b'C', b'0' + i as u8], &[i as u8; 4000])
                    .unwrap();
            }
            let info = Info {
                pack_id: [chunks as u8; 16],
                bbox: [0; 4],
                build_time: 0,
                name: "P".into(),
                snapshot: String::new(),
            };
            (staging, w.finish(&info).unwrap())
        };
        let open = || pack::Pack::open(&out.join("p.omp.000")).unwrap();

        let (staging, paths) = build(3);
        replace_pack(&staging, out, "p", &paths).unwrap();
        assert_eq!(open().chunks(), 4);

        // A build that stops before `replace_pack` leaves the old pack openable...
        let _ = build(1);
        assert_eq!(open().info.pack_id, [3; 16]);

        // ...and the next one clears its leftovers, and the old pack's extra chunks.
        let (staging, paths) = build(1);
        replace_pack(&staging, out, "p", &paths).unwrap();
        assert_eq!(open().info.pack_id, [1; 16]);
        let mut left: Vec<String> = std::fs::read_dir(out)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(left, ["p.omp.000", "p.omp.001"]);
    }

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
