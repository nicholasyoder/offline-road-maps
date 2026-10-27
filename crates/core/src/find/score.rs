//! Scoring: how well a candidate's names match, whether its town, admin areas and state
//! match the locality, its importance, and its distance from the view.

use std::io;
use std::rc::Rc;

use super::{Locality, QTok, Reading, Run, Scored};
use crate::fuzzy;
use crate::geo::{self, P};
use crate::normalize::{is_direction, is_generic, search_tokens, tokens};
use crate::search::{Entry, Kind};

/// Matching qualities: an exact token, a prefix of the last word, one and two edits.
const PREFIX: f64 = 0.9;
const EDIT1: f64 = 0.85;
const EDIT2: f64 = 0.7;
/// Points off per word of a name the query leaves out (half for a generic word), at most.
const EXTRA_POINTS: f64 = 4.0;
const MAX_EXTRA_POINTS: f64 = 12.0;
/// Points for a name that is the whole query, nothing left out on either side.
const WHOLE_NAME: f64 = 6.0;

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
pub(super) fn match_name<N: AsRef<[String]>>(
    qs: &[QTok],
    names: &[N],
    fuzzy: bool,
) -> Option<(f64, f64)> {
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

fn bbox_inside(b: &[u32; 4], p: P) -> bool {
    b[0] <= p[0] && p[0] <= b[2] && b[1] <= p[1] && p[1] <= b[3]
}

/// The radius a place with no area stands for, by importance.
pub(super) fn place_radius_m(importance: u8) -> f64 {
    match importance {
        160.. => 15_000.0,
        130.. => 8_000.0,
        100.. => 4_000.0,
        _ => 2_000.0,
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

impl Run<'_> {
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

    /// Distance from the view centre to `p` as a nearness between 0 and 1.
    fn nearness(&self, p: P) -> f64 {
        let Some(c) = self.near else {
            return 0.0;
        };
        let scale = self.view_scale_m();
        let d = geo::dist(c, p) * geo::metres_per_unit(c);
        1.0 / (1.0 + d / scale)
    }

    /// How far "near the view" reaches: three tile widths at the view's zoom.
    pub(super) fn view_scale_m(&self) -> f64 {
        3.0 * geo::EARTH_CIRCUMFERENCE_M / 2f64.powf(self.zoom.clamp(0.0, 20.0))
    }

    /// The candidates of reading `r`, scored, best first.
    pub(super) fn candidates(
        &mut self,
        r: &Reading,
        postcode: Option<&Locality>,
    ) -> io::Result<Vec<Scored>> {
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
        let chosen = self.regions(r, postcode, &localities, &ids)?;

        let mut out = Vec::new();
        for (k, id) in chosen.into_iter().enumerate() {
            if k % 64 == 63 {
                self.check()?;
            }
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
}

#[cfg(test)]
mod tests {
    use super::super::readings::GENERIC_WEIGHT;
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
}
