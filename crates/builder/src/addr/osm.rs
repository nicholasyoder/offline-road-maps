//! OSM address records, interpolation ranges and the named street ways addresses attach
//! to. Two passes: ways (streets, addressed buildings, interpolation lines), then nodes
//! (address points and the locations the ways need).

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result};
use omp_core::normalize::{NumberKind, house_number};
use osmpbf::{BlobDecode, BlobReader, Element, PrimitiveBlock};
use rayon::prelude::*;
use serde::Serialize;

use super::tiger::{Range, RangeSrc};
use super::{Raw, Src};
use crate::measure::geom::{self, BBox, P};
use crate::measure::nodes::{NodeSet, RankedLocations};
use crate::measure::poly::Poly;

/// A named highway way. Addresses attach to these.
#[derive(Clone)]
pub struct StreetWay {
    pub id: i64,
    /// `name` first, then alternates and each `ref`.
    pub names: Vec<String>,
    /// How many of `names` are names; the rest are refs.
    pub nnames: usize,
    /// Index of its `highway` value in [`HIGHWAYS`].
    pub class: u8,
    pub refs: Vec<i64>,
    pub coords: Vec<P>,
}

/// `highway` values of streets, roughly by importance; anything else is `HIGHWAYS.len()`.
pub const HIGHWAYS: &[&str] = &[
    "motorway",
    "trunk",
    "primary",
    "secondary",
    "tertiary",
    "motorway_link",
    "trunk_link",
    "primary_link",
    "secondary_link",
    "tertiary_link",
    "unclassified",
    "residential",
    "living_street",
    "busway",
    "road",
    "service",
    "pedestrian",
    "track",
];

pub fn highway_class(v: &str) -> u8 {
    HIGHWAYS
        .iter()
        .position(|h| *h == v)
        .unwrap_or(HIGHWAYS.len()) as u8
}

#[derive(Default, Serialize)]
pub struct OsmStats {
    pub address_nodes: u64,
    pub address_ways: u64,
    /// Counted only; multipolygon buildings with addresses are rare.
    pub address_relations: u64,
    /// `addr:housenumber` with `addr:place` and no `addr:street`: kept with the place as the
    /// street name, so they end up unattached.
    pub place_only: u64,
    /// Neither `addr:street` nor `addr:place`.
    pub no_street: u64,
    /// `addr:housenumber` lists like "12;14", split into one record each.
    pub number_lists: u64,
    pub interpolation_ways: u64,
    /// One per pair of consecutive numbered nodes on an interpolation way.
    pub interpolation_ranges: u64,
    /// House numbers those ranges span (what expanding them into points would give).
    pub interpolation_numbers: u64,
    /// Pairs that give no range: missing or non-plain numbers, wrong parity, `alphabetic`,
    /// or more than 10,000 numbers apart.
    pub interpolation_failed_pairs: u64,
    pub street_ways: u64,
}

pub struct OsmAddresses {
    pub raws: Vec<Raw>,
    pub ranges: Vec<Range>,
    pub streets: Vec<StreetWay>,
    pub stats: OsmStats,
}

/// Highway values that are not streets an address can be on.
const NOT_STREETS: &[&str] = &[
    "abandoned",
    "bridleway",
    "bus_stop",
    "construction",
    "corridor",
    "cycleway",
    "elevator",
    "footway",
    "path",
    "platform",
    "proposed",
    "raceway",
    "razed",
    "steps",
];
const NAME_KEYS: &[&str] = &[
    "name",
    "alt_name",
    "official_name",
    "old_name",
    "short_name",
    "loc_name",
    "name:en",
    "name:fr",
];
const MAX_INTERPOLATED: u32 = 10_000;

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

fn get<'a>(tags: &[(&'a str, &'a str)], key: &str) -> Option<&'a str> {
    tags.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

/// An `addr:*` tag set on a node or way.
struct Tagged {
    numbers: String,
    street: String,
    unit: String,
    city: String,
}

fn tagged(tags: &[(&str, &str)], stats: &mut OsmStats) -> Option<Tagged> {
    let numbers = get(tags, "addr:housenumber")?;
    let street = match (get(tags, "addr:street"), get(tags, "addr:place")) {
        (Some(s), _) => s,
        (None, Some(p)) => {
            stats.place_only += 1;
            p
        }
        (None, None) => {
            stats.no_street += 1;
            ""
        }
    };
    Some(Tagged {
        numbers: numbers.to_owned(),
        street: street.to_owned(),
        unit: get(tags, "addr:unit").unwrap_or_default().to_owned(),
        city: get(tags, "addr:city").unwrap_or_default().trim().to_owned(),
    })
}

fn push_raws(t: Tagged, p: P, out: &mut Vec<Raw>, stats: &mut OsmStats) {
    let parts: Vec<&str> = t
        .numbers
        .split([';', ','])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if parts.len() > 1 {
        stats.number_lists += 1;
    }
    for n in parts {
        out.push(Raw {
            src: Src::Osm,
            file: 0,
            number: n.to_owned(),
            street: t.street.clone(),
            unit: t.unit.clone(),
            city: t.city.clone(),
            p,
        });
    }
}

struct Interp {
    id: i64,
    refs: Vec<i64>,
    step: Option<u32>,
    parity: Option<u32>,
    street: Option<String>,
}

#[derive(Default)]
struct WayBlock {
    streets: Vec<StreetWay>,
    addr_ways: Vec<(Vec<i64>, Tagged)>,
    interps: Vec<Interp>,
    stats: OsmStats,
}

#[derive(Default)]
struct NodeBlock {
    raws: Vec<Raw>,
    /// Numbered nodes on interpolation ways: `(id, number, addr:street)`.
    endpoints: Vec<(i64, String, String)>,
    stats: OsmStats,
}

/// `addresses`: also collect addressed ways and interpolation lines, not only streets.
fn ways_block(block: &PrimitiveBlock, out: &mut WayBlock, addresses: bool) {
    let mut tags = Vec::new();
    for el in block.elements() {
        match el {
            Element::Way(w) => {
                tags.clear();
                tags.extend(w.tags());
                if let Some(h) = get(&tags, "highway")
                    && !NOT_STREETS.contains(&h)
                {
                    let split = |vs: &mut dyn Iterator<Item = &str>| {
                        let mut v: Vec<String> = vs
                            .flat_map(|v| v.split(';'))
                            .map(|s| s.trim().to_owned())
                            .filter(|s| !s.is_empty())
                            .collect();
                        v.dedup();
                        v
                    };
                    let mut names = split(&mut NAME_KEYS.iter().filter_map(|k| get(&tags, k)));
                    let nnames = names.len();
                    names.extend(split(&mut get(&tags, "ref").into_iter()));
                    if !names.is_empty() {
                        out.streets.push(StreetWay {
                            id: w.id(),
                            names,
                            nnames,
                            class: highway_class(h),
                            refs: w.refs().collect(),
                            coords: Vec::new(),
                        });
                    }
                }
                if !addresses {
                    continue;
                }
                if let Some(kind) = get(&tags, "addr:interpolation") {
                    let (step, parity) = match kind {
                        "odd" => (Some(2), Some(1)),
                        "even" => (Some(2), Some(0)),
                        "all" => (Some(1), None),
                        n => (n.parse().ok().filter(|&s| s > 0), None),
                    };
                    out.interps.push(Interp {
                        id: w.id(),
                        refs: w.refs().collect(),
                        step,
                        parity,
                        street: get(&tags, "addr:street").map(str::to_owned),
                    });
                } else if let Some(t) = tagged(&tags, &mut out.stats) {
                    out.stats.address_ways += 1;
                    out.addr_ways.push((w.refs().collect(), t));
                }
            }
            Element::Relation(r) if r.tags().any(|(k, _)| k == "addr:housenumber") => {
                out.stats.address_relations += 1;
            }
            _ => {}
        }
    }
}

fn nodes_block(
    block: &PrimitiveBlock,
    out: &mut NodeBlock,
    locs: &RankedLocations,
    endpoints: &HashSet<i64>,
    addresses: bool,
) {
    let mut tags = Vec::new();
    let visit = |out: &mut NodeBlock, id: i64, lat: i32, lon: i32, tags: &[(&str, &str)]| {
        let p = geom::project_e7(lat, lon);
        locs.set(id, p);
        if tags.is_empty() || !addresses {
            return;
        }
        if endpoints.contains(&id)
            && let Some(n) = get(tags, "addr:housenumber")
        {
            let street = get(tags, "addr:street").unwrap_or_default();
            out.endpoints.push((id, n.to_owned(), street.to_owned()));
        }
        if let Some(t) = tagged(tags, &mut out.stats) {
            out.stats.address_nodes += 1;
            push_raws(t, p, &mut out.raws, &mut out.stats);
        }
    };
    for el in block.elements() {
        tags.clear();
        match el {
            Element::DenseNode(n) => {
                tags.extend(n.tags());
                visit(out, n.id(), n.decimicro_lat(), n.decimicro_lon(), &tags);
            }
            Element::Node(n) => {
                tags.extend(n.tags());
                visit(out, n.id(), n.decimicro_lat(), n.decimicro_lon(), &tags);
            }
            _ => {}
        }
    }
}

fn merge(into: &mut OsmStats, s: &OsmStats) {
    into.address_nodes += s.address_nodes;
    into.address_ways += s.address_ways;
    into.address_relations += s.address_relations;
    into.place_only += s.place_only;
    into.no_street += s.no_street;
    into.number_lists += s.number_lists;
}

#[derive(Serialize)]
pub struct SplitStats {
    /// The whole read; the per-subdivision runs only count streets and interpolation ranges.
    pub osm: OsmStats,
    pub points: u64,
    pub ranges: u64,
    pub streets: u64,
    /// In no subdivision's boundary (other countries, open water).
    pub points_outside: u64,
    pub ranges_outside: u64,
    /// Per subdivision, in the order given: points, ranges and streets it got.
    pub per_subdivision: Vec<[u64; 3]>,
}

/// Splits `all` by `polys`: each address point and interpolation range (by its first
/// vertex) goes to the first polygon that contains it, and each street to every polygon
/// whose bounds, grown by `margin_deg`, its bounds touch.
pub fn split(
    all: OsmAddresses,
    polys: &[Poly],
    margin_deg: f64,
) -> (Vec<OsmAddresses>, SplitStats) {
    let owner = |p: P| polys.iter().position(|poly| poly.contains(p));
    let point_owner: Vec<Option<usize>> = all.raws.par_iter().map(|r| owner(r.p)).collect();
    let range_owner: Vec<Option<usize>> = all
        .ranges
        .par_iter()
        .map(|r| r.line.first().and_then(|&p| owner(p)))
        .collect();
    let bounds: Vec<BBox> = polys.iter().map(|p| p.bbox(margin_deg)).collect();
    let street_owners: Vec<Vec<usize>> = all
        .streets
        .par_iter()
        .map(|s| {
            let b = BBox::of(s.coords.iter().copied());
            let rect = [b.min[0], b.min[1], b.max[0], b.max[1]].map(i64::from);
            (0..polys.len())
                .filter(|&i| geom::bbox_overlaps(&bounds[i], &rect))
                .collect()
        })
        .collect();

    let mut parts: Vec<OsmAddresses> = polys
        .iter()
        .map(|_| OsmAddresses {
            raws: Vec::new(),
            ranges: Vec::new(),
            streets: Vec::new(),
            stats: OsmStats::default(),
        })
        .collect();
    let mut stats = SplitStats {
        points: all.raws.len() as u64,
        ranges: all.ranges.len() as u64,
        streets: all.streets.len() as u64,
        osm: all.stats,
        points_outside: 0,
        ranges_outside: 0,
        per_subdivision: Vec::new(),
    };
    for (r, o) in all.raws.into_iter().zip(point_owner) {
        match o {
            Some(i) => parts[i].raws.push(r),
            None => stats.points_outside += 1,
        }
    }
    for (r, o) in all.ranges.into_iter().zip(range_owner) {
        match o {
            Some(i) => {
                let part = &mut parts[i];
                part.stats.interpolation_ranges += 1;
                part.stats.interpolation_numbers += r.numbers();
                part.ranges.push(r);
            }
            None => stats.ranges_outside += 1,
        }
    }
    for (s, owners) in all.streets.into_iter().zip(street_owners) {
        if let Some((&last, rest)) = owners.split_last() {
            for &i in rest {
                parts[i].streets.push(s.clone());
            }
            parts[last].streets.push(s);
        }
    }
    for p in &mut parts {
        p.stats.street_ways = p.streets.len() as u64;
        stats.per_subdivision.push([
            p.raws.len() as u64,
            p.ranges.len() as u64,
            p.stats.street_ways,
        ]);
    }
    (parts, stats)
}

pub fn read(path: &Path) -> Result<OsmAddresses> {
    read_with(path, true)
}

/// Only the street ways, with their coordinates (for `build`, until it merges addresses).
pub fn read_streets(path: &Path) -> Result<Vec<StreetWay>> {
    Ok(read_with(path, false)?.streets)
}

fn read_with(path: &Path, addresses: bool) -> Result<OsmAddresses> {
    let mut stats = OsmStats::default();
    let mut streets = Vec::new();
    let mut addr_ways = Vec::new();
    let mut interps = Vec::new();
    for mut b in pass(path, WayBlock::default, |block, out| {
        ways_block(block, out, addresses)
    })? {
        merge(&mut stats, &b.stats);
        streets.append(&mut b.streets);
        addr_ways.append(&mut b.addr_ways);
        interps.append(&mut b.interps);
    }
    streets.sort_unstable_by_key(|s| s.id);
    stats.street_ways = streets.len() as u64;
    stats.interpolation_ways = interps.len() as u64;

    let needed = NodeSet::new();
    let all_refs = streets
        .iter()
        .flat_map(|s| &s.refs)
        .chain(addr_ways.iter().flat_map(|(r, _)| r))
        .chain(interps.iter().flat_map(|i| &i.refs));
    for &id in all_refs {
        needed.insert(id);
    }
    let endpoint_ids: HashSet<i64> = interps.iter().flat_map(|i| i.refs.clone()).collect();

    let locs = RankedLocations::new(needed);
    let mut raws = Vec::new();
    let mut endpoints: HashMap<i64, (String, String)> = HashMap::new();
    for mut b in pass(path, NodeBlock::default, |block, out| {
        nodes_block(block, out, &locs, &endpoint_ids, addresses)
    })? {
        merge(&mut stats, &b.stats);
        raws.append(&mut b.raws);
        endpoints.extend(b.endpoints.into_iter().map(|(id, n, s)| (id, (n, s))));
    }

    for s in &mut streets {
        let refs = std::mem::take(&mut s.refs);
        for id in refs {
            if let Some(p) = locs.get(id) {
                s.refs.push(id);
                s.coords.push(p);
            }
        }
    }
    streets.retain(|s| s.coords.len() >= 2);

    for (refs, t) in addr_ways {
        let coords: Vec<P> = refs.iter().filter_map(|&id| locs.get(id)).collect();
        let p = match geom::closed_way_ring(&coords, true) {
            Some(ring) => geom::centroid(&[ring]),
            None if !coords.is_empty() => coords[coords.len() / 2],
            None => continue,
        };
        push_raws(t, p, &mut raws, &mut stats);
    }

    let mut ranges = Vec::new();
    for i in interps {
        interpolate(&i, &locs, &endpoints, &mut ranges, &mut stats);
    }
    raws.sort_unstable_by_key(|r| r.p);
    Ok(OsmAddresses {
        raws,
        ranges,
        streets,
        stats,
    })
}

/// One range per pair of consecutive numbered nodes of an `addr:interpolation` way. The
/// numbered nodes themselves are already address points.
fn interpolate(
    i: &Interp,
    locs: &RankedLocations,
    endpoints: &HashMap<i64, (String, String)>,
    out: &mut Vec<Range>,
    stats: &mut OsmStats,
) {
    let Some(coords) = i
        .refs
        .iter()
        .map(|&id| locs.get(id))
        .collect::<Option<Vec<P>>>()
    else {
        stats.interpolation_failed_pairs += 1;
        return;
    };
    let numbered: Vec<(usize, &String, &String)> = i
        .refs
        .iter()
        .enumerate()
        .filter_map(|(k, id)| endpoints.get(id).map(|(n, s)| (k, n, s)))
        .collect();
    for pair in numbered.windows(2) {
        let (ka, na, sa) = pair[0];
        let (kb, nb, sb) = pair[1];
        let plain = |n: &str| house_number(n).filter(|h| h.kind == NumberKind::Plain);
        let (Some(a), Some(b), Some(step)) = (plain(na), plain(nb), i.step) else {
            stats.interpolation_failed_pairs += 1;
            continue;
        };
        let (a, b) = (a.number, b.number);
        let span = a.abs_diff(b);
        let parity_ok = i.parity.is_none_or(|p| a % 2 == p && b % 2 == p);
        if span == 0 || span % step != 0 || span / step > MAX_INTERPOLATED || !parity_ok || kb == ka
        {
            stats.interpolation_failed_pairs += 1;
            continue;
        }
        let street = i
            .street
            .clone()
            .or_else(|| [sa, sb].into_iter().find(|s| !s.is_empty()).cloned())
            .unwrap_or_default();
        let range = Range {
            src: RangeSrc::OsmInterpolation,
            tlid: i.id as u64,
            right: None,
            from: a,
            to: b,
            parity: match i.parity {
                Some(1) => b'O',
                Some(_) => b'E',
                None => b'B',
            },
            street,
            line: coords[ka..=kb].to_vec(),
        };
        stats.interpolation_ranges += 1;
        stats.interpolation_numbers += range.numbers();
        out.push(range);
    }
}
