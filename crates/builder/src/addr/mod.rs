//! `omp-builder addr-spike`: Phase 0 step 0.4. Extracts address points from OSM,
//! OpenAddresses and StatCan (NAR, ODA) plus TIGER ranges, attaches the points to OSM
//! streets, deduplicates across sources, and reports coverage, merge quality and bytes per
//! address for the encodings A1–A4.

mod encode;
mod osm;
mod sources;
pub mod streets;
mod tiger;
mod towns;

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use clap::{Args, ValueEnum};
use omp_core::normalize::{HouseNumber, house_number};
use rayon::prelude::*;
use serde::Serialize;

use crate::admin::{self, Admin};
use crate::fetch::manifest::Manifest;
use crate::fetch::regions::Subdivision;
use crate::measure::geom::{self, P};
use crate::measure::peak_rss_mib;
use crate::measure::poly;
use streets::{Streets, cell};

#[derive(Args)]
pub struct AddrArgs {
    /// OSM extract (.osm.pbf).
    input: PathBuf,

    /// States or provinces whose address files to read, as ISO codes (`US-OH`, `CA-QC`), or
    /// `all` for every US state and Canadian province.
    #[arg(long = "subdivision", required = true)]
    subdivisions: Vec<String>,

    /// Run each subdivision on its own. OSM is read once; its address points and
    /// interpolation ranges go to the first subdivision whose boundary contains them
    /// (`<inputs>/poly/<ISO>.poly`, from `fetch --sources poly`), and its streets to every
    /// subdivision they are near. `--json` is then a directory with one `<ISO>.json` each.
    #[arg(long)]
    each: bool,

    /// Sources to read.
    #[arg(
        long,
        value_enum,
        value_delimiter = ',',
        default_value = "osm,openaddresses,nar,oda,tiger"
    )]
    sources: Vec<Input>,

    /// Skip files whose license name contains this text, ignoring case (e.g. `unspecified`).
    #[arg(long)]
    exclude_license: Vec<String>,

    /// Inputs directory holding `manifest.toml` [default: <work dir>/inputs].
    #[arg(long)]
    inputs: Option<PathBuf>,

    /// Write the results here as JSON.
    #[arg(long)]
    json: PathBuf,

    /// Directory for sample TSVs [default: <json dir>/<region>-samples; with --each,
    /// <json dir>/<ISO>-samples].
    #[arg(long)]
    samples: Option<PathBuf>,

    /// Name in the report [default: input file name up to the first '-'].
    #[arg(long)]
    region: Option<String>,

    /// Street attach distance used for the merge; 100, 200 and 500 m are always reported.
    #[arg(long, default_value_t = 200.0)]
    attach_m: f64,

    /// Dedup distance used for the merge; 25, 50, 100, 250 and 1000 m are always reported.
    #[arg(long, default_value_t = 250.0)]
    dedup_m: f64,

    #[arg(long, default_value_t = 19)]
    zstd_level: i32,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Input {
    Osm,
    #[value(name = "openaddresses")]
    OpenAddresses,
    Nar,
    Oda,
    Tiger,
}

/// Where a point came from, in merge priority order: on a duplicate the earlier one is
/// kept. Blockface points are approximate, so they come last. (OSM interpolation ways are
/// ranges, like TIGER, not points.)
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Src {
    Osm,
    OpenAddresses,
    Nar,
    Oda,
    NarBlockface,
}

impl Src {
    const ALL: [Src; 5] = [
        Src::Osm,
        Src::OpenAddresses,
        Src::Nar,
        Src::Oda,
        Src::NarBlockface,
    ];

    fn as_str(self) -> &'static str {
        match self {
            Src::Osm => "osm",
            Src::OpenAddresses => "openaddresses",
            Src::Nar => "nar",
            Src::Oda => "oda",
            Src::NarBlockface => "nar_blockface",
        }
    }

    fn approx(self) -> bool {
        self == Src::NarBlockface
    }

    fn bit(self) -> u8 {
        1 << self as u8
    }
}

/// One address record as read, before normalization.
pub struct Raw {
    pub src: Src,
    /// Index into the report's file list.
    pub file: u32,
    pub number: String,
    pub street: String,
    pub unit: String,
    /// The postal town the source gives (OpenAddresses `city`, NAR `MAIL_MUN_NAME`, ODA
    /// `city`, OSM `addr:city`); empty if none.
    pub city: String,
    pub p: P,
}

const NONE: u32 = u32::MAX;
const ATTACH_M: [f64; 3] = [100.0, 200.0, 500.0];
const DEDUP_M: [f64; 5] = [25.0, 50.0, 100.0, 250.0, 1000.0];
/// Approximate points (NAR blockface) join a cluster of the same address on the same street
/// within this distance, whatever the dedup distance: they are only mid-block estimates.
const APPROX_DEDUP_M: f64 = 1000.0;
/// Upper bounds of the conflict distance buckets; the last bucket is everything beyond.
const CONFLICT_BUCKETS_M: [f64; 5] = [50.0, 100.0, 250.0, 1000.0, 5000.0];
/// A TIGER range "has a point" if an address on the same street, in the range, lies
/// within this distance of the segment.
const TIGER_NEAR_M: f64 = 100.0;

/// SGC province codes, used in NAR file names and ODA `pruid`.
const SGC: &[(&str, &str)] = &[
    ("NL", "10"),
    ("PE", "11"),
    ("NS", "12"),
    ("NB", "13"),
    ("QC", "24"),
    ("ON", "35"),
    ("MB", "46"),
    ("SK", "47"),
    ("AB", "48"),
    ("BC", "59"),
    ("YT", "60"),
    ("NT", "61"),
    ("NU", "62"),
];

#[derive(Clone, Copy)]
struct Att {
    comp: u32,
    way: u32,
    /// Attached by the exact key, not the loose one.
    strict: bool,
    offset_m: f64,
    dist_m: f64,
    left: bool,
}

struct Rec {
    src: Src,
    file: u32,
    num: u32,
    suffix: u32,
    street: u32,
    key: u32,
    unit: u32,
    city: u32,
    p: P,
    att: Option<Att>,
}

#[derive(Default)]
struct Interner {
    map: HashMap<String, u32>,
    list: Vec<String>,
}

impl Interner {
    fn new() -> Self {
        let mut i = Self::default();
        i.intern("");
        i
    }

    fn intern(&mut self, s: &str) -> u32 {
        if let Some(&id) = self.map.get(s) {
            return id;
        }
        let id = self.list.len() as u32;
        self.map.insert(s.to_owned(), id);
        self.list.push(s.to_owned());
        id
    }
}

#[derive(Serialize)]
struct FileRow {
    path: String,
    source: String,
    license: String,
    excluded: bool,
    records: u64,
    /// Records kept as addresses (at `--dedup-m`), and conflicts where this file's record
    /// is the one that disagrees with a kept address.
    kept: u64,
    conflicts: u64,
}

#[derive(Serialize)]
struct LicenseRow {
    source: String,
    license: String,
    files: u64,
    records: u64,
    excluded: bool,
}

#[derive(Serialize)]
struct SourceRow {
    source: Src,
    records: u64,
    /// Empty house number (e.g. parcels without an address).
    no_number: u64,
    /// House numbers [`house_number`] cannot parse.
    number_exceptions: u64,
    /// House number 0, dropped.
    zero_number: u64,
    /// No street name at all.
    no_street: u64,
    with_unit: u64,
    /// Records that name a postal town.
    with_city: u64,
    kinds: BTreeMap<&'static str, u64>,
}

#[derive(Serialize)]
struct AttachRow {
    source: Src,
    /// Records with a number and a street name.
    records: u64,
    /// Attached by the exact key, at 100 / 200 / 500 m.
    strict: [u64; 3],
    /// Not by the exact key, but by the loose key, at 100 / 200 / 500 m.
    loose: [u64; 3],
}

#[derive(Serialize, Default)]
struct DedupRow {
    dedup_m: f64,
    /// Unique addresses after the merge.
    addresses: u64,
    /// `kept>dropped` source pairs.
    duplicates: BTreeMap<String, u64>,
    /// Records with a unit whose civic address the same source already gave.
    unit_rows: u64,
    conflicts_attached: u64,
    conflicts_unattached: u64,
    /// Conflict distance to the nearest kept same-key address, by [`CONFLICT_BUCKETS_M`].
    conflict_m: [u64; 6],
    /// For attached conflicts, the distance between the two along the street, by the same
    /// buckets. A small gap with a large distance is one lot placed at the road by one source
    /// and at the building by the other.
    conflict_along_m: [u64; 6],
    /// Attached conflicts with both points on the same side of the street.
    conflict_same_side: u64,
    /// `kept>other` source pairs of the conflicts.
    conflict_pairs: BTreeMap<String, u64>,
}

#[derive(Serialize)]
struct CoverageRow {
    source: Src,
    new_addresses: u64,
    cumulative: u64,
}

#[derive(Serialize)]
struct MergedStats {
    addresses: u64,
    attached: u64,
    unattached: u64,
    approximate: u64,
    with_suffix: u64,
    units: u64,
    street_components: u64,
    streets_with_addresses: u64,
    street_km_with_addresses: f64,
}

#[derive(Serialize, Default)]
struct RangeRow {
    source: Option<tiger::RangeSrc>,
    ranges: u64,
    /// House numbers the ranges span.
    numbers: u64,
    /// Ranges with an address on the same street, in the range, within `TIGER_NEAR_M` of the
    /// line: from OSM points, and from all merged points.
    with_osm_point: u64,
    with_merged_point: u64,
    share_with_osm_point: f64,
    share_with_merged_point: f64,
    /// Ranges with no merged point: kept for interpolation.
    fallback_ranges: u64,
    fallback_numbers: u64,
    /// Segments (TIGER `TLID`s, OSM ways) where every range is a fallback.
    fallback_segments: u64,
    /// Fallback ranges attached to an OSM street; the rest have no street to go on.
    fallback_attached: u64,
}

#[derive(Serialize)]
struct Stage {
    name: &'static str,
    secs: f64,
    peak_rss_mib: f64,
}

#[derive(Serialize)]
struct Report {
    kind: &'static str,
    region: String,
    input: String,
    subdivisions: Vec<String>,
    attach_m: f64,
    dedup_m: f64,
    zstd_level: i32,
    excluded_licenses: Vec<String>,
    wall_s: f64,
    peak_rss_mib: f64,
    stages: Vec<Stage>,
    osm: Option<osm::OsmStats>,
    openaddresses: Option<sources::Skipped>,
    nar: Option<sources::NarStats>,
    oda: Option<sources::Skipped>,
    files: Vec<FileRow>,
    licenses: Vec<LicenseRow>,
    sources: Vec<SourceRow>,
    attach: Vec<AttachRow>,
    dedup: Vec<DedupRow>,
    coverage: Vec<CoverageRow>,
    merged: MergedStats,
    tiger: Option<tiger::TigerStats>,
    ranges: Vec<RangeRow>,
    encoding: encode::EncodingReport,
    towns: towns::TownReport,
}

struct Stages {
    list: Vec<Stage>,
    t: Instant,
}

impl Stages {
    fn new() -> Self {
        Self {
            list: Vec::new(),
            t: Instant::now(),
        }
    }

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

fn dist_m(a: P, b: P) -> f64 {
    let d = (f64::from(a[0]) - f64::from(b[0])).hypot(f64::from(a[1]) - f64::from(b[1]));
    d * geom::metres_per_unit(a)
}

fn dist_to_line_m(line: &[P], p: P) -> f64 {
    let (px, py) = (f64::from(p[0]), f64::from(p[1]));
    let mut best = f64::INFINITY;
    for s in line.windows(2) {
        let (ax, ay) = (f64::from(s[0][0]), f64::from(s[0][1]));
        let (dx, dy) = (f64::from(s[1][0]) - ax, f64::from(s[1][1]) - ay);
        let len2 = dx * dx + dy * dy;
        let t = if len2 > 0.0 {
            (((px - ax) * dx + (py - ay) * dy) / len2).clamp(0.0, 1.0)
        } else {
            0.0
        };
        best = best.min((px - ax - t * dx).hypot(py - ay - t * dy));
    }
    best * geom::metres_per_unit(p)
}

/// The point halfway along a polyline.
fn midpoint(line: &[P]) -> P {
    let seg = |s: &[P]| {
        (f64::from(s[1][0]) - f64::from(s[0][0])).hypot(f64::from(s[1][1]) - f64::from(s[0][1]))
    };
    let half = line.windows(2).map(seg).sum::<f64>() / 2.0;
    let mut acc = 0.0;
    for s in line.windows(2) {
        let l = seg(s);
        if acc + l >= half && l > 0.0 {
            let t = (half - acc) / l;
            let lerp = |a: u32, b: u32| (f64::from(a) + (f64::from(b) - f64::from(a)) * t) as u32;
            return [lerp(s[0][0], s[1][0]), lerp(s[0][1], s[1][1])];
        }
        acc += l;
    }
    line[0]
}

/// `k` distinct indexes below `n`, deterministic, in increasing order.
fn sample(n: usize, k: usize, seed: u64) -> Vec<usize> {
    let mut state = seed;
    let mut next = || {
        state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    };
    let mut idx: Vec<usize> = (0..n).collect();
    let k = k.min(n);
    for i in 0..k {
        let j = i + (next() % (n - i) as u64) as usize;
        idx.swap(i, j);
    }
    idx.truncate(k);
    idx.sort_unstable();
    idx
}

fn osm_url(p: P) -> String {
    let (lat, lon) = geom::unproject(p);
    format!("https://www.openstreetmap.org/?mlat={lat:.6}&mlon={lon:.6}#map=19/{lat:.6}/{lon:.6}")
}

fn lat_lon(p: P) -> String {
    let (lat, lon) = geom::unproject(p);
    format!("{lat:.6}\t{lon:.6}")
}

struct Selected {
    osm: bool,
    openaddresses: Vec<(u32, PathBuf)>,
    nar: Option<(u32, PathBuf, Vec<&'static str>)>,
    oda: Vec<(u32, PathBuf, &'static str, &'static str)>,
    tiger: Vec<PathBuf>,
}

/// Picks the manifest files for the subdivisions and sources, and applies the license filter.
fn select(
    args: &AddrArgs,
    inputs: &Path,
    subs: &[Subdivision],
    files: &mut Vec<FileRow>,
) -> Result<Selected> {
    let manifest = Manifest::load_or_default(&inputs.join("manifest.toml"))?;
    if manifest.files.is_empty() {
        bail!(
            "no files in {}; run `fetch` first",
            inputs.join("manifest.toml").display()
        );
    }
    let wants = |i: Input| args.sources.contains(&i);
    let excluded = |license: &str| {
        let l = license.to_lowercase();
        args.exclude_license
            .iter()
            .any(|x| l.contains(&x.to_lowercase()))
    };
    let mut add = |path: &str, source: &str, license: &str| -> Option<u32> {
        let ex = excluded(license);
        files.push(FileRow {
            path: path.to_owned(),
            source: source.to_owned(),
            license: license.to_owned(),
            excluded: ex,
            records: 0,
            kept: 0,
            conflicts: 0,
        });
        (!ex).then_some(files.len() as u32 - 1)
    };
    let mut sel = Selected {
        osm: wants(Input::Osm),
        openaddresses: Vec::new(),
        nar: None,
        oda: Vec::new(),
        tiger: Vec::new(),
    };
    let fips: Vec<&str> = subs
        .iter()
        .filter_map(|s| match s {
            Subdivision::UsState { fips, .. } => Some(*fips),
            Subdivision::CaProvince { .. } => None,
        })
        .collect();
    let provinces: Vec<(&'static str, &'static str)> = subs
        .iter()
        .filter_map(|s| match s {
            Subdivision::CaProvince { abbr } => SGC.iter().find(|(a, _)| a == abbr).copied(),
            Subdivision::UsState { .. } => None,
        })
        .collect();
    let prefixes: Vec<String> = subs
        .iter()
        .map(|s| format!("openaddresses/{}/", s.oa_prefix()))
        .collect();
    for f in &manifest.files {
        let path = inputs.join(&f.path);
        match f.source.as_str() {
            "openaddresses"
                if wants(Input::OpenAddresses)
                    && prefixes.iter().any(|p| f.path.starts_with(p)) =>
            {
                if let Some(i) = add(&f.path, &f.source, &f.license.name) {
                    sel.openaddresses.push((i, path));
                }
            }
            "tiger" if wants(Input::Tiger) => {
                let name = f.path.rsplit('/').next().unwrap_or_default();
                let state = name.split('_').nth(2).and_then(|g| g.get(..2));
                if state.is_some_and(|s| fips.contains(&s))
                    && add(&f.path, &f.source, &f.license.name).is_some()
                {
                    sel.tiger.push(path);
                }
            }
            "nar" if wants(Input::Nar) && !provinces.is_empty() => {
                // The newest release; paths are `statcan/nar/<YYYYMM>.zip`.
                if sel.nar.as_ref().is_none_or(|(_, p, _)| *p < path)
                    && let Some(i) = add(&f.path, &f.source, &f.license.name)
                {
                    sel.nar = Some((i, path, provinces.iter().map(|p| p.1).collect()));
                }
            }
            "oda" if wants(Input::Oda) => {
                for &(abbr, code) in &provinces {
                    if f.path == format!("statcan/oda/ODA_{abbr}_v1.zip")
                        && let Some(i) = add(&f.path, &f.source, &f.license.name)
                    {
                        sel.oda.push((i, path.clone(), abbr, code));
                    }
                }
            }
            _ => {}
        }
    }
    Ok(sel)
}

pub fn run(args: AddrArgs, work_dir: &Path) -> Result<()> {
    let start = Instant::now();
    let mut stages = Stages::new();
    let mut subs: Vec<Subdivision> = Vec::new();
    for s in &args.subdivisions {
        if s.eq_ignore_ascii_case("all") {
            subs.extend(Subdivision::all());
            continue;
        }
        subs.push(
            Subdivision::from_iso(&s.to_uppercase())
                .with_context(|| format!("unknown subdivision {s:?}; use e.g. US-OH or CA-QC"))?,
        );
    }
    subs.dedup();
    let inputs = args
        .inputs
        .clone()
        .unwrap_or_else(|| work_dir.join("inputs"));
    let read_osm = args.sources.contains(&Input::Osm);
    let admin = admin::load(&args.input)?;
    stages.done("admin");

    if !args.each {
        let region = args.region.clone().unwrap_or_else(|| {
            let name = args.input.file_name().unwrap_or_default().to_string_lossy();
            name.split(['-', '.']).next().unwrap_or_default().to_owned()
        });
        let samples_dir = args.samples.clone().unwrap_or_else(|| {
            args.json
                .parent()
                .unwrap_or(Path::new("."))
                .join(format!("{region}-samples"))
        });
        let osm = read_osm.then(|| osm::read(&args.input)).transpose()?;
        stages.done("osm");
        let run = Run {
            region,
            subs: &subs,
            json: args.json.clone(),
            samples_dir,
            start,
            stages,
        };
        return run_one(&args, &inputs, run, osm, &admin);
    }

    let polys: Vec<poly::Poly> = subs
        .iter()
        .map(|s| poly::Poly::read(&inputs.join(format!("poly/{}.poly", s.iso()))))
        .collect::<Result<_>>()?;
    std::fs::create_dir_all(&args.json)?;
    let mut parts: Vec<Option<osm::OsmAddresses>> = match read_osm {
        true => {
            let all = osm::read(&args.input)?;
            stages.done("osm");
            let (parts, split) = osm::split(all, &polys, STREET_MARGIN_DEG);
            stages.done("split");
            let report = SplitReport {
                kind: "addr-split",
                input: args.input.display().to_string(),
                subdivisions: subs.iter().map(|s| s.iso()).collect(),
                wall_s: start.elapsed().as_secs_f64(),
                peak_rss_mib: peak_rss_mib(),
                stages: std::mem::take(&mut stages.list),
                split,
            };
            let path = args.json.join("osm-split.json");
            std::fs::write(&path, serde_json::to_string_pretty(&report)?)
                .with_context(|| format!("write {}", path.display()))?;
            parts.into_iter().map(Some).collect()
        }
        false => subs.iter().map(|_| None).collect(),
    };
    for (i, sub) in subs.iter().enumerate() {
        let iso = sub.iso();
        let run = Run {
            json: args.json.join(format!("{iso}.json")),
            samples_dir: match &args.samples {
                Some(d) => d.join(&iso),
                None => args.json.join(format!("{iso}-samples")),
            },
            region: iso,
            subs: std::slice::from_ref(sub),
            start: Instant::now(),
            stages: Stages::new(),
        };
        run_one(&args, &inputs, run, parts[i].take(), &admin)?;
    }
    Ok(())
}

/// Streets this far outside a subdivision's boundary still go to it, for addresses near the
/// border (about 5 km).
const STREET_MARGIN_DEG: f64 = 0.05;

#[derive(Serialize)]
struct SplitReport {
    kind: &'static str,
    input: String,
    subdivisions: Vec<String>,
    wall_s: f64,
    peak_rss_mib: f64,
    stages: Vec<Stage>,
    split: osm::SplitStats,
}

/// One merge run: its name, subdivisions and outputs.
struct Run<'a> {
    region: String,
    subs: &'a [Subdivision],
    json: PathBuf,
    samples_dir: PathBuf,
    start: Instant,
    stages: Stages,
}

/// Merges the address sources of `run.subs` with the OSM records already read.
fn run_one(
    args: &AddrArgs,
    inputs: &Path,
    run: Run,
    osm: Option<osm::OsmAddresses>,
    admin: &Admin,
) -> Result<()> {
    let Run {
        region,
        subs,
        json,
        samples_dir,
        start,
        mut stages,
    } = run;
    std::fs::create_dir_all(&samples_dir)?;

    let mut files = vec![FileRow {
        path: args.input.display().to_string(),
        source: "osm".into(),
        license: "ODbL-1.0".into(),
        excluded: false,
        records: 0,
        kept: 0,
        conflicts: 0,
    }];
    let sel = select(args, inputs, subs, &mut files)?;
    eprintln!(
        "addr-spike {region}: {} OpenAddresses files, {} TIGER files, NAR {}, ODA {}",
        sel.openaddresses.len(),
        sel.tiger.len(),
        if sel.nar.is_some() { "yes" } else { "no" },
        sel.oda.len()
    );

    // Extract.
    let mut raws: Vec<Raw> = Vec::new();
    let osm = osm.filter(|_| sel.osm).map(|mut o| {
        raws.append(&mut o.raws);
        o
    });
    let oa = if sel.openaddresses.is_empty() {
        None
    } else {
        let (mut r, s) = sources::openaddresses(&sel.openaddresses)?;
        raws.append(&mut r);
        Some(s)
    };
    stages.done("openaddresses");
    let nar = match &sel.nar {
        Some((file, path, codes)) => {
            let (mut r, s) = sources::nar(path, codes, *file)?;
            raws.append(&mut r);
            Some(s)
        }
        None => None,
    };
    let oda = if sel.oda.is_empty() {
        None
    } else {
        let mut total = sources::Skipped::default();
        for (file, path, abbr, code) in &sel.oda {
            let (mut r, s) = sources::oda(path, abbr, code, *file)?;
            raws.append(&mut r);
            total.bad_rows += s.bad_rows;
            total.no_location += s.no_location;
        }
        Some(total)
    };
    stages.done("statcan");
    let (mut ranges, tiger_stats) = if sel.tiger.is_empty() {
        (Vec::new(), None)
    } else {
        let (r, s) = tiger::read(&sel.tiger)?;
        (r, Some(s))
    };
    stages.done("tiger");

    // Normalize.
    for r in &raws {
        files[r.file as usize].records += 1;
    }
    let parsed: Vec<Option<HouseNumber>> =
        raws.par_iter().map(|r| house_number(&r.number)).collect();
    let mut street_names = Interner::new();
    let mut suffixes = Interner::new();
    let mut units = Interner::new();
    let mut cities = Interner::new();
    let mut source_rows: BTreeMap<Src, SourceRow> = BTreeMap::new();
    let mut exceptions: Vec<(Src, String, String, P)> = Vec::new();
    let mut recs: Vec<Rec> = Vec::with_capacity(raws.len());
    for (r, hn) in raws.into_iter().zip(parsed) {
        let row = source_rows.entry(r.src).or_insert_with(|| SourceRow {
            source: r.src,
            records: 0,
            no_number: 0,
            number_exceptions: 0,
            zero_number: 0,
            no_street: 0,
            with_unit: 0,
            with_city: 0,
            kinds: BTreeMap::new(),
        });
        row.records += 1;
        let Some(hn) = hn else {
            if r.number.trim().is_empty() {
                row.no_number += 1;
                continue;
            }
            row.number_exceptions += 1;
            exceptions.push((r.src, r.number, r.street, r.p));
            continue;
        };
        if hn.number == 0 {
            // OpenAddresses uses 0 for "unknown".
            row.zero_number += 1;
            continue;
        }
        *row.kinds.entry(hn.kind.as_str()).or_default() += 1;
        if r.street.is_empty() {
            row.no_street += 1;
            continue;
        }
        if !r.unit.is_empty() {
            row.with_unit += 1;
        }
        if !r.city.is_empty() {
            row.with_city += 1;
        }
        recs.push(Rec {
            src: r.src,
            file: r.file,
            num: hn.number,
            suffix: suffixes.intern(&hn.suffix),
            street: street_names.intern(&r.street),
            key: NONE,
            unit: units.intern(&r.unit),
            city: cities.intern(&r.city),
            p: r.p,
            att: None,
        });
    }
    let mut keys = streets::Keys::default();
    let street_keys = keys.intern_names(&street_names.list);
    for r in &mut recs {
        r.key = street_keys[r.street as usize];
    }
    let (osm_stats, osm_streets) = match osm {
        Some(mut o) => {
            ranges.append(&mut o.ranges);
            (Some(o.stats), o.streets)
        }
        None => (None, Vec::new()),
    };
    let streets = Streets::build(osm_streets, &mut keys);
    stages.done("normalize");

    // Attach.
    let loose_from = ATTACH_M[0].min(args.attach_m);
    let found: Vec<(f64, f64, Option<Att>)> = recs
        .par_iter()
        .map(|r| {
            let hs = streets.nearest(r.key, false, r.p);
            let ds = hs.map_or(f64::INFINITY, |h| h.dist_m);
            let lk = keys.loose[r.key as usize];
            let hl = if ds > loose_from {
                streets.nearest(lk, true, r.p)
            } else {
                None
            };
            let dl = hl.map_or(f64::INFINITY, |h| h.dist_m);
            let pick = if ds <= args.attach_m {
                hs.map(|h| (h, r.key, true))
            } else if dl <= args.attach_m {
                hl.map(|h| (h, lk, false))
            } else {
                None
            };
            let att = pick.and_then(|(h, k, strict)| {
                streets.locate(&h, k).map(|(comp, offset_m)| Att {
                    comp,
                    way: h.way,
                    strict,
                    offset_m,
                    dist_m: h.dist_m,
                    left: h.left,
                })
            });
            (ds, dl, att)
        })
        .collect();
    let mut attach: BTreeMap<Src, AttachRow> = BTreeMap::new();
    for (r, (ds, dl, att)) in recs.iter_mut().zip(&found) {
        let row = attach.entry(r.src).or_insert_with(|| AttachRow {
            source: r.src,
            records: 0,
            strict: [0; 3],
            loose: [0; 3],
        });
        row.records += 1;
        for (i, t) in ATTACH_M.iter().enumerate() {
            if ds <= t {
                row.strict[i] += 1;
            } else if dl <= t {
                row.loose[i] += 1;
            }
        }
        r.att = *att;
    }
    drop(found);
    stages.done("attach");

    // Dedup: group by (street component or unattached key, number, suffix), sources in
    // priority order, then cluster greedily within each group.
    let group = |r: &Rec| match r.att {
        Some(a) => (false, a.comp, keys.dirs[r.key as usize], r.num, r.suffix),
        None => (true, r.key, 0, r.num, r.suffix),
    };
    let mut order: Vec<u32> = (0..recs.len() as u32).collect();
    order.par_sort_unstable_by_key(|&i| {
        let r = &recs[i as usize];
        (group(r), r.src, i)
    });
    let mut bounds = Vec::new();
    let mut s = 0;
    for i in 1..=order.len() {
        if i == order.len() || group(&recs[order[i] as usize]) != group(&recs[order[s] as usize]) {
            bounds.push((s, i));
            s = i;
        }
    }
    let mut dedup = Vec::new();
    let mut chosen: Option<Merge> = None;
    let mut ds_list: Vec<f64> = DEDUP_M.to_vec();
    if !ds_list.contains(&args.dedup_m) {
        ds_list.push(args.dedup_m);
    }
    for &d in &ds_list {
        let keep = d == args.dedup_m;
        let (row, merge) = cluster(&recs, &order, &bounds, d, keep);
        dedup.push(row);
        if keep {
            chosen = Some(merge);
        }
    }
    let Merge {
        reps,
        assign,
        conflicts,
    } = chosen.expect("dedup_m is in the list");
    for &r in &reps {
        files[recs[r as usize].file as usize].kept += 1;
    }
    for &(_, other, _) in &conflicts {
        files[recs[other as usize].file as usize].conflicts += 1;
    }
    stages.done("dedup");

    // Per merged address: source mask and units.
    let mut masks = vec![0u8; reps.len()];
    let mut cluster_units: Vec<(u32, u32)> = Vec::new();
    for (i, r) in recs.iter().enumerate() {
        let c = assign[i];
        masks[c as usize] |= r.src.bit();
        if r.unit != 0 {
            cluster_units.push((c, r.unit));
        }
    }
    cluster_units.sort_unstable();
    cluster_units.dedup();
    let mut coverage = Vec::new();
    let mut cumulative = 0;
    for src in Src::ALL {
        let n = reps
            .iter()
            .filter(|&&r| recs[r as usize].src == src)
            .count() as u64;
        if n == 0 && !source_rows.contains_key(&src) {
            continue;
        }
        cumulative += n;
        coverage.push(CoverageRow {
            source: src,
            new_addresses: n,
            cumulative,
        });
    }

    // Ranges: which have a point, then the rest (fallbacks) on OSM streets.
    let range_keys = {
        let mut uniq = Interner::new();
        let ids: Vec<u32> = ranges.iter().map(|r| uniq.intern(&r.street)).collect();
        let strict = keys.intern_names(&uniq.list);
        ids.into_iter()
            .map(|i| strict[i as usize])
            .collect::<Vec<u32>>()
    };
    let mut by_cell: HashMap<(u32, i32, i32), Vec<u32>> = HashMap::new();
    for (c, &r) in reps.iter().enumerate() {
        let rec = &recs[r as usize];
        let (x, y) = cell(rec.p);
        by_cell
            .entry((keys.loose[rec.key as usize], x, y))
            .or_default()
            .push(c as u32);
    }
    let hits: Vec<(bool, bool)> = ranges
        .par_iter()
        .zip(&range_keys)
        .map(|(t, &k)| {
            let lk = keys.loose[k as usize];
            let b = geom::BBox::of(t.line.iter().copied());
            let (lo, hi) = (cell(b.min), cell(b.max));
            let (mut any, mut osm) = (false, false);
            for x in lo.0 - 1..=hi.0 + 1 {
                for y in lo.1 - 1..=hi.1 + 1 {
                    for &c in by_cell.get(&(lk, x, y)).into_iter().flatten() {
                        let rec = &recs[reps[c as usize] as usize];
                        if !t.contains(rec.num) || dist_to_line_m(&t.line, rec.p) > TIGER_NEAR_M {
                            continue;
                        }
                        any = true;
                        osm |= masks[c as usize] & Src::Osm.bit() != 0;
                        if osm {
                            return (true, true);
                        }
                    }
                }
            }
            (any, osm)
        })
        .collect();
    drop(by_cell);
    let fallback: Vec<(tiger::RangeSrc, encode::TigerEntry)> = (0..ranges.len())
        .into_par_iter()
        .filter(|&i| !hits[i].0)
        .filter_map(|i| {
            let t = &ranges[i];
            let k = range_keys[i];
            let lk = keys.loose[k as usize];
            let mid = midpoint(&t.line);
            let (hit, key) = match streets.nearest(k, false, mid) {
                Some(h) if h.dist_m <= args.attach_m => (h, k),
                _ => match streets.nearest(lk, true, mid) {
                    Some(h) if h.dist_m <= args.attach_m => (h, lk),
                    _ => return None,
                },
            };
            let s = streets.project(hit.way, t.line[0]);
            let e = streets.project(hit.way, *t.line.last().unwrap());
            let (comp, start_m) = streets.locate(&s, key)?;
            let (_, end_m) = streets.locate(&e, key)?;
            // TIGER sides are relative to the segment; an OSM interpolation line lies on
            // its side of the street.
            let left = match t.right {
                Some(right) if end_m >= start_m => !right,
                Some(right) => right,
                None => hit.left,
            };
            Some((
                t.src,
                encode::TigerEntry {
                    comp,
                    start_m,
                    end_m,
                    from: t.from,
                    to: t.to,
                    parity: t.parity,
                    left,
                },
            ))
        })
        .collect();
    let mut range_rows: BTreeMap<tiger::RangeSrc, RangeRow> = BTreeMap::new();
    let mut segments: HashMap<(tiger::RangeSrc, u64), bool> = HashMap::new();
    for (i, t) in ranges.iter().enumerate() {
        let row = range_rows.entry(t.src).or_insert_with(|| RangeRow {
            source: Some(t.src),
            ..Default::default()
        });
        let (any, osm) = hits[i];
        row.ranges += 1;
        row.numbers += t.numbers();
        row.with_osm_point += u64::from(osm);
        row.with_merged_point += u64::from(any);
        if !any {
            row.fallback_ranges += 1;
            row.fallback_numbers += t.numbers();
        }
        *segments.entry((t.src, t.tlid)).or_insert(true) &= !any;
    }
    for ((src, _), all_fallback) in segments {
        if all_fallback && let Some(row) = range_rows.get_mut(&src) {
            row.fallback_segments += 1;
        }
    }
    let mut range_entries: Vec<(tiger::RangeSrc, Vec<encode::TigerEntry>)> = Vec::new();
    for (src, e) in fallback {
        if let Some(row) = range_rows.get_mut(&src) {
            row.fallback_attached += 1;
        }
        match range_entries.iter_mut().find(|(s, _)| *s == src) {
            Some((_, v)) => v.push(e),
            None => range_entries.push((src, vec![e])),
        }
    }
    range_entries.sort_by_key(|(s, _)| *s);
    for row in range_rows.values_mut() {
        let n = row.ranges.max(1) as f64;
        row.share_with_osm_point = row.with_osm_point as f64 / n;
        row.share_with_merged_point = row.with_merged_point as f64 / n;
    }
    drop(ranges);
    stages.done("ranges");

    // Encode.
    let mut entries = Vec::new();
    let mut unattached = Vec::new();
    let mut unit_lists: Vec<Vec<String>> = vec![Vec::new(); reps.len()];
    for (c, u) in &cluster_units {
        unit_lists[*c as usize].push(units.list[*u as usize].clone());
    }
    let mut with_suffix = 0;
    let mut approximate = 0;
    for (c, &r) in reps.iter().enumerate() {
        let rec = &recs[r as usize];
        let suffix = suffixes.list[rec.suffix as usize].clone();
        with_suffix += u64::from(!suffix.is_empty());
        approximate += u64::from(rec.src.approx());
        let units = std::mem::take(&mut unit_lists[c]);
        match rec.att {
            Some(a) => entries.push(encode::Entry {
                comp: a.comp,
                num: rec.num,
                offset_m: a.offset_m,
                lateral_m: a.dist_m,
                left: a.left,
                osm: rec.src == Src::Osm,
                approx: rec.src.approx(),
                suffix,
                units,
            }),
            None => unattached.push(encode::Unattached {
                p: rec.p,
                num: rec.num,
                name: street_names.list[rec.street as usize].clone(),
                osm: rec.src == Src::Osm,
                approx: rec.src.approx(),
                suffix,
                units,
            }),
        }
    }
    let mut comps_used: Vec<u32> = entries.iter().map(|e| e.comp).collect();
    comps_used.sort_unstable();
    comps_used.dedup();
    let merged = MergedStats {
        addresses: reps.len() as u64,
        attached: entries.len() as u64,
        unattached: unattached.len() as u64,
        approximate,
        with_suffix,
        units: cluster_units.len() as u64,
        street_components: streets.comps.len() as u64,
        streets_with_addresses: comps_used.len() as u64,
        street_km_with_addresses: comps_used
            .iter()
            .map(|&c| streets.comps[c as usize].length_m)
            .sum::<f64>()
            / 1000.0,
    };
    let encoding = encode::measure(
        &mut entries,
        &mut unattached,
        &mut range_entries,
        args.zstd_level,
    )?;
    stages.done("encode");

    let votes: Vec<(u32, u32)> = recs
        .iter()
        .filter(|r| r.city != 0)
        .filter_map(|r| r.att.map(|a| (a.way, r.city)))
        .collect();
    let points: Vec<P> = reps.iter().map(|&r| recs[r as usize].p).collect();
    let iso = match subs {
        [one] => Some(one.iso()),
        _ => None,
    };
    let town_report = towns::measure(
        admin,
        &streets,
        iso.as_deref(),
        &votes,
        &cities.list,
        &points,
        &samples_dir,
    )?;
    drop((votes, points));
    stages.done("towns");

    write_samples(
        &samples_dir,
        &recs,
        &street_names,
        &suffixes,
        &streets,
        &reps,
        &masks,
        &conflicts,
        &exceptions,
    )?;
    stages.done("samples");

    let mut licenses: BTreeMap<(String, String, bool), (u64, u64)> = BTreeMap::new();
    for f in &files {
        let e = licenses
            .entry((f.source.clone(), f.license.clone(), f.excluded))
            .or_default();
        e.0 += 1;
        e.1 += f.records;
    }
    let report = Report {
        kind: "addr",
        region,
        input: args.input.display().to_string(),
        subdivisions: subs.iter().map(|s| s.iso()).collect(),
        attach_m: args.attach_m,
        dedup_m: args.dedup_m,
        zstd_level: args.zstd_level,
        excluded_licenses: args.exclude_license.clone(),
        wall_s: start.elapsed().as_secs_f64(),
        peak_rss_mib: peak_rss_mib(),
        stages: stages.list,
        osm: osm_stats,
        openaddresses: oa,
        nar,
        oda,
        licenses: licenses
            .into_iter()
            .map(
                |((source, license, excluded), (files, records))| LicenseRow {
                    source,
                    license,
                    files,
                    records,
                    excluded,
                },
            )
            .collect(),
        files,
        sources: source_rows.into_values().collect(),
        attach: attach.into_values().collect(),
        dedup,
        coverage,
        merged,
        tiger: tiger_stats,
        ranges: range_rows.into_values().collect(),
        encoding,
        towns: town_report,
    };
    if let Some(dir) = json.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&json, serde_json::to_string_pretty(&report)?)
        .with_context(|| format!("write {}", json.display()))?;
    eprintln!(
        "wrote {} and {} in {:.0} s, peak RSS {:.0} MiB",
        json.display(),
        samples_dir.display(),
        report.wall_s,
        report.peak_rss_mib
    );
    Ok(())
}

/// The merge at the chosen dedup distance.
struct Merge {
    /// The kept record of each cluster (each unique address).
    reps: Vec<u32>,
    /// Each record's cluster.
    assign: Vec<u32>,
    /// `(kept record, conflicting record, distance)`.
    conflicts: Vec<(u32, u32, f64)>,
}

/// Greedy clustering within each group: a record joins the first cluster whose kept record
/// is within `d` metres, else starts a new one (a conflict if the group already had one).
/// Returns the stats and, with `keep`, the [`Merge`].
fn cluster(
    recs: &[Rec],
    order: &[u32],
    bounds: &[(usize, usize)],
    d: f64,
    keep: bool,
) -> (DedupRow, Merge) {
    let mut row = DedupRow {
        dedup_m: d,
        ..Default::default()
    };
    let mut reps: Vec<u32> = Vec::new();
    let mut assign = if keep {
        vec![NONE; recs.len()]
    } else {
        Vec::new()
    };
    let mut conflicts = Vec::new();
    let mut local: Vec<(u32, u32, u8)> = Vec::new();
    let mut pairs: HashMap<(Src, Src), u64> = HashMap::new();
    let mut conflict_pairs: HashMap<(Src, Src), u64> = HashMap::new();
    for &(s, e) in bounds {
        local.clear();
        for &i in &order[s..e] {
            let r = &recs[i as usize];
            let mut nearest = (f64::INFINITY, NONE);
            let mut joined = None;
            let limit = if r.src.approx() {
                d.max(APPROX_DEDUP_M)
            } else {
                d
            };
            for (k, &(cid, rep, mask)) in local.iter().enumerate() {
                let dm = dist_m(recs[rep as usize].p, r.p);
                if dm <= limit {
                    joined = Some((k, cid, rep, mask));
                    break;
                }
                if dm < nearest.0 {
                    nearest = (dm, rep);
                }
            }
            match joined {
                Some((k, cid, rep, mask)) => {
                    if r.unit != 0 && mask & r.src.bit() != 0 {
                        row.unit_rows += 1;
                    } else {
                        *pairs.entry((recs[rep as usize].src, r.src)).or_default() += 1;
                    }
                    local[k].2 |= r.src.bit();
                    if keep {
                        assign[i as usize] = cid;
                    }
                }
                None => {
                    if !local.is_empty() {
                        if r.att.is_some() {
                            row.conflicts_attached += 1;
                        } else {
                            row.conflicts_unattached += 1;
                        }
                        let bucket = CONFLICT_BUCKETS_M
                            .iter()
                            .position(|&b| nearest.0 < b)
                            .unwrap_or(CONFLICT_BUCKETS_M.len());
                        row.conflict_m[bucket] += 1;
                        *conflict_pairs
                            .entry((recs[nearest.1 as usize].src, r.src))
                            .or_default() += 1;
                        if let (Some(a), Some(b)) = (recs[nearest.1 as usize].att, r.att) {
                            let along = (a.offset_m - b.offset_m).abs();
                            let bucket = CONFLICT_BUCKETS_M
                                .iter()
                                .position(|&b| along < b)
                                .unwrap_or(CONFLICT_BUCKETS_M.len());
                            row.conflict_along_m[bucket] += 1;
                            row.conflict_same_side += u64::from(a.left == b.left);
                        }
                        if keep {
                            conflicts.push((nearest.1, i, nearest.0));
                        }
                    }
                    let cid = row.addresses as u32;
                    row.addresses += 1;
                    local.push((cid, i, r.src.bit()));
                    if keep {
                        reps.push(i);
                        assign[i as usize] = cid;
                    }
                }
            }
        }
    }
    let label = |((a, b), n): ((Src, Src), u64)| (format!("{}>{}", a.as_str(), b.as_str()), n);
    row.duplicates = pairs.into_iter().map(label).collect();
    row.conflict_pairs = conflict_pairs.into_iter().map(label).collect();
    (
        row,
        Merge {
            reps,
            assign,
            conflicts,
        },
    )
}

#[allow(clippy::too_many_arguments)]
fn write_samples(
    dir: &Path,
    recs: &[Rec],
    street_names: &Interner,
    suffixes: &Interner,
    streets: &Streets,
    reps: &[u32],
    masks: &[u8],
    conflicts: &[(u32, u32, f64)],
    exceptions: &[(Src, String, String, P)],
) -> Result<()> {
    let number = |r: &Rec| format!("{}{}", r.num, suffixes.list[r.suffix as usize]);
    let street = |r: &Rec| street_names.list[r.street as usize].as_str();
    let write = |name: &str, text: String| {
        let p = dir.join(name);
        std::fs::write(&p, text).with_context(|| format!("write {}", p.display()))
    };

    // Unattached records per source, with the street names nearby.
    let mut text = String::from("source\tnumber\tstreet\tlat\tlon\tnearby_streets\turl\n");
    for src in Src::ALL {
        let idx: Vec<usize> = (0..recs.len())
            .filter(|&i| recs[i].src == src && recs[i].att.is_none())
            .collect();
        for k in sample(idx.len(), 40, 1) {
            let r = &recs[idx[k]];
            let near: Vec<String> = streets
                .names_near(r.p, 200.0)
                .into_iter()
                .take(4)
                .map(|(n, d)| format!("{n} ({d:.0} m)"))
                .collect();
            let _ = writeln!(
                text,
                "{}\t{}\t{}\t{}\t{}\t{}",
                src.as_str(),
                number(r),
                street(r),
                lat_lon(r.p),
                near.join("; "),
                osm_url(r.p)
            );
        }
    }
    write("unattached.tsv", text)?;

    // Attached only by the loose key: what the strict key misses.
    let mut text = String::from("source\tnumber\tstreet\tattached_to\tdistance_m\turl\n");
    for src in Src::ALL {
        let idx: Vec<usize> = (0..recs.len())
            .filter(|&i| recs[i].src == src && recs[i].att.is_some_and(|a| !a.strict))
            .collect();
        for k in sample(idx.len(), 40, 5) {
            let r = &recs[idx[k]];
            let a = r.att.unwrap();
            let comp = &streets.comps[a.comp as usize];
            let _ = writeln!(
                text,
                "{}\t{}\t{}\t{}\t{:.0}\t{}",
                src.as_str(),
                number(r),
                street(r),
                streets.ways[comp.ways[0] as usize].name,
                a.dist_m,
                osm_url(r.p)
            );
        }
    }
    write("loose-attached.tsv", text)?;

    let mut text = String::from(
        "number\tkept_source\tkept_street\tkept_lat\tkept_lon\tother_source\tother_street\tother_lat\tother_lon\tdistance_m\tattached\tkept_url\tother_url\n",
    );
    for k in sample(conflicts.len(), 100, 2) {
        let (a, b, d) = conflicts[k];
        let (ra, rb) = (&recs[a as usize], &recs[b as usize]);
        let _ = writeln!(
            text,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{d:.0}\t{}\t{}\t{}",
            number(ra),
            ra.src.as_str(),
            street(ra),
            lat_lon(ra.p),
            rb.src.as_str(),
            street(rb),
            lat_lon(rb.p),
            rb.att.is_some(),
            osm_url(ra.p),
            osm_url(rb.p)
        );
    }
    write("conflicts.tsv", text)?;

    let mut text =
        String::from("number\tstreet\tsources\tlat\tlon\tattached_to\tdistance_m\turl\n");
    for k in sample(reps.len(), 50, 3) {
        let r = &recs[reps[k] as usize];
        let srcs: Vec<&str> = Src::ALL
            .iter()
            .filter(|s| masks[k] & s.bit() != 0)
            .map(|s| s.as_str())
            .collect();
        let (way, dist) = match r.att {
            Some(a) => {
                let comp = &streets.comps[a.comp as usize];
                let name = &streets.ways[comp.ways[0] as usize].name;
                (name.clone(), format!("{:.0}", a.dist_m))
            }
            None => ("-".into(), "-".into()),
        };
        let _ = writeln!(
            text,
            "{}\t{}\t{}\t{}\t{way}\t{dist}\t{}",
            number(r),
            street(r),
            srcs.join(","),
            lat_lon(r.p),
            osm_url(r.p)
        );
    }
    write("merged.tsv", text)?;

    let mut text = String::from("source\traw_number\tstreet\tlat\tlon\n");
    for src in Src::ALL {
        let idx: Vec<usize> = (0..exceptions.len())
            .filter(|&i| exceptions[i].0 == src)
            .collect();
        for k in sample(idx.len(), 30, 4) {
            let (_, n, s, p) = &exceptions[idx[k]];
            let _ = writeln!(text, "{}\t{n}\t{s}\t{}", src.as_str(), lat_lon(*p));
        }
    }
    write("number-exceptions.tsv", text)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_is_deterministic_and_distinct() {
        let a = sample(1000, 10, 7);
        assert_eq!(a, sample(1000, 10, 7));
        assert_eq!(a.len(), 10);
        assert!(a.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(sample(3, 10, 7), [0, 1, 2]);
    }
}
