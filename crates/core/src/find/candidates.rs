//! Candidates: postings and their intersection, the localities a reading names, the
//! regions candidates are read from, and how much of each a search level reads.

use std::io;
use std::rc::Rc;

use fst::automaton::{Automaton, Str};
use fst::{IntoStreamer, Streamer};

use super::score::{match_name, place_radius_m};
use super::{Locality, QTok, Reading, Run};
use crate::fuzzy;
use crate::geo;
use crate::normalize::search_tokens;
use crate::query::Parsed;
use crate::search::{Entry, Kind};

/// A token in fewer entries than this is also matched within edits ("Clevland" is a street
/// somewhere, but more likely Cleveland).
const RARE: usize = 30;
/// Localities kept per reading, most important first; candidates are read from each with
/// [`Bounds::per_region`].
pub(super) const MAX_LOCALITIES: usize = 6;
/// How far beyond a town's area or place node its postal addresses are looked for.
const POSTAL_REACH_M: f64 = 10_000.0;
/// Postings longer than this are read only in part (most important first).
const MAX_POSTINGS: usize = 400_000;

/// How much of the data a search level reads (phase2-plan.md 2.7b). Level 0 answers each
/// keystroke; the deeper ones run while the user pauses, each a superset of the one before.
#[derive(Clone, Copy, Debug)]
pub(super) struct Bounds {
    /// Candidates read per region of a reading (a locality, the view, …).
    per_region: usize,
    /// Entries read to find the places a locality names: places and areas are important, so
    /// near the start of a postings list.
    locality_ids: usize,
    /// Localities kept for a reading whose first [`MAX_LOCALITIES`] gave no address: a
    /// small town that shares its name with larger ones is searched too, and the street
    /// there that has the number wins.
    wide_localities: usize,
    /// Candidates read from each locality past the first [`MAX_LOCALITIES`]: the less
    /// important towns, with few streets of any one name.
    per_small_locality: usize,
    /// Candidates tried for a house number.
    pub(super) locate: usize,
    /// Tokens a prefix expands to, and the IDs read from each.
    expansions: usize,
    expansion_ids: usize,
    /// Run the wider town pass, the pass within edits and the pass without the words being
    /// typed whatever the passes before found.
    pub(super) always: bool,
}

/// The search levels, from the keystroke's to the deepest.
pub(super) const LEVELS: [Bounds; 4] = [
    Bounds {
        per_region: 300,
        locality_ids: 800,
        wide_localities: 30,
        per_small_locality: 30,
        locate: 30,
        expansions: 64,
        expansion_ids: 3_000,
        always: false,
    },
    Bounds {
        per_region: 300,
        locality_ids: 800,
        wide_localities: 30,
        per_small_locality: 30,
        locate: 30,
        expansions: 64,
        expansion_ids: 3_000,
        always: true,
    },
    Bounds {
        per_region: 1_200,
        locality_ids: 3_200,
        wide_localities: 120,
        per_small_locality: 60,
        locate: 120,
        expansions: 256,
        expansion_ids: 12_000,
        always: true,
    },
    Bounds {
        per_region: 4_800,
        locality_ids: 12_800,
        wide_localities: 480,
        per_small_locality: 120,
        locate: 480,
        expansions: 1_024,
        expansion_ids: 48_000,
        always: true,
    },
];

/// Locality words, the state and the fuzzy pass.
pub(super) type LocalityKey = (String, Option<u32>, bool);

/// A token's postings and whether it was left out of them (a common generic token).
type Posted = (Rc<Vec<u32>>, bool);

/// Postings as read: the IDs, whether the token was left out of them, and whether they are
/// the whole list.
pub(super) struct Read {
    pub(super) ids: Rc<Vec<u32>>,
    stopped: bool,
    whole: bool,
}

/// Localities as found: the entries read and the localities kept.
pub(super) struct Towns {
    ids: usize,
    keep: usize,
    pub(super) list: Rc<Vec<Locality>>,
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

pub(super) fn half_diagonal_m(b: [u32; 4]) -> f64 {
    geo::dist([b[0], b[1]], [b[2], b[3]]) / 2.0 * geo::metres_per_unit([b[0], b[1]])
}

/// The admin area an entry stands for: an area's own, or the one a place node names.
fn own_area(e: &Entry) -> Option<u32> {
    match e.kind {
        Kind::Admin => Some(e.reference as u32),
        Kind::Place => (e.reference as u32).checked_sub(1),
        _ => None,
    }
}

impl Run<'_> {
    /// The postings of one token, read at most to `limit`, and whether it is a generic
    /// token left out of postings; `None` if no entry holds it.
    fn postings(&mut self, token: &str, limit: usize) -> io::Result<Option<Posted>> {
        let Some(offset) = self.s.index.tokens().get(token) else {
            return Ok(None);
        };
        // Postings are read most important first, so a shorter read is the start of a
        // longer one.
        if let Some(r) = self.postings.get(token)
            && (r.whole || r.ids.len() >= limit)
        {
            let ids = if r.ids.len() <= limit {
                r.ids.clone()
            } else {
                Rc::new(r.ids[..limit].to_vec())
            };
            return Ok(Some((ids, r.stopped)));
        }
        let p = self.s.index.postings_at(offset, limit)?;
        let r = Read {
            whole: p.ids.len() as u64 == p.stored,
            ids: Rc::new(p.ids),
            stopped: p.total > p.stored,
        };
        let v = (r.ids.clone(), r.stopped);
        self.postings.insert(token.to_owned(), r);
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
                if found.len() >= self.b.expansions {
                    break;
                }
            }
            for t in found {
                if let Some((ids, _)) = self.postings(&t, self.b.expansion_ids)? {
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
    pub(super) fn name_ids(&mut self, qs: &[QTok]) -> io::Result<Rc<Vec<u32>>> {
        let key: String = qs
            .iter()
            .map(|q| {
                format!(
                    "{}/{:?}/{}/{}/{}/{}|",
                    q.text, q.prefix, q.fuzzy, q.generic, self.fuzzy, self.b.expansions
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

    /// Whether any word of the query is a rare token.
    pub(super) fn any_rare(&mut self, parsed: &Parsed) -> io::Result<bool> {
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
    pub(super) fn is_rare(&mut self, token: &str) -> io::Result<bool> {
        if fuzzy::allowed(token.len()) == 0 || token.starts_with(|c: char| c.is_ascii_digit()) {
            return Ok(false);
        }
        Ok(self
            .postings(token, MAX_POSTINGS)?
            .is_none_or(|(ids, _)| ids.len() < RARE))
    }

    pub(super) fn chain(&self, e: &Entry) -> Vec<u32> {
        let mut chain = match e.admin {
            Some(a) if (a as usize) < self.s.admin.areas().len() => self.s.admin.parents(a),
            _ => Vec::new(),
        };
        if let Some(a) = own_area(e).filter(|&a| (a as usize) < self.s.admin.areas().len()) {
            chain.insert(0, a);
        }
        chain
    }

    /// The places, admin areas and postal codes the words `loc` name, most important first.
    pub(super) fn localities(&mut self, r: &Reading) -> io::Result<Rc<Vec<Locality>>> {
        let keep = if self.wide {
            self.b.wide_localities
        } else {
            MAX_LOCALITIES
        };
        self.localities_within(r, self.b.locality_ids, keep)
    }

    /// The `keep` most important of [`Self::localities`] among the first `max_ids` entries
    /// holding the words.
    pub(super) fn localities_within(
        &mut self,
        r: &Reading,
        max_ids: usize,
        keep: usize,
    ) -> io::Result<Rc<Vec<Locality>>> {
        let key = (r.loc_text.clone(), r.state, self.fuzzy);
        // Most important first: a read of more entries, keeping more, holds this one.
        if let Some(f) = self.localities.get(&key)
            && f.ids >= max_ids
            && f.keep >= keep
        {
            return Ok(if f.list.len() <= keep {
                f.list.clone()
            } else {
                Rc::new(f.list[..keep].to_vec())
            });
        }
        let ids = self.name_ids(&r.loc)?;
        let mut out: Vec<Locality> = Vec::new();
        for (i, &id) in ids.iter().take(max_ids).enumerate() {
            if i % 256 == 255 {
                self.check()?;
            }
            let e = self.entry(id)?;
            // Postings run by importance bucket: once enough are found, the rest are less
            // important than all of them.
            if out.len() >= keep && out.iter().all(|l| l.importance >> 4 > e.importance >> 4) {
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
        out.truncate(keep);
        let v = Rc::new(out);
        let covers = self
            .localities
            .get(&key)
            .is_none_or(|f| max_ids >= f.ids && keep >= f.keep);
        if covers {
            self.localities.insert(
                key,
                Towns {
                    ids: max_ids,
                    keep,
                    list: v.clone(),
                },
            );
        }
        Ok(v)
    }

    pub(super) fn postcode_locality(&mut self, code: &str) -> io::Result<Option<Locality>> {
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

    /// The candidate IDs of `ids` a reading reads, sorted: each region gives its most
    /// important (lowest IDs): each locality, else the postal code's surroundings, else the
    /// state and the view (near first) or the most important anywhere.
    pub(super) fn regions(
        &self,
        r: &Reading,
        postcode: Option<&Locality>,
        localities: &[Locality],
        ids: &[u32],
    ) -> io::Result<Vec<u32>> {
        let mut chosen: Vec<u32> = Vec::new();
        if postcode.is_none() && r.state.is_none() && localities.is_empty() {
            chosen.extend(ids.iter().copied().take(self.b.per_region));
        }
        let mut take = |run: &Self, b: [u32; 4], n: usize| -> io::Result<()> {
            let iv = run.s.index.intervals(&[b])?;
            chosen.extend(within(ids, &iv).into_iter().take(n));
            Ok(())
        };
        if !localities.is_empty() {
            for (i, l) in localities.iter().enumerate() {
                let n = if i < MAX_LOCALITIES {
                    self.b.per_region
                } else {
                    self.b.per_small_locality
                };
                take(self, l.bbox, n)?;
            }
            if let Some(st) = r.state.and_then(|s| self.s.admin.area(s)) {
                take(self, st.bbox, self.b.per_region)?;
            }
        } else {
            match (postcode, r.state.and_then(|s| self.s.admin.area(s))) {
                (Some(pc), _) => take(self, pc.bbox, 2 * self.b.per_region)?,
                (None, Some(st)) => take(self, st.bbox, 2 * self.b.per_region)?,
                (None, None) => {}
            }
            if let Some(c) = self.near {
                let scale = self.view_scale_m();
                for radius in [scale / 2.0, 4.0 * scale] {
                    take(self, geo::bbox_around(c, radius), self.b.per_region)?;
                }
            }
        }
        chosen.sort_unstable();
        chosen.dedup();
        Ok(chosen)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
