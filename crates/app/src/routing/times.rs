//! The schedule in the route page (phase 4 step 4.5): the *Depart at / Arrive by*
//! settings, the schedule worked out from them on the UI thread (it walks the legs once, a
//! few milliseconds even coast to coast), each step's clock in its own zone, the zone
//! crossings, and the town each night is near, which a thread of its own finds: it opens
//! the search index the first time it is asked, so a trip without times costs nothing.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use jiff::tz::TimeZone;
use jiff::{Timestamp, Zoned, civil};
use omp_core::geo::P;
use omp_core::pack::Pack;
use omp_core::route::Step;
pub use omp_core::schedule::parse_time;
use omp_core::schedule::{
    self, Breaks, Crossing, Day, Kind, Local, Schedule, Settings, When, difference,
};
use omp_core::schema::{self, Schema};
use omp_core::search::Index;

use super::{Place, Routed, duration_text};
use crate::config;

/// Whether the trip is timed, and from which end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Off,
    Depart,
    ArriveBy,
}

/// The settings as the route page edits them: text fields, checked when used.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub mode: Mode,
    /// `2026-11-01` and `08:00`, local to the start (or for arrive-by, the destination).
    pub date: String,
    pub time: String,
    pub s: config::Schedule,
}

impl Plan {
    pub fn new(s: config::Schedule) -> Self {
        Plan {
            mode: Mode::Off,
            date: String::new(),
            time: s.depart.clone(),
            s,
        }
    }

    /// Times the trip from `mode`; a date not yet set is the next day the time comes in
    /// `tz` (today, or tomorrow once it has passed).
    pub fn set_mode(&mut self, mode: Mode, tz: &TimeZone) {
        if mode != Mode::Off && self.date.trim().parse::<civil::Date>().is_err() {
            let now = Timestamp::now().to_zoned(tz.clone());
            let time = parse_time(&self.time).unwrap_or(civil::time(8, 0, 0, 0));
            let date = match now.time() < time {
                true => now.date(),
                false => now.date().tomorrow().unwrap_or(now.date()),
            };
            self.date = date.to_string();
        }
        self.mode = mode;
    }

    /// The stop whose zone the date and time are in.
    pub fn anchor(&self, stops: usize) -> usize {
        match self.mode {
            Mode::ArriveBy => stops.saturating_sub(1),
            _ => 0,
        }
    }

    /// The schedule's settings, or what is wrong with a field; `None` with no time set.
    pub fn settings(
        &self,
        places: &[&Place],
        stops: &[P],
        local: &Local,
    ) -> Option<Result<Settings, String>> {
        let at = self.anchor(stops.len());
        let when = match self.mode {
            Mode::Off => return None,
            m => (m, stops.get(at)?),
        };
        Some((|| {
            let date: civil::Date =
                self.date.trim().parse().map_err(|_| {
                    format!("“{}” is not a date like 2026-11-01.", self.date.trim())
                })?;
            let time = parse_time(&self.time)
                .ok_or_else(|| format!("“{}” is not a time like 08:00.", self.time.trim()))?;
            let t = local
                .tz(*when.1)
                .to_timestamp(date.to_datetime(time))
                .map_err(|e| e.to_string())?;
            let s = &self.s;
            let window = match s.window {
                false => None,
                true => Some((
                    parse_time(&s.day_start).ok_or("The driving day's start is not a time.")?,
                    parse_time(&s.day_end).ok_or("The driving day's end is not a time.")?,
                )),
            };
            let day = match (window, s.max_drive) {
                (None, false) => None,
                (w, max) => Some(Day {
                    start: w.map_or(Day::default().start, |w| w.0),
                    end: w.map(|w| w.1),
                    max_drive_s: max.then_some(s.max_drive_h * 3600.0),
                }),
            };
            Ok(Settings {
                when: match when.0 {
                    Mode::ArriveBy => When::ArriveBy(t),
                    _ => When::Depart(t),
                },
                stays: places.iter().map(|p| p.stay).collect(),
                day,
                breaks: s.breaks.then_some(Breaks {
                    every_s: s.break_every_h * 3600.0,
                    for_s: s.break_min * 60.0,
                }),
            })
        })())
    }
}

/// The schedule of the route shown, as the page draws it.
pub struct Timed {
    pub sched: Schedule,
    /// Each step's clock, in its own zone.
    pub steps: Vec<Zoned>,
    pub crossings: Vec<Crossing>,
    /// The departure's zone, and the computer's.
    pub dep: TimeZone,
    pub system: TimeZone,
}

/// A time as shown: "14:05 CDT", and if it is in another zone than the departure's, the
/// difference and a tip with the time in the departure's zone and the computer's.
#[derive(Clone, Debug)]
pub struct Shown {
    pub text: String,
    /// "1 h behind departure"; `None` in the departure's zone.
    pub against: Option<String>,
    pub hover: Option<String>,
}

/// "14:05 CDT", or with `day` "Sat 14:05 CDT".
pub fn clock(z: &Zoned, day: bool) -> String {
    match day {
        true => z.strftime("%a %H:%M %Z").to_string(),
        false => z.strftime("%H:%M %Z").to_string(),
    }
}

/// "1 h", "30 min", "1 h 30 min".
fn hours(s: i32) -> String {
    let m = s.unsigned_abs() / 60;
    match (m / 60, m % 60) {
        (h, 0) => format!("{h} h"),
        (0, m) => format!("{m} min"),
        (h, m) => format!("{h} h {m} min"),
    }
}

impl Timed {
    pub fn new(r: &Routed, stops: &[P], settings: &Settings, local: &Local) -> Option<Self> {
        let sched = schedule::schedule(&r.trip, stops, settings, local)?;
        let dep = sched.departure()?.time_zone().clone();
        let crossings = schedule::crossings(&r.trip, &r.steps, &sched, local);
        // Each step in the zone the crossings before it lead to, so the clocks and the
        // crossing rows agree.
        let mut tz = r.steps.first().map_or(dep.clone(), |s| local.tz(s.point));
        let mut next = crossings.iter().peekable();
        let steps = r
            .steps
            .iter()
            .enumerate()
            .map(|(i, s)| {
                while let Some(c) = next.next_if(|c| c.step < i) {
                    tz = c.tz.clone();
                }
                let arriving = matches!(
                    s.kind,
                    omp_core::route::Kind::Stop | omp_core::route::Kind::Arrive
                );
                sched.clock(s.at_s, arriving).to_zoned(tz.clone())
            })
            .collect();
        Some(Timed {
            sched,
            steps,
            crossings,
            dep,
            system: TimeZone::try_system().unwrap_or(TimeZone::UTC),
        })
    }

    pub fn show(&self, z: &Zoned, day: bool) -> Shown {
        let text = clock(z, day);
        let d = difference(z.timestamp(), z.time_zone(), &self.dep);
        if d == 0 {
            return Shown {
                text,
                against: None,
                hover: None,
            };
        }
        let side = if d < 0 { "behind" } else { "ahead of" };
        let against = Some(format!("{} {side} departure", hours(d)));
        let mut tips = vec![format!(
            "{} in the departure's zone",
            clock(&z.timestamp().to_zoned(self.dep.clone()), day)
        )];
        if difference(z.timestamp(), z.time_zone(), &self.system) != 0
            && difference(z.timestamp(), &self.dep, &self.system) != 0
        {
            tips.push(format!(
                "{} on this computer",
                clock(&z.timestamp().to_zoned(self.system.clone()), day)
            ));
        }
        Shown {
            text,
            against,
            hover: Some(tips.join("\n")),
        }
    }
}

/// What a note in the directions is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoteKind {
    /// A day's end: the event's index; along a leg, it can be made a stop.
    Night(usize),
    Break,
    Zone,
}

/// A row the schedule adds to the directions, after step `after`.
#[derive(Clone, Debug)]
pub struct Note {
    pub after: usize,
    pub kind: NoteKind,
    pub text: String,
    pub sub: Option<String>,
    pub time: Option<Zoned>,
    /// Where it is, for the map's marker.
    pub point: P,
    /// It is not at a stop: *Make overnight stop* applies.
    pub movable: bool,
}

/// "Enter Central Time: clocks go back 1 h".
pub fn crossing_text(abbr: &str, change_s: i32) -> String {
    let zone = schedule::zone_name(abbr).map_or_else(|| format!("{abbr} time"), str::to_owned);
    let clocks = match change_s {
        0 => "clocks stay the same".to_string(),
        c if c > 0 => format!("clocks go forward {}", hours(c)),
        c => format!("clocks go back {}", hours(c)),
    };
    format!("Enter {zone}: {clocks}")
}

/// The town a night is near, or the road it is on: "near Amarillo".
fn place_text(towns: &Towns, p: P, road: &str) -> String {
    match towns.get(p) {
        Some(Some(name)) => format!("near {name}"),
        _ if !road.is_empty() => format!("on {road}"),
        _ => String::new(),
    }
}

/// The schedule's rows in the directions: each night ("Day 2"), break and zone crossing,
/// after the step it falls in.
pub fn notes(steps: &[Step], t: &Timed, towns: &Towns) -> Vec<Note> {
    let mut out = Vec::new();
    for c in &t.crossings {
        out.push(Note {
            after: c.step,
            kind: NoteKind::Zone,
            text: crossing_text(&c.abbr, c.change_s),
            sub: None,
            time: Some(t.sched.clock(c.at_s, false).to_zoned(c.tz.clone())),
            point: c.point,
            movable: false,
        });
    }
    for (k, e) in t.sched.events.iter().enumerate() {
        // The last step of its leg not after it: at a stop, the stop itself.
        let after = steps
            .iter()
            .rposition(|s| s.leg == e.leg && s.at_s <= e.at_s + 1e-6)
            .unwrap_or(0);
        let road = steps.get(after).map(|s| s.road.text()).unwrap_or_default();
        let (kind, text, sub) = match e.kind {
            Kind::Night => {
                let at = match e.stop {
                    Some(i) => format!("at stop {}", i + 1),
                    None => place_text(towns, e.point, &road),
                };
                let ends = format!("Day {} ends {at} at {}", e.day - 1, clock(&e.from, false));
                (
                    NoteKind::Night(k),
                    format!("Day {} · {}", e.day, e.until.strftime("%A %b %-d")),
                    Some(format!("{ends}; on at {}", clock(&e.until, false))),
                )
            }
            Kind::Break => {
                let s = (e.until.timestamp().as_second() - e.from.timestamp().as_second()) as f64;
                let near = place_text(towns, e.point, &road);
                (
                    NoteKind::Break,
                    format!("Break, {}", duration_text(s)),
                    (!near.is_empty()).then_some(near),
                )
            }
        };
        out.push(Note {
            after,
            kind,
            text,
            sub,
            time: Some(e.from.clone()),
            point: e.point,
            movable: e.kind == Kind::Night && e.stop.is_none(),
        });
    }
    out.sort_by(|a, b| {
        let at = |n: &Note| n.time.as_ref().map(|z| z.timestamp());
        a.after.cmp(&b.after).then(at(a).cmp(&at(b)))
    });
    out
}

/// Points and the towns they are near, if any.
type Found = Vec<(P, Option<String>)>;

/// The towns nights are near, found on a thread of its own.
pub struct Towns {
    first_chunk: PathBuf,
    wake: Arc<dyn Fn() + Send + Sync>,
    tx: Option<Sender<Vec<P>>>,
    rx: Option<Receiver<Found>>,
    known: HashMap<P, Option<String>>,
    asked: HashSet<P>,
}

impl Towns {
    pub fn new(first_chunk: PathBuf, wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        Towns {
            first_chunk,
            wake,
            tx: None,
            rx: None,
            known: HashMap::new(),
            asked: HashSet::new(),
        }
    }

    /// The town at `p`: `None` if not known yet, `Some(None)` if there is none.
    pub fn get(&self, p: P) -> Option<&Option<String>> {
        self.known.get(&p)
    }

    /// Asks for the towns of `points` not known or asked yet; starts the thread the first
    /// time.
    pub fn ask(&mut self, points: impl IntoIterator<Item = P>) {
        let new: Vec<P> = points
            .into_iter()
            .filter(|p| !self.known.contains_key(p) && self.asked.insert(*p))
            .collect();
        if new.is_empty() {
            return;
        }
        if self.tx.is_none() {
            let (tx, jobs) = channel::<Vec<P>>();
            let (answers, rx) = channel();
            let (first, wake) = (self.first_chunk.clone(), self.wake.clone());
            let spawned = std::thread::Builder::new()
                .name("towns".into())
                .spawn(move || towns(&first, &jobs, &answers, &*wake));
            if spawned.is_err() {
                return;
            }
            (self.tx, self.rx) = (Some(tx), Some(rx));
        }
        if let Some(tx) = &self.tx {
            let _ = tx.send(new);
        }
    }

    /// Takes the answers come in; whether there were any.
    pub fn poll(&mut self) -> bool {
        let Some(rx) = &self.rx else { return false };
        let mut any = false;
        for batch in rx.try_iter() {
            for (p, name) in batch {
                self.asked.remove(&p);
                self.known.insert(p, name);
            }
            any = true;
        }
        any
    }
}

/// Opens the search index and the `places` classes once, then answers each batch.
fn towns(
    first: &std::path::Path,
    jobs: &Receiver<Vec<P>>,
    answers: &Sender<Found>,
    wake: &dyn Fn(),
) {
    let opened = Pack::open(first).map(Arc::new).and_then(|pack| {
        let places: Vec<String> = match pack.section(schema::ID) {
            Some(s) => Schema::decode(&pack.read_section(s)?)?
                .layer("places")
                .map(|(_, l)| l.classes.iter().map(|c| c.name.clone()).collect())
                .unwrap_or_default(),
            None => Vec::new(),
        };
        Ok((Index::open(pack)?, places))
    });
    for batch in jobs {
        let found = batch
            .into_iter()
            .map(|p| {
                let name = match &opened {
                    Ok((Some(index), places)) => index
                        .town_near(p, places)
                        .ok()
                        .flatten()
                        .map(|(e, _)| e.name),
                    _ => None,
                };
                (p, name)
            })
            .collect();
        if answers.send(found).is_err() {
            return;
        }
        wake();
    }
}
