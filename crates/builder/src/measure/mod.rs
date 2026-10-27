//! `omp-builder measure`: the Phase 0 builder spike. Reads a PBF through the tag whitelist,
//! cuts a z0–z14 tile pyramid, and reports feature counts, vertex counts and bytes for every
//! layer × zoom × encoding variant, plus routing and search size estimates. A thin command
//! over the production modules ([`crate::pbf`], [`crate::tiles`]); only the routing and
//! search estimates are its own.

mod routing;
mod search;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use clap::{Args, ValueEnum};
use omp_core::whitelist::{GeometryKind, Whitelist};
use serde::Serialize;

use crate::pbf::features as osm;
use crate::peak_rss_mib;
use crate::poly;
use crate::tiles::{self, Band, Cell, Variant, compress, water};

fn parse_band(s: &str) -> Result<Band, String> {
    Band::parse(s).ok_or_else(|| format!("{s:?} is not k/n with k < n"))
}

#[derive(Args)]
pub struct MeasureArgs {
    /// OSM extract (.osm.pbf).
    input: PathBuf,

    /// Tag whitelist [default: the one built into omp-core].
    #[arg(long)]
    whitelist: Option<PathBuf>,

    /// Write the results here as JSON.
    #[arg(long)]
    json: PathBuf,

    /// Name in the report [default: input file name up to the first '-'].
    #[arg(long)]
    region: Option<String>,

    /// Node location strategy.
    #[arg(long, value_enum, default_value_t = NodeStrategy::Sparse)]
    nodes: NodeStrategy,

    /// File for `--nodes dense`. Must be on a filesystem with sparse files (ext4, not exFAT).
    #[arg(long)]
    node_index: Option<PathBuf>,

    /// osmdata.openstreetmap.de water polygons [default: <work dir>/inputs/osmdata/water-polygons-split-4326.zip, skipped if missing].
    #[arg(long)]
    water: Option<PathBuf>,

    /// Keep only the ocean pieces that meet this boundary (`.poly`, e.g. from `fetch --sources
    /// poly`), not every piece in the PBF's bbox. Matters for a continent, whose bbox holds
    /// whole oceans.
    #[arg(long)]
    clip_poly: Option<PathBuf>,

    /// Cut only band `k` of `n` (`k/n`): the z6 tile columns with `x % n == k`, plus z0–z5
    /// in band 0, keeping only the features they need. For regions too big for one run;
    /// `report` adds up runs of the same region that differ only in band. Each band reads
    /// every node location, so this saves feature memory only.
    #[arg(long, value_parser = parse_band)]
    band: Option<Band>,

    /// Directory for the temporary tile store [default: <work dir>/tmp].
    #[arg(long)]
    temp: Option<PathBuf>,

    #[arg(long, default_value_t = 19)]
    zstd_level: i32,

    /// Routing estimate: build the contraction hierarchy, only count the road graph (for a
    /// continent, where the CH does not fit), or skip it.
    #[arg(long, value_enum, default_value_t = Routing::Ch)]
    routing: Routing,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Routing {
    Ch,
    Graph,
    None,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum, Serialize)]
#[serde(rename_all = "lowercase")]
enum NodeStrategy {
    /// Needed-nodes-only: bitset of referenced IDs, then a sorted array of their locations.
    Sparse,
    /// One slot per node ID in a sparse mmap file.
    Dense,
}

#[derive(Serialize)]
struct Report {
    region: String,
    /// `[k, n]` for a `--band` run.
    band: Option<[u32; 2]>,
    input: String,
    input_bytes: u64,
    whitelist_version: u32,
    node_strategy: NodeStrategy,
    zstd_level: i32,
    threads: usize,
    build: Build,
    read: osm::ReadStats,
    water: Option<WaterStats>,
    classes: Vec<ClassRow>,
    tag_values: BTreeMap<String, BTreeMap<String, u64>>,
    /// Bytes per layer × zoom × variant. V0–V2 count features and vertices; V3–V5 only bytes.
    cells: Vec<CellRow>,
    mvt: Vec<MvtRow>,
    dictionaries: Vec<compress::DictInfo>,
    tile_index: Vec<compress::IndexStats>,
    random_read: Vec<compress::ReadBench>,
    routing: Option<routing::RoutingStats>,
    search: search::SearchStats,
}

#[derive(Serialize)]
struct Build {
    wall_s: f64,
    peak_rss_mib: f64,
    stages: Vec<Stage>,
    node_index_disk_bytes: u64,
    tile_store_bytes: u64,
}

#[derive(Serialize)]
struct Stage {
    name: &'static str,
    secs: f64,
    /// Peak RSS so far, read after the stage.
    peak_rss_mib: f64,
}

#[derive(Serialize)]
struct WaterStats {
    file: String,
    clip_poly: Option<String>,
    records_scanned: u64,
    /// Pieces in the bbox but not meeting `clip_poly`, dropped.
    outside_poly: u64,
    polygons: u64,
}

#[derive(Serialize)]
struct ClassRow {
    layer: String,
    class: String,
    tile: bool,
    minzoom: u8,
    #[serde(flatten)]
    counts: osm::ClassCount,
    features: u64,
    vertices: u64,
    z14_features: u64,
    z14_v1_bytes: u64,
}

#[derive(Serialize)]
struct CellRow {
    variant: Variant,
    layer: String,
    zoom: u8,
    #[serde(flatten)]
    cell: Cell,
}

#[derive(Serialize)]
struct MvtRow {
    zoom: u8,
    tiles: u64,
    bytes: u64,
    gzip_bytes: u64,
}

struct Stages {
    list: Vec<Stage>,
    t: Instant,
}

impl Stages {
    fn done(&mut self, name: &'static str) {
        let secs = self.t.elapsed().as_secs_f64();
        let rss = peak_rss_mib();
        eprintln!("  {name}: {secs:.1} s, peak RSS {rss:.0} MiB");
        self.list.push(Stage {
            name,
            secs,
            peak_rss_mib: rss,
        });
        self.t = Instant::now();
    }
}

pub fn run(args: MeasureArgs, work_dir: &Path) -> Result<()> {
    let start = Instant::now();
    let wl_text = match &args.whitelist {
        Some(p) => std::fs::read_to_string(p).with_context(|| format!("read {}", p.display()))?,
        None => omp_core::whitelist::DEFAULT.to_owned(),
    };
    let wl = Whitelist::parse(&wl_text).context("parse whitelist")?;
    let region = args.region.clone().unwrap_or_else(|| {
        let name = args.input.file_name().unwrap_or_default().to_string_lossy();
        name.split(['-', '.']).next().unwrap_or_default().to_owned()
    });
    let strategy = match (args.nodes, &args.node_index) {
        (NodeStrategy::Sparse, _) => osm::Strategy::Sparse,
        (NodeStrategy::Dense, Some(p)) => osm::Strategy::Dense(p),
        (NodeStrategy::Dense, None) => bail!("--nodes dense needs --node-index <file on ext4>"),
    };
    let temp = args.temp.clone().unwrap_or_else(|| work_dir.join("tmp"));
    std::fs::create_dir_all(&temp)?;
    if let Some(dir) = args.json.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let layer_names: Vec<String> = wl.layers.iter().map(|l| l.name.clone()).collect();
    let mut stages = Stages {
        list: Vec::new(),
        t: Instant::now(),
    };

    eprintln!("measure {region}: {}", args.input.display());
    let band = args.band.unwrap_or(Band::ALL);
    let mut data = osm::read(
        &args.input,
        &wl,
        strategy,
        args.routing == Routing::Ch,
        band,
        &[],
    )?;
    stages.done("read");

    let restrictions = data.stats.restriction_relations;
    let routing = match args.routing {
        Routing::Ch => {
            let roads = &wl.layers[wl.layer_index("roads").context("no roads layer")?];
            let names: Vec<String> = roads.classes.iter().map(|c| c.name.clone()).collect();
            Some(routing::build(&data.roads, &names, restrictions))
        }
        Routing::Graph => Some(routing::count_graph(&args.input, &wl, restrictions)?),
        Routing::None => None,
    };
    data.roads = Vec::new();
    stages.done("routing");

    // Per-feature totals count each feature in the one band that owns it.
    let owned: Vec<&osm::Feature> = data
        .features
        .iter()
        .filter(|f| band.owns(f.geom.bbox()))
        .collect();
    let search = search::build(owned.iter().copied(), &wl)?;
    drop(owned);
    stages.done("search");

    let water_zip = args
        .water
        .clone()
        .unwrap_or_else(|| work_dir.join("inputs/osmdata/water-polygons-split-4326.zip"));
    let water = match (data.bbox, water_zip.exists()) {
        (Some(bbox), true) => {
            let layer = wl.layer_index("water").context("no water layer")?;
            let class = wl.layers[layer]
                .classes
                .iter()
                .position(|c| c.external && c.name == "ocean")
                .context("no external ocean class in the water layer")?;
            let clip = args
                .clip_poly
                .as_deref()
                .map(poly::Poly::read)
                .transpose()?;
            let (feats, scanned, outside) =
                water::read(&water_zip, bbox, clip.as_ref(), layer as u8, class as u16)?;
            let stats = WaterStats {
                file: water_zip.display().to_string(),
                clip_poly: args.clip_poly.as_ref().map(|p| p.display().to_string()),
                records_scanned: scanned,
                outside_poly: outside,
                polygons: feats.len() as u64,
            };
            let minzoom = tiles::minzooms(&wl);
            data.features.extend(feats.into_iter().filter(|f| {
                band.keeps(f.geom.bbox(), minzoom[f.layer as usize][f.class as usize])
            }));
            Some(stats)
        }
        (None, true) => {
            eprintln!("  water: the PBF header has no bbox, skipped");
            None
        }
        _ => {
            eprintln!("  water: {} not found, skipped", water_zip.display());
            None
        }
    };
    stages.done("water");

    let store_path = temp.join(format!("{region}-tiles.v2"));
    let pyramid = tiles::build(&data.features, &wl, &store_path, band)?;
    stages.done("tiles");

    let comp = compress::run(&store_path, &pyramid.entries, &layer_names, args.zstd_level)?;
    let _ = std::fs::remove_file(&store_path);
    stages.done("compress");

    // Per-class rows: element counts from the read, geometry totals from the features.
    let mut feats: BTreeMap<(u8, u16), (u64, u64)> = BTreeMap::new();
    for f in data.features.iter().filter(|f| band.owns(f.geom.bbox())) {
        let e = feats.entry((f.layer, f.class)).or_default();
        e.0 += 1;
        e.1 += f.geom.vertices() as u64;
    }
    let mut classes = Vec::new();
    for (li, layer) in wl.layers.iter().enumerate() {
        for (ci, class) in layer.classes.iter().enumerate() {
            let key = (li as u8, ci as u16);
            let (features, vertices) = feats.get(&key).copied().unwrap_or_default();
            let (z14_v1_bytes, z14_features) = pyramid
                .stats
                .class_z14
                .get(&key)
                .copied()
                .unwrap_or_default();
            let mut counts = data.class_counts[li][ci].clone();
            if layer.geometry_of(ci) != GeometryKind::Point {
                counts.area_m2_buckets = [0; 4];
            }
            classes.push(ClassRow {
                layer: layer.name.clone(),
                class: class.name.clone(),
                tile: class.tile,
                minzoom: class.minzoom,
                counts,
                features,
                vertices,
                z14_features,
                z14_v1_bytes,
            });
        }
    }
    let mut cells: Vec<CellRow> = pyramid
        .stats
        .cells
        .iter()
        .chain(comp.cells.iter())
        .map(|(&(variant, layer, zoom), cell)| CellRow {
            variant,
            layer: layer_names[layer as usize].clone(),
            zoom,
            cell: *cell,
        })
        .collect();
    cells.sort_by(|a, b| (a.variant, &a.layer, a.zoom).cmp(&(b.variant, &b.layer, b.zoom)));

    let report = Report {
        region,
        band: args.band.map(|b| [b.k, b.n]),
        input: args.input.display().to_string(),
        input_bytes: std::fs::metadata(&args.input)?.len(),
        whitelist_version: wl.version,
        node_strategy: args.nodes,
        zstd_level: args.zstd_level,
        threads: rayon::current_num_threads(),
        build: Build {
            wall_s: start.elapsed().as_secs_f64(),
            peak_rss_mib: peak_rss_mib(),
            node_index_disk_bytes: data.stats.node_index_disk_bytes,
            tile_store_bytes: pyramid.store_bytes,
            stages: stages.list,
        },
        read: data.stats,
        water,
        classes,
        tag_values: data.tag_values,
        cells,
        mvt: pyramid
            .stats
            .mvt
            .iter()
            .map(|(&zoom, &(tiles, bytes, gzip_bytes))| MvtRow {
                zoom,
                tiles,
                bytes,
                gzip_bytes,
            })
            .collect(),
        dictionaries: comp.dictionaries,
        tile_index: comp.index,
        random_read: comp.random_read,
        routing,
        search,
    };
    let json = serde_json::to_string_pretty(&report)?;
    std::fs::write(&args.json, json).with_context(|| format!("write {}", args.json.display()))?;
    eprintln!(
        "wrote {} in {:.0} s, peak RSS {:.0} MiB",
        args.json.display(),
        report.build.wall_s,
        report.build.peak_rss_mib
    );
    Ok(())
}
