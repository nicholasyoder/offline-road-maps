//! The search engine (phase 2 step 2.6): readings of a query, candidate entries from the
//! postings, filters from the town, state and postal code it names, house numbers resolved
//! to addresses, and ranking.
//!
//! A query is read several ways ([`query::parse`] takes out the house number, unit and postal
//! code): its last words may name a state or province, and the words before may split into
//! a name and a locality, at a comma if there is one. For each reading, the candidates are
//! the entries holding every telling token of the name (the intersection of their
//! postings), kept to the localities' areas, the state, the postal code's surroundings, or
//! else the most important ones and those around the view. Entry IDs follow
//! [`search::order_key`], so a region is a few ID intervals ([`search::Index::intervals`])
//! and a postings list is cut to it without reading entries. Each candidate is then scored:
//! how well its names match, whether its town, admin areas and state match the locality,
//! its importance, and its distance from the view, scaled by zoom.

use std::collections::HashMap;
use std::io;
use std::rc::Rc;

use fst::automaton::{Automaton, Str};
use fst::{IntoStreamer, Streamer};

use crate::addr::Loose;
use crate::fuzzy;
use crate::geo::{self, P};
use crate::normalize::{is_direction, is_generic, loose_key, search_tokens, street_key, tokens};
use crate::query::{self, Number, Parsed, Word};
use crate::search::{self, Entry, Found, Kind, Search};

/// A search.
#[derive(Clone, Debug)]
pub struct Request<'a> {
    pub text: &'a str,
    /// The centre of the map view; results near it rank higher.
    pub near: Option<P>,
    /// The view's zoom, which sets how far "near" reaches: the continent at z5, the
    /// neighbourhood at z14.
    pub zoom: f64,
    pub limit: usize,
    /// The last word is still being typed, so it also matches as a prefix.
    pub prefix: bool,
}

impl<'a> Request<'a> {
    pub fn new(text: &'a str) -> Self {
        Self {
            text,
            near: None,
            zoom: 5.0,
            limit: 10,
            prefix: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HitKind {
    /// A house number on a street, or an unattached address point.
    Address,
    Entry(Kind),
}

/// A search result.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub kind: HitKind,
    /// The entry's class: road class, place class, admin level, …
    pub class: u16,
    /// As shown: "123 Main Street", "Springfield".
    pub name: String,
    /// Where it is: "Springfield, Illinois".
    pub context: String,
    pub p: P,
    /// The radius to fit in the view when flying there.
    pub radius_m: f64,
    /// Interpolated from a range, or an NAR blockface point.
    pub approx: bool,
    /// A street found for an address query whose number it does not have.
    pub number_missing: bool,
    pub postcode: Option<String>,
    /// An address's units.
    pub units: Vec<String>,
    pub entry: Option<u32>,
    pub street: Option<u32>,
    pub score: f64,
    /// Every word of the query matched exactly.
    pub exact: bool,
}

/// A token of the query.
#[derive(Clone, Debug)]
struct QTok {
    text: String,
    /// The last word as typed, folded, when it may be a prefix ("s" rather than "south").
    prefix: Option<String>,
    /// It may be missing from a name: a street type, direction or article, or a word
    /// like "and", "city" or "Jr".
    generic: bool,
    /// What it weighs in the name match: 1, or less for a word that may be missing.
    weight: f64,
    /// Match it within edits: it is in no entry or in very few, so likely a typo.
    fuzzy: bool,
}

/// A town, place, admin area or postal code a reading's locality words name.
#[derive(Clone, Debug)]
struct Locality {
    area: Option<u32>,
    p: P,
    radius_m: f64,
    bbox: [u32; 4],
    importance: u8,
}

/// One way to read the query.
#[derive(Clone, Debug)]
struct Reading {
    number: Option<Number>,
    name: Vec<QTok>,
    loc: Vec<QTok>,
    /// The words of `loc`, joined, as a cache key.
    loc_text: String,
    state: Option<u32>,
    /// The words read as the state.
    state_words: Vec<Word>,
}

/// Matching qualities: an exact token, a prefix of the last word, one and two edits.
const PREFIX: f64 = 0.9;
const EDIT1: f64 = 0.85;
const EDIT2: f64 = 0.7;
/// A token in fewer entries than this is also matched within edits ("Clevland" is a street
/// somewhere, but more likely Cleveland).
const RARE: usize = 30;
/// Candidates read per region of a reading (a locality, the view, …).
const PER_REGION: usize = 300;
/// Entries read to find the places a locality names: places and areas are important, so
/// near the start of a postings list.
const MAX_LOCALITY_IDS: usize = 800;
/// Localities kept per reading, most important first.
const MAX_LOCALITIES: usize = 6;
/// Entries read to check whether a state's name is also a town's ("Québec").
const PROBE_IDS: usize = 300;
/// How far beyond a town's area or place node its postal addresses are looked for.
const POSTAL_REACH_M: f64 = 10_000.0;
/// Words of a town's name that may be left out ("City of Hamilton", "Ville de Laval").
const TOWN_WORDS: &[&str] = &[
    "city",
    "town",
    "township",
    "village",
    "of",
    "ville",
    "cite",
    "municipality",
    "municipalite",
    "county",
    "comte",
    "borough",
];
/// Words any name may leave out: "Quebec City", "Rock and Roll Boulevard" ("Rock & Roll"),
/// "Dr Martin Luther King Jr Drive".
const SOFT_WORDS: &[&str] = &["and", "et", "city", "dr", "doctor", "junior"];
/// The weights of a street type, direction or article, and of a soft word, against 1.
const GENERIC_WEIGHT: f64 = 0.5;
const SOFT_WEIGHT: f64 = 0.15;
/// Points off per word of a name the query leaves out (half for a generic word), at most.
const EXTRA_POINTS: f64 = 4.0;
const MAX_EXTRA_POINTS: f64 = 12.0;
/// Points for a name that is the whole query, nothing left out on either side.
const WHOLE_NAME: f64 = 6.0;
/// Candidates tried for a house number, and how far below the best they may score.
const MAX_LOCATE: usize = 30;
const LOCATE_WITHIN: f64 = 15.0;
/// Tokens a prefix expands to, and the IDs read from each.
const MAX_EXPANSIONS: usize = 64;
const EXPANSION_IDS: usize = 3_000;
/// Postings longer than this are read only in part (most important first).
const MAX_POSTINGS: usize = 400_000;

/// Traditional abbreviations OSM's `ISO3166-2` codes leave out, as `(word, code)`.
const STATE_WORDS: &[(&str, &str)] = &[
    ("ont", "ON"),
    ("que", "QC"),
    ("pq", "QC"),
    ("alta", "AB"),
    ("sask", "SK"),
    ("man", "MB"),
    ("nfld", "NL"),
    ("pei", "PE"),
    ("calif", "CA"),
    ("mass", "MA"),
    ("penn", "PA"),
    ("wash", "WA"),
];

fn titlecase(s: &str) -> String {
    if s.chars().any(char::is_lowercase) {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len());
    let mut start = true;
    for c in s.chars() {
        if start {
            out.extend(c.to_uppercase());
        } else {
            out.extend(c.to_lowercase());
        }
        start = !c.is_alphanumeric() && c != '\'';
    }
    out
}

/// An entry's name as shown: names from address sources only ("THORNBIRD CRES") in mixed
/// case.
fn shown_name(e: &Entry) -> String {
    match e.kind {
        Kind::AddressStreet => titlecase(&e.name),
        _ => e.name.clone(),
    }
}

/// A house number as shown: "123A", "123 1/2", "123-125", "12 bis".
pub fn number_text(number: u32, suffix: &str) -> String {
    match suffix {
        "" => number.to_string(),
        s if s.starts_with('-') => format!("{number}{s}"),
        s if s.contains('/') => format!("{number} {s}"),
        s if s.len() == 1 => format!("{number}{s}"),
        s => format!("{number} {}", s.to_lowercase()),
    }
}

/// How well query token `q` matches entry token `t`.
fn quality(q: &QTok, t: &str, fuzzy: bool) -> f64 {
    if q.text == t {
        return 1.0;
    }
    if let Some(p) = &q.prefix
        && t.starts_with(p.as_str())
    {
        // A longer completion is a little less likely.
        return PREFIX - 0.002 * (t.len() - p.len()).min(20) as f64;
    }
    if fuzzy || q.fuzzy {
        let max = fuzzy::allowed(q.text.len().min(t.len()));
        if max > 0 && q.text.len().abs_diff(t.len()) <= usize::from(max) {
            match fuzzy::distance(q.text.as_bytes(), t.as_bytes()) {
                1 => return EDIT1,
                2 if max >= 2 => return EDIT2,
                _ => {}
            }
        }
    }
    0.0
}

/// How well `qs` match one of the entry's names: their weighted quality (generic tokens
/// weigh half and may be missing), and the number of the name's own telling tokens the
/// query leaves out. `None` if a telling query token matches nothing.
fn match_name<N: AsRef<[String]>>(qs: &[QTok], names: &[N], fuzzy: bool) -> Option<(f64, f64)> {
    let all: Vec<&String> = names.iter().flat_map(|n| n.as_ref()).collect();
    // A direction the names contradict ("N High St" against "South High Street").
    let dirs = qs
        .iter()
        .filter(|q| q.prefix.is_none() && is_direction(&q.text));
    for q in dirs {
        let theirs = || all.iter().filter(|t| is_direction(t));
        if theirs().next().is_some() && !theirs().any(|t| **t == q.text) {
            return None;
        }
    }
    let (mut got, mut total) = (0.0, 0.0);
    for q in qs {
        let w = q.weight;
        let best = all.iter().map(|t| quality(q, t, fuzzy)).fold(0.0, f64::max);
        if best == 0.0 && !q.generic {
            return None;
        }
        got += w * best;
        total += w;
    }
    let extras = names
        .iter()
        .map(|name| {
            name.as_ref()
                .iter()
                .filter(|t| !qs.iter().any(|q| quality(q, t, fuzzy) > 0.0))
                .map(|t| if is_generic(t) { 0.5 } else { 1.0 })
                .sum::<f64>()
        })
        .fold(f64::INFINITY, f64::min);
    Some((if total > 0.0 { got / total } else { 1.0 }, extras))
}

/// Sorted, deduplicated union.
fn union(lists: Vec<Rc<Vec<u32>>>) -> Vec<u32> {
    let mut v: Vec<u32> = lists.iter().flat_map(|l| l.iter().copied()).collect();
    v.sort_unstable();
    v.dedup();
    v
}

fn intersect(a: &[u32], b: &[u32]) -> Vec<u32> {
    let (small, large) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    small
        .iter()
        .copied()
        .filter(|x| large.binary_search(x).is_ok())
        .collect()
}

/// The IDs of `ids` inside the half-open `intervals`.
fn within(ids: &[u32], intervals: &[(u32, u32)]) -> Vec<u32> {
    let mut out = Vec::new();
    for &(a, b) in intervals {
        let i = ids.partition_point(|&x| x < a);
        let j = ids.partition_point(|&x| x < b);
        out.extend_from_slice(&ids[i..j]);
    }
    out
}

fn bbox_inside(b: &[u32; 4], p: P) -> bool {
    b[0] <= p[0] && p[0] <= b[2] && b[1] <= p[1] && p[1] <= b[3]
}

/// A bbox grown by `m` metres on every side.
fn grow(b: [u32; 4], m: f64) -> [u32; 4] {
    let r = (m / geo::metres_per_unit([b[0], b[1]])) as u32;
    [
        b[0].saturating_sub(r),
        b[1].saturating_sub(r),
        b[2].saturating_add(r),
        b[3].saturating_add(r),
    ]
}

fn half_diagonal_m(b: [u32; 4]) -> f64 {
    geo::dist([b[0], b[1]], [b[2], b[3]]) / 2.0 * geo::metres_per_unit([b[0], b[1]])
}

/// The radius a place with no area stands for, by importance.
fn place_radius_m(importance: u8) -> f64 {
    match importance {
        160.. => 15_000.0,
        130.. => 8_000.0,
        100.. => 4_000.0,
        _ => 2_000.0,
    }
}

/// The admin area an entry stands for: an area's own, or the one a place node names.
fn own_area(e: &Entry) -> Option<u32> {
    match e.kind {
        Kind::Admin => Some(e.reference as u32),
        Kind::Place => (e.reference as u32).checked_sub(1),
        _ => None,
    }
}

/// Among names that match the query whole, places and areas first: "Mount Vernon" is the
/// town before Mount Vernon Road.
fn kind_bonus(k: Kind) -> f64 {
    match k {
        Kind::Place | Kind::Admin => 8.0,
        Kind::Poi | Kind::Feature | Kind::Postcode => 2.0,
        Kind::Street | Kind::AddressStreet => 0.0,
    }
}

/// A scored candidate, before it becomes a hit.
struct Scored {
    id: u32,
    entry: Rc<Entry>,
    chain: Vec<u32>,
    score: f64,
    exact: bool,
}

/// Locality words, the state, the fuzzy pass and the entries read.
type LocalityKey = (String, Option<u32>, bool, usize);
type Tokens = Vec<String>;

/// A token's postings and whether it was left out of them (a common generic token).
type Posted = (Rc<Vec<u32>>, bool);

/// One query's state: the postings, entries and localities it has read.
struct Run<'a> {
    s: &'a Search,
    req: &'a Request<'a>,
    fuzzy: bool,
    /// Rare tokens are matched within edits too (a second pass).
    rare: bool,
    postings: HashMap<(String, usize), Posted>,
    entries: HashMap<u32, Rc<Entry>>,
    localities: HashMap<LocalityKey, Rc<Vec<Locality>>>,
    /// Per entry, its names' tokens.
    names: HashMap<u32, Rc<Vec<Vec<String>>>>,
    /// Candidate IDs per list of query tokens.
    name_ids: HashMap<String, Rc<Vec<u32>>>,
    /// Context tokens per town and deepest area.
    contexts: HashMap<(Option<u32>, Option<u32>), Rc<Tokens>>,
    towns: HashMap<u32, Rc<Vec<String>>>,
}

impl<'a> Run<'a> {
    fn entry(&mut self, id: u32) -> io::Result<Rc<Entry>> {
        if let Some(e) = self.entries.get(&id) {
            return Ok(e.clone());
        }
        let e = Rc::new(self.s.index.entry(id)?);
        self.entries.insert(id, e.clone());
        Ok(e)
    }

    fn names(&mut self, id: u32, e: &Entry) -> Rc<Vec<Vec<String>>> {
        self.names
            .entry(id)
            .or_insert_with(|| {
                if e.kind == Kind::Postcode {
                    return Rc::new(vec![vec![crate::normalize::postcode_token(&e.name)]]);
                }
                Rc::new(
                    std::iter::once(&e.name)
                        .chain(&e.alts)
                        .map(|n| search_tokens(n))
                        .collect(),
                )
            })
            .clone()
    }

    /// The postings of one token, read at most to `limit`, and whether it is a generic
    /// token left out of postings; `None` if no entry holds it.
    fn postings(&mut self, token: &str, limit: usize) -> io::Result<Option<Posted>> {
        let Some(offset) = self.s.index.tokens().get(token) else {
            return Ok(None);
        };
        let key = (token.to_owned(), limit);
        if let Some(v) = self.postings.get(&key) {
            return Ok(Some(v.clone()));
        }
        let p = self.s.index.postings_at(offset, limit)?;
        let v = (Rc::new(p.ids), p.total > p.stored);
        self.postings.insert(key, v.clone());
        Ok(Some(v))
    }

    /// The entries that may match token `q`: its postings, its completions if it is being
    /// typed, and the tokens near it when fuzzy. `None` if it should not narrow the
    /// candidates: a generic token left out of postings.
    fn token_ids(&mut self, q: &QTok) -> io::Result<Option<Vec<u32>>> {
        let mut lists = Vec::new();
        let exact = self.postings(&q.text, MAX_POSTINGS)?;
        if let Some((ids, stopped)) = exact {
            if stopped && q.generic {
                return Ok(None);
            }
            lists.push(ids);
        }
        if let Some(p) = &q.prefix {
            let map = self.s.index.tokens();
            let mut stream = map.search(Str::new(p).starts_with()).into_stream();
            let mut found = Vec::new();
            while let Some((k, _)) = stream.next() {
                found.push(String::from_utf8_lossy(k).into_owned());
                if found.len() >= MAX_EXPANSIONS {
                    break;
                }
            }
            for t in found {
                if let Some((ids, _)) = self.postings(&t, EXPANSION_IDS)? {
                    lists.push(ids);
                }
            }
        }
        if self.fuzzy || q.fuzzy || lists.is_empty() {
            for (t, _, _) in fuzzy::near_tokens(self.s.index.tokens(), &q.text, 24) {
                if let Some((ids, _)) = self.postings(&t, MAX_POSTINGS / 4)? {
                    lists.push(ids);
                }
            }
        }
        Ok(Some(union(lists)))
    }

    /// The entries holding every telling token of `qs` (all of them if none is telling).
    fn name_ids(&mut self, qs: &[QTok]) -> io::Result<Rc<Vec<u32>>> {
        let key: String = qs
            .iter()
            .map(|q| {
                format!(
                    "{}/{:?}/{}/{}/{}|",
                    q.text, q.prefix, q.fuzzy, q.generic, self.fuzzy
                )
            })
            .collect();
        if let Some(v) = self.name_ids.get(&key) {
            return Ok(v.clone());
        }
        let v = Rc::new(self.intersect_tokens(qs)?);
        self.name_ids.insert(key, v.clone());
        Ok(v)
    }

    fn intersect_tokens(&mut self, qs: &[QTok]) -> io::Result<Vec<u32>> {
        let mut lists: Vec<Vec<u32>> = Vec::new();
        let telling = qs.iter().any(|q| !q.generic);
        for q in qs {
            if telling && q.generic {
                continue;
            }
            match self.token_ids(q)? {
                Some(ids) => lists.push(ids),
                None if !telling => {
                    // Every token is generic and left out of postings except where it is
                    // all a name has ("Avenue Road"): those entries are posted.
                    let ids = self.postings(&q.text, MAX_POSTINGS)?;
                    lists.push(ids.map(|(v, _)| (*v).clone()).unwrap_or_default());
                }
                None => {}
            }
        }
        lists.sort_by_key(Vec::len);
        let mut it = lists.into_iter();
        let Some(mut acc) = it.next() else {
            return Ok(Vec::new());
        };
        for l in it {
            acc = intersect(&acc, &l);
            if acc.is_empty() {
                break;
            }
        }
        Ok(acc)
    }

    fn chain(&self, e: &Entry) -> Vec<u32> {
        let mut chain = match e.admin {
            Some(a) if (a as usize) < self.s.admin.areas().len() => self.s.admin.parents(a),
            _ => Vec::new(),
        };
        if let Some(a) = own_area(e).filter(|&a| (a as usize) < self.s.admin.areas().len()) {
            chain.insert(0, a);
        }
        chain
    }

    /// The tokens of where an entry is: its town and the names of its areas below the state.
    fn context_tokens(&mut self, e: &Entry, chain: &[u32]) -> Rc<Vec<String>> {
        let key = (e.town, chain.first().copied());
        if let Some(v) = self.contexts.get(&key) {
            return v.clone();
        }
        let mut out = Vec::new();
        if let Some(t) = e.town.and_then(|t| self.s.index.town(t)) {
            out.extend(search_tokens(t));
        }
        for &a in chain {
            let area = &self.s.admin.areas()[a as usize];
            if area.level <= 4 {
                continue;
            }
            out.extend(search_tokens(&area.name));
            for (_, n) in &area.alt_names {
                out.extend(search_tokens(n));
            }
            if !area.abbr.is_empty() {
                out.extend(tokens(&area.abbr));
            }
        }
        out.sort_unstable();
        out.dedup();
        let v = Rc::new(out);
        self.contexts.insert(key, v.clone());
        v
    }

    fn town_tokens(&mut self, town: u32) -> Option<Rc<Vec<String>>> {
        if let Some(v) = self.towns.get(&town) {
            return Some(v.clone());
        }
        let v = Rc::new(search_tokens(self.s.index.town(town)?));
        self.towns.insert(town, v.clone());
        Some(v)
    }

    /// The places, admin areas and postal codes the words `loc` name, most important first.
    fn localities(&mut self, r: &Reading) -> io::Result<Rc<Vec<Locality>>> {
        self.localities_within(r, MAX_LOCALITY_IDS)
    }

    /// [`Self::localities`] among the first `max_ids` entries holding the words.
    fn localities_within(&mut self, r: &Reading, max_ids: usize) -> io::Result<Rc<Vec<Locality>>> {
        let key = (r.loc_text.clone(), r.state, self.fuzzy, max_ids);
        if let Some(v) = self.localities.get(&key) {
            return Ok(v.clone());
        }
        let ids = self.name_ids(&r.loc)?;
        let mut out: Vec<Locality> = Vec::new();
        for &id in ids.iter().take(max_ids) {
            let e = self.entry(id)?;
            // Postings run by importance bucket: once enough are found, the rest are less
            // important than all of them.
            if out.len() >= MAX_LOCALITIES
                && out.iter().all(|l| l.importance >> 4 > e.importance >> 4)
            {
                break;
            }
            if !matches!(e.kind, Kind::Place | Kind::Admin | Kind::Postcode) {
                continue;
            }
            let names = self.names(id, &e);
            let Some((q, extras)) = match_name(&r.loc, &names, self.fuzzy) else {
                continue;
            };
            if q < 0.5 || extras >= 1.0 {
                continue;
            }
            let chain = self.chain(&e);
            if r.state.is_some_and(|s| !chain.contains(&s)) {
                continue;
            }
            let area = own_area(&e).and_then(|a| self.s.admin.area(a).map(|x| (a, x)));
            // States and countries are read as such, not as towns.
            if area.is_some_and(|(_, x)| x.level <= 4) {
                continue;
            }
            // Candidates come from well beyond the town: a postal town often reaches far
            // past its limits or its place node. Their town names decide.
            let (area, bbox, radius_m) = match area {
                Some((a, x)) => (
                    Some(a),
                    grow(x.bbox, POSTAL_REACH_M),
                    half_diagonal_m(x.bbox),
                ),
                None => {
                    let r = if e.kind == Kind::Postcode {
                        (e.reference as f64).max(1_000.0) + 2_000.0
                    } else {
                        place_radius_m(e.importance)
                    };
                    (None, geo::bbox_around(e.p, r.max(POSTAL_REACH_M)), r)
                }
            };
            out.push(Locality {
                area,
                p: e.p,
                radius_m,
                bbox,
                importance: e.importance,
            });
        }
        out.sort_by_key(|l| std::cmp::Reverse(l.importance));
        out.truncate(MAX_LOCALITIES);
        let v = Rc::new(out);
        self.localities.insert(key, v.clone());
        Ok(v)
    }

    /// Distance from the view centre to `p` as a nearness between 0 and 1.
    fn nearness(&self, p: P) -> f64 {
        let Some(c) = self.req.near else {
            return 0.0;
        };
        let scale = self.view_scale_m();
        let d = geo::dist(c, p) * geo::metres_per_unit(c);
        1.0 / (1.0 + d / scale)
    }

    /// How far "near the view" reaches: three tile widths at the view's zoom.
    fn view_scale_m(&self) -> f64 {
        3.0 * 40_075_016.0 / 2f64.powf(self.req.zoom.clamp(0.0, 20.0))
    }

    /// The candidates of reading `r`, scored, best first.
    fn candidates(&mut self, r: &Reading, postcode: Option<&Locality>) -> io::Result<Vec<Scored>> {
        if r.name.is_empty() {
            return Ok(Vec::new());
        }
        let ids = self.name_ids(&r.name)?;
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let localities = if r.loc.is_empty() {
            Rc::new(Vec::new())
        } else {
            self.localities(r)?
        };
        // Each region gives its most important candidates (lowest IDs): each locality,
        // else the postal code's surroundings, else the state and the view (near first) or
        // the most important anywhere.
        let mut chosen: Vec<u32> = Vec::new();
        if postcode.is_none() && r.state.is_none() && localities.is_empty() {
            chosen.extend(ids.iter().copied().take(PER_REGION));
        }
        let mut take = |run: &Self, b: [u32; 4], n: usize| -> io::Result<()> {
            let iv = run.s.index.intervals(&[b])?;
            chosen.extend(within(&ids, &iv).into_iter().take(n));
            Ok(())
        };
        if !localities.is_empty() {
            for l in localities.iter() {
                take(self, l.bbox, PER_REGION)?;
            }
            if let Some(st) = r.state.and_then(|s| self.s.admin.area(s)) {
                take(self, st.bbox, PER_REGION)?;
            }
        } else {
            match (postcode, r.state.and_then(|s| self.s.admin.area(s))) {
                (Some(pc), _) => take(self, pc.bbox, 2 * PER_REGION)?,
                (None, Some(st)) => take(self, st.bbox, 2 * PER_REGION)?,
                (None, None) => {}
            }
            if let Some(c) = self.req.near {
                let scale = self.view_scale_m();
                for radius in [scale / 2.0, 4.0 * scale] {
                    take(self, geo::bbox_around(c, radius), PER_REGION)?;
                }
            }
        }
        chosen.sort_unstable();
        chosen.dedup();

        let mut out = Vec::new();
        for id in chosen {
            let e = self.entry(id)?;
            if r.number.is_some() && !matches!(e.kind, Kind::Street | Kind::AddressStreet) {
                continue;
            }
            let names = self.names(id, &e);
            let Some((name_q, extras)) = match_name(&r.name, &names, self.fuzzy) else {
                continue;
            };
            let chain = self.chain(&e);
            if r.state.is_some_and(|s| !chain.contains(&s)) {
                continue;
            }
            let mut loc_q = 1.0;
            if !r.loc.is_empty() {
                let town = e.town.and_then(|t| self.town_tokens(t));
                let by_town = town
                    .and_then(|t| match_name(&r.loc, &[t.as_slice()], self.fuzzy))
                    .map_or(0.0, |m| m.0);
                let ctx = self.context_tokens(&e, &chain);
                let ctx = [ctx.as_slice()];
                // The postal town is what people write; a township or county of that name
                // counts a little less.
                let named = match_name(&r.loc, &ctx, self.fuzzy)
                    .map_or(0.0, |m| m.0)
                    .min(0.9)
                    .max(by_town);
                let inside = localities.iter().any(|l| match l.area {
                    Some(a) => chain.contains(&a) || bbox_inside(&l.bbox, e.p) && named > 0.0,
                    None => geo::dist(l.p, e.p) * geo::metres_per_unit(l.p) <= l.radius_m,
                });
                if named >= 0.89 {
                    loc_q = named;
                } else if inside {
                    loc_q = 0.8;
                } else {
                    continue;
                }
            }
            let whole = r.loc.is_empty() && r.state.is_none() && extras == 0.0 && name_q >= 1.0;
            let mut score = 100.0 * name_q
                - 40.0 * (1.0 - loc_q)
                - (EXTRA_POINTS * extras).min(MAX_EXTRA_POINTS)
                + if whole { WHOLE_NAME } else { 0.0 }
                + 30.0 * f64::from(e.importance) / 255.0
                + 25.0 * self.nearness(e.p)
                + if extras == 0.0 {
                    kind_bonus(e.kind)
                } else {
                    0.0
                };
            // Of a long street's records, the one in the town first.
            let to_town = localities
                .iter()
                .map(|l| {
                    let d = geo::dist(l.p, e.p) * geo::metres_per_unit(l.p);
                    1.0 / (1.0 + d / l.radius_m.max(1_500.0))
                })
                .fold(0.0, f64::max);
            score += 10.0 * to_town;
            if let Some(pc) = postcode
                && geo::dist(pc.p, e.p) * geo::metres_per_unit(pc.p) > pc.radius_m
            {
                score -= 10.0;
            }
            out.push(Scored {
                id,
                entry: e,
                chain,
                score,
                exact: name_q >= 1.0 && loc_q >= 0.89,
            });
        }
        out.sort_by(|a, b| b.score.total_cmp(&a.score));
        Ok(out)
    }

    /// The readings of `parsed`: a state at the end or not, a locality split, a number or
    /// not.
    fn readings(&self, parsed: &Parsed) -> Vec<Reading> {
        let states = self.states(&parsed.words);
        let mut out = Vec::new();
        let number_options: Vec<Option<&Number>> = std::iter::once(None)
            .chain(parsed.numbers.iter().map(Some))
            .collect();
        for &(used, state) in &states {
            let words = &parsed.words[..parsed.words.len() - used];
            let at_end = used == 0 && parsed.postcode.is_none();
            let mut splits: Vec<usize> = vec![words.len()];
            let commas: Vec<usize> = (0..words.len().saturating_sub(1))
                .filter(|&i| words[i].comma)
                .map(|i| i + 1)
                .collect();
            if commas.is_empty() {
                splits.extend(1..words.len());
            } else {
                splits.extend(commas);
            }
            for &k in &splits {
                for &number in &number_options {
                    let mut name_words: Vec<Word> = Vec::new();
                    if number.is_none() {
                        name_words.extend(parsed.lead.iter().cloned());
                    }
                    name_words.extend(words[..k].iter().cloned());
                    let loc_words = &words[k..];
                    if name_words.is_empty() {
                        continue;
                    }
                    let prefix = parsed.prefix && at_end;
                    let name = qtoks(&name_words, prefix && loc_words.is_empty());
                    let mut loc = qtoks(loc_words, prefix);
                    for q in &mut loc {
                        if TOWN_WORDS.contains(&q.text.as_str()) {
                            q.generic = true;
                            q.weight = SOFT_WEIGHT;
                        }
                    }
                    out.push(Reading {
                        number: number.cloned(),
                        name,
                        loc,
                        loc_text: join(loc_words),
                        state,
                        state_words: parsed.words[parsed.words.len() - used..].to_vec(),
                    });
                }
            }
        }
        out
    }

    /// The ways the last words can name a state, province or country: `(words used,
    /// area)`, with `(0, None)` first.
    fn states(&self, words: &[Word]) -> Vec<(usize, Option<u32>)> {
        let mut out = vec![(0, None)];
        let n = words.len();
        let names = self.s.state_names();
        for k in 1..=3.min(n.saturating_sub(1)) {
            let t: Vec<String> = words[n - k..]
                .iter()
                .flat_map(|w| tokens(&w.text))
                .collect();
            if t.is_empty() {
                continue;
            }
            let mut joined = t.join(" ");
            if let Some((_, code)) = STATE_WORDS.iter().find(|(w, _)| k == 1 && *w == joined) {
                joined = code.to_ascii_lowercase();
            }
            let from = names.partition_point(|(n, _)| *n < joined);
            for (_, a) in names[from..].iter().take_while(|(n, _)| *n == joined) {
                if !out.iter().any(|&(u, s)| u == k && s == Some(*a)) {
                    out.push((k, Some(*a)));
                }
            }
        }
        out
    }

    fn context(&self, e: &Entry, chain: &[u32]) -> String {
        let areas = self.s.admin.areas();
        let mut parts: Vec<&str> = Vec::new();
        let town = e
            .town
            .and_then(|t| self.s.index.town(t))
            .filter(|t| !t.is_empty())
            .or_else(|| {
                chain
                    .iter()
                    .map(|&a| &areas[a as usize])
                    .find(|a| (7..=10).contains(&a.level) && a.name != e.name)
                    .map(|a| a.name.as_str())
            });
        if let Some(t) = town.filter(|t| *t != e.name) {
            parts.push(t);
        }
        let mut state = None;
        for &a in chain {
            let area = &areas[a as usize];
            if area.level == 4 && area.name != e.name {
                state = Some(area.name.as_str());
            }
        }
        match state {
            Some(s) => parts.push(s),
            None => {
                if let Some(c) = chain
                    .iter()
                    .map(|&a| &areas[a as usize])
                    .find(|a| a.level == 2 && a.name != e.name)
                {
                    parts.push(&c.name);
                }
            }
        }
        parts.join(", ")
    }

    fn radius_m(&self, e: &Entry, chain: &[u32]) -> io::Result<f64> {
        Ok(match e.kind {
            Kind::Admin | Kind::Place => {
                match chain
                    .first()
                    .filter(|_| e.kind == Kind::Admin || e.reference > 0)
                {
                    Some(&a) => half_diagonal_m(self.s.admin.areas()[a as usize].bbox),
                    None => place_radius_m(e.importance) / 2.0,
                }
            }
            Kind::Street => self.s.street_radius_m(e.reference as u32)?,
            Kind::Postcode | Kind::AddressStreet => (e.reference as f64).max(300.0),
            Kind::Poi => 150.0,
            Kind::Feature => 2_000.0,
        })
    }

    fn entry_hit(&self, c: &Scored) -> io::Result<Hit> {
        let e = &c.entry;
        Ok(Hit {
            kind: HitKind::Entry(e.kind),
            class: e.class,
            name: shown_name(e),
            context: self.context(e, &c.chain),
            p: e.p,
            // A street's needs its record: left to the hits shown (`Search::find`).
            radius_m: if e.kind == Kind::Street {
                f64::NAN
            } else {
                self.radius_m(e, &c.chain)?
            },
            approx: false,
            number_missing: false,
            postcode: None,
            units: Vec::new(),
            entry: Some(c.id),
            street: (e.kind == Kind::Street).then_some(e.reference as u32),
            score: c.score,
            exact: c.exact,
        })
    }

    /// Unattached points numbered `number` within `radius_m` of `p` whose street name has
    /// the key of `name`: for a street known only from addresses (kind 6), or one whose
    /// addresses spell it differently enough not to attach ("ch. Arnold").
    fn loose_points(&self, name: &str, p: P, radius_m: f64, number: u32) -> io::Result<Vec<Loose>> {
        let Some(addresses) = &self.s.addresses else {
            return Ok(Vec::new());
        };
        let key = loose_key(&street_key(name));
        let mut out = Vec::new();
        for ((x, y), c) in self.s.grid.around(p, radius_m.min(5_000.0))? {
            if !c.loose {
                continue;
            }
            for l in addresses.loose_in(search::loose_cell(x, y))? {
                if l.number == number && loose_key(&street_key(&l.street)) == key {
                    out.push(l);
                }
            }
        }
        Ok(out)
    }

    /// The address hits of a reading with a house number, from its street candidates.
    fn addresses(
        &mut self,
        number: &Number,
        unit: Option<&str>,
        postcode: Option<&str>,
        cands: &[Scored],
    ) -> io::Result<Vec<Hit>> {
        let n = &number.number;
        let unit = number.unit.as_deref().or(unit);
        let unit_tok = unit.map(|u| tokens(u).join(" "));
        let mut out = Vec::new();
        // The streets that match about as well as the best: the number decides among them.
        let best = cands.first().map_or(0.0, |c| c.score);
        for c in cands
            .iter()
            .take(MAX_LOCATE)
            .take_while(|c| c.score >= best - LOCATE_WITHIN)
        {
            let e = &c.entry;
            let mut base = self.entry_hit(c)?;
            base.name = format!("{} {}", number_text(n.number, &n.suffix), base.name);
            base.kind = HitKind::Address;
            base.radius_m = 50.0;
            base.entry = Some(c.id);
            let loose = |run: &Self, radius_m: f64| -> io::Result<_> {
                let mut pts = run.loose_points(&e.name, e.p, radius_m, n.number)?;
                pts.sort_by_key(|l| (l.suffix != n.suffix, l.approx, !l.osm));
                Ok(pts
                    .into_iter()
                    .next()
                    .map(|l| (l.p, l.approx, l.suffix, l.postcode, l.units, false)))
            };
            let found = match e.kind {
                Kind::Street => {
                    let mut located = self.s.locate(e.reference as u32, n.number, &n.suffix)?;
                    // Of several points with the number, an exact one, then OSM's.
                    located.sort_by_key(|l| match &l.found {
                        Found::Point(a) => (a.suffix != n.suffix, l.approx, !a.osm),
                        Found::Range(_) => (true, true, true),
                    });
                    match located.into_iter().next() {
                        Some(l) => Some(match l.found {
                            Found::Point(a) => {
                                (l.p, l.approx, a.suffix, a.postcode, a.units, false)
                            }
                            Found::Range(_) => (l.p, true, String::new(), None, Vec::new(), true),
                        }),
                        None => loose(self, self.radius_m(e, &c.chain)? + 300.0)?,
                    }
                }
                Kind::AddressStreet => loose(self, e.reference as f64 * 2.0 + 300.0)?,
                _ => None,
            };
            let mut hit = base;
            match found {
                Some((p, approx, suffix, code, units, range)) => {
                    hit.p = p;
                    hit.approx = approx;
                    hit.name = format!("{} {}", number_text(n.number, &suffix), shown_name(e));
                    hit.score += if range { 25.0 } else { 30.0 };
                    if suffix != n.suffix {
                        hit.score -= 3.0;
                        hit.exact = false;
                    }
                    if let (Some(code), Some(a)) = (code, &self.s.addresses) {
                        hit.postcode = Some(a.postcode(code)?);
                    }
                    if let Some(q) = postcode {
                        if hit.postcode.as_deref() == Some(q) {
                            hit.score += 5.0;
                        } else if hit.postcode.is_some() {
                            hit.score -= 5.0;
                        }
                    }
                    if let Some(u) = &unit_tok
                        && units.iter().any(|x| tokens(x).ends_with(&tokens(u)))
                    {
                        hit.score += 3.0;
                    }
                    hit.units = units.iter().map(|u| titlecase(u)).collect();
                }
                None => {
                    hit = self.entry_hit(c)?;
                    hit.number_missing = true;
                    hit.exact = false;
                    hit.score -= 10.0;
                }
            }
            out.push(hit);
        }
        Ok(out)
    }

    fn all(&mut self, parsed: &Parsed) -> io::Result<Vec<Hit>> {
        let postcode = match &parsed.postcode {
            Some(code) => self.postcode_locality(code)?,
            None => None,
        };
        let mut hits = Vec::new();
        let readings = self.readings(parsed);
        // Readings with a house number first; once one finds the address, the number is
        // not read as part of a name ("100 Mile House" has no number 100).
        let (with, without): (Vec<Reading>, Vec<Reading>) =
            readings.into_iter().partition(|r| r.number.is_some());
        let n_with = with.len();
        for (i, mut r) in with.into_iter().chain(without).enumerate() {
            if i == n_with
                && hits
                    .iter()
                    .any(|h: &Hit| h.kind == HitKind::Address && h.exact)
            {
                break;
            }
            if self.rare {
                for q in r.name.iter_mut().chain(r.loc.iter_mut()) {
                    q.fuzzy = self.is_rare(&q.text)?;
                }
            }
            // Words that name both a state and a town ("Québec") are read as the town
            // first: it says more.
            let mut penalty = 0.0;
            let code = r.state_words.len() == 1 && r.state_words[0].text.chars().count() <= 3;
            if r.state.is_some() && !code {
                let probe = Reading {
                    number: None,
                    name: Vec::new(),
                    loc: qtoks(&r.state_words, false),
                    loc_text: join(&r.state_words),
                    state: None,
                    state_words: Vec::new(),
                };
                if !self.localities_within(&probe, PROBE_IDS)?.is_empty() {
                    penalty = 6.0;
                }
            }
            let mut cands = self.candidates(&r, postcode.as_ref())?;
            for c in &mut cands {
                c.score -= penalty;
            }
            match &r.number {
                Some(n) => {
                    let found = self.addresses(
                        n,
                        parsed.unit.as_deref(),
                        parsed.postcode.as_deref(),
                        &cands,
                    )?;
                    hits.extend(found);
                }
                None => {
                    for c in cands.iter().take(self.req.limit.max(10) * 3) {
                        hits.push(self.entry_hit(c)?);
                    }
                }
            }
        }
        Ok(hits)
    }

    /// Whether any word of the query is a rare token.
    fn any_rare(&mut self, parsed: &Parsed) -> io::Result<bool> {
        for w in parsed.words.iter().chain(&parsed.lead) {
            for t in search_tokens(&w.text) {
                if self.is_rare(&t)? {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    /// Whether a token is worth matching within edits too: long enough, and in no entry or
    /// in fewer than [`RARE`].
    fn is_rare(&mut self, token: &str) -> io::Result<bool> {
        if fuzzy::allowed(token.len()) == 0 || token.starts_with(|c: char| c.is_ascii_digit()) {
            return Ok(false);
        }
        Ok(self
            .postings(token, MAX_POSTINGS)?
            .is_none_or(|(ids, _)| ids.len() < RARE))
    }

    fn postcode_locality(&mut self, code: &str) -> io::Result<Option<Locality>> {
        let token = crate::normalize::postcode_token(code);
        let Some((ids, _)) = self.postings(&token, 16)? else {
            return Ok(None);
        };
        for &id in ids.iter() {
            let e = self.entry(id)?;
            if e.kind == Kind::Postcode {
                let r = (e.reference as f64).max(1_000.0) + 2_000.0;
                return Ok(Some(Locality {
                    area: None,
                    p: e.p,
                    radius_m: r,
                    bbox: geo::bbox_around(e.p, r),
                    importance: e.importance,
                }));
            }
        }
        Ok(None)
    }
}

fn join(words: &[Word]) -> String {
    words
        .iter()
        .map(|w| w.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The query tokens of some words; the last may be a prefix.
fn qtoks(words: &[Word], prefix: bool) -> Vec<QTok> {
    let text = join(words);
    let mut out: Vec<QTok> = search_tokens(&text)
        .into_iter()
        .map(|t| {
            let soft = SOFT_WORDS.contains(&t.as_str());
            QTok {
                generic: is_generic(&t) || soft,
                weight: if soft {
                    SOFT_WEIGHT
                } else if is_generic(&t) {
                    GENERIC_WEIGHT
                } else {
                    1.0
                },
                text: t,
                prefix: None,
                fuzzy: false,
            }
        })
        .collect();
    if prefix
        && let Some(raw) = words.last().and_then(|w| tokens(&w.text).pop())
        && let Some(last) = out.last_mut()
    {
        last.prefix = Some(raw);
        // Being typed, it may become anything: a type word is not yet known to be one.
        last.generic = false;
        last.weight = 1.0;
    }
    out
}

/// Whether two hits show the same thing: same kind, name and context, close together.
/// A street's records (carriageways, pieces split at towns and at 20 km) are one, whatever
/// their towns, when they meet or nearly; `radius` gives a street hit's half length.
fn same(a: &Hit, b: &Hit, radius: &mut impl FnMut(&Hit) -> io::Result<f64>) -> io::Result<bool> {
    if a.kind != b.kind || !a.name.eq_ignore_ascii_case(&b.name) {
        return Ok(false);
    }
    let (near_m, context) = match a.kind {
        HitKind::Address => (100.0, true),
        HitKind::Entry(Kind::Street) => (radius(a)? + radius(b)? + 1_000.0, a.number_missing),
        _ => (3_000.0, true),
    };
    Ok((!context || a.context == b.context)
        && geo::dist(a.p, b.p) * geo::metres_per_unit(a.p) <= near_m)
}

impl Search {
    /// The folded names, alternative names and codes of the areas of level 4 and below
    /// (states, provinces, countries), sorted, with their area.
    fn state_names(&self) -> &[(String, u32)] {
        self.states.get_or_init(|| {
            let mut v = Vec::new();
            for (i, a) in self.admin.areas().iter().enumerate() {
                if a.level > 4 {
                    // Areas are ordered by level.
                    break;
                }
                let names = std::iter::once(a.name.as_str())
                    .chain(a.alt_names.iter().map(|(_, n)| n.as_str()))
                    .chain((!a.abbr.is_empty()).then_some(a.abbr.as_str()));
                for n in names {
                    let t = tokens(n).join(" ");
                    if !t.is_empty() {
                        v.push((t, i as u32));
                    }
                }
            }
            v.sort_unstable();
            v.dedup();
            v
        })
    }

    /// The radius to fit a street in a view: half its length, at least 100 m.
    fn street_radius_m(&self, id: u32) -> io::Result<f64> {
        Ok((self.streets.street(id)?.length_m as f64 / 2.0).max(100.0))
    }

    /// Searches the pack: the best `req.limit` hits, best first.
    pub fn find(&self, req: &Request) -> io::Result<Vec<Hit>> {
        let parsed = query::parse(req.text, req.prefix);
        if parsed.words.is_empty() && parsed.lead.is_empty() {
            return Ok(Vec::new());
        }
        let mut run = Run {
            s: self,
            req,
            fuzzy: false,
            rare: false,
            postings: HashMap::new(),
            entries: HashMap::new(),
            localities: HashMap::new(),
            names: HashMap::new(),
            name_ids: HashMap::new(),
            contexts: HashMap::new(),
            towns: HashMap::new(),
        };
        let mut hits = run.all(&parsed)?;
        // A second pass within edits: of every token if nothing matched exactly, else of
        // the rare ones ("Clevland" is a street, but more likely Cleveland), unless an
        // address was found.
        let exact = hits.iter().any(|h| h.exact && !h.number_missing);
        let address = hits.iter().any(|h| h.kind == HitKind::Address && h.exact);
        if !exact || !address && run.any_rare(&parsed)? {
            run.fuzzy = !exact;
            run.rare = true;
            for mut h in run.all(&parsed)? {
                h.exact = false;
                hits.push(h);
            }
        }
        hits.sort_by(|a, b| b.score.total_cmp(&a.score));
        let mut out: Vec<Hit> = Vec::new();
        let mut radii: HashMap<u32, f64> = HashMap::new();
        let mut radius = |h: &Hit| -> io::Result<f64> {
            match h.street {
                Some(id) if h.radius_m.is_nan() => match radii.get(&id) {
                    Some(&r) => Ok(r),
                    None => {
                        let r = self.street_radius_m(id)?;
                        radii.insert(id, r);
                        Ok(r)
                    }
                },
                _ => Ok(h.radius_m),
            }
        };
        for mut h in hits {
            let mut dup = false;
            for o in &out {
                if same(o, &h, &mut radius)? {
                    dup = true;
                    break;
                }
            }
            if !dup {
                h.radius_m = radius(&h)?;
                out.push(h);
                if out.len() >= req.limit {
                    break;
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(t: &str) -> QTok {
        QTok {
            text: t.into(),
            prefix: None,
            generic: is_generic(t),
            weight: if is_generic(t) { GENERIC_WEIGHT } else { 1.0 },
            fuzzy: false,
        }
    }

    #[test]
    fn matches_names() {
        let names =
            |s: &[&str]| -> Vec<Vec<String>> { s.iter().map(|n| search_tokens(n)).collect() };
        let main = names(&["Main Street"]);
        assert_eq!(
            match_name(&[q("main"), q("street")], &main, false),
            Some((1.0, 0.0))
        );
        // A missing type word costs, a missing telling word rules out.
        let (got, _) = match_name(&[q("main"), q("avenue")], &main, false).unwrap();
        assert!((got - 1.0 / 1.5).abs() < 1e-9);
        assert_eq!(match_name(&[q("elm")], &main, false), None);
        // Extras: the entry's telling words the query leaves out.
        let (_, extras) = match_name(&[q("main")], &names(&["Old Main Street"]), false).unwrap();
        assert!((extras - 1.5).abs() < 1e-9);
        // Fuzzy only when asked.
        assert_eq!(match_name(&[q("mian")], &main, false), None);
        assert!(match_name(&[q("mian")], &main, true).is_some());
        let mut p = q("spr");
        p.prefix = Some("spr".into());
        let (got, _) = match_name(&[p], &names(&["Springfield"]), false).unwrap();
        assert!(got > 0.85 && got < PREFIX);
    }

    #[test]
    fn shows_numbers_and_names() {
        assert_eq!(number_text(123, ""), "123");
        assert_eq!(number_text(123, "A"), "123A");
        assert_eq!(number_text(123, "1/2"), "123 1/2");
        assert_eq!(number_text(123, "-125"), "123-125");
        assert_eq!(number_text(12, "BIS"), "12 bis");
        assert_eq!(titlecase("THORNBIRD CRES"), "Thornbird Cres");
        assert_eq!(titlecase("O'NEIL RD"), "O'neil Rd");
        assert_eq!(titlecase("McDonald Road"), "McDonald Road");
    }

    #[test]
    fn sets_and_intervals() {
        assert_eq!(intersect(&[1, 3, 5, 7], &[3, 4, 5]), [3, 5]);
        assert_eq!(within(&[1, 3, 5, 7, 9], &[(2, 6), (9, 10)]), [3, 5, 9]);
        assert_eq!(
            union(vec![Rc::new(vec![1, 4]), Rc::new(vec![2, 4])]),
            [1, 2, 4]
        );
    }
}
