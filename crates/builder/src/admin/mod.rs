//! Admin hierarchy (phase 2 step 2.1): admin areas from OSM boundary relations, their
//! chains by point in polygon, the town shown for streets, places and POIs, and the `SADM`
//! and `SAPL` sections. `omp-builder admin` reads a PBF and reports what it found.

pub mod encode;
pub mod index;
pub mod read;
pub mod towns;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use clap::Args;
use rayon::prelude::*;
use serde::Serialize;

use crate::addr::streets::morton;
use crate::geom::{self, BBox, P};
use crate::peak_rss_mib;
use index::Index;
pub use read::{Area, Place, PlaceClass};

/// Simplification of the rings the builder tests points against.
pub const BUILD_TOLERANCE_M: f64 = 5.0;
/// Simplification of the polygons stored for the app (`SAPL`).
pub const PACK_TOLERANCE_M: f64 = 50.0;

/// Admin areas, ordered by level and then along a Morton curve; an area's ID in the pack
/// is its position here.
pub struct Admin {
    pub areas: Vec<Area>,
    /// Label point of each area: its `label` node, else a pole of inaccessibility.
    pub labels: Vec<P>,
    /// The smallest area of a lower level that holds the label point.
    pub parents: Vec<Option<u32>>,
    /// Level 8 areas that are a city, town or village rather than a township.
    pub towns: Vec<bool>,
    pub index: Index,
    pub places: Vec<Place>,
    place_cells: HashMap<(i32, i32), Vec<u32>>,
    pub stats: read::ReadStats,
}

/// Grid for the nearest place: 2^20 world units, about 7.5 km at 40° N.
const PLACE_SHIFT: u32 = 20;

fn place_cell(p: P) -> (i32, i32) {
    ((p[0] >> PLACE_SHIFT) as i32, (p[1] >> PLACE_SHIFT) as i32)
}

/// Whether a level 8 area is a city, town or village whose name people write as their
/// town, rather than a township (Ohio's civil townships, Ontario's "Township of …").
fn town_like(a: &Area) -> bool {
    if a.level != 8 {
        return false;
    }
    let township = |s: &str| {
        let s = s.to_lowercase();
        s.contains("township") || s.contains("canton") || s == "unorganized territory"
    };
    !(a.border_type.as_deref().is_some_and(township)
        || township(&a.name)
        || a.place.as_deref().is_some_and(township))
}

impl Admin {
    pub fn build(data: read::AdminData) -> Self {
        let read::AdminData {
            areas,
            places,
            stats,
        } = data;
        let mut areas: Vec<(Area, P)> = areas
            .into_par_iter()
            .map(|mut a| {
                // Only the simplified rings are needed from here on.
                let rings = index::simplify_rings(&a.rings, BUILD_TOLERANCE_M);
                if !rings.is_empty() {
                    a.rings = rings;
                }
                let label = a.label.unwrap_or_else(|| {
                    let b = BBox::of(a.rings.iter().flatten().copied());
                    let span = f64::from((b.max[0] - b.min[0]).max(b.max[1] - b.min[1]));
                    geom::polylabel(&a.rings, (span / 200.0).max(64.0), 4096)
                });
                (a, label)
            })
            .collect();
        areas.sort_by_key(|(a, l)| (a.level, morton(*l), a.osm_id));
        let (out_areas, labels): (Vec<Area>, Vec<P>) = areas.into_iter().unzip();
        let index = Index::build(&out_areas.iter().map(|a| &a.rings[..]).collect::<Vec<_>>());
        let parents = (0..out_areas.len())
            .into_par_iter()
            .map(|i| {
                let level = out_areas[i].level;
                index
                    .containing(labels[i])
                    .into_iter()
                    .filter(|&j| out_areas[j as usize].level < level)
                    .max_by_key(|&j| out_areas[j as usize].level)
            })
            .collect();
        let towns = out_areas.iter().map(town_like).collect();
        let mut place_cells: HashMap<(i32, i32), Vec<u32>> = HashMap::new();
        for (i, p) in places.iter().enumerate() {
            place_cells
                .entry(place_cell(p.p))
                .or_default()
                .push(i as u32);
        }
        Self {
            areas: out_areas,
            labels,
            parents,
            towns,
            index,
            places,
            place_cells,
            stats,
        }
    }

    /// The areas containing `p`, from the lowest level (country) up.
    pub fn chain(&self, p: P) -> Vec<u32> {
        self.index.containing(p)
    }

    /// The area at `level` in a chain.
    pub fn at_level(&self, chain: &[u32], level: u8) -> Option<u32> {
        chain
            .iter()
            .copied()
            .find(|&a| self.areas[a as usize].level == level)
    }

    /// The nearest place node within `max_m`.
    pub fn nearest_place(&self, p: P, max_m: f64) -> Option<u32> {
        self.nearest_place_where(p, max_m, |_| true)
    }

    /// The nearest city, town or village node within `max_m`.
    pub fn nearest_major_place(&self, p: P, max_m: f64) -> Option<u32> {
        self.nearest_place_where(p, max_m, |pl| pl.class != PlaceClass::Hamlet)
    }

    /// The nearest place node within `max_m` whose name has this [`towns::key`].
    pub fn nearest_place_named(&self, p: P, key: &str, max_m: f64) -> Option<u32> {
        self.nearest_place_where(p, max_m, |pl| towns::key(&pl.name) == key)
    }

    fn nearest_place_where(&self, p: P, max_m: f64, ok: impl Fn(&Place) -> bool) -> Option<u32> {
        let mpu = geom::metres_per_unit(p);
        let cell_m = f64::from(1u32 << PLACE_SHIFT) * mpu;
        let reach = (max_m / cell_m).ceil() as i32;
        let (cx, cy) = place_cell(p);
        let mut best: Option<(f64, u32)> = None;
        // Rings of cells outwards, stopping once a ring cannot beat the best so far.
        for r in 0..=reach {
            if best.is_some_and(|(d, _)| d < f64::from(r - 1).max(0.0) * cell_m) {
                break;
            }
            for x in cx - r..=cx + r {
                for y in cy - r..=cy + r {
                    if (x - cx).abs() != r && (y - cy).abs() != r {
                        continue;
                    }
                    for &i in self.place_cells.get(&(x, y)).into_iter().flatten() {
                        let place = &self.places[i as usize];
                        if !ok(place) {
                            continue;
                        }
                        let q = place.p;
                        let d = (f64::from(q[0]) - f64::from(p[0]))
                            .hypot(f64::from(q[1]) - f64::from(p[1]))
                            * mpu;
                        if d <= max_m && best.is_none_or(|(bd, _)| d < bd) {
                            best = Some((d, i));
                        }
                    }
                }
            }
        }
        best.map(|(_, i)| i)
    }
}

/// `US-OH` → `OH`, `CA-QC` → `QC`; a country code stays whole.
pub fn abbreviation(iso: &str) -> &str {
    iso.split_once('-').map_or(iso, |(_, s)| s)
}

#[derive(Args)]
pub struct AdminArgs {
    /// OSM extract (.osm.pbf).
    input: PathBuf,

    /// Write the report here as JSON.
    #[arg(long)]
    json: PathBuf,

    /// Also print the chain and fallback town at these points (`lat,lon`).
    #[arg(long, value_parser = parse_lat_lon)]
    at: Vec<(f64, f64)>,
}

fn parse_lat_lon(s: &str) -> Result<(f64, f64), String> {
    let (lat, lon) = s.split_once(',').ok_or("expected lat,lon")?;
    let f = |v: &str| v.trim().parse::<f64>().map_err(|e| e.to_string());
    Ok((f(lat)?, f(lon)?))
}

#[derive(Serialize, Default)]
struct LevelRow {
    areas: u64,
    /// Level 8 only: cities, towns and villages (not townships).
    towns: u64,
    without_parent: u64,
    vertices_build: u64,
    vertices_pack: u64,
}

#[derive(Serialize, Default)]
struct PlaceRow {
    places: u64,
    /// Places whose chain has an area at each level.
    with_level: BTreeMap<u8, u64>,
    /// Places in a level 8 town (city, town or village area).
    in_town: u64,
    /// Places in no admin area at all.
    without_chain: u64,
}

#[derive(Serialize)]
struct Report {
    kind: &'static str,
    input: String,
    wall_s: f64,
    peak_rss_mib: f64,
    read: read::ReadStats,
    levels: BTreeMap<u8, LevelRow>,
    index_boundary_cells: u64,
    index_full_cells: u64,
    index_segments: u64,
    /// Level 9–10 areas whose label point is in no level 8 area (a borough that would hide
    /// its city if it were the only area there).
    sub_areas_without_city: u64,
    places: BTreeMap<String, PlaceRow>,
    /// Places by state or province (the level 4 area's abbreviation).
    places_by_region: BTreeMap<String, PlaceRow>,
    sections: encode::Sizes,
}

pub fn run(args: AdminArgs) -> Result<()> {
    let start = Instant::now();
    eprintln!("admin: {}", args.input.display());
    let data = read::read(&args.input)?;
    eprintln!(
        "  read {} areas, {} places: {:.1} s, peak RSS {:.0} MiB",
        data.areas.len(),
        data.places.len(),
        start.elapsed().as_secs_f64(),
        peak_rss_mib()
    );
    let admin = Admin::build(data);
    eprintln!(
        "  index: {:.1} s, peak RSS {:.0} MiB",
        start.elapsed().as_secs_f64(),
        peak_rss_mib()
    );
    let (_, _, sizes) = encode::sections(&admin)?;
    for &(lat, lon) in &args.at {
        let p = geom::project(lat, lon);
        let chain = admin.chain(p);
        let names: Vec<String> = chain
            .iter()
            .map(|&a| {
                let area = &admin.areas[a as usize];
                let town = if admin.towns[a as usize] { " town" } else { "" };
                format!("{} {}{town}", area.level, area.name)
            })
            .collect();
        eprintln!(
            "  {lat},{lon}: {}; area town {:?}, nearest place {:?}, nearest major place {:?}",
            names.join(" > "),
            towns::area_town(&admin, &chain),
            towns::place_town(&admin, p, false),
            towns::place_town(&admin, p, true),
        );
    }

    let mut levels: BTreeMap<u8, LevelRow> = BTreeMap::new();
    for (i, a) in admin.areas.iter().enumerate() {
        let row = levels.entry(a.level).or_default();
        row.areas += 1;
        row.towns += u64::from(admin.towns[i]);
        row.without_parent += u64::from(admin.parents[i].is_none() && a.level > 2);
        row.vertices_build += a.rings.iter().map(|r| r.len() as u64).sum::<u64>();
    }
    for (a, r) in admin.areas.iter().zip(&sizes.pack_vertices) {
        levels.entry(a.level).or_default().vertices_pack += r;
    }
    let sub_areas_without_city = admin
        .areas
        .iter()
        .enumerate()
        .filter(|(i, a)| a.level > 8 && admin.at_level(&admin.chain(admin.labels[*i]), 8).is_none())
        .count() as u64;

    let chains: Vec<Vec<u32>> = admin.places.par_iter().map(|p| admin.chain(p.p)).collect();
    let mut places: BTreeMap<String, PlaceRow> = BTreeMap::new();
    let mut by_region: BTreeMap<String, PlaceRow> = BTreeMap::new();
    for (p, chain) in admin.places.iter().zip(&chains) {
        let region = admin
            .at_level(chain, 4)
            .and_then(|a| admin.areas[a as usize].iso.as_deref())
            .unwrap_or("none")
            .to_owned();
        let class = format!("{:?}", p.class).to_lowercase();
        for row in [
            places.entry(class).or_default(),
            by_region.entry(region).or_default(),
        ] {
            row.places += 1;
            row.without_chain += u64::from(chain.is_empty());
            row.in_town += u64::from(chain.iter().any(|&a| admin.towns[a as usize]));
            let mut seen: Vec<u8> = chain
                .iter()
                .map(|&a| admin.areas[a as usize].level)
                .collect();
            seen.dedup();
            for l in seen {
                *row.with_level.entry(l).or_default() += 1;
            }
        }
    }

    let report = Report {
        kind: "admin",
        input: args.input.display().to_string(),
        wall_s: start.elapsed().as_secs_f64(),
        peak_rss_mib: peak_rss_mib(),
        levels,
        index_boundary_cells: admin.index.boundary_cells,
        index_full_cells: admin.index.full_cells,
        index_segments: admin.index.segments,
        sub_areas_without_city,
        places,
        places_by_region: by_region,
        sections: sizes,
        read: admin.stats,
    };
    if let Some(dir) = args.json.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&args.json, serde_json::to_string_pretty(&report)?)
        .with_context(|| format!("write {}", args.json.display()))?;
    eprintln!(
        "wrote {}: SADM {} B, SAPL {} B; {:.0} s, peak RSS {:.0} MiB",
        args.json.display(),
        report.sections.sadm,
        report.sections.sapl,
        report.wall_s,
        report.peak_rss_mib
    );
    Ok(())
}

/// Reads and builds the admin areas of a PBF, logging the time.
pub fn load(path: &Path) -> Result<Admin> {
    let t = Instant::now();
    let admin = Admin::build(read::read(path)?);
    eprintln!(
        "  admin: {} areas, {} places, {:.1} s, peak RSS {:.0} MiB",
        admin.areas.len(),
        admin.places.len(),
        t.elapsed().as_secs_f64(),
        peak_rss_mib()
    );
    Ok(admin)
}
