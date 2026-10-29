//! The route page's trip as a saved trip (phase 4 step 4.7, [`crate::trips`]): its stops,
//! options and schedule to a [`TripFile`] and back. A leg whose route was picked among its
//! alternatives keeps the point where it leaves the quickest (`Route::via`); opening the trip
//! looks for that leg's alternatives at once and picks the one passing that point again.

use omp_core::geo::{self, P};
use omp_core::route::{Ends, FERRY, TOLL};
use omp_core::schedule::Stay;

use super::times::Mode;
use super::{LegAlts, Place, RouteUi};
use crate::trips::{Named, Optimize, StopFile, StopKind, Times, TimesMode, TripFile, VERSION};

/// How near a leg's alternative must pass its saved via point to be the one picked again,
/// in metres.
const VIA_M: f64 = 100.0;

impl RouteUi {
    /// The trip as a file says it; `pack` names the pack it is made on.
    pub fn to_file(&self, pack: &str) -> TripFile {
        let places = self.places();
        let vias = self.vias(places.len());
        let mut stops = Vec::with_capacity(places.len() + vias.len());
        for (i, p) in places.iter().enumerate() {
            if let Some(v) = i
                .checked_sub(1)
                .and_then(|leg| vias.get(leg).copied().flatten())
            {
                let (lat, lon) = geo::unproject(v);
                stops.push(StopFile {
                    name: None,
                    lat,
                    lon,
                    street: None,
                    minutes: 0.0,
                    kind: StopKind::Via,
                });
            }
            let (lat, lon) = geo::unproject(p.p);
            stops.push(StopFile {
                name: p.name.clone(),
                lat,
                lon,
                street: p.street_name.clone(),
                minutes: p.stay.s / 60.0,
                kind: match p.stay.overnight {
                    true => StopKind::Overnight,
                    false => StopKind::Stop,
                },
            });
        }
        TripFile {
            version: VERSION,
            pack: pack.into(),
            avoid_tolls: self.avoid & TOLL != 0,
            avoid_ferries: self.avoid & FERRY != 0,
            optimize: Optimize {
                keep_first: self.ends.keep_first,
                keep_last: self.ends.keep_last,
                round_trip: self.ends.round_trip,
            },
            schedule: Times {
                mode: match self.plan.mode {
                    Mode::Off => TimesMode::Off,
                    Mode::Depart => TimesMode::Depart,
                    Mode::ArriveBy => TimesMode::ArriveBy,
                },
                date: self.plan.date.clone(),
                time: self.plan.time.clone(),
                settings: self.plan.s.clone(),
            },
            stops,
        }
    }

    /// Per leg of `n` stops, where its route leaves the quickest: the alternative picked,
    /// or a saved one not yet found again.
    fn vias(&self, n: usize) -> Vec<Option<P>> {
        let legs = n.saturating_sub(1);
        let points: Vec<P> = self.places().iter().map(|p| p.p).collect();
        let current = self.route.is_some() && self.routed_stops == points;
        (0..legs)
            .map(|leg| {
                let picked = match (current, self.alts.get(leg), self.chosen.get(leg)) {
                    (true, Some(LegAlts::Found(c)), Some(&k)) if k > 0 => {
                        c.routes.get(k).and_then(|r| r.via)
                    }
                    _ => None,
                };
                picked.or_else(|| self.wanted.get(leg).copied().flatten())
            })
            .collect()
    }

    /// Puts the trip of `file` in the page and asks for its route; `named` is its file.
    pub fn open_file(&mut self, file: &TripFile, named: Named) {
        let mut places = Vec::new();
        let mut wanted = Vec::new();
        let mut via = None;
        for s in &file.stops {
            let p = geo::project(s.lat, s.lon);
            if s.kind == StopKind::Via {
                via = (!places.is_empty()).then_some(p);
                continue;
            }
            if !places.is_empty() {
                wanted.push(via.take());
            }
            places.push(Place {
                p,
                street: None,
                street_name: s.street.clone(),
                name: s.name.clone(),
                stay: Stay {
                    s: s.minutes.max(0.0) * 60.0,
                    overnight: s.kind == StopKind::Overnight,
                },
            });
        }
        self.avoid = (if file.avoid_tolls { TOLL } else { 0 })
            | (if file.avoid_ferries { FERRY } else { 0 });
        let o = file.optimize;
        self.ends = Ends {
            keep_first: o.keep_first,
            keep_last: o.keep_last,
            round_trip: o.round_trip,
        };
        let t = &file.schedule;
        self.plan.mode = match t.mode {
            TimesMode::Off => Mode::Off,
            TimesMode::Depart => Mode::Depart,
            TimesMode::ArriveBy => Mode::ArriveBy,
        };
        self.plan.date = t.date.clone();
        self.plan.time = t.time.clone();
        self.plan.s = t.settings.clone();
        let n = places.len();
        self.put_stops(places, n >= 1, n >= 2);
        self.reroute();
        self.wanted = wanted;
        self.retime();
        self.opened = true;
        self.named = Some(named);
    }

    /// The trip has changes since it was saved or opened, or was never saved.
    pub fn unsaved(&self) -> bool {
        self.named
            .as_ref()
            .is_none_or(|n| n.file.compared() != self.to_file("").compared())
    }

    /// Leg `leg`'s alternatives came: picks the one passing its saved via point, if it
    /// waits for one. A leg with none passing it keeps the quickest, and says so.
    pub(super) fn pick_wanted(&mut self, leg: usize) {
        let Some(v) = self.wanted.get_mut(leg).and_then(Option::take) else {
            return;
        };
        let Some(LegAlts::Found(c)) = self.alts.get(leg) else {
            self.lost.push(leg);
            return;
        };
        let best = c
            .routes
            .iter()
            .enumerate()
            .map(|(k, r)| (k, passes(&r.leg.points, v)))
            .filter(|&(_, d)| d <= VIA_M)
            .min_by(|a, b| a.1.total_cmp(&b.1));
        match best {
            Some((k, _)) => self.choose(leg, k),
            None => self.lost.push(leg),
        }
    }
}

/// How near the line through `points` passes `v`, in metres.
fn passes(points: &[P], v: P) -> f64 {
    let q = geo::to_unit(v);
    let near = |a: P, b: P| {
        let (a, b) = (geo::to_unit(a), geo::to_unit(b));
        let d = [b[0] - a[0], b[1] - a[1]];
        let len2 = d[0] * d[0] + d[1] * d[1];
        let t = match len2 > 0.0 {
            true => (((q[0] - a[0]) * d[0] + (q[1] - a[1]) * d[1]) / len2).clamp(0.0, 1.0),
            false => 0.0,
        };
        [a[0] + d[0] * t, a[1] + d[1] * t]
    };
    let best = match points {
        [] => return f64::INFINITY,
        [a] => geo::to_unit(*a),
        _ => points
            .windows(2)
            .map(|w| near(w[0], w[1]))
            .min_by(|a, b| {
                let da = (a[0] - q[0]).hypot(a[1] - q[1]);
                let db = (b[0] - q[0]).hypot(b[1] - q[1]);
                da.total_cmp(&db)
            })
            .unwrap_or(q),
    };
    geo::distance_m(best, q)
}
