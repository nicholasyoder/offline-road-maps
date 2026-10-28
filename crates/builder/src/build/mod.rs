//! `omp-builder build`: cuts a PBF into a V4 tile pyramid and writes it as a pack with
//! `META`, `SCHM`, `TDIC`, `TDAT` and `TIDX` (see `docs/format.md`). [`run`] lists the
//! stages:
//!
//! 1. Admin areas (`SADM`, `SAPL`), so every place and POI can be given its chain of admin
//!    areas (for now only the coverage is reported).
//! 2. [`read_bands`]: the whitelisted features ([`features::read`]: needed-nodes store, ring
//!    assembly, and the alternative names search wants), cut into z6 split tiles. `--bands
//!    n` cuts the pyramid in `n` passes, freeing the features in between, and every band
//!    appends its V2 payloads to one temp store.
//! 3. [`addresses_and_streets`], after the bands so their memory is free: the addresses of
//!    `--addresses` merged per state and province ([`merge`], step 2.4), then the street
//!    table (`SSTR`, `SGEO`), split where the postal town of its addresses changes, at
//!    admin 8 boundaries where it has none, and at the length cap. The addresses are placed
//!    on it and written as `ASTR`, `ASUF`, `AUNI`, `ALSE`, `ARNG` and `APCD`
//!    ([`store`]). The spatial grid (`SGRD`) lists the streets and unattached addresses in
//!    each z14 cell.
//! 4. [`search_index`] (`SENT`, `STWN`, `SFST`, `SPST`): its streets from the table before
//!    it is dropped, its places, POIs and labels from the bands, and its postcodes and the
//!    streets known only from their addresses from the merge.
//! 5. The car routing graph ([`graph::make`], step 3.1), its own PBF passes after the
//!    address and search stages have freed their memory, and its contraction hierarchy
//!    ([`ch::contract`], step 3.2), encoded as the routing sections ([`routing::sections`],
//!    step 3.3, with segments named by the street table's way map) and measured against
//!    other layouts ([`routing::layout`]).
//! 6. [`write_pack`]: dictionaries trained once from the whole tile store, then tiles
//!    compressed and written in zoom, then Hilbert order, then the other sections, the
//!    routing ones last.
//!
//! The PBF is read through one [`Pbf`], so each pass reads only the blobs of the element
//! types it needs. With `--reuse-stages`, the admin areas, the bands, the address merge,
//! the routing graph and its hierarchy are saved in and reused from a cache ([`cache`]).
//!
//! `META`'s sources and settings are in [`meta`], and what `build` prints in [`report`].

mod cache;
pub mod inspect;
mod meta;
pub(crate) mod readers;
mod report;
mod route_readers;

use std::fs::File;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use anyhow::{Context, Result, bail, ensure};
use clap::Args;
use omp_core::meta::{self as pack_meta, Meta};
use omp_core::pack::{self, DEFAULT_CHUNK_CAPACITY, Info, Writer};
use omp_core::schema::{self, Schema};
use omp_core::tile::hilbert;
use omp_core::tiles::{FrameCompressor, LEVEL, TileWriter};
use omp_core::whitelist::Whitelist;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use xxhash_rust::xxh3::{Xxh3, xxh3_128};

use crate::addr::postcodes::{self, Postcode};
use crate::addr::{self, Input, SourceOpts, merge, store};
use crate::admin::{self, Admin, encode as admin_encode};
use crate::fetch::regions::Subdivision;
use crate::mib;
use crate::pbf::{self, Pbf, features};
use crate::peak_rss_mib;
use crate::poly::Poly;
use crate::routing::{self, layout};
use crate::search::{self, AddressStreet, encode as search_encode};
use crate::street::{self, encode as street_encode, grid};
use crate::tiles::{self, Band, Entry, Store, compress, water};
use crate::{ch, graph};
use cache::{Cache, Key};
use meta::Provenance;
use report::{ChainStats, Timer, ZoomTotals};

/// Tiles compressed in parallel before they are written in order.
const BATCH_TILES: usize = 16_384;
/// Random pairs, long and short, the routing layouts are compared on.
const LAYOUT_PAIRS: usize = 500;

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

    /// Also print the sizes of the address and `SENT` sections at 4 and 32 KiB blocks, and of
    /// the spatial grid at z13 and z15 (phase 2 steps 2.4–2.6); the pack is the same.
    #[arg(long, hide = true)]
    measure_blocks: bool,

    /// Reuse the admin areas, the merged addresses and the band tiles of an earlier build
    /// with the same input and options, from `<temp>/cache/`, and save them there when there
    /// are none. For dev loops: the key does not cover the code, so bump a stage's version in
    /// `build/cache.rs` after changing what it outputs.
    #[arg(long)]
    reuse_stages: bool,

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

/// What [`run`] works out from the arguments before it reads anything.
struct Setup {
    pbf: Pbf,
    wl: Whitelist,
    /// The region name shown in the app.
    name: String,
    /// The chunk files' name: `<file_name>.omp.000`, …
    file_name: String,
    staging: PathBuf,
    /// The temporary tile store.
    store_path: PathBuf,
    /// West, south, east, north, from the PBF header.
    bounds: [f64; 4],
    water: Option<PathBuf>,
    clip: Option<Poly>,
    /// Regions the route coverage is reported by.
    regions: Vec<(String, Poly)>,
    addresses: merge::Options,
    provenance: Provenance,
    /// With `--reuse-stages`.
    stages: Option<Stages>,
}

/// The stage cache and the keys of the stages it holds.
struct Stages {
    cache: Cache,
    admin: Key,
    tiles: Key,
    addresses: Key,
    graph: Key,
    ch: Key,
}

impl Stages {
    fn new(args: &BuildArgs, temp: &Path, wl_text: &str, opts: &merge::Options) -> Result<Self> {
        let cache = Cache::new(temp, &args.input)?;
        let file = |p: &Option<PathBuf>| p.as_deref().map(cache::fingerprint).transpose();
        let admin = format!("{}", cache::ADMIN);
        let tiles = format!(
            "{admin} {}\n{:032x}\n{} {:?} {:?}",
            cache::TILES,
            xxh3_128(wl_text.as_bytes()),
            args.bands,
            file(&args.water)?,
            file(&args.clip_poly)?,
        );
        let addresses = format!(
            "{admin} {}\n{:?}\n{:?} {:?} {} {} {}\n{}",
            cache::ADDRESSES,
            opts.subdivisions,
            opts.sources.sources,
            opts.sources.exclude_license,
            opts.inputs.display(),
            opts.attach_m,
            opts.dedup_m,
            cache::fingerprint(&opts.inputs.join("manifest.toml"))?,
        );
        Ok(Stages {
            admin: cache.key("admin", &admin),
            tiles: cache.key("tiles", &tiles),
            addresses: cache.key("addresses", &addresses),
            graph: cache.key("graph", &format!("{}", cache::GRAPH)),
            ch: cache.key("ch", &format!("{} {}", cache::GRAPH, cache::CH)),
            cache,
        })
    }

    /// The cache and the key `key` picks, with `--reuse-stages`.
    fn at(s: &Option<Stages>, key: impl Fn(&Stages) -> &Key) -> Option<(&Cache, &Key)> {
        s.as_ref().map(|s| (&s.cache, key(s)))
    }
}

impl Setup {
    fn new(args: &BuildArgs, work_dir: &Path) -> Result<Self> {
        ensure!(
            (1..=64).contains(&args.bands),
            "--bands must be between 1 and 64"
        );
        let wl_text = match &args.whitelist {
            Some(p) => {
                std::fs::read_to_string(p).with_context(|| format!("read {}", p.display()))?
            }
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
        std::fs::create_dir_all(&args.out)
            .with_context(|| format!("create {}", args.out.display()))?;
        let staging = staging_dir(&args.out)?;

        let water = args
            .water
            .clone()
            .unwrap_or_else(|| work_dir.join("inputs/osmdata/water-polygons-split-4326.zip"));
        let water = water.exists().then_some(water);
        let manifest_path = args
            .manifest
            .clone()
            .unwrap_or_else(|| work_dir.join("inputs/manifest.toml"));
        let provenance = Provenance::load(&manifest_path, &args.input, water.as_deref())?;
        let bounds = pbf::header_bounds(&args.input)?
            .context("the PBF header has no bounding box; the pack needs one")?;
        let clip = args.clip_poly.as_deref().map(Poly::read).transpose()?;
        let addresses = merge::Options {
            subdivisions: Subdivision::parse_list(&args.addresses)?,
            sources: SourceOpts {
                sources: args.address_sources.clone(),
                exclude_license: args.exclude_license.clone(),
            },
            inputs: provenance.dir.clone(),
            attach_m: merge::ATTACH_M,
            dedup_m: merge::DEDUP_M,
        };
        let regions = load_regions(
            &args
                .route_regions
                .clone()
                .unwrap_or_else(|| work_dir.join("inputs/poly")),
        )?;
        let stages = match args.reuse_stages {
            true => Some(Stages::new(args, &temp, &wl_text, &addresses)?),
            false => None,
        };
        let store_path = match &stages {
            Some(s) => s.cache.file(&s.tiles, "v2"),
            None => temp.join(format!("{file_name}-build.v2")),
        };
        Ok(Self {
            pbf: Pbf::open(&args.input)?,
            wl,
            name,
            store_path,
            file_name,
            staging,
            bounds,
            water,
            clip,
            regions,
            addresses,
            provenance,
            stages,
        })
    }
}

pub fn run(args: BuildArgs, work_dir: &Path) -> Result<()> {
    let start = Instant::now();
    let mut setup = Setup::new(&args, work_dir)?;
    eprintln!("build {}: {}", setup.name, args.input.display());
    let mut t = Timer::new();
    let admin = admin::load_from(|| {
        cache::stage(Stages::at(&setup.stages, |s| &s.admin), || {
            admin::read::read(&setup.pbf)
        })
    })?;
    t.stage("admin");
    if let Some(s) = &setup.stages
        && !setup.store_path.exists()
    {
        s.cache.forget(&s.tiles);
    }
    let bands = cache::stage(Stages::at(&setup.stages, |s| &s.tiles), || {
        read_bands(&args, &setup, &admin, &mut t)
    })?;
    let mut search = search::Builder::new();
    let mut streets = addresses_and_streets(&args, &setup, &admin, &mut search, &mut t)?;
    if let Some(a) = &streets.addresses {
        setup.provenance.add_files(&a.files);
    }
    let (search_sections, search_sizes) = search_index(
        &args,
        &setup,
        &admin,
        search,
        bands.collected,
        &mut streets,
        &mut t,
    )?;
    let routing = cache::stage(Stages::at(&setup.stages, |s| &s.graph), || {
        graph::make(&setup.pbf)
    })?;
    graph::print_stats(&routing.stats);
    t.stage("routing graph");
    let hierarchy = cache::stage(Stages::at(&setup.stages, |s| &s.ch), || {
        Ok(ch::contract(&routing.csr, &ch::Params::default()))
    })?;
    ch::print_stats(&hierarchy.stats);
    t.stage("routing CH");
    let way_streets = std::mem::take(&mut streets.way_streets);
    let (route_sections, route_sizes, order, lens) = routing::sections(
        &routing,
        &hierarchy,
        &way_streets,
        &routing::Params::default(),
    )?;
    drop(way_streets);
    routing::print_sizes(&route_sizes);
    t.stage("routing sections");
    let pairs = [
        ("long", ch::check::pairs(&routing, LAYOUT_PAIRS, 1)),
        (
            "short",
            layout::short_pairs(&routing, &order, LAYOUT_PAIRS, 2),
        ),
    ];
    for (name, p) in &pairs {
        layout::print(&layout::compare(
            &routing, &hierarchy, &order, &lens, p, name,
        ));
    }
    drop((routing, hierarchy, order, lens));
    t.stage("routing layout");
    let build_time = match args.deterministic {
        true => 0,
        false => jiff::Timestamp::now().as_second(),
    };
    let meta = meta::meta(
        &setup.name,
        setup.bounds,
        build_time,
        std::mem::take(&mut setup.provenance.sources),
        &meta::Settings {
            wl: &setup.wl,
            zstd_level: args.zstd_level,
            postcodes: streets.postcodes.len(),
            addresses: streets.addresses.as_ref().map(|a| (&a.sizes, &a.rows[..])),
            exclude_license: &args.exclude_license,
            input: &args.input,
            water: setup.water.as_deref(),
            clip_poly: args.clip_poly.as_deref(),
        },
    );
    let address_sizes = streets.addresses.as_ref().map(|a| a.sizes.clone());
    let (sadm, sapl, admin_sizes) = admin_encode::sections(&admin)?;
    drop(admin);
    let (sstr, sgeo, street_sizes) = streets.sections;
    let mut sections = vec![
        (admin_encode::SADM, sadm),
        (admin_encode::SAPL, sapl),
        (street_encode::SSTR, sstr),
        (street_encode::SGEO, sgeo),
        (grid::SGRD, streets.sgrd),
    ];
    sections.extend(search_sections.list());
    if let Some(a) = streets.addresses {
        sections.extend(a.sections.list());
    }
    let written = write_pack(
        &args,
        &setup,
        bands.entries,
        meta,
        sections,
        &route_sections,
        &mut t,
    )?;
    drop(route_sections);

    report::print_zooms(&written.zooms);
    report::print_left_out(&bands.cut, &setup.wl);
    eprintln!(
        "  admin: {} areas, SADM {:.2} MiB, SAPL {:.2} MiB ({} blocks)",
        admin_sizes.areas,
        mib(admin_sizes.sadm),
        mib(admin_sizes.sapl),
        admin_sizes.sapl_blocks
    );
    bands.chains.print();
    street::print_sizes(&street_sizes);
    routing::print_sizes(&route_sizes);
    report::print_search_sizes(&search_sizes);
    if let Some(s) = &address_sizes {
        report::print_address_sizes(s);
    }
    eprintln!(
        "wrote {} in {} chunk(s), {:.1} MiB (tiles {} B, TIDX {} B, TDIC {} dictionaries); \
         {:.0} s, peak RSS {:.0} MiB, temp store {:.1} MiB",
        args.out
            .join(format!("{}.omp.000", setup.file_name))
            .display(),
        written.chunks,
        mib(written.bytes),
        written.tdat,
        written.tidx,
        written.dictionaries,
        start.elapsed().as_secs_f64(),
        peak_rss_mib(),
        mib(bands.store_bytes)
    );
    Ok(())
}
/// What the bands leave for the later stages.
#[derive(Serialize, Deserialize)]
struct Bands {
    /// The tile store's entries, sorted, and its size.
    entries: Vec<Entry>,
    store_bytes: u64,
    cut: tiles::Stats,
    chains: ChainStats,
    collected: search::Collector,
}

/// Reads the features band by band, adds the ocean polygons, and cuts each band's tiles into
/// the store. Each band's features are freed before the next is read.
fn read_bands(args: &BuildArgs, setup: &Setup, admin: &Admin, t: &mut Timer) -> Result<Bands> {
    let wl = &setup.wl;
    let mut store = Store::create(&setup.store_path)?;
    let mut bands = Bands {
        entries: Vec::new(),
        store_bytes: 0,
        cut: tiles::Stats::default(),
        chains: ChainStats::default(),
        collected: search::Collector::new(wl),
    };
    let keep_tags = bands.collected.keep_tags();
    // The nodes the ways need, from band 0's read.
    let mut marked = None;
    for k in 0..args.bands {
        let band = Band { k, n: args.bands };
        if args.bands > 1 {
            eprintln!(" band {k}/{}", args.bands);
        }
        let mut data = features::read(
            &setup.pbf,
            wl,
            features::Strategy::Sparse(&mut marked),
            false,
            band,
            &setup.regions,
            Some(&keep_tags),
        )?;
        data.roads = Vec::new();
        t.stage("read");
        if k == 0 {
            report::print_routes(&data.stats.routes);
        }
        if let (Some(zip), Some(bbox)) = (&setup.water, data.bbox) {
            let layer = wl.layer_index("water").context("no water layer")?;
            let class = wl.layers[layer]
                .classes
                .iter()
                .position(|c| c.external && c.name == "ocean")
                .context("no external ocean class in the water layer")?;
            let (feats, _, _) =
                water::read(zip, bbox, setup.clip.as_ref(), layer as u8, class as u16)?;
            let minzoom = tiles::minzooms(wl);
            data.features.extend(feats.into_iter().filter(|f| {
                band.keeps(f.geom.bbox(), minzoom[f.layer as usize][f.class as usize])
            }));
            t.stage("water");
        }
        bands.chains.add(&data.features, wl, admin);
        bands.collected.add(&data.features, &data.kept_tags);
        let cut = tiles::cut_pyramid(&data.features, wl, &mut store, band, false);
        bands.cut.merge(cut);
        drop(data);
        t.stage("tiles");
    }
    (bands.entries, bands.store_bytes) = store.finish()?;
    Ok(bands)
}

/// The street table's sections and the spatial grid, the address sections if any, and what
/// search needs from the merge.
struct Streets {
    sections: (Vec<u8>, Vec<u8>, street_encode::Sizes),
    sgrd: Vec<u8>,
    /// The street of each piece of each way, to name the routing segments.
    way_streets: street::WayStreets,
    addresses: Option<Addresses>,
    postcodes: Vec<Postcode>,
    address_streets: Vec<AddressStreet>,
}

struct Addresses {
    sections: store::Sections,
    sizes: store::Sizes,
    rows: Vec<merge::SubRow>,
    /// Manifest paths of the address files read.
    files: Vec<String>,
}

/// Merges the addresses, if any, builds the street table with their postal towns, adds its
/// streets to `search`, and places the addresses on it. The table and the merge are freed
/// before returning.
fn addresses_and_streets(
    args: &BuildArgs,
    setup: &Setup,
    admin: &Admin,
    search: &mut search::Builder,
    t: &mut Timer,
) -> Result<Streets> {
    let (ways, merged) = cache::stage(Stages::at(&setup.stages, |s| &s.addresses), || {
        if setup.addresses.subdivisions.is_empty() {
            return Ok((addr::osm::read_streets(&setup.pbf)?, None));
        }
        let osm = addr::osm::read(&setup.pbf)?;
        t.stage("addresses read");
        let (m, ways) = merge::merge(&setup.addresses, admin, osm)?;
        t.stage("addresses merged");
        Ok((ways, Some(m)))
    })?;
    let table = match &merged {
        Some(m) => street::Table::build(&ways, admin, &m.votes, &m.cities.list),
        None => street::Table::build(&ways, admin, &[], &[]),
    };
    let way_streets = table.way_streets(&ways);
    drop(ways);
    street::print_stats(&table.stats);
    search.add_streets(&table, admin);
    let loose: Vec<_> = merged
        .as_ref()
        .map(|m| m.unattached.iter().map(|u| u.p).collect())
        .unwrap_or_default();
    let (sgrd, sizes) = grid::section(&table, &loose, omp_core::grid::ZOOM, grid::BLOCK)?;
    grid::print_sizes(&sizes);
    if args.measure_blocks {
        for z in [13, 15] {
            grid::print_sizes(&grid::section(&table, &loose, z, grid::BLOCK)?.1);
        }
    }
    drop(loose);
    let sections = street_encode::sections(&table, street_encode::BLOCK)?;
    t.stage("streets");

    let Some(mut m) = merged else {
        drop(table);
        t.stage("addresses");
        return Ok(Streets {
            sections,
            sgrd,
            way_streets,
            addresses: None,
            postcodes: Vec::new(),
            address_streets: Vec::new(),
        });
    };
    m.votes = Vec::new();
    let attached = std::mem::take(&mut m.attached);
    let (inp, dropped) = store::place(&table, attached, &m);
    // The table and the attached records are not needed once placed.
    drop(table);
    let (secs, mut sizes) = store::sections(&inp, store::BLOCK)?;
    sizes.ranges_dropped = dropped;
    report::print_address_sizes(&sizes);
    if args.measure_blocks {
        for kib in [4, 32] {
            report::print_address_sizes(&store::sections(&inp, kib << 10)?.1);
        }
    }
    drop(inp);
    let address_streets = merge::address_streets(&m, admin);
    let postcodes = postcodes::combine(std::mem::take(&mut m.postcodes));
    report::print_address_rows(&m.rows, &address_streets, postcodes.len());
    let addresses = Addresses {
        sections: secs,
        sizes,
        rows: std::mem::take(&mut m.rows),
        files: std::mem::take(&mut m.files),
    };
    drop(m);
    t.stage("addresses");
    Ok(Streets {
        sections,
        sgrd,
        way_streets,
        addresses: Some(addresses),
        postcodes,
        address_streets,
    })
}

/// The search sections, from the streets added to `search`, the features `collected` from
/// the bands, and the postcodes and address-only streets of `streets` (which it frees).
fn search_index(
    args: &BuildArgs,
    setup: &Setup,
    admin: &Admin,
    search: search::Builder,
    collected: search::Collector,
    streets: &mut Streets,
    t: &mut Timer,
) -> Result<(search_encode::Sections, search_encode::Sizes)> {
    let address_streets = std::mem::take(&mut streets.address_streets);
    let index = search.finish(
        admin,
        &setup.wl,
        collected,
        &streets.postcodes,
        &address_streets,
    )?;
    drop(address_streets);
    search::print_stats(&index.stats);
    let (sections, sizes) = search_encode::sections(&index, search_encode::BLOCK)?;
    report::print_search_sizes(&sizes);
    if args.measure_blocks {
        for kib in [4, 32] {
            report::print_search_sizes(&search_encode::sections(&index, kib << 10)?.1);
        }
    }
    drop(index);
    t.stage("search");
    Ok((sections, sizes))
}

/// What [`write_pack`] wrote.
struct Written {
    chunks: usize,
    bytes: u64,
    zooms: Vec<ZoomTotals>,
    tdat: u64,
    tidx: u64,
    dictionaries: usize,
}

/// Trains the dictionaries on the tile store, then writes `META`, `SCHM`, the tiles,
/// `sections` and the routing sections into the staging directory, and moves the finished
/// pack into `--out`.
fn write_pack(
    args: &BuildArgs,
    setup: &Setup,
    mut entries: Vec<Entry>,
    meta: Meta,
    sections: Vec<([u8; 4], Vec<u8>)>,
    route_sections: &routing::Sections,
    t: &mut Timer,
) -> Result<Written> {
    let store_path = &setup.store_path;
    let store_file =
        File::open(store_path).with_context(|| format!("open {}", store_path.display()))?;
    let layer_names: Vec<String> = setup.wl.layers.iter().map(|l| l.name.clone()).collect();
    let (dicts, _) = compress::train(&store_file, &entries, &layer_names);
    t.stage("dictionaries");

    let mut w = Writer::create(&setup.staging, &setup.file_name, args.chunk_capacity)?;
    let mut written = vec![
        w.section(pack_meta::ID, &meta.encode()?)?,
        w.section(schema::ID, &Schema::from_whitelist(&setup.wl).encode()?)?,
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
    let mut zooms = vec![ZoomTotals::default(); tiles::MAX_ZOOM as usize + 1];
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
    written.extend([tdat, tidx]);
    for (id, bytes) in sections {
        written.push(w.section(id, &bytes)?);
    }
    written.extend(route_sections.write(&mut w)?);
    drop(store_file);
    if setup.stages.is_none() {
        let _ = std::fs::remove_file(store_path);
    }
    t.stage("compress");

    let pack_id = match args.deterministic {
        true => {
            let mut h = Xxh3::new();
            for s in &written {
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
        bbox: setup.bounds.map(e7),
        build_time: meta.build_time,
        name: setup.name.clone(),
        snapshot: setup.provenance.snapshot.clone(),
    };
    let paths = w.finish(&info)?;
    let paths = replace_pack(&setup.staging, &args.out, &setup.file_name, &paths)?;
    t.stage("write");
    let bytes = paths
        .iter()
        .map(|p| std::fs::metadata(p).map(|m| m.len()))
        .sum::<std::io::Result<u64>>()?;
    Ok(Written {
        chunks: paths.len(),
        bytes,
        zooms,
        tdat: tdat.len,
        tidx: tidx.len,
        dictionaries: dicts.0.values().filter(|d| !d.is_empty()).count(),
    })
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
    }
}
