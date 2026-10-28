//! Directions (phase 3 step 3.5): a trip's pieces turned into steps a driver can follow,
//! modelled on OSRM's guidance (`src/extractor/guidance`, `src/engine/guidance`): a step
//! where the road changes or the route leaves the obvious way on, typed by the angle
//! between the roads' geometry and by their classes (turns, forks, ramps, exits, merges,
//! roundabouts, ferries), with turns a few metres apart collapsed into one.
//!
//! The graph has no node adjacency of its own, so the roads at a junction are found in
//! `RGRD`: every segment that ends at a node passes through the node's cell. Only
//! junctions where the road changes or the route bends are looked at this way.

use std::collections::HashMap;
use std::io;
use std::ops::Range;

use super::{FERRY_CLASS, Leg, Routing, Snap, Stop, Trip, cell_xy};
use crate::geo::{self, P};
use crate::street::Streets;

/// How far along a road its bearing at a junction is measured.
const BEARING_M: f64 = 15.0;
/// Two turns closer than this are one maneuver (a dual carriageway's gap, a jog).
const COLLAPSE_M: f64 = 30.0;
/// A name that lasts less than this between two straight steps is not worth a step.
const FLAP_M: f64 = 100.0;
/// A stop nearer its road than this is on it, on neither side.
const SIDE_M: f64 = 3.0;
/// A piece this short at either end of a leg is a stop at a node: it gets no step.
const END_M: f64 = 2.0;
/// How far ahead a route number's direction ("I-71 North") is judged.
const HEADING_M: f64 = 30_000.0;
/// Route numbers shown with a road; the rest are left out.
const SHOWN_REFS: usize = 2;
/// OSRM's turn function (`car.lua`), which ranks alternatives (phase 4 step 4.2): a turn
/// costs up to this many seconds by its angle,
const TURN_PENALTY_S: f64 = 7.5;
/// less to the right than to the left, as on roads driven on the right,
const TURN_BIAS: f64 = 1.075;
/// and a U-turn this much more.
const UTURN_PENALTY_S: f64 = 20.0;

/// Where a road goes from the way it was driven, by its angle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Modifier {
    Straight,
    SlightRight,
    Right,
    SharpRight,
    UTurn,
    SharpLeft,
    Left,
    SlightLeft,
}

impl Modifier {
    /// From the turn's angle in degrees, positive to the right. OSRM's thresholds, about.
    pub fn from_angle(a: f64) -> Self {
        let m = a.abs();
        let right = a > 0.0;
        match m {
            m if m <= 20.0 => Modifier::Straight,
            m if m <= 45.0 && right => Modifier::SlightRight,
            m if m <= 45.0 => Modifier::SlightLeft,
            m if m <= 120.0 && right => Modifier::Right,
            m if m <= 120.0 => Modifier::Left,
            m if m <= 165.0 && right => Modifier::SharpRight,
            m if m <= 165.0 => Modifier::SharpLeft,
            _ => Modifier::UTurn,
        }
    }

    /// A turn back onto another road is a sharp turn, not a U-turn.
    fn no_uturn(self, a: f64) -> Self {
        match (self, a > 0.0) {
            (Modifier::UTurn, true) => Modifier::SharpRight,
            (Modifier::UTurn, false) => Modifier::SharpLeft,
            (m, _) => m,
        }
    }

    pub fn text(self) -> &'static str {
        match self {
            Modifier::Straight => "straight",
            Modifier::SlightRight => "slight right",
            Modifier::Right => "right",
            Modifier::SharpRight => "sharp right",
            Modifier::UTurn => "U-turn",
            Modifier::SharpLeft => "sharp left",
            Modifier::Left => "left",
            Modifier::SlightLeft => "slight left",
        }
    }
}

/// Which side of the road a stop is on, as the route arrives or leaves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The start of a leg: "Head north on Main St".
    Depart,
    /// A stop between legs.
    Stop,
    /// The end of the trip.
    Arrive,
    /// "Turn left onto Main St"; a straight one only when another road goes straighter.
    Turn,
    /// The road's name changes with no turn to make: "Continue onto Main St".
    Continue,
    UTurn,
    /// "Keep left at the fork".
    Fork,
    /// Onto a ramp from a road that is not a motorway or trunk.
    Ramp,
    /// Off a motorway or trunk onto a ramp: "Take exit 12 toward Columbus".
    Exit,
    /// Onto a motorway or trunk from a ramp, beside its traffic.
    Merge,
    /// "At the roundabout, take the 2nd exit".
    Roundabout,
    Ferry,
    /// Off a ferry onto a road.
    OffFerry,
}

/// A road's name and route numbers, as signed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Road {
    pub name: String,
    /// Most important first (Interstate, US, then the rest), with a US route's signed
    /// direction where the route judges it ("I-71 North").
    pub refs: Vec<String>,
}

impl Road {
    pub fn is_empty(&self) -> bool {
        self.name.is_empty() && self.refs.is_empty()
    }

    /// "I-71 North / Main St", or empty.
    pub fn text(&self) -> String {
        let mut parts: Vec<&str> = self
            .refs
            .iter()
            .take(SHOWN_REFS)
            .map(String::as_str)
            .collect();
        if !self.name.is_empty() {
            parts.push(&self.name);
        }
        parts.join(" / ")
    }

    /// The same road for directions: a route number in common, or the same name.
    fn same(&self, o: &Road) -> bool {
        if !self.refs.is_empty() && !o.refs.is_empty() {
            return self.refs.iter().any(|r| o.refs.contains(r));
        }
        self.name == o.name && self.refs == o.refs
    }
}

/// One instruction.
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub kind: Kind,
    pub modifier: Modifier,
    /// The turn's angle in degrees, positive to the right.
    pub angle: f64,
    /// The road the step leads onto (beyond a ramp, for [`Kind::Ramp`]); the ferry, for
    /// [`Kind::Ferry`].
    pub road: Road,
    /// The road is the one the route was already on: "to stay on".
    pub stay: bool,
    /// The exit number (`highway=motorway_junction` or `junction:ref`).
    pub exit: String,
    /// Signs: `destination:ref` and `destination`, joined.
    pub toward: String,
    /// The exit to take at a roundabout, from 1; 0 if the route ends on the ring.
    pub roundabout_exit: u32,
    /// Where the stop is, for [`Kind::Depart`], [`Kind::Stop`] and [`Kind::Arrive`].
    pub side: Option<Side>,
    /// Compass heading, for [`Kind::Depart`] ("north").
    pub heading: &'static str,
    /// The stop number from 1, for [`Kind::Stop`] and [`Kind::Arrive`].
    pub stop: usize,
    pub leg: usize,
    /// The pieces of the leg driven until the next step.
    pub pieces: Range<usize>,
    /// Their points in the leg's.
    pub points: Range<usize>,
    /// Where the step is.
    pub point: P,
    /// Driven until the next step.
    pub metres: f64,
    pub time_s: f64,
    /// Driven before it, from the trip's start.
    pub at_m: f64,
    pub at_s: f64,
}

fn ordinal(n: u32) -> String {
    let suffix = match (n % 10, n % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

impl Step {
    /// The instruction in English (phase 3 decision 11), without distances.
    pub fn text(&self) -> String {
        let road = self.road.text();
        let onto = |verb: &str| match road.is_empty() {
            true => verb.to_owned(),
            false if self.stay => format!("{verb} to stay on {road}"),
            false => format!("{verb} onto {road}"),
        };
        let toward = |s: String| match self.toward.is_empty() {
            true => s,
            false => format!("{s} toward {}", self.toward),
        };
        let side = match self.side {
            Some(Side::Left) => ", on the left",
            Some(Side::Right) => ", on the right",
            None => "",
        };
        let lr = |m: Modifier| match m {
            Modifier::SlightLeft | Modifier::Left | Modifier::SharpLeft => " left",
            Modifier::SlightRight | Modifier::Right | Modifier::SharpRight => " right",
            _ => "",
        };
        match self.kind {
            Kind::Depart => match road.is_empty() {
                true => format!("Head {}", self.heading),
                false => format!("Head {} on {road}", self.heading),
            },
            Kind::Stop => format!("Arrive at stop {}{side}", self.stop),
            Kind::Arrive => format!("Arrive at your destination{side}"),
            Kind::Turn => match self.modifier {
                Modifier::Straight => onto("Go straight"),
                m => onto(&format!("Turn {}", m.text())),
            },
            Kind::Continue => match self.modifier {
                Modifier::SlightLeft => onto("Bear left"),
                Modifier::SlightRight => onto("Bear right"),
                _ => onto("Continue"),
            },
            Kind::UTurn => onto("Make a U-turn"),
            Kind::Fork => toward(onto(&format!("Keep{}", lr(self.modifier)))),
            Kind::Ramp => toward(onto(&format!(
                "Take the ramp{}",
                match lr(self.modifier) {
                    "" => String::new(),
                    s => format!(" on the{s}"),
                }
            ))),
            Kind::Exit => {
                let side = match lr(self.modifier) {
                    " left" => " on the left",
                    _ => "",
                };
                let s = match self.exit.is_empty() {
                    true => format!("Take the exit{side}"),
                    false => format!("Take exit {}{side}", self.exit),
                };
                toward(onto(&s))
            }
            Kind::Merge => toward(onto("Merge")),
            Kind::Roundabout => match self.roundabout_exit {
                0 => onto("Enter the roundabout"),
                n => onto(&format!("At the roundabout, take the {} exit", ordinal(n))),
            },
            Kind::Ferry => match road.is_empty() {
                true => "Take the ferry".into(),
                false => format!("Take the ferry ({road})"),
            },
            Kind::OffFerry => onto("Leave the ferry"),
        }
    }
}

/// Street names and route numbers by `SSTR` ID: the street table, or a stand-in in tests.
pub trait Names {
    /// The street's name and refs.
    fn street_road(&self, id: u32) -> io::Result<Road>;
}

impl Names for Streets {
    fn street_road(&self, id: u32) -> io::Result<Road> {
        let s = self.street(id)?;
        let mut road = Road {
            name: s.name,
            refs: s.refs,
        };
        // The street table names a way that has only refs by its first.
        if looks_like_ref(&road.name) {
            road.refs.push(std::mem::take(&mut road.name));
        }
        Ok(road)
    }
}

/// "I 71", "SR 315", "A-20", "401": a route number rather than a name.
fn looks_like_ref(name: &str) -> bool {
    let (prefix, number) = match name.rsplit_once([' ', '-']) {
        Some((p, n)) => (p, n),
        None => ("", name),
    };
    let digits = number.trim_end_matches(|c: char| c.is_ascii_alphabetic());
    !digits.is_empty()
        && digits.len() <= 4
        && digits.chars().all(|c| c.is_ascii_digit())
        && number.len() - digits.len() <= 1
        && prefix.len() <= 4
        && prefix.chars().all(|c| c.is_ascii_uppercase())
}

/// Compass bearing from `a` to `b` in degrees, clockwise from north (Mercator keeps
/// angles).
fn bearing(a: P, b: P) -> f64 {
    let dx = f64::from(b[0]) - f64::from(a[0]);
    let dy = f64::from(b[1]) - f64::from(a[1]);
    dx.atan2(-dy).to_degrees().rem_euclid(360.0)
}

/// The angle from bearing `a` to bearing `b`, in `(-180, 180]`, positive clockwise.
fn turn_angle(a: f64, b: f64) -> f64 {
    let d = (b - a).rem_euclid(360.0);
    if d > 180.0 { d - 360.0 } else { d }
}

/// The bearing of `pts` leaving `pts[0]`, over about [`BEARING_M`].
fn leaving(pts: impl Iterator<Item = P> + Clone) -> f64 {
    let mut it = pts.clone();
    let Some(first) = it.next() else { return 0.0 };
    let unit_m = geo::metres_per_unit(first);
    let mut last = first;
    for p in it {
        last = p;
        if geo::dist(first, p) * unit_m >= BEARING_M {
            break;
        }
    }
    bearing(first, last)
}

fn compass(b: f64) -> &'static str {
    const NAMES: [&str; 8] = [
        "north",
        "northeast",
        "east",
        "southeast",
        "south",
        "southwest",
        "west",
        "northwest",
    ];
    NAMES[((b + 22.5).rem_euclid(360.0) / 45.0) as usize % 8]
}

/// Classes, as in [`crate::street::CLASSES`].
fn is_link(class: u8) -> bool {
    (5..=9).contains(&class)
}

/// A motorway or trunk.
fn is_fast(class: u8) -> bool {
    class <= 1
}

/// Interstate first, then US routes, then the rest, each as tagged.
fn ref_rank(r: &str) -> u8 {
    match r {
        r if r.starts_with("I ") || r.starts_with("I-") => 0,
        r if r.starts_with("US ") => 1,
        _ => 2,
    }
}

/// "I 71" as signed, "I-71".
fn ref_text(r: &str) -> String {
    match r.strip_prefix("I ") {
        Some(n) => format!("I-{n}"),
        None => r.to_owned(),
    }
}

/// The signed direction of a US route number going from `a` to `b`: odd numbers run
/// north and south, even ones east and west. Only Interstate and US routes follow the rule.
fn route_heading(r: &str, a: P, b: P) -> Option<&'static str> {
    if ref_rank(r) > 1 {
        return None;
    }
    let digits: String = r
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(char::is_ascii_digit)
        .collect();
    let n: u32 = digits.parse().ok()?;
    let dx = f64::from(b[0]) - f64::from(a[0]);
    let dy = f64::from(b[1]) - f64::from(a[1]);
    if dx.hypot(dy) * geo::metres_per_unit(a) < 500.0 {
        return None;
    }
    Some(match (n % 2 == 1, dy < 0.0, dx > 0.0) {
        (true, true, _) => "North",
        (true, false, _) => "South",
        (false, _, true) => "East",
        (false, _, false) => "West",
    })
}

/// The seconds OSRM's turn function gives a turn of `angle` degrees (positive to the
/// right): a sigmoid from next to nothing straight on to [`TURN_PENALTY_S`] turning back.
pub fn turn_cost_s(angle: f64) -> f64 {
    let x = angle.abs() / 180.0;
    let e = match angle >= 0.0 {
        true => 13.0 / TURN_BIAS * x - 6.5 * TURN_BIAS,
        false => 13.0 * TURN_BIAS * x - 6.5 / TURN_BIAS,
    };
    TURN_PENALTY_S / (1.0 + (-e).exp())
}

/// The turns a leg makes, from its geometry, as alternatives are ranked (phase 4 step 4.2,
/// decision 3).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Turns {
    /// Seconds by OSRM's turn function at every junction where the leg changes segment,
    /// and [`UTURN_PENALTY_S`] for each U-turn.
    pub cost_s: f64,
    /// Junctions where it turns by more than 45°, U-turns left out.
    pub turns: u32,
    /// U-turns as the directions show them: back along the road at a junction, or two
    /// turns closer than [`COLLAPSE_M`] that together turn back (across a median).
    pub uturns: u32,
}

impl Leg {
    pub fn turns(&self) -> Turns {
        let (n, pts) = (self.pieces.len(), &self.points);
        // A stop at a node: its piece of next to nothing makes no turn, as in directions.
        let tiny = |k: usize| n > 1 && self.pieces[k].metres < END_M;
        let first = usize::from(n > 0 && tiny(0));
        let end = n.saturating_sub(usize::from(n > 1 && tiny(n - 1) && n - 1 > first));
        // Each junction's angle and where it is along the leg.
        let mut joints = Vec::new();
        let mut at_m: f64 = self.pieces[..first.min(n)].iter().map(|p| p.metres).sum();
        for k in first + 1..end {
            at_m += self.pieces[k - 1].metres;
            let at = self.pieces[k].points.start;
            let (from, to) = (self.pieces[k - 1].points.start, self.pieces[k].points.end);
            let back = leaving(pts[from..=at].iter().rev().copied());
            let out = leaving(pts[at..to].iter().copied());
            joints.push((turn_angle((back + 180.0).rem_euclid(360.0), out), at_m));
        }
        let mut t = Turns::default();
        let mut i = 0;
        while i < joints.len() {
            let (a, m) = joints[i];
            t.cost_s += turn_cost_s(a);
            let pair = joints.get(i + 1).filter(|&&(b, mb)| {
                mb - m < COLLAPSE_M
                    && a.abs() <= 150.0
                    && b.abs() <= 150.0
                    && turn_angle(0.0, a + b).abs() > 165.0
            });
            if a.abs() > 165.0 || pair.is_some() {
                t.uturns += 1;
                t.cost_s += UTURN_PENALTY_S;
            } else if a.abs() > 45.0 {
                t.turns += 1;
            }
            if let Some(&(b, _)) = pair {
                t.cost_s += turn_cost_s(b);
                i += 1;
            }
            i += 1;
        }
        t
    }
}

/// What directions need of one piece of a leg.
#[derive(Clone, Debug)]
struct Info {
    seg: u32,
    class: u8,
    roundabout: bool,
    road: Road,
    /// For a ramp: the signs.
    toward: String,
    exit: String,
    /// The plain node the piece ends at.
    end: u32,
}

impl Info {
    fn ferry(&self) -> bool {
        self.class == FERRY_CLASS
    }
}

/// A road at a junction.
#[derive(Clone, Copy, Debug)]
struct Arm {
    seg: u32,
    /// A car may leave the junction along it.
    leave: bool,
    /// A car may come into the junction along it.
    arrive: bool,
    /// Its bearing leaving the junction.
    bearing: f64,
    class: u8,
    roundabout: bool,
}

/// A step before its stretch is measured.
#[derive(Clone, Debug)]
struct Raw {
    kind: Kind,
    angle: f64,
    modifier: Modifier,
    /// The piece the step starts.
    piece: usize,
    /// The piece whose road it names.
    road_at: usize,
    stay: bool,
    exit: String,
    toward: String,
    roundabout_exit: u32,
}

impl Raw {
    fn new(kind: Kind, angle: f64, piece: usize) -> Self {
        Raw {
            kind,
            angle,
            modifier: Modifier::from_angle(angle),
            piece,
            road_at: piece,
            stay: false,
            exit: String::new(),
            toward: String::new(),
            roundabout_exit: 0,
        }
    }
}

/// Turns the pieces of a leg into steps.
struct Guide<'a> {
    r: &'a Routing,
    names: Option<&'a dyn Names>,
    streets: HashMap<u32, Road>,
}

impl Guide<'_> {
    fn road(&mut self, street: Option<u32>, label: &super::Label) -> io::Result<Road> {
        let mut road = match (street, self.names) {
            (Some(id), Some(n)) => match self.streets.get(&id) {
                Some(r) => r.clone(),
                None => {
                    let r = n.street_road(id)?;
                    self.streets.insert(id, r.clone());
                    r
                }
            },
            _ => Road::default(),
        };
        if road.is_empty() {
            road.name = label.name.clone();
            road.refs = label
                .refs
                .split(';')
                .map(str::trim)
                .filter(|x| !x.is_empty())
                .map(String::from)
                .collect();
        }
        road.refs.sort_by_key(|r| ref_rank(r));
        Ok(road)
    }

    fn infos(&mut self, leg: &Leg) -> io::Result<Vec<Info>> {
        let mut out = Vec::with_capacity(leg.pieces.len());
        for p in &leg.pieces {
            let seg = self.r.segment(p.dir >> 1)?;
            let label = match seg.label {
                Some(l) => self.r.label(l)?,
                None => super::Label::default(),
            };
            let toward = [&label.destination_ref, &label.destination]
                .iter()
                .flat_map(|s| s.split(';'))
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ref_text)
                .collect::<Vec<_>>()
                .join(", ");
            out.push(Info {
                seg: p.dir >> 1,
                class: seg.class,
                roundabout: seg.roundabout,
                road: self.road(seg.street, &label)?,
                toward,
                exit: label.junction_ref.replace(';', "/"),
                end: match p.dir & 1 {
                    0 => seg.b,
                    _ => seg.a,
                },
            });
        }
        Ok(out)
    }

    /// The roads at plain node `v`, at `p`.
    fn arms(&self, v: u32, p: P) -> io::Result<Vec<Arm>> {
        let (x, y) = cell_xy(p);
        let mut out = Vec::new();
        for s in self.r.cell(x, y)?.segments() {
            self.r.with_segment(s, |seg| {
                let arm = |leave, arrive, bearing| Arm {
                    seg: s,
                    leave,
                    arrive,
                    bearing,
                    class: seg.class,
                    roundabout: seg.roundabout,
                };
                if seg.a == v {
                    let b = leaving(seg.points.iter().copied());
                    out.push(arm(true, seg.backward.is_some(), b));
                }
                if seg.b == v {
                    let b = leaving(seg.points.iter().rev().copied());
                    out.push(arm(seg.backward.is_some(), true, b));
                }
            })?;
        }
        Ok(out)
    }

    /// The steps of one leg, before their stretches are measured.
    fn raw(&self, leg: &Leg, info: &[Info]) -> io::Result<Vec<Raw>> {
        let n = info.len();
        let pts = &leg.points;
        // The junction before piece `k`: its node and point, and the bearings in and out,
        // each along its own piece so that a junction close by does not bend it.
        let joint = |k: usize| {
            let at = leg.pieces[k].points.start;
            let (from, to) = (leg.pieces[k - 1].points.start, leg.pieces[k].points.end);
            let back = leaving(pts[from..=at].iter().rev().copied());
            let out = leaving(pts[at..to].iter().copied());
            (
                info[k - 1].end,
                pts[at],
                (back + 180.0).rem_euclid(360.0),
                out,
            )
        };
        // A stop at a node: the leg's first or last piece is next to nothing.
        let tiny = |k: usize| n > 1 && leg.pieces[k].metres < END_M;
        let first = usize::from(tiny(0));
        let end = n - usize::from(tiny(n - 1) && n - 1 > first);
        let mut depart = Raw::new(Kind::Depart, 0.0, 0);
        depart.road_at = first;
        let mut steps = vec![depart];
        let mut k = first + 1;
        while k < end {
            let (prev, cur) = (&info[k - 1], &info[k]);
            let (v, p, in_b, out_b) = joint(k);
            let angle = turn_angle(in_b, out_b);
            // The end of a ramp onto the road its step named.
            let announced = steps.last().is_some_and(|s| {
                s.road_at == k && s.road_at > s.piece && info[k].road.same(&info[s.road_at].road)
            });
            if announced && angle.abs() <= 45.0 && !cur.roundabout && !cur.ferry() {
                k += 1;
                continue;
            }
            if cur.ferry() && !prev.ferry() {
                steps.push(Raw::new(Kind::Ferry, angle, k));
            } else if prev.ferry() && !cur.ferry() {
                steps.push(Raw::new(Kind::OffFerry, angle, k));
            } else if cur.roundabout && !prev.roundabout {
                // Count the exits passed on the ring, then the one taken.
                let mut j = k;
                while j < n && info[j].roundabout {
                    j += 1;
                }
                let mut exits = 0;
                for m in k + 1..=j.min(n - 1) {
                    let (v, p, in_b, out_b) = joint(m);
                    let ours = turn_angle(in_b, out_b);
                    for a in self.arms(v, p)? {
                        if !a.leave || a.roundabout || a.seg == info[m - 1].seg {
                            continue;
                        }
                        // At the exit taken, only the exits before it (to its right).
                        if m == j && (a.seg == info[m].seg || turn_angle(in_b, a.bearing) <= ours) {
                            continue;
                        }
                        exits += 1;
                    }
                }
                let mut s = Raw::new(Kind::Roundabout, angle, k);
                if j < n {
                    s.roundabout_exit = exits + 1;
                    s.road_at = j;
                }
                steps.push(s);
                k = j.max(k + 1);
                continue;
            } else if !(cur.roundabout && prev.roundabout)
                && let Some(s) = self.junction(info, k, v, p, in_b, angle)?
            {
                steps.push(s);
            }
            k += 1;
        }
        steps.push(Raw::new(Kind::Arrive, 0.0, n));
        Ok(steps)
    }

    /// The step at the junction before piece `k`, if it needs one.
    fn junction(
        &self,
        info: &[Info],
        k: usize,
        v: u32,
        p: P,
        in_b: f64,
        angle: f64,
    ) -> io::Result<Option<Raw>> {
        let (prev, cur) = (&info[k - 1], &info[k]);
        let (pl, cl) = (is_link(prev.class), is_link(cur.class));
        let (pf, cf) = (is_fast(prev.class), is_fast(cur.class));
        let same = prev.road.same(&cur.road) && (!cl || prev.toward == cur.toward);
        // The usual case, a road going on through a junction, needs no look around.
        if same && angle.abs() < 35.0 && pl == cl && !(pf && cl) {
            return Ok(None);
        }
        let arms = self.arms(v, p)?;
        // Other ways on, and other ways in (for merges).
        let others: Vec<f64> = arms
            .iter()
            .filter(|a| a.leave && a.seg != prev.seg && a.seg != cur.seg)
            .map(|a| turn_angle(in_b, a.bearing))
            .collect();
        let joined = arms
            .iter()
            .any(|a| a.arrive && a.seg != prev.seg && a.seg != cur.seg && is_fast(a.class));
        // A U-turn only back along the road; onto another, a sharp turn.
        let back = cur.seg == prev.seg || same;
        let m = match back {
            true => Modifier::from_angle(angle),
            false => Modifier::from_angle(angle).no_uturn(angle),
        };
        let mut s = Raw::new(Kind::Turn, angle, k);
        s.modifier = m;
        s.stay = same && prev.road.name == cur.road.name;
        // The signs and exit number of a ramp, from its first piece that has them.
        let ramp_signs = |s: &mut Raw| {
            let mut j = k;
            while j < info.len() && is_link(info[j].class) {
                if s.toward.is_empty() {
                    s.toward = info[j].toward.clone();
                }
                if s.exit.is_empty() {
                    s.exit = info[j].exit.clone();
                }
                j += 1;
            }
            s.road_at = j.min(info.len() - 1);
            s.stay = false;
        };
        if pf && cl && !others.is_empty() {
            s.kind = Kind::Exit;
            if let Some(x) = self.r.junction(v)? {
                s.exit = x.replace(';', "/");
            }
            ramp_signs(&mut s);
            // Left or right of the road going on.
            let main = others
                .iter()
                .copied()
                .min_by(|a, b| a.abs().total_cmp(&b.abs()))
                .unwrap_or(0.0);
            s.modifier = match angle < main {
                true => Modifier::SlightLeft,
                false => Modifier::SlightRight,
            };
            return Ok(Some(s));
        }
        if !pl && !pf && cl {
            s.kind = Kind::Ramp;
            ramp_signs(&mut s);
            return Ok(Some(s));
        }
        if pl && cf && joined && angle.abs() < 60.0 {
            s.kind = Kind::Merge;
            return Ok(Some(s));
        }
        if others.is_empty() {
            return Ok(match () {
                _ if same || (pl && cl) => None,
                _ if m == Modifier::UTurn => {
                    s.kind = Kind::UTurn;
                    Some(s)
                }
                // An unnamed road going on: nothing to say.
                _ if angle.abs() <= 45.0 && cur.road.is_empty() => None,
                _ if angle.abs() <= 45.0 => {
                    s.kind = Kind::Continue;
                    Some(s)
                }
                _ => Some(s),
            });
        }
        // A fork: another way on close to ours, both near straight on.
        let near = others
            .iter()
            .copied()
            .filter(|o| o.abs() <= 50.0 && (o - angle).abs() >= 5.0)
            .min_by(|a, b| (a - angle).abs().total_cmp(&(b - angle).abs()));
        let straightest = others.iter().all(|o| angle.abs() <= o.abs() + 5.0);
        if let Some(o) = near.filter(|_| angle.abs() <= 45.0) {
            if same && straightest && !(pf || pl) {
                return Ok(None);
            }
            s.kind = Kind::Fork;
            s.modifier = match angle < o {
                true => Modifier::SlightLeft,
                false => Modifier::SlightRight,
            };
            if pl && cl {
                ramp_signs(&mut s);
            }
            return Ok(Some(s));
        }
        Ok(match m {
            Modifier::UTurn => {
                s.kind = Kind::UTurn;
                Some(s)
            }
            Modifier::Straight | Modifier::SlightLeft | Modifier::SlightRight
                if (same || cur.road.is_empty()) && straightest =>
            {
                None
            }
            Modifier::Straight | Modifier::SlightLeft | Modifier::SlightRight if straightest => {
                s.kind = Kind::Continue;
                Some(s)
            }
            _ => Some(s),
        })
    }
}

/// Folds turns a few metres apart into one, and drops a name that comes and goes.
fn collapse(steps: Vec<Raw>, leg: &Leg, info: &[Info]) -> Vec<Raw> {
    let metres = |a: usize, b: usize| leg.pieces[a..b].iter().map(|p| p.metres).sum::<f64>();
    let turnish = |k: Kind| matches!(k, Kind::Turn | Kind::Continue | Kind::UTurn);
    let mut out: Vec<Raw> = Vec::with_capacity(steps.len());
    for s in steps {
        let Some(last) = out.last() else {
            out.push(s);
            continue;
        };
        let gap = metres(last.piece, s.piece);
        // A U-turn stays a step of its own: folded into the next turn it would read as a
        // turn the route does not make.
        let uturn = |r: &Raw| r.angle.abs() > 150.0;
        if turnish(last.kind) && turnish(s.kind) && gap < COLLAPSE_M && !uturn(last) && !uturn(&s) {
            let mut last = out.pop().unwrap();
            let before = &info[last.piece - 1].road;
            last.angle = turn_angle(0.0, last.angle + s.angle);
            last.modifier = Modifier::from_angle(last.angle);
            last.road_at = s.road_at;
            let after = &info[s.road_at].road;
            last.stay = before.same(after) && before.name == after.name;
            if !before.same(after) {
                last.modifier = last.modifier.no_uturn(last.angle);
            }
            last.kind = match last.modifier {
                Modifier::UTurn => Kind::UTurn,
                Modifier::Straight if before.same(after) => continue,
                Modifier::Straight => Kind::Continue,
                _ => Kind::Turn,
            };
            out.push(last);
            continue;
        }
        // A name for a short stretch between two straight steps.
        if last.kind == Kind::Continue
            && s.kind == Kind::Continue
            && gap < FLAP_M
            && info[last.piece - 1].road.same(&info[s.road_at].road)
        {
            out.pop();
            continue;
        }
        out.push(s);
    }
    out
}

/// The side of the line arriving at `at` (travelling along `bearing`) that `p` is on.
fn side(at: P, bearing: f64, p: P) -> Option<Side> {
    if geo::dist(at, p) * geo::metres_per_unit(at) < SIDE_M {
        return None;
    }
    match turn_angle(bearing, self::bearing(at, p)) > 0.0 {
        true => Some(Side::Right),
        false => Some(Side::Left),
    }
}

impl Routing {
    /// The road of each piece of `leg`: its street's name and refs, else its label's,
    /// refs most important first and as tagged.
    pub fn roads(&self, leg: &Leg, names: Option<&dyn Names>) -> io::Result<Vec<Road>> {
        let mut g = Guide {
            r: self,
            names,
            streets: HashMap::new(),
        };
        Ok(g.infos(leg)?.into_iter().map(|i| i.road).collect())
    }

    /// The trip's directions: per routed leg, a depart, the steps on the way, and a stop
    /// (or, at the end, the arrival). `stops` are the trip's, for the side of the road;
    /// `names` gives street names and refs (the pack's [`Streets`]).
    pub fn directions(
        &self,
        trip: &Trip,
        stops: &[Stop],
        names: Option<&dyn Names>,
    ) -> io::Result<Vec<Step>> {
        let mut g = Guide {
            r: self,
            names,
            streets: HashMap::new(),
        };
        let mut out = Vec::new();
        let (mut at_m, mut at_s) = (0.0, 0.0);
        let last_leg = trip.legs.len().saturating_sub(1);
        for (li, leg) in trip.legs.iter().enumerate() {
            let Some(leg) = leg else { continue };
            if leg.pieces.is_empty() {
                continue;
            }
            let info = g.infos(leg)?;
            let raw = collapse(g.raw(leg, &info)?, leg, &info);
            let snaps: [Option<&Snap>; 2] = [trip.snaps[li].as_ref(), trip.snaps[li + 1].as_ref()];
            for (i, s) in raw.iter().enumerate() {
                let end = raw.get(i + 1).map_or(leg.pieces.len(), |n| n.piece);
                let pieces = s.piece.min(end)..end;
                let (metres, time_s) = leg.pieces[pieces.clone()]
                    .iter()
                    .fold((0.0, 0.0), |(m, t), p| (m + p.metres, t + p.time_s));
                let points = match pieces.is_empty() {
                    true => {
                        let e = leg.points.len() - 1;
                        e..leg.points.len()
                    }
                    false => leg.pieces[pieces.start].points.start..leg.pieces[end - 1].points.end,
                };
                let mut road = match s.kind {
                    Kind::Depart | Kind::Stop | Kind::Arrive if pieces.is_empty() => {
                        Road::default()
                    }
                    _ => info[s.road_at.min(info.len() - 1)].road.clone(),
                };
                // Route numbers' directions, from where the route follows the road.
                if !road.refs.is_empty() && !matches!(s.kind, Kind::Stop | Kind::Arrive) {
                    let from = s.road_at.min(info.len() - 1);
                    let mut to = from;
                    let mut run = 0.0;
                    while to + 1 < info.len() && info[to + 1].road.same(&road) && run < HEADING_M {
                        run += leg.pieces[to].metres;
                        to += 1;
                    }
                    let (a, b) = (
                        leg.points[leg.pieces[from].points.start],
                        leg.points[leg.pieces[to].points.end - 1],
                    );
                    road.refs = road
                        .refs
                        .iter()
                        .map(|r| match route_heading(r, a, b) {
                            Some(h) => format!("{} {h}", ref_text(r)),
                            None => ref_text(r),
                        })
                        .collect();
                }
                // Signs that only repeat the road's name.
                let toward = s
                    .toward
                    .split(", ")
                    .filter(|t| !t.eq_ignore_ascii_case(&road.name))
                    .collect::<Vec<_>>()
                    .join(", ");
                let point = leg.points[points.start];
                let mut step = Step {
                    kind: s.kind,
                    modifier: s.modifier,
                    angle: s.angle,
                    road,
                    stay: s.stay,
                    exit: s.exit.clone(),
                    toward,
                    roundabout_exit: s.roundabout_exit,
                    side: None,
                    heading: "",
                    stop: 0,
                    leg: li,
                    pieces,
                    points,
                    point,
                    metres,
                    time_s,
                    at_m,
                    at_s,
                };
                match s.kind {
                    Kind::Depart => {
                        let b = leaving(leg.points.iter().copied());
                        step.heading = compass(b);
                        step.side = snaps[0].and_then(|_| side(point, b, stops[li].p));
                    }
                    Kind::Arrive => {
                        let b = (leaving(leg.points.iter().rev().copied()) + 180.0) % 360.0;
                        let end = *leg.points.last().unwrap();
                        step.side = snaps[1].and_then(|_| side(end, b, stops[li + 1].p));
                        step.point = end;
                        step.stop = li + 2;
                        if li != last_leg {
                            step.kind = Kind::Stop;
                        }
                    }
                    _ => {}
                }
                at_m += metres;
                at_s += time_s;
                out.push(step);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn angles_and_modifiers() {
        assert_eq!(turn_angle(350.0, 10.0), 20.0);
        assert_eq!(turn_angle(10.0, 350.0), -20.0);
        assert_eq!(turn_angle(0.0, 180.0), 180.0);
        assert_eq!(Modifier::from_angle(10.0), Modifier::Straight);
        assert_eq!(Modifier::from_angle(-30.0), Modifier::SlightLeft);
        assert_eq!(Modifier::from_angle(90.0), Modifier::Right);
        assert_eq!(Modifier::from_angle(-140.0), Modifier::SharpLeft);
        assert_eq!(Modifier::from_angle(175.0), Modifier::UTurn);
        // y runs south: up the map is north, right is east.
        assert_eq!(bearing([100, 100], [100, 50]), 0.0);
        assert_eq!(bearing([100, 100], [150, 100]), 90.0);
        assert_eq!(compass(100.0), "east");
        assert_eq!(compass(350.0), "north");
        assert_eq!(compass(225.0), "southwest");
    }

    #[test]
    fn turn_costs_follow_osrm() {
        // OSRM's car.lua at these angles, to the hundredth of a second.
        let r = |a: f64| (turn_cost_s(a) * 100.0).round() / 100.0;
        assert_eq!(r(0.0), 0.01);
        assert_eq!(r(90.0), 2.11);
        assert_eq!(r(-90.0), 5.39);
        assert_eq!(r(180.0), 7.45);
        assert!(turn_cost_s(-45.0) > turn_cost_s(45.0));
    }

    /// A leg along `pts`, a piece between each pair of points.
    fn leg_along(pts: &[P]) -> Leg {
        let pieces = (1..pts.len())
            .map(|i| super::super::Piece {
                dir: (i as u32) << 1,
                start: 0.0,
                end: 1.0,
                metres: geo::dist(pts[i - 1], pts[i]) * geo::metres_per_unit(pts[i]),
                time_s: 1.0,
                points: i - 1..i + 1,
            })
            .collect();
        Leg {
            weight: 0,
            time_s: 0.0,
            metres: 0.0,
            pieces,
            points: pts.to_vec(),
            uses: 0,
            broken: 0,
            settled: 0,
        }
    }

    #[test]
    fn turns_and_uturns_from_the_geometry() {
        // 100 m east, a right turn south, 100 m, then back north the way it came.
        let at = |e: f64, s: f64| {
            let p = geo::project(40.0, -83.0);
            let u = 1.0 / geo::metres_per_unit(p);
            [
                (f64::from(p[0]) + e * u) as u32,
                (f64::from(p[1]) + s * u) as u32,
            ]
        };
        let t = leg_along(&[
            at(0.0, 0.0),
            at(100.0, 0.0),
            at(100.0, 100.0),
            at(100.0, 0.0),
        ])
        .turns();
        assert_eq!((t.turns, t.uturns), (1, 1));
        assert!(
            (t.cost_s - (2.11 + 7.45 + 20.0)).abs() < 0.05,
            "{}",
            t.cost_s
        );
        // Across a median: left, 20 m, left again onto the other carriageway.
        let t = leg_along(&[
            at(0.0, 0.0),
            at(100.0, 0.0),
            at(100.0, -20.0),
            at(0.0, -20.0),
        ])
        .turns();
        assert_eq!((t.turns, t.uturns), (0, 1));
        assert!(
            (t.cost_s - (2.0 * 5.39 + 20.0)).abs() < 0.05,
            "{}",
            t.cost_s
        );
        // The same 200 m apart are two left turns.
        let t = leg_along(&[
            at(0.0, 0.0),
            at(100.0, 0.0),
            at(100.0, -200.0),
            at(0.0, -200.0),
        ])
        .turns();
        assert_eq!((t.turns, t.uturns), (2, 0));
    }

    #[test]
    fn route_numbers_and_their_directions() {
        let a = geo::project(40.0, -83.0);
        let north = geo::project(40.1, -83.0);
        let west = geo::project(40.0, -83.2);
        assert_eq!(route_heading("I 71", a, north), Some("North"));
        assert_eq!(route_heading("I 71", north, a), Some("South"));
        assert_eq!(route_heading("US 40", a, west), Some("West"));
        assert_eq!(route_heading("SR 315", a, north), None);
        assert_eq!(route_heading("I 70", a, a), None);
        assert_eq!(ref_text("I 71"), "I-71");
        let mut refs = vec!["SR 16".to_owned(), "US 40".into(), "I 70".into()];
        refs.sort_by_key(|r| ref_rank(r));
        assert_eq!(refs, ["I 70", "US 40", "SR 16"]);
        let r = |name: &str, refs: &[&str]| Road {
            name: name.into(),
            refs: refs.iter().map(|s| s.to_string()).collect(),
        };
        assert!(r("Innerbelt", &["I 71", "I 90"]).same(&r("", &["I 71"])));
        assert!(!r("Main St", &["SR 10"]).same(&r("Main St", &["SR 11"])));
        assert!(r("Main St", &[]).same(&r("Main St", &[])));
        assert_eq!(r("Main St", &["US 33"]).text(), "US 33 / Main St");
        for x in ["I 71", "SR 315", "A-20", "401", "US 33A"] {
            assert!(looks_like_ref(x), "{x}");
        }
        for x in ["Main St", "Route 20", "Avenue 2", "", "5th Avenue"] {
            assert!(!looks_like_ref(x), "{x}");
        }
    }

    #[test]
    fn step_text() {
        let mut s = Step {
            kind: Kind::Turn,
            modifier: Modifier::Left,
            angle: -90.0,
            road: Road {
                name: "Main St".into(),
                refs: vec![],
            },
            stay: false,
            exit: String::new(),
            toward: String::new(),
            roundabout_exit: 0,
            side: None,
            heading: "",
            stop: 0,
            leg: 0,
            pieces: 0..0,
            points: 0..0,
            point: [0, 0],
            metres: 0.0,
            time_s: 0.0,
            at_m: 0.0,
            at_s: 0.0,
        };
        assert_eq!(s.text(), "Turn left onto Main St");
        s.stay = true;
        assert_eq!(s.text(), "Turn left to stay on Main St");
        s.stay = false;
        s.kind = Kind::Exit;
        s.modifier = Modifier::SlightRight;
        s.exit = "12".into();
        s.toward = "Columbus".into();
        assert_eq!(s.text(), "Take exit 12 onto Main St toward Columbus");
        s.modifier = Modifier::SlightLeft;
        s.road = Road::default();
        assert_eq!(s.text(), "Take exit 12 on the left toward Columbus");
        s.road.name = "Main St".into();
        s.kind = Kind::Roundabout;
        s.roundabout_exit = 2;
        assert_eq!(
            s.text(),
            "At the roundabout, take the 2nd exit onto Main St"
        );
        s.kind = Kind::Arrive;
        s.side = Some(Side::Right);
        assert_eq!(s.text(), "Arrive at your destination, on the right");
        assert_eq!(ordinal(11), "11th");
        assert_eq!(ordinal(22), "22nd");
    }
}
