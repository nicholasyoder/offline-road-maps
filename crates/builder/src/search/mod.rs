//! Search entries and index (phase 2 step 2.3): one entry per admin area, place, street,
//! POI, named map label (lakes, parks) and postcode, an `fst` from token to postings, and
//! the postings themselves. Written by `build` as `SENT`, `STWN`, `SFST` and `SPST`
//! (`docs/format.md`); the query side is step 2.6.
//!
//! **Entry IDs** run in importance order: by [`bucket`] (highest first), then along a
//! Morton curve. So a postings list, which is in ID order, holds the most important
//! candidates first, and the entries near each other in one bucket share blocks.
//!
//! **Places and admin areas** of the same name are one entry: a place node inside an area
//! of a matching level with the same name key (place "Columbus" in the city of Columbus,
//! place=state "Ohio" in Ohio) becomes a place entry that refers to the area, and the area
//! gets no entry of its own.

pub mod encode;

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Instant;

use anyhow::Result;
use omp_core::normalize::{is_generic, postcode_token, search_tokens};
use omp_core::whitelist::Whitelist;
use osmpbf::Element;
use rayon::prelude::*;
use serde::Serialize;

use crate::addr::osm::HIGHWAYS;
use crate::addr::postcodes::Postcode;
use crate::addr::streets::morton;
use crate::admin::read::{ALT_NAMES, parse_population};
use crate::admin::towns::{self, FALLBACK, Fallbacks, Located, Source};
use crate::admin::{self, Admin};
use crate::measure::geom::{Geometry, P};
use crate::measure::osm::Feature;
use crate::measure::peak_rss_mib;
use crate::measure::tiles;
use crate::street::{self, Table};

/// A token in more entries than this is left out of postings when it is
/// [`is_generic`] ("street", "road", "north", "de"), except for the entries all
/// of whose tokens are left out. North America's longest lists are in the size table.
pub const STOP_MIN: u64 = 100_000;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// An admin area with no place node of its name; `class` is its level, `reference` its
    /// admin ID.
    Admin = 0,
    /// A `places` layer node; `class` is its class in the layer, `reference` the admin ID
    /// + 1 of the area it stands for, or 0.
    Place = 1,
    /// A street record; `class` is its road class (`SSTR` flags), `reference` its street ID.
    Street = 2,
    /// A `pois` layer feature; `class` is its class in the layer.
    Poi = 3,
    /// A postcode; `class` 0 for five digits, 1 for Canadian; `reference` the radius in
    /// metres holding 90% of its addresses.
    Postcode = 4,
    /// A `labels` layer feature (lake, river, park, protected area, forest); `class` is
    /// its class in the layer.
    Feature = 5,
}

impl Kind {
    pub const ALL: [Kind; 6] = [
        Kind::Admin,
        Kind::Place,
        Kind::Street,
        Kind::Poi,
        Kind::Postcode,
        Kind::Feature,
    ];

    pub fn from_code(c: u8) -> Option<Self> {
        Self::ALL.get(c as usize).copied()
    }
}

pub struct Entry {
    pub kind: Kind,
    pub class: u16,
    pub importance: u8,
    /// The name shown (`name`, as on the map).
    pub name: String,
    /// Other names that are searchable but not shown (decision 6), and a street's refs.
    pub alts: Vec<String>,
    pub p: P,
    /// The deepest admin area holding `p` (an area's or a merged place's parent).
    pub admin: Option<u32>,
    /// Index + 1 into [`Index::towns`], or 0.
    pub town: u32,
    pub reference: u64,
}

/// Importance buckets: postings and entry IDs run from the highest bucket down.
pub fn bucket(importance: u8) -> u8 {
    importance >> 4
}

/// Ten times the decimal log of `population`, less 30, between 0 and 45: 1,000 people
/// give 0, a million give 30.
fn population_bonus(population: Option<u64>) -> u32 {
    population.filter(|&p| p > 0).map_or(0, |p| {
        (10.0 * (p as f64).log10() - 30.0).clamp(0.0, 45.0) as u32
    })
}

fn clamp(v: u32) -> u8 {
    v.min(255) as u8
}

pub fn place_importance(class: &str, population: Option<u64>) -> u8 {
    let base = match class {
        "country" => 250,
        "state" => 225,
        "ocean" => 200,
        "sea" => 180,
        "city" => 160,
        "town" => 130,
        "bay" => 120,
        "village" => 105,
        "suburb" => 95,
        "hamlet" => 80,
        "neighbourhood" => 75,
        "locality" => 55,
        _ => 35,
    };
    clamp(base + population_bonus(population))
}

pub fn admin_importance(level: u8, town: bool, population: Option<u64>) -> u8 {
    let base = match level {
        2 => 250,
        3 => 235,
        4 => 225,
        5 => 190,
        6 => 170,
        8 if town => 130,
        7 | 8 => 105,
        9 => 90,
        _ => 75,
    };
    clamp(base + population_bonus(population))
}

/// By road class, plus up to 20 for length (4 per doubling over 250 m).
pub fn street_importance(class: u8, length_m: f64) -> u8 {
    let base: u32 = match HIGHWAYS.get(class as usize).copied() {
        Some("motorway") => 150,
        Some("trunk") => 140,
        Some("primary") => 130,
        Some("secondary") => 120,
        Some("tertiary") => 110,
        Some("unclassified") => 90,
        Some("residential") => 85,
        Some("living_street" | "pedestrian") => 75,
        Some("road") => 70,
        Some(c) if c.ends_with("_link") => 60,
        Some("busway" | "service") => 50,
        Some("track") => 45,
        _ => 40,
    };
    let bonus = (4.0 * (length_m / 250.0).max(1.0).log2()).min(20.0) as u32;
    clamp(base + bonus)
}

pub fn poi_importance(class: &str) -> u8 {
    match class {
        "airport" => 170,
        "hospital" => 130,
        "attraction" => 120,
        "rest_area" => 100,
        "hotel" | "campground" => 95,
        "fuel" => 90,
        "restaurant" => 85,
        "fast_food" => 80,
        "charging_station" => 75,
        "parking" => 50,
        _ => 60,
    }
}

/// Labels by the zoom they first show at: 12 per zoom level below z14, from 60.
pub fn feature_importance(minzoom: u8) -> u8 {
    clamp(60 + 12 * u32::from(14u8.saturating_sub(minzoom)))
}

pub const POSTCODE_IMPORTANCE: u8 = 110;

/// Which layer a feature comes from.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Layer {
    Places,
    Pois,
    Labels,
}

/// A place, POI or label feature, as collected over the bands.
struct Feat {
    layer: Layer,
    id: i64,
    class: u16,
    name: String,
    p: P,
    population: Option<u64>,
    minzoom: u8,
}

/// Collects the named point features of the `places`, `pois` and `labels` layers, each
/// once over all bands.
pub struct Collector {
    layers: Vec<(u8, Layer, Option<u8>, Option<u8>)>,
    minzoom: Vec<Vec<u8>>,
    seen: HashSet<(u8, i64)>,
    feats: Vec<Feat>,
}

impl Collector {
    pub fn new(wl: &Whitelist) -> Self {
        let prop = |l: usize, name: &str| {
            wl.layers[l]
                .props
                .iter()
                .position(|p| p == name)
                .map(|i| i as u8)
        };
        let layers = [
            ("places", Layer::Places),
            ("pois", Layer::Pois),
            ("labels", Layer::Labels),
        ]
        .into_iter()
        .filter_map(|(n, k)| {
            let l = wl.layer_index(n)?;
            Some((l as u8, k, prop(l, "name"), prop(l, "population")))
        })
        .collect();
        Self {
            layers,
            minzoom: tiles::minzooms(wl),
            seen: HashSet::new(),
            feats: Vec::new(),
        }
    }

    pub fn add(&mut self, features: &[Feature]) {
        for f in features {
            let Some(&(_, layer, Some(name_prop), pop_prop)) =
                self.layers.iter().find(|l| l.0 == f.layer)
            else {
                continue;
            };
            let Geometry::Point(p) = f.geom else { continue };
            let prop = |k: u8| f.props.iter().find(|(pk, _)| *pk == k).map(|(_, v)| &**v);
            let Some(name) = prop(name_prop).map(str::trim).filter(|n| !n.is_empty()) else {
                continue;
            };
            if !self.seen.insert((f.layer, f.id)) {
                continue;
            }
            self.feats.push(Feat {
                layer,
                id: f.id,
                class: f.class,
                name: name.to_owned(),
                p,
                population: pop_prop.and_then(prop).and_then(parse_population),
                minzoom: tiles::feature_minzoom(&self.minzoom, f),
            });
        }
    }
}

/// Tags read as alternative names of places, POIs and labels (decision 6); `brand` only
/// for POIs. Values may be `;` lists.
fn alt_keys() -> impl Iterator<Item = &'static str> {
    ALT_NAMES.iter().copied().chain(["brand"])
}

/// One pass over the PBF for the [`alt_keys`] of the features `ids` (`Feature::id`).
fn read_alts(path: &Path, ids: &HashSet<i64>) -> Result<HashMap<i64, Vec<(String, String)>>> {
    let keys: Vec<&str> = alt_keys().collect();
    let blocks = admin::read::pass(path, Vec::new, |block, out| {
        let mut visit = |fid: i64, tags: &mut dyn Iterator<Item = (&str, &str)>| {
            if !ids.contains(&fid) {
                return;
            }
            let found: Vec<(String, String)> = tags
                .filter(|(k, _)| keys.contains(k))
                .flat_map(|(k, v)| {
                    v.split(';')
                        .map(str::trim)
                        .filter(|v| !v.is_empty())
                        .map(move |v| (k.to_owned(), v.to_owned()))
                })
                .collect();
            if !found.is_empty() {
                out.push((fid, found));
            }
        };
        for el in block.elements() {
            match el {
                Element::DenseNode(n) => visit(n.id() * 4, &mut n.tags()),
                Element::Node(n) => visit(n.id() * 4, &mut n.tags()),
                Element::Way(w) => visit(w.id() * 4 + 1, &mut w.tags()),
                Element::Relation(r) => visit(r.id() * 4 + 2, &mut r.tags()),
            }
        }
    })?;
    Ok(blocks.into_iter().flatten().collect())
}

/// Alternative names that are not just the name again: distinct, with a token set other
/// than the name's.
fn distinct_alts(name: &str, alts: impl IntoIterator<Item = String>) -> Vec<String> {
    let own = token_set(name);
    let mut out: Vec<String> = Vec::new();
    for a in alts {
        if a != name && !out.contains(&a) && token_set(&a) != own {
            out.push(a);
        }
    }
    out.sort_unstable();
    out
}

fn token_set(s: &str) -> Vec<String> {
    let mut t = search_tokens(s);
    t.sort_unstable();
    t
}

/// The levels an area may have to stand for a place of this class.
fn merge_levels(class: &str) -> &'static [u8] {
    match class {
        "country" => &[2],
        "state" => &[3, 4],
        "city" | "town" | "village" | "hamlet" | "isolated_dwelling" => &[5, 6, 7, 8],
        "suburb" | "neighbourhood" | "locality" => &[7, 8, 9, 10],
        _ => &[],
    }
}

/// Place classes that are a town themselves, so they need none shown with them.
fn is_town_class(class: &str) -> bool {
    matches!(
        class,
        "country" | "state" | "ocean" | "sea" | "bay" | "city" | "town" | "village"
    )
}

#[derive(Serialize, Default)]
pub struct Stats {
    pub entries: u64,
    pub by_kind: Vec<(Kind, u64)>,
    /// Places that stand for an admin area, and the areas left with an entry of their own.
    pub places_merged: u64,
    pub admin_unmerged: u64,
    pub with_alts: u64,
    pub with_town: u64,
    pub towns: u64,
    pub tokens: u64,
    /// Tokens left out of postings ([`STOP_MIN`]), and the entries they still list.
    pub stopped: Vec<(String, u64, u64)>,
    pub postings: u64,
    /// The longest lists before stopping: `(token, entries)`.
    pub longest: Vec<(String, u64)>,
    pub alt_pass_s: f64,
    pub build_s: f64,
}

/// The entries in ID order, the town table and the postings.
pub struct Index {
    pub entries: Vec<Entry>,
    /// Sorted; entries refer to them by index + 1.
    pub towns: Vec<String>,
    /// `(token, entry IDs in order, entries holding the token)`, sorted by token.
    pub postings: Vec<(String, Vec<u32>, u64)>,
    pub stats: Stats,
}

/// Builds the entries over the bands: streets first (from the table, before it is
/// dropped), then the features, then the rest in [`Builder::finish`].
pub struct Builder {
    entries: Vec<Entry>,
    towns: HashMap<String, u32>,
    town_list: Vec<String>,
    /// Streets whose town comes from their addresses, for the neighbour fallbacks.
    postal: Vec<Located>,
}

impl Default for Builder {
    fn default() -> Self {
        Self::new()
    }
}

impl Builder {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            towns: HashMap::new(),
            town_list: Vec::new(),
            postal: Vec::new(),
        }
    }

    fn town(&mut self, name: &str) -> u32 {
        if name.is_empty() {
            return 0;
        }
        if let Some(&id) = self.towns.get(name) {
            return id;
        }
        self.town_list.push(name.to_owned());
        let id = self.town_list.len() as u32;
        self.towns.insert(name.to_owned(), id);
        id
    }

    /// One entry per street record, at its middle.
    pub fn add_streets(&mut self, table: &Table, admin: &Admin) {
        let mids: Vec<P> = table
            .streets
            .par_iter()
            .map(|s| street::point_at(&s.parts, &s.along, s.along.length_m() / 2.0))
            .collect();
        self.entries.reserve(table.streets.len());
        for (id, (s, &p)) in table.streets.iter().zip(&mids).enumerate() {
            let town = self.town(&s.town);
            let mut alts = s.alt_names.clone();
            alts.extend(s.refs.iter().cloned());
            self.entries.push(Entry {
                kind: Kind::Street,
                class: u16::from(s.class),
                importance: street_importance(s.class, s.along.length_m()),
                name: s.name.clone(),
                alts,
                p,
                admin: s.admin,
                town,
                reference: id as u64,
            });
        }
        let postal: Vec<Located> = table
            .streets
            .par_iter()
            .zip(&mids)
            .filter(|(s, _)| s.town_source == Source::Addresses)
            .map(|(s, &p)| Located {
                p,
                chain: admin.chain(p),
                postal: Some((s.town.clone(), towns::key(&s.town))),
            })
            .collect();
        self.postal.extend(postal);
    }

    /// Admin areas, the collected features and the postcodes; then IDs and postings.
    pub fn finish(
        mut self,
        admin: &Admin,
        wl: &Whitelist,
        collected: Collector,
        pbf: &Path,
        postcodes: &[Postcode],
    ) -> Result<Index> {
        let start = Instant::now();
        let mut stats = Stats::default();
        let feats = collected.feats;
        let ids: HashSet<i64> = feats.iter().map(|f| f.id).collect();
        let alts = read_alts(pbf, &ids)?;
        drop(ids);
        stats.alt_pass_s = start.elapsed().as_secs_f64();
        eprintln!(
            "  search: alternative names of {} features, {:.1} s, peak RSS {:.0} MiB",
            feats.len(),
            stats.alt_pass_s,
            peak_rss_mib()
        );
        let class_name = |layer: &str, class: u16| -> &str {
            wl.layer_index(layer)
                .and_then(|l| wl.layers[l].classes.get(class as usize))
                .map_or("", |c| c.name.as_str())
        };
        let feat_alts = |f: &Feat| -> Vec<String> {
            let brand = f.layer == Layer::Pois;
            alts.get(&f.id)
                .into_iter()
                .flatten()
                .filter(|(k, _)| brand || k != "brand")
                .map(|(_, v)| v.clone())
                .collect()
        };

        // Places, and the admin areas they stand for.
        let places: Vec<usize> = (0..feats.len())
            .filter(|&i| feats[i].layer == Layer::Places)
            .collect();
        let chains: Vec<Vec<u32>> = feats.par_iter().map(|f| admin.chain(f.p)).collect();
        let mut claims: HashMap<u32, (u8, usize)> = HashMap::new();
        for &i in &places {
            let f = &feats[i];
            let class = class_name("places", f.class);
            let levels = merge_levels(class);
            let key = towns::key(&f.name);
            let Some(&a) = chains[i].iter().rev().find(|&&a| {
                let area = &admin.areas[a as usize];
                levels.contains(&area.level) && towns::key(&area.name) == key
            }) else {
                continue;
            };
            let imp = place_importance(class, f.population);
            let c = claims.entry(a).or_insert((imp, i));
            if (imp, std::cmp::Reverse(i)) > (c.0, std::cmp::Reverse(c.1)) {
                *c = (imp, i);
            }
        }
        let merged: HashMap<usize, u32> = claims.iter().map(|(&a, &(_, i))| (i, a)).collect();
        stats.places_merged = merged.len() as u64;

        // Rows for the town fallbacks: the postal streets, then each entry that needs one.
        let mut needs_town: Vec<usize> = Vec::new();
        for (i, f) in feats.iter().enumerate() {
            let wants = match f.layer {
                Layer::Pois => true,
                Layer::Places => !is_town_class(class_name("places", f.class)),
                Layer::Labels => false,
            };
            if wants {
                needs_town.push(i);
            }
        }
        let mut rows = std::mem::take(&mut self.postal);
        let first = rows.len();
        rows.extend(needs_town.iter().map(|&i| Located {
            p: feats[i].p,
            chain: chains[i].clone(),
            postal: None,
        }));
        rows.extend(postcodes.iter().map(|pc| Located {
            p: pc.p,
            chain: admin.chain(pc.p),
            postal: None,
        }));
        let fb = Fallbacks::new(admin, &rows);
        let found: Vec<Option<String>> = (first..rows.len())
            .into_par_iter()
            .map(|r| fb.first(r, &FALLBACK).map(|(_, (name, _))| name))
            .collect();
        let (feat_towns, postcode_towns) = found.split_at(needs_town.len());
        let mut town_of: HashMap<usize, &str> = HashMap::new();
        for (&i, t) in needs_town.iter().zip(feat_towns) {
            if let Some(t) = t
                && towns::key(t) != towns::key(&feats[i].name)
            {
                town_of.insert(i, t);
            }
        }

        for (i, f) in feats.iter().enumerate() {
            let town = town_of.get(&i).map_or(0, |t| self.town(t));
            let mut entry = Entry {
                kind: Kind::Place,
                class: f.class,
                importance: 0,
                name: f.name.clone(),
                alts: Vec::new(),
                p: f.p,
                admin: chains[i].last().copied(),
                town,
                reference: 0,
            };
            let mut alt_list = feat_alts(f);
            match f.layer {
                Layer::Places => {
                    entry.importance =
                        place_importance(class_name("places", f.class), f.population);
                    if let Some(&a) = merged.get(&i) {
                        let area = &admin.areas[a as usize];
                        entry.importance = entry.importance.max(admin_importance(
                            area.level,
                            admin.towns[a as usize],
                            area.population,
                        ));
                        alt_list.extend(area.alt_names.iter().map(|(_, v)| v.clone()));
                        entry.admin = admin.parents[a as usize];
                        entry.reference = u64::from(a) + 1;
                    }
                }
                Layer::Pois => {
                    entry.kind = Kind::Poi;
                    entry.importance = poi_importance(class_name("pois", f.class));
                }
                Layer::Labels => {
                    entry.kind = Kind::Feature;
                    entry.importance = feature_importance(f.minzoom);
                }
            }
            entry.alts = distinct_alts(&entry.name, alt_list);
            self.entries.push(entry);
        }
        drop(alts);

        // Admin areas no place stands for.
        let claimed: HashSet<u32> = claims.keys().copied().collect();
        for (a, area) in admin.areas.iter().enumerate() {
            if claimed.contains(&(a as u32)) || area.name.is_empty() {
                continue;
            }
            stats.admin_unmerged += 1;
            self.entries.push(Entry {
                kind: Kind::Admin,
                class: u16::from(area.level),
                importance: admin_importance(area.level, admin.towns[a], area.population),
                name: area.name.clone(),
                alts: distinct_alts(&area.name, area.alt_names.iter().map(|(_, v)| v.clone())),
                p: admin.labels[a],
                admin: admin.parents[a],
                town: 0,
                reference: a as u64,
            });
        }

        for (pc, town) in postcodes.iter().zip(postcode_towns) {
            let town = town.as_deref().map_or(0, |t| self.town(t));
            self.entries.push(Entry {
                kind: Kind::Postcode,
                class: u16::from(pc.code.contains(' ')),
                importance: POSTCODE_IMPORTANCE,
                name: pc.code.clone(),
                alts: Vec::new(),
                p: pc.p,
                admin: admin.chain(pc.p).last().copied(),
                town,
                reference: pc.radius_m.round() as u64,
            });
        }

        Ok(index(self.entries, self.town_list, stats, start, STOP_MIN))
    }
}

/// The tokens an entry is found by.
pub fn entry_tokens(e: &Entry) -> Vec<String> {
    if e.kind == Kind::Postcode {
        return vec![postcode_token(&e.name)];
    }
    let mut t = search_tokens(&e.name);
    for a in &e.alts {
        for w in search_tokens(a) {
            if !t.contains(&w) {
                t.push(w);
            }
        }
    }
    t
}

/// Orders the entries into IDs, sorts the town table and builds the postings, leaving out
/// generic tokens in more than `stop_min` entries.
fn index(
    mut entries: Vec<Entry>,
    town_list: Vec<String>,
    mut stats: Stats,
    start: Instant,
    stop_min: u64,
) -> Index {
    entries.par_sort_unstable_by(|a, b| {
        (
            std::cmp::Reverse(bucket(a.importance)),
            morton(a.p),
            a.kind,
            &a.name,
            a.reference,
        )
            .cmp(&(
                std::cmp::Reverse(bucket(b.importance)),
                morton(b.p),
                b.kind,
                &b.name,
                b.reference,
            ))
    });

    // Towns sorted, entries renumbered.
    let mut order: Vec<u32> = (0..town_list.len() as u32).collect();
    order.sort_by(|&a, &b| town_list[a as usize].cmp(&town_list[b as usize]));
    let mut new_id = vec![0u32; town_list.len() + 1];
    for (k, &old) in order.iter().enumerate() {
        new_id[old as usize + 1] = k as u32 + 1;
    }
    let towns: Vec<String> = order
        .iter()
        .map(|&i| town_list[i as usize].clone())
        .collect();
    for e in &mut entries {
        e.town = new_id[e.town as usize];
    }

    // Tokens.
    let per_entry: Vec<Vec<String>> = entries.par_iter().map(entry_tokens).collect();
    let mut ids: HashMap<String, u32> = HashMap::new();
    let mut names: Vec<String> = Vec::new();
    let mut counts: Vec<u64> = Vec::new();
    let mut token_ids: Vec<Vec<u32>> = Vec::with_capacity(entries.len());
    for toks in per_entry {
        let mut v = Vec::with_capacity(toks.len());
        for t in toks {
            let id = match ids.get(&t) {
                Some(&id) => id,
                None => {
                    let id = names.len() as u32;
                    ids.insert(t.clone(), id);
                    names.push(t);
                    counts.push(0);
                    id
                }
            };
            counts[id as usize] += 1;
            v.push(id);
        }
        token_ids.push(v);
    }
    drop(ids);
    let stopped: Vec<bool> = names
        .iter()
        .zip(&counts)
        .map(|(n, &c)| c > stop_min && is_generic(n))
        .collect();
    let stop = &stopped;
    let mut pairs: Vec<(u32, u32)> = token_ids
        .par_iter()
        .enumerate()
        .flat_map_iter(|(e, toks)| {
            let stopped = stop;
            let all_stopped = toks.iter().all(|&t| stopped[t as usize]);
            toks.iter()
                .filter(move |&&t| all_stopped || !stopped[t as usize])
                .map(move |&t| (t, e as u32))
                .collect::<Vec<_>>()
        })
        .collect();
    drop(token_ids);
    pairs.par_sort_unstable();
    let mut lists: Vec<Vec<u32>> = vec![Vec::new(); names.len()];
    for g in pairs.chunk_by(|a, b| a.0 == b.0) {
        lists[g[0].0 as usize] = g.iter().map(|x| x.1).collect();
    }
    stats.postings = pairs.len() as u64;
    drop(pairs);
    let mut by_name: Vec<u32> = (0..names.len() as u32).collect();
    by_name.par_sort_unstable_by(|&a, &b| names[a as usize].cmp(&names[b as usize]));
    let mut longest: Vec<(String, u64)> =
        names.iter().cloned().zip(counts.iter().copied()).collect();
    longest.par_sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    longest.truncate(40);
    stats.longest = longest;
    let mut postings = Vec::with_capacity(names.len());
    let mut lists: Vec<Option<Vec<u32>>> = lists.into_iter().map(Some).collect();
    for &t in &by_name {
        let t = t as usize;
        let list = lists[t].take().unwrap();
        if stopped[t] {
            stats
                .stopped
                .push((names[t].clone(), counts[t], list.len() as u64));
        }
        postings.push((std::mem::take(&mut names[t]), list, counts[t]));
    }

    stats.entries = entries.len() as u64;
    stats.by_kind = Kind::ALL
        .iter()
        .map(|&k| (k, entries.iter().filter(|e| e.kind == k).count() as u64))
        .collect();
    stats.with_alts = entries.iter().filter(|e| !e.alts.is_empty()).count() as u64;
    stats.with_town = entries.iter().filter(|e| e.town != 0).count() as u64;
    stats.towns = towns.len() as u64;
    stats.tokens = postings.len() as u64;
    stats.stopped.sort_by_key(|a| std::cmp::Reverse(a.1));
    stats.build_s = start.elapsed().as_secs_f64();
    Index {
        entries,
        towns,
        postings,
        stats,
    }
}

pub fn print_stats(s: &Stats) {
    let kinds: Vec<String> = s
        .by_kind
        .iter()
        .map(|(k, n)| format!("{k:?} {n}"))
        .collect();
    eprintln!(
        "  search: {} entries ({}); {} places stand for an admin area, {} areas on their own; \
         {} with alternative names, {} with a town ({} towns); {} tokens, {} postings; {:.1} s",
        s.entries,
        kinds.join(", "),
        s.places_merged,
        s.admin_unmerged,
        s.with_alts,
        s.with_town,
        s.towns,
        s.tokens,
        s.postings,
        s.build_s
    );
    let top: Vec<String> = s
        .longest
        .iter()
        .take(25)
        .map(|(t, n)| format!("{t} {n}"))
        .collect();
    eprintln!("  search: longest lists: {}", top.join(", "));
    let stopped: Vec<String> = s
        .stopped
        .iter()
        .map(|(t, n, kept)| format!("{t} {n} (kept {kept})"))
        .collect();
    eprintln!(
        "  search: {} tokens left out of postings (over {STOP_MIN} entries): {}",
        s.stopped.len(),
        stopped.join(", ")
    );
}

#[cfg(test)]
mod tests;
