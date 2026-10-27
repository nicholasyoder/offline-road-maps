//! The readings of a query, and the hits of each in turn.

use std::io;

use super::candidates::MAX_LOCALITIES;
use super::{Hit, HitKind, QTok, Reading, Run, Scored};
use crate::normalize::{is_generic, search_tokens, tokens};
use crate::query::{Number, Parsed, Word};
use crate::search::Search;

/// Entry hits kept per reading at the deeper levels, whatever the limit: enough for a list
/// of results asked for once the suggestions' levels are done ([`Session::set_limit`]).
///
/// [`Session::set_limit`]: super::Session::set_limit
const DEEP_HITS: usize = 150;
/// A state, province or country name the last word, still being typed after a comma, may
/// begin, when it has 3 letters or more and begins at most this many ("…, Kentuck"; not
/// "…, New").
const MAX_STATE_PREFIXES: usize = 3;
/// Entries read to check whether a state's name is also a town's ("Québec").
const PROBE_IDS: usize = 300;
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
pub(super) const GENERIC_WEIGHT: f64 = 0.5;
const SOFT_WEIGHT: f64 = 0.15;

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

impl Run<'_> {
    /// The readings of `parsed`: a state at the end or not, a locality split, a number or
    /// not. In the `unfinished` pass, those of the words before the last comma: what is
    /// being typed after it may not say anything yet ("…, Buffalo, New ", "…, Dayton, O").
    fn readings(&self, parsed: &Parsed) -> Vec<Reading> {
        if !self.unfinished {
            return self.readings_of(parsed, &parsed.words, parsed.prefix);
        }
        let n = parsed.words.len();
        match parsed.words[..n.saturating_sub(1)]
            .iter()
            .rposition(|w| w.comma)
        {
            Some(i) if parsed.postcode.is_none() => {
                let mut words = parsed.words[..=i].to_vec();
                words[i].comma = false;
                self.readings_of(parsed, &words, false)
            }
            _ => Vec::new(),
        }
    }

    /// The readings of `words`, the last of which is still being typed if `prefix`.
    fn readings_of(&self, parsed: &Parsed, words: &[Word], prefix: bool) -> Vec<Reading> {
        let states = self.states(words, prefix && parsed.postcode.is_none());
        let mut out = Vec::new();
        let number_options: Vec<Option<&Number>> = std::iter::once(None)
            .chain(parsed.numbers.iter().map(Some))
            .collect();
        let all = words;
        for &(used, state) in &states {
            let words = &all[..all.len() - used];
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
                    let prefix = prefix && at_end;
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
                        state_words: all[all.len() - used..].to_vec(),
                    });
                }
            }
        }
        out
    }

    /// The ways the last words can name a state, province or country: `(words used,
    /// area)`, with `(0, None)` first. With `typing`, the last word may also begin the
    /// name of up to [`MAX_STATE_PREFIXES`] of them.
    fn states(&self, words: &[Word], typing: bool) -> Vec<(usize, Option<u32>)> {
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
            let last = t.last().map_or(0, |w| w.chars().count());
            if typing && last >= 3 && n > k && words[n - k - 1].comma {
                let mut begun: Vec<u32> = names[from..]
                    .iter()
                    .take_while(|(n, _)| n.starts_with(&joined))
                    .map(|&(_, a)| a)
                    .filter(|&a| !out.iter().any(|&(u, s)| u == k && s == Some(a)))
                    .collect();
                begun.sort_unstable();
                begun.dedup();
                if begun.len() <= MAX_STATE_PREFIXES {
                    out.extend(begun.into_iter().map(|a| (k, Some(a))));
                }
            }
        }
        out
    }

    /// The hits of every reading of `parsed`, in this pass.
    pub(super) fn all(&mut self, parsed: &Parsed) -> io::Result<Vec<Hit>> {
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
                if !self
                    .localities_within(&probe, PROBE_IDS, MAX_LOCALITIES)?
                    .is_empty()
                {
                    penalty = 6.0;
                }
            }
            self.check()?;
            let mut cands = self.candidates(&r, postcode.as_ref())?;
            for c in &mut cands {
                c.score -= penalty;
            }
            // Towns past the most important of that name: when they hold no address, or
            // at the deeper levels whatever they hold.
            let full = !r.loc.is_empty() && self.localities(&r)?.len() >= MAX_LOCALITIES;
            let wider = |run: &mut Self| -> io::Result<Vec<Scored>> {
                run.wide = true;
                let cands = run.candidates(&r, postcode.as_ref());
                run.wide = false;
                let mut cands = cands?;
                for c in &mut cands {
                    c.score -= penalty;
                }
                Ok(cands)
            };
            match &r.number {
                Some(n) => {
                    let unit = parsed.unit.as_deref();
                    let code = parsed.postcode.as_deref();
                    let mut found = self.addresses(n, unit, code, &cands)?;
                    let none = !found.iter().any(|h| h.kind == HitKind::Address);
                    if full && (none || self.b.always) {
                        let cands = wider(self)?;
                        let more = self.addresses(n, unit, code, &cands)?;
                        if self.b.always {
                            found.extend(more);
                        } else {
                            found = more;
                        }
                    }
                    hits.extend(found);
                }
                None => {
                    let n = if self.b.always {
                        DEEP_HITS.max(self.limit * 3)
                    } else {
                        self.limit.max(10) * 3
                    };
                    for c in cands.iter().take(n) {
                        hits.push(self.entry_hit(c)?);
                    }
                    if full && self.b.always {
                        for c in wider(self)?.iter().take(n) {
                            hits.push(self.entry_hit(c)?);
                        }
                    }
                }
            }
        }
        Ok(hits)
    }
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
