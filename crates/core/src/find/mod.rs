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
//!
//! One file per stage, each adding its methods to [`Run`]: `readings` (the readings and the
//! hits of each), `candidates` (postings, localities, regions and the bounds per level),
//! `score`, `numbers` (house numbers resolved to addresses) and `hits` (a candidate as
//! shown, and duplicates).

mod candidates;
mod hits;
mod numbers;
mod readings;
mod score;

use std::collections::HashMap;
use std::io;
use std::rc::Rc;

use crate::geo::P;
use crate::normalize::search_tokens;
use crate::query::{self, Number, Parsed, Word};
use crate::search::{Entry, Kind, Search};

use candidates::{Bounds, LEVELS, LocalityKey, Read, Towns};
use hits::same;
pub use numbers::number_text;

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
    /// The query is still being typed: its last word also matches as a prefix (unless a
    /// space follows it), and if nothing is found, the words after the last comma are left
    /// out.
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

/// A scored candidate, before it becomes a hit.
struct Scored {
    id: u32,
    entry: Rc<Entry>,
    chain: Vec<u32>,
    score: f64,
    exact: bool,
}

/// Points off a hit of the pass without the words being typed, when it runs beside hits
/// that use them: it leaves out what the user typed.
const UNFINISHED_POINTS: f64 = 20.0;

type Tokens = Vec<String>;

/// One query's state: the postings, entries and localities it has read. Its caches serve
/// every level of a [`Session`]; a larger bound reads on from what they hold.
struct Run<'a> {
    s: &'a Search,
    near: Option<P>,
    zoom: f64,
    limit: usize,
    b: Bounds,
    /// Asked between readings, towns and candidates whether to give up: a newer query.
    stop: Option<Box<dyn Fn() -> bool>>,
    fuzzy: bool,
    /// Rare tokens are matched within edits too (a second pass).
    rare: bool,
    /// The words after the last comma are left out (a last pass while typing).
    unfinished: bool,
    /// [`Bounds::wide_localities`] localities are read, not [`candidates::MAX_LOCALITIES`].
    wide: bool,
    postings: HashMap<String, Read>,
    entries: HashMap<u32, Rc<Entry>>,
    localities: HashMap<LocalityKey, Towns>,
    /// Per entry, its names' tokens.
    names: HashMap<u32, Rc<Vec<Vec<String>>>>,
    /// Candidate IDs per list of query tokens.
    name_ids: HashMap<String, Rc<Vec<u32>>>,
    /// Context tokens per town and deepest area.
    contexts: HashMap<(Option<u32>, Option<u32>), Rc<Tokens>>,
    towns: HashMap<u32, Rc<Vec<String>>>,
}

impl<'a> Run<'a> {
    /// An `Interrupted` error once the search is to stop.
    fn check(&self) -> io::Result<()> {
        match &self.stop {
            Some(f) if f() => Err(io::ErrorKind::Interrupted.into()),
            _ => Ok(()),
        }
    }

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
}

impl Search {
    /// The radius to fit a street in a view: half its length, at least 100 m.
    fn street_radius_m(&self, id: u32) -> io::Result<f64> {
        Ok((self.streets.street(id)?.length_m as f64 / 2.0).max(100.0))
    }

    /// Searches the pack: the best `req.limit` hits, best first. The keystroke's search,
    /// level 0 of a [`Session`].
    pub fn find(&self, req: &Request) -> io::Result<Vec<Hit>> {
        Ok(self.start(req).level(0)?.unwrap_or_default())
    }

    /// A search to run level by level, deeper while the user pauses.
    pub fn start(&self, req: &Request) -> Session<'_> {
        Session {
            parsed: query::parse(req.text, req.prefix),
            prefix: req.prefix,
            run: Run {
                s: self,
                near: req.near,
                zoom: req.zoom,
                limit: req.limit,
                b: LEVELS[0],
                stop: None,
                fuzzy: false,
                rare: false,
                unfinished: false,
                wide: false,
                postings: HashMap::new(),
                entries: HashMap::new(),
                localities: HashMap::new(),
                names: HashMap::new(),
                name_ids: HashMap::new(),
                contexts: HashMap::new(),
                towns: HashMap::new(),
            },
            hits: Vec::new(),
            radii: HashMap::new(),
            done: None,
        }
    }
}

/// One query searched level by level ([`Search::start`]): level 0 is [`Search::find`],
/// each deeper level reads more of the data (more towns of a name, more completions of a
/// word being typed, more candidates per region) and adds what it finds to the hits so far,
/// so the list only grows or improves. Levels share the postings, entries and localities
/// read. A session holds `Rc`s, so it stays on the thread that started it.
pub struct Session<'a> {
    parsed: Parsed,
    prefix: bool,
    run: Run<'a>,
    /// Every hit found so far, before duplicates are removed.
    hits: Vec<Hit>,
    radii: HashMap<u32, f64>,
    done: Option<usize>,
}

impl Session<'_> {
    /// How many levels there are.
    pub const LEVELS: usize = LEVELS.len();

    /// Gives up a level when `stop` returns true, asked often enough that a level ends
    /// within milliseconds of it.
    pub fn stop_when(&mut self, stop: impl Fn() -> bool + 'static) {
        self.run.stop = Some(Box::new(stop));
    }

    /// The best hits for limit `limit` from now on (the results panel asks for more than
    /// the suggestions). The deeper levels keep enough hits for a limit up to 50.
    pub fn set_limit(&mut self, limit: usize) {
        self.run.limit = limit;
    }

    /// The best hits of every level run so far.
    pub fn hits(&mut self) -> io::Result<Vec<Hit>> {
        self.best()
    }

    /// The deepest level run to the end.
    pub fn done(&self) -> Option<usize> {
        self.done
    }

    /// Runs level `d` (below [`Self::LEVELS`]) and gives the best hits of every level run
    /// so far, or `None` if it was stopped.
    pub fn level(&mut self, d: usize) -> io::Result<Option<Vec<Hit>>> {
        if self.parsed.words.is_empty() && self.parsed.lead.is_empty() {
            self.done = Some(d.max(self.done.unwrap_or(0)));
            return Ok(Some(Vec::new()));
        }
        self.run.b = LEVELS[d.min(LEVELS.len() - 1)];
        match self.passes() {
            Ok(hits) => self.hits.extend(hits),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => return Ok(None),
            Err(e) => return Err(e),
        }
        self.done = Some(d.max(self.done.unwrap_or(0)));
        self.best().map(Some)
    }

    /// The hits of one level: the exact pass, then within edits and without the words
    /// being typed when it calls for them.
    fn passes(&mut self) -> io::Result<Vec<Hit>> {
        let (run, parsed) = (&mut self.run, &self.parsed);
        let always = run.b.always;
        run.fuzzy = false;
        run.rare = false;
        run.unfinished = false;
        let mut hits = run.all(parsed)?;
        // A second pass within edits: of every token if nothing matched exactly, else of
        // the rare ones ("Clevland" is a street, but more likely Cleveland), unless an
        // address was found. The deeper levels run it within edits of every token.
        let exact = hits.iter().any(|h| h.exact && !h.number_missing);
        let address = hits.iter().any(|h| h.kind == HitKind::Address && h.exact);
        if always || !exact || !address && run.any_rare(parsed)? {
            run.fuzzy = always || !exact;
            run.rare = true;
            for mut h in run.all(parsed)? {
                h.exact = false;
                hits.push(h);
            }
            run.fuzzy = false;
            run.rare = false;
        }
        // Still typing and nothing found: the words after the last comma may be a town or
        // state not yet typed far enough to name it, so try without them. The deeper
        // levels try it whatever was found, below what uses those words.
        if (always || hits.is_empty()) && self.prefix {
            run.unfinished = true;
            let found = run.all(parsed);
            run.unfinished = false;
            for mut h in found? {
                h.exact = false;
                if always {
                    h.score -= UNFINISHED_POINTS;
                }
                hits.push(h);
            }
        }
        Ok(hits)
    }

    /// The best hits found so far, one of each thing.
    fn best(&mut self) -> io::Result<Vec<Hit>> {
        let s = self.run.s;
        let radii = &mut self.radii;
        self.hits.sort_by(|a, b| b.score.total_cmp(&a.score));
        let mut out: Vec<Hit> = Vec::new();
        let mut radius = |h: &Hit| -> io::Result<f64> {
            match h.street {
                Some(id) if h.radius_m.is_nan() => match radii.get(&id) {
                    Some(&r) => Ok(r),
                    None => {
                        let r = s.street_radius_m(id)?;
                        radii.insert(id, r);
                        Ok(r)
                    }
                },
                _ => Ok(h.radius_m),
            }
        };
        for h in &self.hits {
            let mut dup = false;
            for o in &out {
                if same(o, h, &mut radius)? {
                    dup = true;
                    break;
                }
            }
            if !dup {
                let mut h = h.clone();
                h.radius_m = radius(&h)?;
                out.push(h);
                if out.len() >= self.run.limit {
                    break;
                }
            }
        }
        Ok(out)
    }

    /// Bytes the session's caches hold, roughly: they grow with depth and live as long as
    /// the session.
    pub fn heap_bytes(&self) -> usize {
        let r = &self.run;
        let strings = |v: &[String]| v.iter().map(|t| 24 + t.len()).sum::<usize>();
        r.postings
            .iter()
            .map(|(k, v)| 64 + k.len() + 4 * v.ids.len())
            .sum::<usize>()
            + r.entries
                .values()
                .map(|e| 160 + e.name.len() + e.alts.iter().map(|a| 24 + a.len()).sum::<usize>())
                .sum::<usize>()
            + r.names
                .values()
                .map(|n| 32 + n.iter().map(|t| 24 + strings(t)).sum::<usize>())
                .sum::<usize>()
            + r.name_ids
                .iter()
                .map(|(k, v)| 48 + k.len() + 4 * v.len())
                .sum::<usize>()
            + r.localities
                .values()
                .map(|f| 64 + f.list.len() * std::mem::size_of::<Locality>())
                .sum::<usize>()
            + r.contexts.values().map(|t| 48 + strings(t)).sum::<usize>()
            + r.towns.values().map(|t| 48 + strings(t)).sum::<usize>()
            + self.hits.len() * (std::mem::size_of::<Hit>() + 64)
    }
}
