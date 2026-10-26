//! Reads admin areas (`boundary=administrative` relations) and place nodes from a PBF.
//! Three passes: relations, their member ways, then the nodes those ways need along with
//! the areas' label nodes and every named `place=city|town|village|hamlet` node.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result};
use osmpbf::{BlobDecode, BlobReader, Element, PrimitiveBlock, RelMemberType};
use rayon::prelude::*;
use serde::Serialize;

use crate::measure::geom::{self, Assembly, P};
use crate::measure::nodes::{NodeSet, RankedLocations};

/// Alternative names kept with each area, for search aliases (decision 6).
pub const ALT_NAMES: &[&str] = &[
    "name:en",
    "name:fr",
    "alt_name",
    "short_name",
    "official_name",
    "old_name",
];

/// Place classes a street or POI can take its town from when its addresses give none.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaceClass {
    City,
    Town,
    Village,
    Hamlet,
}

impl PlaceClass {
    fn parse(v: &str) -> Option<Self> {
        Some(match v {
            "city" => Self::City,
            "town" => Self::Town,
            "village" => Self::Village,
            "hamlet" => Self::Hamlet,
            _ => return None,
        })
    }
}

pub struct Area {
    pub osm_id: i64,
    pub level: u8,
    pub name: String,
    /// `(key, value)` from [`ALT_NAMES`].
    pub alt_names: Vec<(String, String)>,
    /// `ISO3166-2` (`US-OH`, `CA-QC`), or `ISO3166-1` on a country.
    pub iso: Option<String>,
    pub population: Option<u64>,
    pub border_type: Option<String>,
    /// The `place` tag of the area itself, else of its label or admin centre node.
    pub place: Option<String>,
    /// The `label` member node, else the `admin_centre` node if it lies inside.
    pub label: Option<P>,
    pub rings: Vec<Vec<P>>,
    /// Some member ways were missing or did not close; `rings` holds what closed.
    pub broken: bool,
}

pub struct Place {
    pub osm_id: i64,
    pub class: PlaceClass,
    pub name: String,
    pub p: P,
}

#[derive(Default, Serialize)]
pub struct LevelStats {
    pub relations: u64,
    pub ok: u64,
    /// Member ways not in the extract (cut at its border).
    pub missing_members: u64,
    /// All members present but they do not close; what closed is kept.
    pub broken: u64,
    /// No ring closed at all; the area is dropped.
    pub empty: u64,
    pub vertices: u64,
    /// `border_type` values (top ones), for telling towns from townships.
    pub border_types: BTreeMap<String, u64>,
    pub with_iso: u64,
    pub with_population: u64,
    pub with_label: u64,
}

#[derive(Default, Serialize)]
pub struct ReadStats {
    pub levels: BTreeMap<u8, LevelStats>,
    pub places: BTreeMap<String, u64>,
    pub member_ways: u64,
    pub member_nodes: u64,
}

pub struct AdminData {
    pub areas: Vec<Area>,
    pub places: Vec<Place>,
    pub stats: ReadStats,
}

struct Rel {
    id: i64,
    level: u8,
    tags: HashMap<String, String>,
    /// `(way id, inner)`.
    ways: Vec<(i64, bool)>,
    label: Option<i64>,
    centre: Option<i64>,
}

fn pass<T: Send>(
    path: &Path,
    init: impl Fn() -> T + Sync + Send,
    f: impl Fn(&PrimitiveBlock, &mut T) + Sync + Send,
) -> Result<Vec<T>> {
    BlobReader::from_path(path)
        .with_context(|| format!("open {}", path.display()))?
        .par_bridge()
        .map(|blob| {
            let mut out = init();
            if let BlobDecode::OsmData(block) = blob?.decode()? {
                f(&block, &mut out);
            }
            Ok(out)
        })
        .collect()
}

/// "12,345" or "12 345" → 12345; anything else that is not a plain number → `None`.
pub fn parse_population(v: &str) -> Option<u64> {
    let digits: String = v
        .trim()
        .chars()
        .filter(|c| !matches!(c, ',' | ' ' | '\u{a0}' | '\u{202f}'))
        .collect();
    digits.parse().ok()
}

fn relations_block(block: &PrimitiveBlock, out: &mut Vec<Rel>) {
    const KEEP: &[&str] = &[
        "name",
        "ISO3166-2",
        "ISO3166-1",
        "population",
        "border_type",
        "place",
    ];
    for el in block.elements() {
        let Element::Relation(r) = el else { continue };
        let mut boundary = false;
        let mut level = None;
        let mut tags = HashMap::new();
        for (k, v) in r.tags() {
            match k {
                "boundary" => boundary = v == "administrative",
                "admin_level" => level = v.trim().parse::<u8>().ok(),
                _ if KEEP.contains(&k) || ALT_NAMES.contains(&k) => {
                    tags.insert(k.to_owned(), v.trim().to_owned());
                }
                _ => {}
            }
        }
        let Some(level) = level.filter(|l| (2..=10).contains(l)) else {
            continue;
        };
        if !boundary || tags.get("name").is_none_or(|n| n.is_empty()) {
            continue;
        }
        let mut rel = Rel {
            id: r.id(),
            level,
            tags,
            ways: Vec::new(),
            label: None,
            centre: None,
        };
        for m in r.members() {
            let Ok(role) = m.role() else { continue };
            match (m.member_type, role) {
                (RelMemberType::Way, "outer" | "") => rel.ways.push((m.member_id, false)),
                (RelMemberType::Way, "inner") => rel.ways.push((m.member_id, true)),
                (RelMemberType::Node, "label") => rel.label = Some(m.member_id),
                (RelMemberType::Node, "admin_centre") => rel.centre = Some(m.member_id),
                _ => {}
            }
        }
        out.push(rel);
    }
}

#[derive(Default)]
struct NodeBlock {
    /// `(id, point, place tag)` of label and admin centre nodes.
    labels: Vec<(i64, P, Option<String>)>,
    places: Vec<Place>,
}

pub fn read(path: &Path) -> Result<AdminData> {
    let mut rels: Vec<Rel> = pass(path, Vec::new, relations_block)?
        .into_iter()
        .flatten()
        .collect();
    rels.sort_unstable_by_key(|r| r.id);
    let member_ids: HashSet<i64> = rels.iter().flat_map(|r| &r.ways).map(|w| w.0).collect();
    let label_ids: HashSet<i64> = rels
        .iter()
        .flat_map(|r| [r.label, r.centre])
        .flatten()
        .collect();

    let way_refs: HashMap<i64, Vec<i64>> = pass(path, Vec::new, |block, out| {
        for el in block.elements() {
            if let Element::Way(w) = el
                && member_ids.contains(&w.id())
            {
                out.push((w.id(), w.refs().collect::<Vec<i64>>()));
            }
        }
    })?
    .into_iter()
    .flatten()
    .collect();
    let needed = NodeSet::new();
    for refs in way_refs.values() {
        for &id in refs {
            needed.insert(id);
        }
    }
    let locs = RankedLocations::new(needed);

    let blocks = pass(path, NodeBlock::default, |block, out| {
        let mut visit =
            |id: i64, lat: i32, lon: i32, tags: &mut dyn Iterator<Item = (&str, &str)>| {
                let p = geom::project_e7(lat, lon);
                locs.set(id, p);
                let label = label_ids.contains(&id);
                let (mut place, mut name) = (None, None);
                for (k, v) in tags {
                    match k {
                        "place" => place = Some(v),
                        "name" => name = Some(v),
                        _ => {}
                    }
                }
                if label {
                    out.labels.push((id, p, place.map(str::to_owned)));
                }
                if let (Some(class), Some(name)) = (place.and_then(PlaceClass::parse), name)
                    && !name.trim().is_empty()
                {
                    out.places.push(Place {
                        osm_id: id,
                        class,
                        name: name.trim().to_owned(),
                        p,
                    });
                }
            };
        for el in block.elements() {
            match el {
                Element::DenseNode(n) => {
                    visit(n.id(), n.decimicro_lat(), n.decimicro_lon(), &mut n.tags())
                }
                Element::Node(n) => {
                    visit(n.id(), n.decimicro_lat(), n.decimicro_lon(), &mut n.tags())
                }
                _ => {}
            }
        }
    })?;
    let mut labels: HashMap<i64, (P, Option<String>)> = HashMap::new();
    let mut places = Vec::new();
    for b in blocks {
        labels.extend(b.labels.into_iter().map(|(id, p, pl)| (id, (p, pl))));
        places.extend(b.places);
    }
    places.sort_unstable_by_key(|p| p.osm_id);

    let mut stats = ReadStats {
        member_ways: way_refs.len() as u64,
        member_nodes: locs.len() as u64,
        ..Default::default()
    };
    for p in &places {
        *stats
            .places
            .entry(format!("{:?}", p.class).to_lowercase())
            .or_default() += 1;
    }
    let ways: HashMap<i64, Vec<P>> = way_refs
        .into_par_iter()
        .map(|(id, refs)| (id, refs.iter().filter_map(|&n| locs.get(n)).collect()))
        .collect();
    drop(locs);

    let built: Vec<(u8, Option<Area>, bool, bool)> = rels
        .into_par_iter()
        .map(|mut r| {
            let present: Vec<(&[P], bool)> = r
                .ways
                .iter()
                .filter_map(|(id, inner)| ways.get(id).map(|c| (c.as_slice(), *inner)))
                .collect();
            let missing = present.len() < r.ways.len();
            let (rings, broken) = match geom::assemble(&present) {
                Assembly::Ok(rings) => (rings, missing),
                Assembly::Broken(rings) => (rings, true),
            };
            if rings.is_empty() {
                return (r.level, None, missing, broken);
            }
            let node = |id: Option<i64>| id.and_then(|id| labels.get(&id));
            let inside = |p: P| crate::admin::index::ring_contains(&rings, p);
            let label = node(r.label)
                .map(|l| l.0)
                .or_else(|| node(r.centre).map(|l| l.0).filter(|&p| inside(p)));
            let place = r
                .tags
                .remove("place")
                .or_else(|| node(r.label).or(node(r.centre)).and_then(|l| l.1.clone()));
            let mut alt_names: Vec<(String, String)> = ALT_NAMES
                .iter()
                .filter_map(|k| r.tags.get(*k).map(|v| ((*k).to_owned(), v.clone())))
                .filter(|(_, v)| !v.is_empty())
                .collect();
            alt_names.sort();
            let area = Area {
                osm_id: r.id,
                level: r.level,
                name: r.tags.remove("name").unwrap_or_default(),
                alt_names,
                iso: r
                    .tags
                    .remove("ISO3166-2")
                    .or_else(|| r.tags.remove("ISO3166-1"))
                    .filter(|s| !s.is_empty()),
                population: r.tags.get("population").and_then(|v| parse_population(v)),
                border_type: r.tags.remove("border_type").filter(|s| !s.is_empty()),
                place,
                label,
                rings,
                broken,
            };
            (area.level, Some(area), missing, broken)
        })
        .collect();
    let mut areas = Vec::new();
    for (level, a, missing, broken) in built {
        let s = stats.levels.entry(level).or_default();
        s.relations += 1;
        s.missing_members += u64::from(missing);
        // Counted even when dropped, so the extract's cut shows.
        let Some(a) = a else {
            s.empty += 1;
            continue;
        };
        s.broken += u64::from(broken && !missing);
        s.ok += u64::from(!broken);
        s.vertices += a.rings.iter().map(|r| r.len() as u64).sum::<u64>();
        s.with_iso += u64::from(a.iso.is_some());
        s.with_population += u64::from(a.population.is_some());
        s.with_label += u64::from(a.label.is_some());
        if let Some(b) = &a.border_type {
            *s.border_types.entry(b.clone()).or_default() += 1;
        }
        areas.push(a);
    }
    Ok(AdminData {
        areas,
        places,
        stats,
    })
}
