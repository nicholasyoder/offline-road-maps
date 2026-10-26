//! The town of each street (phase 2 step 2.1, decision 4): the postal town most of its
//! addresses give, else a fallback. Measured here on the spike's streets; step 2.2 moves
//! it onto the street table.

use std::collections::{BTreeMap, HashMap};
use std::io::Write as _;
use std::path::Path;

use anyhow::Result;
use rayon::prelude::*;
use serde::Serialize;

use super::streets::{Streets, cell};
use crate::admin::Admin;
use crate::admin::towns::{self, Source};
use crate::measure::geom::{self, P};

/// Rows of each kind written to `towns.tsv`.
const SAMPLES: usize = 200;

/// The fallbacks, in the order tried for a street with no postal town. Tried on the
/// streets that have one, as if they had none, this order names their postal town for
/// 95.9% in Ohio and 98.1% in Quebec; the area first, then the nearest place (the plan's
/// first order) for 70.8% and 96.6%.
pub const FALLBACK: [Method; 5] = [
    Method::LocalNeighbours,
    Method::Area,
    Method::Neighbours,
    Method::MajorPlace,
    Method::AnyPlace,
];

/// One way to name a street's town without its own addresses.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Method {
    /// The admin 8 city, town or village it lies in.
    Area,
    /// The postal town most streets within about 1 km give, counting only streets in the
    /// same municipality or township (the deepest level 7 or 8 area).
    LocalNeighbours,
    /// The postal town most streets within about 1 km give.
    Neighbours,
    /// The nearest city, town or village node.
    MajorPlace,
    /// The nearest place node, hamlets included.
    AnyPlace,
}

impl Method {
    fn source(self) -> Source {
        match self {
            Method::Area => Source::Admin,
            Method::LocalNeighbours | Method::Neighbours => Source::Neighbours,
            Method::MajorPlace | Method::AnyPlace => Source::Place,
        }
    }
}

#[derive(Serialize, Default)]
pub struct TownReport {
    /// Streets: components under their ways' own name (not alternates or refs), in the
    /// region.
    pub streets: u64,
    /// Streets read for the run but outside the region, left out of the counts.
    pub outside_region: u64,
    /// Where each street's town comes from, with [`FALLBACK`].
    pub by_source: BTreeMap<Source, u64>,
    /// Each fallback tried on the streets that have a postal town, as if they had none:
    /// `[answered, same town as the postal one]`. Also for [`FALLBACK`] and for the
    /// plan's first order (area, then nearest place).
    pub holdout: BTreeMap<String, [u64; 2]>,
    /// Streets with a postal town that lie in a city, town or village area, and those of
    /// them where the two name different towns.
    pub postal_in_town_area: u64,
    pub postal_differs_from_area: u64,
    /// Streets whose middle lies in no city, town or village area: unincorporated land and
    /// townships.
    pub unincorporated_streets: u64,
    pub unincorporated_with_postal: u64,
    /// Deduplicated addresses, and those in no city, town or village area.
    pub addresses: u64,
    pub unincorporated_addresses: u64,
    /// Streets in no admin area at all, and streets missing each level.
    pub without_chain: u64,
    pub missing_level: BTreeMap<u8, u64>,
    /// Streets in a level 9 or 10 area but no level 8 one.
    pub sub_area_without_city: u64,
}

/// One hold-out test: the method or order, whether it answered, and whether it named the
/// postal town.
type Test = (String, bool, bool);

/// One street's result.
struct StreetTown {
    comp: u32,
    p: P,
    chain: Vec<u32>,
    in_region: bool,
    /// Display name and key of the postal town.
    postal: Option<(String, String)>,
}

/// `votes`: `(way, city)` for each attached address record that names a city, indexing
/// `cities`. `points`: the deduplicated addresses. `region`: count only the streets in the
/// state or province with this ISO code; streets across the border still count as
/// neighbours.
pub fn measure(
    admin: &Admin,
    streets: &Streets,
    region: Option<&str>,
    votes: &[(u32, u32)],
    cities: &[String],
    points: &[P],
    samples: &Path,
) -> Result<TownReport> {
    let mut way_votes: HashMap<u32, HashMap<u32, u32>> = HashMap::new();
    for &(w, c) in votes {
        if !towns::key(&cities[c as usize]).is_empty() {
            *way_votes.entry(w).or_default().entry(c).or_default() += 1;
        }
    }
    let primary: Vec<u32> = (0..streets.comps.len() as u32)
        .filter(|&c| streets.is_primary(c))
        .collect();
    let rows: Vec<StreetTown> = primary
        .par_iter()
        .map(|&c| {
            let comp = &streets.comps[c as usize];
            let p = streets.point_along(c, comp.length_m / 2.0);
            let chain = admin.chain(p);
            // Votes by the town each spelling resolves to.
            let mut by_town: HashMap<String, (u32, String)> = HashMap::new();
            for &w in &comp.ways {
                for (&city, &n) in way_votes.get(&w).into_iter().flatten() {
                    let name = towns::display(admin, &cities[city as usize], &chain, p);
                    let e = by_town.entry(towns::key(&name)).or_insert((0, name));
                    e.0 += n;
                }
            }
            let postal = by_town
                .into_iter()
                .max_by(|a, b| a.1.0.cmp(&b.1.0).then(b.0.cmp(&a.0)))
                .map(|(k, (_, name))| (name, k));
            let in_region = region.is_none_or(|iso| {
                admin
                    .at_level(&chain, 4)
                    .is_some_and(|a| admin.areas[a as usize].iso.as_deref() == Some(iso))
            });
            StreetTown {
                comp: c,
                p,
                chain,
                in_region,
                postal,
            }
        })
        .collect();

    // Postal towns by the spike's ~1 km grid, for the neighbours fallback.
    let mut near: HashMap<(i32, i32), Vec<usize>> = HashMap::new();
    for (i, s) in rows.iter().enumerate() {
        if s.postal.is_some() {
            near.entry(cell(s.p)).or_default().push(i);
        }
    }
    // The deepest level 7 or 8 area of each street.
    let local: Vec<Option<u32>> = rows
        .iter()
        .map(|s| {
            s.chain
                .iter()
                .copied()
                .filter(|&a| matches!(admin.areas[a as usize].level, 7 | 8))
                .max_by_key(|&a| admin.areas[a as usize].level)
        })
        .collect();
    let neighbours = |i: usize, same_area: bool| -> Option<(String, String)> {
        if same_area && local[i].is_none() {
            return None;
        }
        let (cx, cy) = cell(rows[i].p);
        let mut counts: HashMap<&str, (u32, &str)> = HashMap::new();
        for x in cx - 1..=cx + 1 {
            for y in cy - 1..=cy + 1 {
                for &j in near.get(&(x, y)).into_iter().flatten() {
                    if j == i || (same_area && local[j] != local[i]) {
                        continue;
                    }
                    if let Some((name, k)) = &rows[j].postal {
                        counts.entry(k).or_insert((0, name)).0 += 1;
                    }
                }
            }
        }
        counts
            .into_iter()
            .max_by(|a, b| a.1.0.cmp(&b.1.0).then(b.0.cmp(a.0)))
            .map(|(k, (_, name))| (name.to_owned(), k.to_owned()))
    };
    let answer = |i: usize, m: Method| -> Option<(String, String)> {
        let s = &rows[i];
        let name = match m {
            Method::Area => towns::area_town(admin, &s.chain),
            Method::LocalNeighbours => return neighbours(i, true),
            Method::Neighbours => return neighbours(i, false),
            Method::MajorPlace => towns::place_town(admin, s.p, true),
            Method::AnyPlace => towns::place_town(admin, s.p, false),
        }?;
        let k = towns::key(&name);
        Some((name, k))
    };
    let first =
        |i: usize, order: &[Method]| order.iter().find_map(|&m| answer(i, m).map(|a| (m, a)));

    // Each street's town; and the hold-out test on streets with a postal town.
    let plan_order = [Method::Area, Method::AnyPlace];
    let area_first = [
        Method::Area,
        Method::Neighbours,
        Method::MajorPlace,
        Method::AnyPlace,
    ];
    let neighbours_first = [
        Method::Neighbours,
        Method::Area,
        Method::MajorPlace,
        Method::AnyPlace,
    ];
    let results: Vec<(Source, Vec<Test>)> = (0..rows.len())
        .into_par_iter()
        .map(|i| {
            let s = &rows[i];
            if !s.in_region {
                return (Source::None, Vec::new());
            }
            let Some((_, postal)) = &s.postal else {
                let source = first(i, &FALLBACK).map_or(Source::None, |(m, _)| m.source());
                return (source, Vec::new());
            };
            let mut tests: Vec<(String, Option<String>)> = [
                Method::Area,
                Method::LocalNeighbours,
                Method::Neighbours,
                Method::MajorPlace,
                Method::AnyPlace,
            ]
            .into_iter()
            .map(|m| (format!("{m:?}").to_lowercase(), answer(i, m).map(|a| a.1)))
            .collect();
            tests.push((
                "fallback_order".into(),
                first(i, &FALLBACK).map(|(_, a)| a.1),
            ));
            tests.push(("plan_order".into(), first(i, &plan_order).map(|(_, a)| a.1)));
            tests.push(("area_first".into(), first(i, &area_first).map(|(_, a)| a.1)));
            tests.push((
                "neighbours_first".into(),
                first(i, &neighbours_first).map(|(_, a)| a.1),
            ));
            let tests = tests
                .into_iter()
                .map(|(name, a)| {
                    let hit = a.as_ref() == Some(postal);
                    (name, a.is_some(), hit)
                })
                .collect();
            (Source::Addresses, tests)
        })
        .collect();

    let mut r = TownReport {
        streets: rows.iter().filter(|s| s.in_region).count() as u64,
        outside_region: rows.iter().filter(|s| !s.in_region).count() as u64,
        addresses: points.len() as u64,
        ..Default::default()
    };
    r.unincorporated_addresses = points
        .par_iter()
        .filter(|&&p| !admin.chain(p).iter().any(|&a| admin.towns[a as usize]))
        .count() as u64;
    let name_of = |s: &StreetTown| {
        let w = streets.comps[s.comp as usize].ways[0];
        streets.ways[w as usize].name.clone()
    };
    let chain_names = |s: &StreetTown| {
        s.chain
            .iter()
            .map(|&a| admin.areas[a as usize].name.as_str())
            .collect::<Vec<_>>()
            .join(" > ")
    };
    let lat_lon = |p: P| {
        let (lat, lon) = geom::unproject(p);
        format!("{lat:.5},{lon:.5}")
    };
    let mut differs_area: Vec<String> = Vec::new();
    let mut fallback_misses: Vec<String> = Vec::new();
    for (i, (s, (source, tests))) in rows.iter().zip(&results).enumerate() {
        if !s.in_region {
            continue;
        }
        *r.by_source.entry(*source).or_default() += 1;
        for (name, answered, hit) in tests {
            let e = r.holdout.entry(name.clone()).or_default();
            e[0] += u64::from(*answered);
            e[1] += u64::from(*hit);
            if name == "fallback_order" && *answered && !hit {
                let (m, (town, _)) = first(i, &FALLBACK).unwrap();
                fallback_misses.push(format!(
                    "fallback_{m:?}\t{}\t{}\t{town}\t{}\t{}",
                    name_of(s),
                    lat_lon(s.p),
                    s.postal.as_ref().unwrap().0,
                    chain_names(s)
                ));
            }
        }
        if s.chain.is_empty() {
            r.without_chain += 1;
        }
        for level in [2, 4, 6, 8] {
            if admin.at_level(&s.chain, level).is_none() {
                *r.missing_level.entry(level).or_default() += 1;
            }
        }
        let city = admin.at_level(&s.chain, 8);
        if city.is_none() && s.chain.iter().any(|&a| admin.areas[a as usize].level > 8) {
            r.sub_area_without_city += 1;
        }
        let town_area = city.filter(|&a| admin.towns[a as usize]);
        if town_area.is_none() {
            r.unincorporated_streets += 1;
            r.unincorporated_with_postal += u64::from(s.postal.is_some());
        }
        if let (Some((postal, k)), Some(a)) = (&s.postal, town_area) {
            r.postal_in_town_area += 1;
            let area = &admin.areas[a as usize].name;
            if *k != towns::key(area) {
                r.postal_differs_from_area += 1;
                differs_area.push(format!(
                    "postal_vs_area\t{}\t{}\t{postal}\t{area}\t{}",
                    name_of(s),
                    lat_lon(s.p),
                    chain_names(s)
                ));
            }
        }
    }

    std::fs::create_dir_all(samples)?;
    let mut f = std::io::BufWriter::new(std::fs::File::create(samples.join("towns.tsv"))?);
    writeln!(f, "kind\tstreet\tlat,lon\ttown\tpostal or area\tchain")?;
    for list in [&differs_area, &fallback_misses] {
        for &i in &super::sample(list.len(), SAMPLES, 7) {
            writeln!(f, "{}", list[i])?;
        }
    }
    Ok(r)
}
