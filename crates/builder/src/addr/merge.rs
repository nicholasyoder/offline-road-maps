//! The address merge in `build` (phase 2 step 2.4): `addr-spike`'s stages, run once per state
//! and province as its `--each` does, against one list of OSM street ways. Each merged
//! address keeps the way it attached to; once the street table is built with the postal
//! town votes gathered here, [`crate::street::Table::locate`] turns it into a street and an
//! offset ([`super::store`]).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::Result;
use omp_core::normalize::street_key;
use rayon::prelude::*;
use serde::Serialize;

use super::attach::How;
use super::osm::{self, OsmAddresses, StreetWay};
use super::postcodes::{self, Postcode};
use super::store;
use super::streets::Streets;
use super::{
    Extracted, FileRow, Interner, Normalized, STREET_MARGIN_DEG, SourceOpts, Src, Stages,
    attach_records, clusters, dedup, extract, fallback_ranges, normalize, select,
};
use crate::admin::{Admin, towns};
use crate::fetch::regions::Subdivision;
use crate::measure::geom::{self, P};
use crate::measure::peak_rss_mib;
use crate::measure::poly::Poly;
use crate::search::AddressStreet;

/// Attach and dedup distances (phase 0 decision 10), as `addr-spike`'s defaults.
pub const ATTACH_M: f64 = 200.0;
pub const DEDUP_M: f64 = 250.0;

pub struct Options {
    pub subdivisions: Vec<Subdivision>,
    pub sources: SourceOpts,
    /// Holds `manifest.toml`, the address files and `poly/<ISO>.poly`.
    pub inputs: PathBuf,
    pub attach_m: f64,
    pub dedup_m: f64,
}

/// The address comes from OSM.
pub const OSM: u8 = 1;
/// Its position is approximate (an NAR blockface point).
pub const APPROX: u8 = 2;
/// Unattached, but a way with the same street key is within about 1 km: the street is in
/// OSM and the address is just far from it, so its name needs no search entry.
pub const NEAR: u8 = 4;
/// Set in [`Merged::units`] on an index into [`Merged::unattached`].
pub const UNATTACHED: u32 = 1 << 31;

/// A merged address attached to an OSM way. Strings are IDs in [`Merged`]'s interners.
pub struct Attached {
    /// Index into the street ways [`merge`] returns.
    pub way: u32,
    pub p: P,
    pub num: u32,
    pub suffix: u32,
    pub code: u32,
    pub flags: u8,
}

/// A merged address with no street within the attach distance.
pub struct Unattached {
    pub p: P,
    pub num: u32,
    pub suffix: u32,
    pub code: u32,
    /// The street name its source gives.
    pub name: u32,
    /// Its postal town as the source spells it, or 0.
    pub city: u32,
    pub flags: u8,
}

/// An interpolation range with no merged point in it, on the way nearest its middle.
pub struct Range {
    pub way: u32,
    pub start: P,
    pub end: P,
    pub mid: P,
    pub from: u32,
    pub to: u32,
    pub parity: u8,
    /// TIGER's side, relative to the segment's direction; `None` for OSM, whose line lies
    /// on its side of the street.
    pub right: Option<bool>,
}

/// One state or province.
#[derive(Serialize, Default)]
pub struct SubRow {
    pub iso: String,
    /// Records with a number and a street, from all sources.
    pub records: u64,
    /// Records attached by each method (strict key, loose key, alias, fuzzy), and not.
    pub by: [u64; 4],
    pub unattached_records: u64,
    pub addresses: u64,
    pub attached: u64,
    pub approximate: u64,
    pub units: u64,
    pub ranges: u64,
    pub fallback_ranges: u64,
    pub secs: f64,
}

pub struct Merged {
    pub attached: Vec<Attached>,
    pub unattached: Vec<Unattached>,
    /// `(address, unit)`: an index into `attached`, or into `unattached` with
    /// [`UNATTACHED`] set; sorted.
    pub units: Vec<(u32, u32)>,
    pub unit_names: Interner,
    pub suffixes: Interner,
    /// Street names of unattached addresses.
    pub names: Interner,
    pub cities: Interner,
    pub codes: Interner,
    /// Postal town votes for the street table: `(way, point, city)` for every record that
    /// attached and names a town.
    pub votes: Vec<(u32, P, u32)>,
    pub ranges: Vec<Range>,
    /// Postcode points per state or province, for [`postcodes::combine`].
    pub postcodes: Vec<Postcode>,
    /// Manifest paths of the address files read (OSM is the input itself).
    pub files: Vec<String>,
    pub rows: Vec<SubRow>,
}

/// Merges the addresses of `opts.subdivisions` with the OSM records `all` (read from
/// `osm_file`). Returns the merge and the street ways its addresses refer to.
pub fn merge(
    opts: &Options,
    admin: &Admin,
    all: OsmAddresses,
    osm_file: &Path,
) -> Result<(Merged, Vec<StreetWay>)> {
    let polys: Vec<Poly> = opts
        .subdivisions
        .iter()
        .map(|s| Poly::read(&opts.inputs.join(format!("poly/{}.poly", s.iso()))))
        .collect::<Result<_>>()?;
    let (parts, owned, ways, split) = osm::split_indexed(all, &polys, STREET_MARGIN_DEG);
    eprintln!(
        "  addresses: OSM {} points, {} ranges, {} street ways; {} points and {} ranges in no \
         subdivision",
        split.points, split.ranges, split.streets, split.points_outside, split.ranges_outside
    );
    let mut m = Merged {
        attached: Vec::new(),
        unattached: Vec::new(),
        units: Vec::new(),
        unit_names: Interner::new(),
        suffixes: Interner::new(),
        names: Interner::new(),
        cities: Interner::new(),
        codes: Interner::new(),
        votes: Vec::new(),
        ranges: Vec::new(),
        postcodes: Vec::new(),
        files: Vec::new(),
        rows: Vec::new(),
    };
    for ((sub, part), idx) in opts.subdivisions.iter().zip(parts).zip(owned) {
        merge_one(opts, admin, sub, part, &idx, &ways, osm_file, &mut m)?;
    }
    m.units.sort_unstable();
    m.files.sort_unstable();
    m.files.dedup();
    Ok((m, ways))
}

/// The global IDs of an interner's strings.
fn translate(from: &Interner, to: &mut Interner) -> Vec<u32> {
    from.list.iter().map(|s| to.intern(s)).collect()
}

#[allow(clippy::too_many_arguments)]
fn merge_one(
    opts: &Options,
    admin: &Admin,
    sub: &Subdivision,
    part: OsmAddresses,
    idx: &[u32],
    ways: &[StreetWay],
    osm_file: &Path,
    m: &mut Merged,
) -> Result<()> {
    let start = Instant::now();
    let mut stages = Stages::quiet();
    let mut files = vec![FileRow {
        path: osm_file.display().to_string(),
        source: "osm".into(),
        license: "ODbL-1.0".into(),
        excluded: false,
        records: 0,
        kept: 0,
        conflicts: 0,
    }];
    let sel = select(
        &opts.sources,
        &opts.inputs,
        std::slice::from_ref(sub),
        &mut files,
    )?;
    let (osm_raws, osm_ranges) = match sel.osm {
        true => (part.raws, part.ranges),
        false => (Vec::new(), Vec::new()),
    };
    let Extracted {
        raws, mut ranges, ..
    } = extract(&sel, osm_raws, &mut stages)?;
    ranges.extend(osm_ranges);
    let Normalized {
        mut recs,
        street_names,
        suffixes,
        units,
        cities,
        codes,
        mut keys,
        ..
    } = normalize(raws, &mut files);
    let local: Vec<StreetWay> = idx.iter().map(|&w| ways[w as usize].clone()).collect();
    let streets = Streets::build(local, &mut keys);
    let attach = attach_records(
        &mut recs,
        &street_names,
        &cities,
        &streets,
        &keys,
        admin,
        opts.attach_m,
    );
    let (_, merge) = dedup(&recs, &keys, opts.dedup_m, false);
    let (masks, cluster_units, cluster_codes) = clusters(&recs, &merge);
    let (_, fallbacks) = fallback_ranges(
        &ranges,
        &mut keys,
        &recs,
        &merge.reps,
        &masks,
        &streets,
        opts.attach_m,
    );
    m.postcodes.extend(postcodes::points(
        &recs,
        &merge.reps,
        &cluster_codes,
        &codes.list,
    ));

    let city = translate(&cities, &mut m.cities);
    let suffix = translate(&suffixes, &mut m.suffixes);
    let code = translate(&codes, &mut m.codes);
    let unit = translate(&units, &mut m.unit_names);
    let mut row = SubRow {
        iso: sub.iso(),
        records: recs.len() as u64,
        addresses: merge.reps.len() as u64,
        units: cluster_units.len() as u64,
        ranges: ranges.len() as u64,
        fallback_ranges: fallbacks.len() as u64,
        ..Default::default()
    };
    for a in attach.values() {
        row.unattached_records += a.records;
    }
    for r in &recs {
        let Some(a) = r.att else { continue };
        let k = match a.how {
            How::Strict => 0,
            How::Loose => 1,
            How::Alias => 2,
            How::Fuzzy => 3,
        };
        row.by[k] += 1;
        row.unattached_records -= 1;
        if r.city != 0 {
            m.votes
                .push((idx[a.way as usize], r.p, city[r.city as usize]));
        }
    }
    let mut slot = Vec::with_capacity(merge.reps.len());
    for (c, &ri) in merge.reps.iter().enumerate() {
        let r = &recs[ri as usize];
        let mut flags = 0;
        if r.src == Src::Osm {
            flags |= OSM;
        }
        if r.src.approx() {
            flags |= APPROX;
            row.approximate += 1;
        }
        let (suffix, code) = (suffix[r.suffix as usize], code[cluster_codes[c] as usize]);
        match r.att {
            Some(a) => {
                slot.push(m.attached.len() as u32);
                m.attached.push(Attached {
                    way: idx[a.way as usize],
                    p: r.p,
                    num: r.num,
                    suffix,
                    code,
                    flags,
                });
                row.attached += 1;
            }
            None => {
                if streets.nearest(r.key, false, r.p).is_some() {
                    flags |= NEAR;
                }
                slot.push(m.unattached.len() as u32 | UNATTACHED);
                m.unattached.push(Unattached {
                    p: r.p,
                    num: r.num,
                    suffix,
                    code,
                    name: m.names.intern(&street_names.list[r.street as usize]),
                    city: city[r.city as usize],
                    flags,
                });
            }
        }
    }
    for (c, u) in cluster_units {
        m.units.push((slot[c as usize], unit[u as usize]));
    }
    m.ranges.extend(fallbacks.into_iter().map(|f| Range {
        way: idx[f.way as usize],
        start: f.start,
        end: f.end,
        mid: f.mid,
        from: f.from,
        to: f.to,
        parity: f.parity,
        right: f.right,
    }));
    m.files.extend(
        files
            .iter()
            .skip(1)
            .filter(|f| !f.excluded)
            .map(|f| f.path.clone()),
    );
    row.secs = start.elapsed().as_secs_f64();
    eprintln!(
        "  addresses {}: {} records ({} strict, {} loose, {} alias, {} fuzzy, {} unattached) → \
         {} addresses, {:.2}% attached, {} units, {} of {} ranges kept; {:.1} s, peak RSS {:.0} MiB",
        row.iso,
        row.records,
        row.by[0],
        row.by[1],
        row.by[2],
        row.by[3],
        row.unattached_records,
        row.addresses,
        100.0 * row.attached as f64 / row.addresses.max(1) as f64,
        row.units,
        row.fallback_ranges,
        row.ranges,
        row.secs,
        peak_rss_mib()
    );
    m.rows.push(row);
    Ok(())
}

/// The streets known only from unattached addresses with no OSM way of their key near
/// ([`NEAR`] unset): one per street key and deepest admin area, at the median of its
/// addresses, named by the commonest spelling (one in mixed case if any) and the postal town
/// most of them give.
pub fn address_streets(m: &Merged, admin: &Admin) -> Vec<AddressStreet> {
    let name_keys: Vec<String> = m.names.list.par_iter().map(|n| street_key(n)).collect();
    let mut key_ids: HashMap<&str, u32> = HashMap::new();
    let key_of: Vec<u32> = name_keys
        .iter()
        .map(|k| {
            let n = key_ids.len() as u32;
            *key_ids.entry(k.as_str()).or_insert(n)
        })
        .collect();
    // By key and z14 cell first, so the admin chain is looked up once per cell.
    let mut pts: Vec<(u32, u32, u32)> = m
        .unattached
        .par_iter()
        .enumerate()
        .filter(|(_, u)| u.flags & NEAR == 0 && !name_keys[u.name as usize].is_empty())
        .map(|(i, u)| (key_of[u.name as usize], store::cell_of(u.p), i as u32))
        .collect();
    pts.par_sort_unstable();
    let cells: Vec<&[(u32, u32, u32)]> = pts.chunk_by(|a, b| (a.0, a.1) == (b.0, b.1)).collect();
    let mut by_area: Vec<(u32, u32, u32)> = cells
        .par_iter()
        .flat_map_iter(|g| {
            let deepest = admin
                .chain(m.unattached[g[0].2 as usize].p)
                .last()
                .copied()
                .unwrap_or(u32::MAX);
            g.iter().map(move |&(k, _, i)| (k, deepest, i))
        })
        .collect();
    drop(pts);
    by_area.par_sort_unstable();
    let groups: Vec<&[(u32, u32, u32)]> =
        by_area.chunk_by(|a, b| (a.0, a.1) == (b.0, b.1)).collect();
    groups
        .par_iter()
        .map(|g| {
            let us: Vec<&Unattached> = g.iter().map(|x| &m.unattached[x.2 as usize]).collect();
            let mut xs: Vec<u32> = us.iter().map(|u| u.p[0]).collect();
            let mut ys: Vec<u32> = us.iter().map(|u| u.p[1]).collect();
            xs.sort_unstable();
            ys.sort_unstable();
            let p = [xs[xs.len() / 2], ys[ys.len() / 2]];
            let mpu = geom::metres_per_unit(p);
            let mut d: Vec<f64> = us
                .iter()
                .map(|u| {
                    (f64::from(u.p[0]) - f64::from(p[0])).hypot(f64::from(u.p[1]) - f64::from(p[1]))
                        * mpu
                })
                .collect();
            d.sort_by(f64::total_cmp);
            let commonest = |ids: &mut dyn Iterator<Item = u32>| {
                let mut counts: HashMap<u32, u32> = HashMap::new();
                for id in ids {
                    *counts.entry(id).or_default() += 1;
                }
                counts
                    .into_iter()
                    .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0)))
                    .map(|c| c.0)
            };
            let mixed = |id: &u32| m.names.list[*id as usize].chars().any(char::is_lowercase);
            let name = commonest(&mut us.iter().map(|u| u.name).filter(mixed))
                .or_else(|| commonest(&mut us.iter().map(|u| u.name)))
                .unwrap();
            let chain = admin.chain(p);
            let postal = commonest(&mut us.iter().map(|u| u.city).filter(|&c| c != 0))
                .map(|c| towns::display(admin, &m.cities.list[c as usize], &chain, p));
            AddressStreet {
                name: m.names.list[name as usize].clone(),
                p,
                chain,
                radius_m: d[((d.len() - 1) as f64 * 0.9) as usize],
                postal,
                addresses: us.len() as u64,
            }
        })
        .collect()
}
