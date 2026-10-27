//! The stages of the address merge, run by [`super::merge`] once per state and province:
//! [`select`] the files, [`extract`] their records, [`normalize`] them, [`attach_records`]
//! to OSM streets, [`dedup`] across sources, gather each merged address's units and
//! postcode ([`clusters`]), and keep the interpolation ranges with no point in them
//! ([`fallback_ranges`]). They were `addr-spike`'s (phase 0 step 0.4, phase 2 steps
//! 2.1–2.4), which also measured them; the spike was retired in refactor step R3.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use clap::ValueEnum;
use omp_core::normalize::{HouseNumber, house_number, postcode};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use super::attach::{self, How};
use super::sources;
use super::streets::{self, Streets, cell};
use super::tiger;
use crate::admin::Admin;
use crate::fetch::manifest::Manifest;
use crate::fetch::regions::Subdivision;
use crate::geom::{self, P};

#[derive(Clone, Copy, PartialEq, Eq, Debug, ValueEnum)]
pub enum Input {
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
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Src {
    Osm,
    OpenAddresses,
    Nar,
    Oda,
    NarBlockface,
}

impl Src {
    pub fn approx(self) -> bool {
        self == Src::NarBlockface
    }
}

/// One address record as read, before normalization.
pub struct Raw {
    pub src: Src,
    pub number: String,
    pub street: String,
    pub unit: String,
    /// The postal town the source gives (OpenAddresses `city`, NAR `MAIL_MUN_NAME`, ODA
    /// `city`, OSM `addr:city`); empty if none.
    pub city: String,
    /// The postal code as the source gives it (OpenAddresses `postcode`, NAR
    /// `MAIL_POSTAL_CODE`, ODA `postal_code`, OSM `addr:postcode`); empty if none.
    pub postcode: String,
    pub p: P,
}

const NONE: u32 = u32::MAX;

/// Records whose exact key finds a street further than this also look by the loose key.
const LOOSE_FROM_M: f64 = 100.0;
/// Approximate points (NAR blockface) join a cluster of the same address on the same street
/// within this distance, whatever the dedup distance: they are only mid-block estimates.
const APPROX_DEDUP_M: f64 = 1000.0;
/// A range "has a point" if an address on the same street, in the range, lies within this
/// distance of the segment.
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

/// The street a record attached to.
#[derive(Clone, Copy)]
pub struct Att {
    /// Its street component, and the way within it.
    pub comp: u32,
    pub way: u32,
    pub how: How,
}

pub struct Rec {
    pub src: Src,
    pub num: u32,
    pub suffix: u32,
    pub street: u32,
    pub key: u32,
    pub unit: u32,
    pub city: u32,
    /// Normalized postcode ([`omp_core::normalize::postcode`]), interned; 0 for none.
    pub postcode: u32,
    pub p: P,
    pub att: Option<Att>,
}

#[derive(Default, Serialize, Deserialize)]
pub struct Interner {
    map: HashMap<String, u32>,
    pub list: Vec<String>,
}

impl Interner {
    pub fn new() -> Self {
        let mut i = Self::default();
        i.intern("");
        i
    }

    pub fn intern(&mut self, s: &str) -> u32 {
        if let Some(&id) = self.map.get(s) {
            return id;
        }
        let id = self.list.len() as u32;
        self.map.insert(s.to_owned(), id);
        self.list.push(s.to_owned());
        id
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

/// Which sources to read and which licenses to leave out.
pub struct SourceOpts {
    pub sources: Vec<Input>,
    /// Skip files whose license name contains one of these, ignoring case.
    pub exclude_license: Vec<String>,
}

pub struct Selected {
    pub osm: bool,
    openaddresses: Vec<PathBuf>,
    nar: Option<(PathBuf, Vec<&'static str>)>,
    oda: Vec<(PathBuf, &'static str, &'static str)>,
    tiger: Vec<PathBuf>,
    /// Manifest paths of the files selected, the excluded ones left out.
    pub files: Vec<String>,
}

/// Picks the manifest files for the subdivisions and sources, and applies the license filter.
pub fn select(args: &SourceOpts, inputs: &Path, subs: &[Subdivision]) -> Result<Selected> {
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
    let mut files = Vec::new();
    let mut add = |path: &str, license: &str| -> bool {
        let keep = !excluded(license);
        if keep {
            files.push(path.to_owned());
        }
        keep
    };
    let mut sel = Selected {
        osm: wants(Input::Osm),
        openaddresses: Vec::new(),
        nar: None,
        oda: Vec::new(),
        tiger: Vec::new(),
        files: Vec::new(),
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
                if add(&f.path, &f.license.name) {
                    sel.openaddresses.push(path);
                }
            }
            "tiger" if wants(Input::Tiger) => {
                let name = f.path.rsplit('/').next().unwrap_or_default();
                let state = name.split('_').nth(2).and_then(|g| g.get(..2));
                if state.is_some_and(|s| fips.contains(&s)) && add(&f.path, &f.license.name) {
                    sel.tiger.push(path);
                }
            }
            "nar" if wants(Input::Nar) && !provinces.is_empty() => {
                // The newest release; paths are `statcan/nar/<YYYYMM>.zip`.
                if sel.nar.as_ref().is_none_or(|(p, _)| *p < path) && add(&f.path, &f.license.name)
                {
                    sel.nar = Some((path, provinces.iter().map(|p| p.1).collect()));
                }
            }
            "oda" if wants(Input::Oda) => {
                for &(abbr, code) in &provinces {
                    if f.path == format!("statcan/oda/ODA_{abbr}_v1.zip")
                        && add(&f.path, &f.license.name)
                    {
                        sel.oda.push((path.clone(), abbr, code));
                    }
                }
            }
            _ => {}
        }
    }
    sel.files = files;
    Ok(sel)
}

/// Reads the selected files, after the OSM records `raws`. Returns the records and the
/// TIGER ranges.
pub fn extract(sel: &Selected, mut raws: Vec<Raw>) -> Result<(Vec<Raw>, Vec<tiger::Range>)> {
    if !sel.openaddresses.is_empty() {
        raws.append(&mut sources::openaddresses(&sel.openaddresses)?);
    }
    if let Some((path, codes)) = &sel.nar {
        raws.append(&mut sources::nar(path, codes)?);
    }
    for (path, abbr, code) in &sel.oda {
        raws.append(&mut sources::oda(path, abbr, code)?);
    }
    let ranges = match sel.tiger.is_empty() {
        true => Vec::new(),
        false => tiger::read(&sel.tiger)?,
    };
    Ok((raws, ranges))
}

/// Records with a house number and a street, their strings interned and their street keys
/// in `keys`.
pub struct Normalized {
    pub recs: Vec<Rec>,
    pub street_names: Interner,
    pub suffixes: Interner,
    pub units: Interner,
    pub cities: Interner,
    pub codes: Interner,
    pub keys: streets::Keys,
}

pub fn normalize(raws: Vec<Raw>) -> Normalized {
    let parsed: Vec<Option<HouseNumber>> =
        raws.par_iter().map(|r| house_number(&r.number)).collect();
    let mut street_names = Interner::new();
    let mut suffixes = Interner::new();
    let mut units = Interner::new();
    let mut cities = Interner::new();
    let mut codes = Interner::new();
    let mut recs: Vec<Rec> = Vec::with_capacity(raws.len());
    for (r, hn) in raws.into_iter().zip(parsed) {
        // No number, one that does not parse, or 0 (OpenAddresses' "unknown"), or no street.
        let Some(hn) = hn.filter(|hn| hn.number != 0) else {
            continue;
        };
        if r.street.is_empty() {
            continue;
        }
        let code = postcode(&r.postcode);
        recs.push(Rec {
            src: r.src,
            num: hn.number,
            suffix: suffixes.intern(&hn.suffix),
            street: street_names.intern(&r.street),
            key: NONE,
            unit: units.intern(&r.unit),
            city: cities.intern(&r.city),
            postcode: code.map_or(0, |c| codes.intern(&c)),
            p: r.p,
            att: None,
        });
    }
    let mut keys = streets::Keys::default();
    let street_keys = keys.intern_names(&street_names.list);
    for r in &mut recs {
        r.key = street_keys[r.street as usize];
    }
    Normalized {
        recs,
        street_names,
        suffixes,
        units,
        cities,
        codes,
        keys,
    }
}

/// Attaches the records within `attach_m` ([`attach::attach`]).
pub fn attach_records(
    recs: &mut [Rec],
    names: &Interner,
    cities: &Interner,
    streets: &Streets,
    keys: &streets::Keys,
    admin: &Admin,
    attach_m: f64,
) {
    let found = attach::attach(
        recs,
        &names.list,
        &cities.list,
        streets,
        keys,
        admin,
        attach_m,
        LOOSE_FROM_M.min(attach_m),
    );
    for (r, hit) in recs.iter_mut().zip(found) {
        r.att = hit.and_then(|(h, k, how)| {
            let (comp, _) = streets.locate(&h, k)?;
            Some(Att {
                comp,
                way: h.way,
                how,
            })
        });
    }
}

/// The merge at the dedup distance.
pub struct Merge {
    /// The kept record of each cluster (each unique address).
    pub reps: Vec<u32>,
    /// Each record's cluster.
    assign: Vec<u32>,
}

/// Groups the records by (street component or unattached key, direction set, number,
/// suffix), sources in priority order, and clusters each group at `dedup_m` ([`cluster`]).
pub fn dedup(recs: &[Rec], keys: &streets::Keys, dedup_m: f64) -> Merge {
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
    cluster(recs, &order, &bounds, dedup_m)
}

/// Per merged address: its units as `(address, unit)` pairs (sorted, distinct), and its kept
/// record's postcode, else the first one another record of the cluster gives.
pub fn clusters(recs: &[Rec], merge: &Merge) -> (Vec<(u32, u32)>, Vec<u32>) {
    let mut units: Vec<(u32, u32)> = Vec::new();
    let mut codes: Vec<u32> = merge
        .reps
        .iter()
        .map(|&r| recs[r as usize].postcode)
        .collect();
    for (i, r) in recs.iter().enumerate() {
        let c = merge.assign[i];
        if codes[c as usize] == 0 {
            codes[c as usize] = r.postcode;
        }
        if r.unit != 0 {
            units.push((c, r.unit));
        }
    }
    units.sort_unstable();
    units.dedup();
    (units, codes)
}

/// An interpolation range with no merged point in it, on the OSM way nearest its middle.
pub struct Fallback {
    pub way: u32,
    pub start: P,
    pub end: P,
    pub mid: P,
    pub from: u32,
    pub to: u32,
    pub parity: u8,
    /// TIGER's side, relative to the segment's direction; `None` for OSM.
    pub right: Option<bool>,
}

/// The ranges with no merged address (`reps`) on their street, in them and within
/// [`TIGER_NEAR_M`] of their line, on the street nearest their middle within `attach_m`.
pub fn fallback_ranges(
    ranges: &[tiger::Range],
    keys: &mut streets::Keys,
    recs: &[Rec],
    reps: &[u32],
    streets: &Streets,
    attach_m: f64,
) -> Vec<Fallback> {
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
    let hits: Vec<bool> = ranges
        .par_iter()
        .zip(&range_keys)
        .map(|(t, &k)| {
            let lk = keys.loose[k as usize];
            let b = geom::BBox::of(t.line.iter().copied());
            let (lo, hi) = (cell(b.min), cell(b.max));
            for x in lo.0 - 1..=hi.0 + 1 {
                for y in lo.1 - 1..=hi.1 + 1 {
                    for &c in by_cell.get(&(lk, x, y)).into_iter().flatten() {
                        let rec = &recs[reps[c as usize] as usize];
                        if t.contains(rec.num) && dist_to_line_m(&t.line, rec.p) <= TIGER_NEAR_M {
                            return true;
                        }
                    }
                }
            }
            false
        })
        .collect();
    drop(by_cell);
    let keys = &*keys;
    (0..ranges.len())
        .into_par_iter()
        .filter(|&i| !hits[i])
        .filter_map(|i| {
            let t = &ranges[i];
            let k = range_keys[i];
            let lk = keys.loose[k as usize];
            let mid = midpoint(&t.line);
            let hit = match streets.nearest(k, false, mid) {
                Some(h) if h.dist_m <= attach_m => h,
                _ => match streets.nearest(lk, true, mid) {
                    Some(h) if h.dist_m <= attach_m => h,
                    _ => return None,
                },
            };
            Some(Fallback {
                way: hit.way,
                start: t.line[0],
                end: *t.line.last().unwrap(),
                mid,
                from: t.from,
                to: t.to,
                parity: t.parity,
                right: t.right,
            })
        })
        .collect()
}

/// Greedy clustering within each group: a record joins the first cluster whose kept record
/// is within `d` metres, else starts a new one.
fn cluster(recs: &[Rec], order: &[u32], bounds: &[(usize, usize)], d: f64) -> Merge {
    let mut reps: Vec<u32> = Vec::new();
    let mut assign = vec![NONE; recs.len()];
    // `(cluster, kept record)` of the group's clusters.
    let mut local: Vec<(u32, u32)> = Vec::new();
    for &(s, e) in bounds {
        local.clear();
        for &i in &order[s..e] {
            let r = &recs[i as usize];
            let limit = if r.src.approx() {
                d.max(APPROX_DEDUP_M)
            } else {
                d
            };
            let joined = local
                .iter()
                .find(|&&(_, rep)| dist_m(recs[rep as usize].p, r.p) <= limit);
            assign[i as usize] = match joined {
                Some(&(cid, _)) => cid,
                None => {
                    let cid = reps.len() as u32;
                    local.push((cid, i));
                    reps.push(i);
                    cid
                }
            };
        }
    }
    Merge { reps, assign }
}
