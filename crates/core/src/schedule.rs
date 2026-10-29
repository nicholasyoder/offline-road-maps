//! A trip's schedule (phase 4 step 4.5): from a departure or an arrive-by time, the time
//! spent at each stop, a daily driving window and breaks, each stop's arrival and departure,
//! where each day's driving ends and the breaks fall, and the clock time at any point of
//! the drive. Times are kept as instants; each is shown in the zone where it happens, which
//! [`Local`] finds from the pack's `TZON` and `jiff`'s bundled rules.
//!
//! Driving times are the profile's (phase 3 decision 9): no traffic or time of day. The
//! day's window is read in the zone where the driver is: its end in the zone of the point
//! where the day would end. Arrive-by is found by bisection on the departure, since the
//! arrival never comes earlier for a later departure.

use jiff::tz::TimeZone;
use jiff::{Timestamp, Zoned, civil};

use crate::geo::{self, P};
use crate::route::{Leg, Step, Trip};
use crate::tz::Zones;

/// Seconds below which a stretch of driving is nothing.
const EPS: f64 = 1e-6;
/// The furthest back arrive-by looks for a departure.
const MAX_DAYS: f64 = 366.0;
/// Driving seconds under which a zone entered and left again is a sliver between borders.
const BLIP_S: f64 = 90.0;
/// A trip given up on: more nights than this means a window too short to make progress.
const MAX_NIGHTS: u32 = 2_000;

/// When the trip is timed from.
#[derive(Clone, Debug, PartialEq)]
pub enum When {
    Depart(Timestamp),
    ArriveBy(Timestamp),
}

/// Each day's driving: it starts at `start` (local time), stops by `end` if set (the next
/// day's if it is not after `start`), and drives at most `max_drive_s` if set.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Day {
    pub start: civil::Time,
    pub end: Option<civil::Time>,
    pub max_drive_s: Option<f64>,
}

impl Default for Day {
    /// 8:00 to 18:00.
    fn default() -> Self {
        Day {
            start: civil::time(8, 0, 0, 0),
            end: Some(civil::time(18, 0, 0, 0)),
            max_drive_s: None,
        }
    }
}

/// A break of `for_s` after every `every_s` of driving.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Breaks {
    pub every_s: f64,
    pub for_s: f64,
}

/// What happens at a stop.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Stay {
    pub s: f64,
    /// It stays the night: driving goes on at the next day's start.
    pub overnight: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub when: When,
    /// Per stop; the start's and the destination's are not used.
    pub stays: Vec<Stay>,
    /// `None`: drive at any hour, with no day breaks but at overnight stops (which then
    /// go on at 8:00).
    pub day: Option<Day>,
    pub breaks: Option<Breaks>,
}

/// Finds the zone of a point: the pack's, else `fallback` (a pack built before `TZON`, or a
/// point outside every zone).
pub struct Local<'a> {
    pub zones: Option<&'a Zones>,
    pub fallback: TimeZone,
}

impl Local<'_> {
    pub fn tz(&self, p: P) -> TimeZone {
        self.zones
            .and_then(|z| z.tz_at(p))
            .map_or_else(|| self.fallback.clone(), |(_, tz)| tz.clone())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Break,
    /// The day's driving ends; the next day's starts at `until`.
    Night,
}

/// A break or a night.
#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub kind: Kind,
    /// The leg it is on, or, for a night at a stop, the leg that ends there.
    pub leg: usize,
    /// At this stop, rather than along the leg.
    pub stop: Option<usize>,
    /// Driving seconds from the trip's start, as [`crate::route::Step::at_s`].
    pub at_s: f64,
    pub point: P,
    pub from: Zoned,
    pub until: Zoned,
    /// The day that starts after it, from 1 for the departure's; the day it is on for a
    /// break.
    pub day: u32,
}

/// A stretch driven without stopping.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Span {
    from_s: f64,
    to_s: f64,
    /// Unix seconds when it starts.
    start: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Schedule {
    /// Per stop, in its zone: the arrival (none for the start) and the departure (none for
    /// the destination).
    pub arrive: Vec<Option<Zoned>>,
    pub depart: Vec<Option<Zoned>>,
    pub events: Vec<Event>,
    /// The time asked for was outside the driving window: the trip leaves at its start.
    pub waited: bool,
    spans: Vec<Span>,
}

fn ts(s: f64) -> Timestamp {
    Timestamp::from_second(s.round() as i64).unwrap_or(Timestamp::UNIX_EPOCH)
}

fn secs(t: Timestamp) -> f64 {
    t.as_second() as f64
}

fn zoned(s: f64, tz: &TimeZone) -> Zoned {
    ts(s).to_zoned(tz.clone())
}

/// The instant `time` on `date` in `tz` (the later one, past a DST gap).
fn instant(date: civil::Date, time: civil::Time, tz: &TimeZone) -> f64 {
    tz.to_timestamp(date.to_datetime(time))
        .map_or(f64::MAX, secs)
}

impl Day {
    /// The window holding `t` (Unix seconds) in `tz`, as `(start, end)`; with no end, the
    /// day from `t` on.
    fn window(&self, t: f64, tz: &TimeZone) -> Option<(f64, f64)> {
        let Some(end) = self.end.filter(|&e| e != self.start) else {
            return Some((t, f64::MAX));
        };
        let today = tz.to_datetime(ts(t)).date();
        let spill = end <= self.start;
        [today.yesterday().ok(), Some(today)]
            .into_iter()
            .flatten()
            .map(|d| {
                let e = match spill {
                    true => d.tomorrow().unwrap_or(d),
                    false => d,
                };
                (instant(d, self.start, tz), instant(e, end, tz))
            })
            .find(|&(s, e)| s <= t + EPS && t + EPS < e)
    }

    /// The first end of a window after `t` in `tz`; none with no end.
    fn next_end(&self, t: f64, tz: &TimeZone) -> f64 {
        let Some(end) = self.end.filter(|&e| e != self.start) else {
            return f64::MAX;
        };
        let today = tz.to_datetime(ts(t)).date();
        let mut d = today.yesterday().unwrap_or(today);
        for _ in 0..4 {
            let e = match end <= self.start {
                true => d.tomorrow().unwrap_or(d),
                false => d,
            };
            let at = instant(e, end, tz);
            if at > t + EPS {
                return at;
            }
            d = d.tomorrow().unwrap_or(d);
        }
        f64::MAX
    }

    /// The first start after `t` (or at it, if `at` is true).
    fn next_start(&self, t: f64, tz: &TimeZone, at: bool) -> f64 {
        let today = tz.to_datetime(ts(t)).date();
        let mut d = today.yesterday().unwrap_or(today);
        for _ in 0..4 {
            let s = instant(d, self.start, tz);
            if s > t + EPS || (at && s >= t - EPS) {
                return s;
            }
            d = d.tomorrow().unwrap_or(d);
        }
        t
    }
}

/// A time of day as typed: `08:00`, `8:00`, `8:30` or `8`.
pub fn parse_time(s: &str) -> Option<civil::Time> {
    let s = s.trim();
    let (h, m) = s.split_once(':').unwrap_or((s, "0"));
    let num = |t: &str| (!t.is_empty() && t.len() <= 2).then(|| t.parse::<i8>().ok())?;
    civil::Time::new(num(h)?, num(m)?, 0, 0).ok()
}

/// The point `s` seconds into `leg`, along its pieces' points.
pub fn point_at(leg: &Leg, s: f64) -> P {
    let mut acc = 0.0;
    for piece in &leg.pieces {
        if piece.time_s <= 0.0 || s > acc + piece.time_s {
            acc += piece.time_s.max(0.0);
            continue;
        }
        let pts = &leg.points[piece.points.clone()];
        let f = ((s - acc) / piece.time_s).clamp(0.0, 1.0);
        let lens: Vec<f64> = pts.windows(2).map(|w| geo::dist(w[0], w[1])).collect();
        let mut want = f * lens.iter().sum::<f64>();
        for (i, &l) in lens.iter().enumerate() {
            if want <= l && l > 0.0 {
                let k = want / l;
                let (a, b) = (pts[i], pts[i + 1]);
                let lerp = |x: u32, y: u32| {
                    (f64::from(x) + (f64::from(y) - f64::from(x)) * k).round() as u32
                };
                return [lerp(a[0], b[0]), lerp(a[1], b[1])];
            }
            want -= l;
        }
        if let Some(&p) = pts.last() {
            return p;
        }
    }
    leg.points.last().copied().unwrap_or([0, 0])
}

/// The schedule of `trip` through `stops` (their points), or `None` if a leg has no route.
pub fn schedule(trip: &Trip, stops: &[P], settings: &Settings, local: &Local) -> Option<Schedule> {
    let legs: Vec<&Leg> = trip
        .legs
        .iter()
        .map(Option::as_ref)
        .collect::<Option<_>>()?;
    if legs.is_empty() || stops.len() != legs.len() + 1 {
        return None;
    }
    match settings.when {
        When::Depart(t) => forward(&legs, stops, settings, local, secs(t)),
        When::ArriveBy(t) => {
            let target = secs(t);
            let arrival = |d: f64| -> Option<f64> {
                let s = forward(&legs, stops, settings, local, d)?;
                Some(secs(s.arrive.last()?.as_ref()?.timestamp()))
            };
            let drive: f64 = legs.iter().map(|l| l.time_s).sum::<f64>()
                + settings.stays.iter().map(|s| s.s).sum::<f64>();
            let mut hi = target - drive;
            if arrival(hi)? > target {
                // Back a day at a time until one arrives in time, then bisect.
                let mut lo = hi - 86_400.0;
                while arrival(lo)? > target {
                    lo -= 86_400.0;
                    if lo < target - MAX_DAYS * 86_400.0 {
                        return None;
                    }
                }
                while hi - lo > 30.0 {
                    let mid = (lo + hi) / 2.0;
                    match arrival(mid)? <= target {
                        true => lo = mid,
                        false => hi = mid,
                    }
                }
                hi = lo;
            }
            // On the minute, which still arrives in time.
            forward(&legs, stops, settings, local, (hi / 60.0).floor() * 60.0)
        }
    }
}

/// The schedule leaving at `depart` (Unix seconds).
fn forward(
    legs: &[&Leg],
    stops: &[P],
    st: &Settings,
    local: &Local,
    depart: f64,
) -> Option<Schedule> {
    let n = stops.len();
    let mut out = Schedule {
        arrive: vec![None; n],
        depart: vec![None; n],
        events: Vec::new(),
        waited: false,
        spans: Vec::new(),
    };
    let mut t = depart;
    if let Some(day) = &st.day {
        let tz = local.tz(stops[0]);
        if day.window(t, &tz).is_none() {
            t = day.next_start(t, &tz, true);
            out.waited = true;
        }
    }
    let (mut day_n, mut nights) = (1u32, 0u32);
    let (mut day_drive, mut since_break) = (0.0f64, 0.0f64);
    let mut base = 0.0;
    for (li, leg) in legs.iter().enumerate() {
        out.depart[li] = Some(zoned(t, &local.tz(stops[li])));
        let mut done = 0.0;
        while leg.time_s - done > EPS {
            let mut chunk = leg.time_s - done;
            let mut kind = None;
            if let Some(b) = &st.breaks {
                let to = (b.every_s - since_break).max(0.0);
                if to < chunk - EPS {
                    (chunk, kind) = (to, Some(Kind::Break));
                }
            }
            if let Some(day) = &st.day {
                if let Some(m) = day.max_drive_s {
                    let to = (m - day_drive).max(0.0);
                    if to < chunk - EPS {
                        (chunk, kind) = (to, Some(Kind::Night));
                    }
                }
                // The window's end where the driver is; if that end is in another zone,
                // the next end there instead, until the zone the day would end in is the
                // one its end came from. Zones that never agree: the earliest end.
                let here = local.tz(point_at(leg, done));
                let mut tz = here.clone();
                let (mut earliest, mut settled) = (f64::MAX, None);
                for _ in 0..3 {
                    let to = match tz == here {
                        true => day.window(t, &tz).map_or(0.0, |(_, e)| e - t),
                        false => day.next_end(t, &tz) - t,
                    }
                    .max(0.0);
                    earliest = earliest.min(to);
                    let there = local.tz(point_at(leg, done + to.min(chunk)));
                    if there == tz {
                        settled = Some(to);
                        break;
                    }
                    tz = there;
                }
                let to = settled.unwrap_or(earliest);
                if to < chunk - EPS {
                    (chunk, kind) = (to, Some(Kind::Night));
                }
            }
            if chunk > EPS {
                out.spans.push(Span {
                    from_s: base + done,
                    to_s: base + done + chunk,
                    start: t,
                });
            }
            t += chunk;
            done += chunk;
            day_drive += chunk;
            since_break += chunk;
            let Some(kind) = kind else { continue };
            let point = point_at(leg, done);
            let tz = local.tz(point);
            let until = match kind {
                Kind::Break => t + st.breaks.map_or(0.0, |b| b.for_s),
                Kind::Night => {
                    let day = st.day.unwrap_or_default();
                    nights += 1;
                    if nights > MAX_NIGHTS {
                        return None;
                    }
                    day_n += 1;
                    day_drive = 0.0;
                    day.next_start(t, &tz, false)
                }
            };
            since_break = 0.0;
            out.events.push(Event {
                kind,
                leg: li,
                stop: None,
                at_s: base + done,
                point,
                from: zoned(t, &tz),
                until: zoned(until, &tz),
                day: day_n,
            });
            t = until;
        }
        base += leg.time_s;
        let at = li + 1;
        let tz = local.tz(stops[at]);
        out.arrive[at] = Some(zoned(t, &tz));
        if at + 1 == n {
            break;
        }
        let stay = st.stays.get(at).copied().unwrap_or_default();
        let arrived = t;
        t += stay.s.max(0.0);
        if st.breaks.is_some_and(|b| stay.s >= b.for_s) {
            since_break = 0.0;
        }
        let day = st.day.unwrap_or_default();
        let wait = match (&st.day, stay.overnight) {
            (_, true) => Some(day.next_start(t, &tz, false)),
            (Some(d), false) if d.window(t, &tz).is_none() => Some(d.next_start(t, &tz, true)),
            _ => None,
        };
        if let Some(until) = wait {
            nights += 1;
            day_n += 1;
            (day_drive, since_break) = (0.0, 0.0);
            out.events.push(Event {
                kind: Kind::Night,
                leg: li,
                stop: Some(at),
                at_s: base,
                point: stops[at],
                from: zoned(arrived, &tz),
                until: zoned(until, &tz),
                day: day_n,
            });
            t = until;
        }
    }
    Some(out)
}

impl Schedule {
    /// The clock at `at_s` driving seconds from the trip's start: where several instants
    /// share it (a stop, a break, a night), the last one's start, or with `arriving` the
    /// first one's end.
    pub fn clock(&self, at_s: f64, arriving: bool) -> Timestamp {
        let i = match arriving {
            true => self.spans.partition_point(|s| s.to_s < at_s - EPS),
            false => self.spans.partition_point(|s| s.to_s <= at_s + EPS),
        };
        let s = self.spans.get(i).or(self.spans.last());
        s.map_or(Timestamp::UNIX_EPOCH, |s| {
            ts(s.start + (at_s - s.from_s).clamp(0.0, s.to_s - s.from_s))
        })
    }

    /// Days the trip drives on: 1 with no nights.
    pub fn days(&self) -> u32 {
        self.events
            .iter()
            .filter(|e| e.kind == Kind::Night)
            .map(|e| e.day)
            .max()
            .unwrap_or(1)
    }

    pub fn departure(&self) -> Option<&Zoned> {
        self.depart.first()?.as_ref()
    }

    pub fn arrival(&self) -> Option<&Zoned> {
        self.arrive.last()?.as_ref()
    }
}

/// Seconds `tz`'s clock is ahead of `other`'s at `t`: −3600 for CDT against EDT.
pub fn difference(t: Timestamp, tz: &TimeZone, other: &TimeZone) -> i32 {
    tz.to_offset(t).seconds() - other.to_offset(t).seconds()
}

/// Where a route enters a zone whose clock reads otherwise.
#[derive(Clone, Debug, PartialEq)]
pub struct Crossing {
    /// The step it falls in: the zone is entered before the next step.
    pub step: usize,
    /// Driving seconds from the trip's start, and where.
    pub at_s: f64,
    pub point: P,
    pub tz: TimeZone,
    /// Its abbreviation then: `CDT`.
    pub abbr: String,
    /// How far clocks go forward there, in seconds; back if negative.
    pub change_s: i32,
}

/// The zone crossings along the directions `steps` of `trip`: where the clock reads
/// otherwise, in offset or abbreviation, at that time. Each step's start is tested, and
/// where the next starts in another zone, the step's line point by point, so a long step
/// through several zones finds each. A zone left within [`BLIP_S`] of entering it is a
/// sliver between borders simplified apart, and never entered. Zones with the same rules (Indianapolis
/// and New York) cross unseen.
pub fn crossings(trip: &Trip, steps: &[Step], sched: &Schedule, local: &Local) -> Vec<Crossing> {
    let Some(first) = steps.first() else {
        return Vec::new();
    };
    let start = local.tz(first.point);
    // Every change of zone: the step it is in, where, and the zone entered.
    let mut changes: Vec<(usize, f64, P, TimeZone)> = Vec::new();
    let mut cur = start.clone();
    let mut change = |cur: &mut TimeZone, c: (usize, f64, P, TimeZone)| {
        // Left soon after it was entered: that zone was never entered, and back in the
        // zone before, nothing changed.
        if changes.last().is_some_and(|last| c.1 - last.1 < BLIP_S) {
            changes.pop();
        }
        let before = changes.last().map_or(&start, |l| &l.3);
        if c.3 != *before {
            changes.push(c.clone());
        }
        *cur = c.3;
    };
    for (i, next) in steps.iter().enumerate().skip(1) {
        let tz = local.tz(next.point);
        if tz == cur {
            continue;
        }
        for (at_s, p) in line(trip, &steps[i - 1]) {
            let here = local.tz(p);
            if here != cur {
                change(&mut cur, (i - 1, at_s, p, here));
            }
        }
        if tz != cur {
            change(&mut cur, (i - 1, next.at_s, next.point, tz));
        }
    }
    let mut out = Vec::new();
    let mut cur = start;
    for (step, at_s, point, tz) in changes {
        let t = sched.clock(at_s, false);
        let (was, now) = (reading(t, &cur), reading(t, &tz));
        if was != now {
            out.push(Crossing {
                step,
                at_s,
                point,
                tz: tz.clone(),
                abbr: now.1,
                change_s: now.0 - was.0,
            });
        }
        cur = tz;
    }
    out
}

/// Step `s`'s line: each point, with the driving seconds from the trip's start there.
fn line(trip: &Trip, s: &Step) -> Vec<(f64, P)> {
    let Some(pts) = trip
        .legs
        .get(s.leg)
        .and_then(Option::as_ref)
        .and_then(|l| l.points.get(s.points.clone()))
    else {
        return Vec::new();
    };
    let total: f64 = pts.windows(2).map(|w| geo::dist(w[0], w[1])).sum();
    let mut along = 0.0;
    pts.iter()
        .enumerate()
        .map(|(k, &p)| {
            if k > 0 {
                along += geo::dist(pts[k - 1], p);
            }
            let f = if total > 0.0 { along / total } else { 0.0 };
            (s.at_s + f * s.time_s, p)
        })
        .collect()
}

/// "Central Time" for the abbreviations of North America's zones; `None` for others.
pub fn zone_name(abbr: &str) -> Option<&'static str> {
    Some(match abbr {
        "EST" | "EDT" => "Eastern Time",
        "CST" | "CDT" => "Central Time",
        "MST" | "MDT" => "Mountain Time",
        "PST" | "PDT" => "Pacific Time",
        "AKST" | "AKDT" => "Alaska Time",
        "HST" | "HDT" => "Hawaii Time",
        "AST" | "ADT" => "Atlantic Time",
        "NST" | "NDT" => "Newfoundland Time",
        _ => return None,
    })
}

/// What a clock shows at `t` in `tz`: its UTC offset in seconds and its abbreviation.
pub fn reading(t: Timestamp, tz: &TimeZone) -> (i32, String) {
    let info = tz.to_offset_info(t);
    (info.offset().seconds(), info.abbreviation().to_string())
}

#[cfg(test)]
#[path = "schedule_tests.rs"]
mod tests;
