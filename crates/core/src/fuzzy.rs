//! Fuzzy token matching (phase 2 step 2.6): an `fst` automaton for the tokens within an edit
//! distance of a query token, counting an insertion, deletion, substitution or swap of
//! neighbours as one edit. Its state is a row of the edit-distance table, so nothing is
//! built ahead (`fst`'s own Levenshtein automaton builds a whole DFA first, which is slow at
//! distance 2).

use fst::{Automaton, IntoStreamer, Streamer};

/// Tokens within `max` edits of `query`.
pub struct Fuzzy<'a> {
    query: &'a [u8],
    max: u8,
}

/// The last two rows of the table and the byte that led to the last one.
#[derive(Clone)]
pub struct Row {
    prev: Vec<u8>,
    cur: Vec<u8>,
    byte: u8,
}

impl<'a> Fuzzy<'a> {
    pub fn new(query: &'a str, max: u8) -> Self {
        Self {
            query: query.as_bytes(),
            max,
        }
    }
}

impl Automaton for Fuzzy<'_> {
    type State = Option<Row>;

    fn start(&self) -> Option<Row> {
        Some(Row {
            prev: Vec::new(),
            cur: (0..=self.query.len()).map(|i| i.min(255) as u8).collect(),
            byte: 0,
        })
    }

    fn is_match(&self, s: &Option<Row>) -> bool {
        s.as_ref()
            .is_some_and(|r| r.cur[self.query.len()] <= self.max)
    }

    fn can_match(&self, s: &Option<Row>) -> bool {
        s.is_some()
    }

    fn accept(&self, s: &Option<Row>, b: u8) -> Option<Row> {
        let r = s.as_ref()?;
        let q = self.query;
        let mut next = Vec::with_capacity(r.cur.len());
        next.push(r.cur[0].saturating_add(1));
        for j in 1..=q.len() {
            let sub = r.cur[j - 1].saturating_add(u8::from(q[j - 1] != b));
            let mut v = sub
                .min(r.cur[j].saturating_add(1))
                .min(next[j - 1].saturating_add(1));
            if j > 1 && !r.prev.is_empty() && q[j - 1] == r.byte && q[j - 2] == b {
                v = v.min(r.prev[j - 2].saturating_add(1));
            }
            next.push(v);
        }
        if next.iter().all(|&v| v > self.max) {
            return None;
        }
        Some(Row {
            prev: r.cur.clone(),
            cur: next,
            byte: b,
        })
    }
}

/// The edits allowed for a token of this many bytes: none under 4, one from 4, two from 8
/// (the plan's thresholds).
pub fn allowed(len: usize) -> u8 {
    match len {
        0..4 => 0,
        4..8 => 1,
        _ => 2,
    }
}

/// The tokens of `map` within the allowed edits of `query`, other than `query` itself, with
/// their `fst` values and distances, at most `limit` of them (nearest first, then in
/// order).
pub fn near_tokens(map: &fst::Map<Vec<u8>>, query: &str, limit: usize) -> Vec<(String, u64, u8)> {
    let max = allowed(query.len());
    if max == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut stream = map.search(Fuzzy::new(query, max)).into_stream();
    while let Some((k, v)) = stream.next() {
        if k != query.as_bytes() {
            let t = String::from_utf8_lossy(k).into_owned();
            out.push((t, v, 0));
        }
    }
    for t in &mut out {
        t.2 = distance(query.as_bytes(), t.0.as_bytes());
    }
    out.sort_by_key(|t| t.2);
    out.truncate(limit);
    out
}

/// Edit distance with swaps of neighbours, as [`Fuzzy`] counts it (optimal string
/// alignment), by the usual table with three rows.
pub fn distance(a: &[u8], b: &[u8]) -> u8 {
    let n = a.len();
    let mut prev2: Vec<u8> = vec![0; n + 1];
    let mut prev: Vec<u8> = (0..=n).map(|i| i.min(255) as u8).collect();
    let mut cur: Vec<u8> = vec![0; n + 1];
    for (i, &c) in b.iter().enumerate() {
        cur[0] = (i + 1).min(255) as u8;
        for j in 1..=n {
            let mut v = prev[j - 1]
                .saturating_add(u8::from(a[j - 1] != c))
                .min(prev[j].saturating_add(1))
                .min(cur[j - 1].saturating_add(1));
            if i > 0 && j > 1 && a[j - 1] == b[i - 1] && a[j - 2] == c {
                v = v.min(prev2[j - 2].saturating_add(1));
            }
            cur[j] = v;
        }
        std::mem::swap(&mut prev2, &mut prev);
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[n]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_edits_and_swaps() {
        assert_eq!(distance(b"dixon", b"dixon"), 0);
        assert_eq!(distance(b"dixon", b"dixion"), 1);
        assert_eq!(distance(b"columbus", b"colubmus"), 1);
        assert_eq!(distance(b"columbus", b"colombus"), 1);
        assert_eq!(distance(b"columbus", b"clumbus"), 1);
        assert_eq!(distance(b"columbus", b"kolumbs"), 2);
        assert_eq!(distance(b"", b"abc"), 3);
    }

    #[test]
    fn finds_near_tokens_in_an_fst() {
        let words = [
            "cincinnati",
            "colombo",
            "columbia",
            "columbus",
            "dixon",
            "main",
        ];
        let map =
            fst::Map::from_iter(words.iter().enumerate().map(|(i, w)| (*w, i as u64))).unwrap();
        let names = |q: &str| -> Vec<String> {
            near_tokens(&map, q, 10).into_iter().map(|t| t.0).collect()
        };
        assert_eq!(names("colubmus"), ["columbus"]);
        assert_eq!(names("cincinatti"), ["cincinnati"]);
        assert_eq!(names("dixion"), ["dixon"]);
        assert_eq!(names("columbus"), ["columbia"], "two edits at 8 letters");
        assert!(names("man").is_empty(), "under 4 letters");
    }
}
