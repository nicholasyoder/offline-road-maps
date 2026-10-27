//! The queries and answers, without the UI: which answer is the latest, the suggestions and
//! results it gives, and the recent searches.

use std::time::{Duration, Instant};

use omp_core::find::Hit;

use super::worker::{Answer, Query};
use crate::camera;

/// Suggestions shown while typing.
pub const SUGGESTIONS: usize = 8;
/// Results asked for by Enter, shown in the results panel.
pub const RESULTS: usize = 50;
/// Searches kept in `config.toml`.
pub const RECENT: usize = 8;

/// Puts `text` first in the recent searches, once.
pub fn remember(recent: &mut Vec<String>, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    recent.retain(|r| !r.eq_ignore_ascii_case(text));
    recent.insert(0, text.to_string());
    recent.truncate(RECENT);
}

/// The results panel: what Enter found.
pub struct Results {
    pub text: String,
    pub hits: Vec<Hit>,
    pub selected: Option<usize>,
}

/// An answer that was not stale.
#[derive(Clone, Debug)]
pub struct Taken {
    pub submit: bool,
    /// From the query being asked to the answer reaching the UI.
    pub latency: Duration,
    pub find: Duration,
    pub level: usize,
    pub more: bool,
    pub hits: usize,
    pub first: Option<Hit>,
    /// The names of the hits, as shown.
    pub names: Vec<String>,
}

/// The queries and answers, without the UI.
#[derive(Default)]
pub struct Session {
    next_id: u64,
    /// The latest query sent; answers to others are dropped.
    latest: Option<Query>,
    answered: bool,
    /// The suggestions and the text they are for.
    pub hits: Vec<Hit>,
    pub hits_for: String,
    /// The search level of the suggestions, and whether deeper ones are still running.
    pub level: usize,
    pub more: bool,
    pub error: Option<String>,
    pub results: Option<Results>,
    pub marker: Option<Hit>,
    /// The last answer taken, for the overlay: its text and timings.
    pub last: Option<(String, Taken)>,
    /// Answers dropped as stale.
    pub dropped: usize,
}

impl Session {
    /// A query for `text` from the view `near` at `zoom`; it becomes the latest.
    pub fn ask(&mut self, text: &str, submit: bool, near: camera::Vec2, zoom: f64) -> Query {
        self.next_id += 1;
        let q = Query {
            id: self.next_id,
            text: text.to_string(),
            near,
            zoom,
            limit: if submit { RESULTS } else { SUGGESTIONS },
            submit,
            asked: Instant::now(),
        };
        self.latest = Some(q.clone());
        self.answered = false;
        q
    }

    /// Forgets the latest query, so its answer is dropped.
    pub fn cancel(&mut self) {
        self.latest = None;
        self.hits.clear();
        self.hits_for.clear();
        self.more = false;
        self.error = None;
    }

    /// Whether the latest query is still out.
    pub fn waiting(&self) -> bool {
        self.latest.is_some() && !self.answered
    }

    /// Whether the latest query is still out, or deeper levels of it are running.
    pub fn searching(&self) -> bool {
        self.waiting() || self.latest.is_some() && self.more
    }

    /// Takes an answer, unless it is stale. An answer to Enter opens the results panel
    /// when there is more than one result, and its first result is given to fly to. A
    /// deeper level's suggestions keep the rows up to `pinned` (highlighted or under the
    /// pointer) where they are.
    pub fn take(&mut self, a: Answer, pinned: Option<usize>) -> Option<Taken> {
        if self.latest.as_ref().map(|q| q.id) != Some(a.query.id) {
            self.dropped += 1;
            return None;
        }
        let deeper = self.answered && !a.query.submit;
        self.answered = true;
        self.more = a.more;
        self.level = a.level;
        let (mut hits, error) = match a.hits {
            Ok(h) => (h, None),
            Err(e) => (Vec::new(), Some(e)),
        };
        if deeper {
            hits = merge(&self.hits, hits, pinned);
        }
        let taken = Taken {
            submit: a.query.submit,
            latency: a.query.asked.elapsed(),
            find: a.find,
            level: a.level,
            more: a.more,
            hits: hits.len(),
            first: hits.first().cloned(),
            names: hits.iter().map(|h| h.name.clone()).collect(),
        };
        self.error = error;
        if a.query.submit {
            self.results = (hits.len() > 1).then(|| Results {
                text: a.query.text.clone(),
                hits,
                selected: Some(0),
            });
            if let Some(h) = &taken.first {
                self.marker = Some(h.clone());
            }
        } else {
            self.hits = hits;
            self.hits_for = a.query.text.clone();
        }
        self.last = Some((a.query.text, taken.clone()));
        Some(taken)
    }
}

/// A deeper level's hits, with `old`'s rows up to `pinned` kept first where they were and
/// the new ones in rank order beneath them.
fn merge(old: &[Hit], new: Vec<Hit>, pinned: Option<usize>) -> Vec<Hit> {
    let Some(k) = pinned.filter(|&k| k < old.len()) else {
        return new;
    };
    let n = new.len().max(k + 1);
    let same = |a: &Hit, b: &Hit| a.kind == b.kind && a.name == b.name && a.p == b.p;
    let mut out = old[..=k].to_vec();
    out.extend(
        new.into_iter()
            .filter(|h| !old[..=k].iter().any(|o| same(o, h))),
    );
    out.truncate(n);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::testing::{answer, hit};
    use omp_core::find::HitKind;
    use omp_core::search::Kind;

    #[test]
    fn stale_answers_are_dropped() {
        let mut s = Session::default();
        let a = s.ask("Col", false, [0.3, 0.4], 8.0);
        let b = s.ask("Colu", false, [0.3, 0.4], 8.0);
        assert_eq!(b.limit, SUGGESTIONS);
        assert!(
            s.take(
                answer(&a, vec![hit(HitKind::Address, 40.0, -83.0, 10.0)]),
                None
            )
            .is_none()
        );
        assert_eq!((s.dropped, s.hits.len()), (1, 0));
        assert!(s.waiting());
        let t = s
            .take(
                answer(&b, vec![hit(HitKind::Address, 40.0, -83.0, 10.0)]),
                None,
            )
            .unwrap();
        assert!(!t.submit && t.hits == 1 && !s.waiting());
        assert_eq!(s.hits_for, "Colu");
        s.cancel();
        assert!(!s.waiting() && s.hits.is_empty());
    }

    #[test]
    fn deeper_levels_keep_the_rows_in_use() {
        let named = |name: &str, lat: f64| Hit {
            name: name.into(),
            ..hit(HitKind::Address, lat, -83.0, 10.0)
        };
        let mut s = Session::default();
        let q = s.ask("12 Main St, Spri", false, [0.3, 0.4], 8.0);
        let level = |hits: Vec<Hit>, level: usize, more: bool| Answer {
            level,
            more,
            ..answer(&q, hits)
        };
        let (a, b, c, d) = (
            named("a", 40.0),
            named("b", 41.0),
            named("c", 42.0),
            named("d", 43.0),
        );
        s.take(level(vec![a.clone(), b.clone()], 0, true), None);
        assert!(s.searching() && !s.waiting());
        // Nothing highlighted: the deeper list as it comes.
        s.take(level(vec![c.clone(), a.clone(), b.clone()], 1, true), None);
        assert_eq!(s.hits, [c.clone(), a.clone(), b.clone()]);
        // The second row highlighted: the first two stay, the rest follow in rank order.
        let t = s
            .take(
                level(vec![d.clone(), a.clone(), c.clone(), b.clone()], 2, false),
                Some(1),
            )
            .unwrap();
        assert_eq!(s.hits, [c, a, d, b]);
        assert!(t.level == 2 && !t.more && !s.searching());
        // A new query drops the rest of the old one's levels.
        let r = s.ask("12 Main St, Sprin", false, [0.3, 0.4], 8.0);
        assert!(s.take(level(Vec::new(), 3, false), None).is_none());
        assert!(s.waiting() && r.id == q.id + 1);
    }

    #[test]
    fn enter_opens_results_and_marks_the_first() {
        let mut s = Session::default();
        let q = s.ask("Springfield", true, [0.3, 0.4], 8.0);
        assert_eq!(q.limit, RESULTS);
        let hits = vec![
            hit(HitKind::Entry(Kind::Place), 39.8, -89.6, 8000.0),
            hit(HitKind::Entry(Kind::Place), 37.2, -93.3, 8000.0),
        ];
        let t = s.take(answer(&q, hits), None).unwrap();
        assert!(t.submit && t.first.is_some());
        assert_eq!(s.results.as_ref().unwrap().hits.len(), 2);
        assert!(s.marker.is_some() && s.hits.is_empty());
        // One result: no panel.
        let q = s.ask("45201", true, [0.3, 0.4], 8.0);
        s.take(
            answer(
                &q,
                vec![hit(HitKind::Entry(Kind::Postcode), 39.1, -84.5, 1000.0)],
            ),
            None,
        );
        assert!(s.results.is_none());
    }

    #[test]
    fn recent_searches_stay_few_and_distinct() {
        let mut r = Vec::new();
        for i in 0..12 {
            remember(&mut r, &format!("query {i}"));
        }
        remember(&mut r, "  QUERY 5 ");
        remember(&mut r, "");
        assert_eq!(r.len(), RECENT);
        assert_eq!(r[0], "QUERY 5");
        assert_eq!(
            r.iter()
                .filter(|q| q.eq_ignore_ascii_case("query 5"))
                .count(),
            1
        );
    }
}
