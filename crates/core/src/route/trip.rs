//! Routes between points (phase 3 step 3.4): a point is snapped to the nearest segment
//! (its own street's, for a search result that names one), the search starts from both
//! ends of that segment with the part of it still to drive, and a trip's legs are routed
//! one after another through the same caches.
//!
//! When a leg has no route with the avoid options, it is routed again with fewer of them,
//! and the leg says which it had to break (phase 3 decision 3).

use std::cmp::Reverse;
use std::collections::HashSet;
use std::io;
use std::ops::Range;

use super::{AVOID, FERRY_CLASS, Path, Routing, Seed, Segment};
use crate::geo::{self, P};

/// A snap farther than this from its point is worth a warning.
pub const FAR_SNAP_M: f64 = 500.0;
/// The radii tried in turn when snapping, in metres; nothing farther is a snap.
const SNAP_RADII_M: [f64; 6] = [50.0, 200.0, 1_000.0, 5_000.0, 20_000.0, 50_000.0];
/// How far a stop's own street may be from it and still be preferred to a nearer road.
const STREET_M: f64 = 1_000.0;
/// How much farther a road in the large component may be than a nearer one in a small
/// component (step 3.1) and still be preferred.
const SMALL_SLACK_M: f64 = 500.0;

/// A point to route through.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stop {
    pub p: P,
    /// The `SSTR` street of a search result (an address, or a street), to snap to first.
    pub street: Option<u32>,
}

impl Stop {
    pub fn at(p: P) -> Self {
        Stop { p, street: None }
    }
}

/// Where a stop meets the graph.
#[derive(Clone, Debug, PartialEq)]
pub struct Snap {
    pub segment: u32,
    /// How far along the segment's points, from 0 at its `a` to 1 at its `b`.
    pub along: f64,
    /// The nearest point of the segment.
    pub point: P,
    /// Metres from the stop.
    pub dist_m: f64,
    /// On the stop's own street.
    pub on_street: bool,
    /// The segment is in a small component (step 3.1).
    pub small: bool,
}

/// A segment direction and the part of it driven, as [`Piece::start`] and [`Piece::end`].
type Span = (u32, f64, f64);

/// A stretch of a leg along one segment direction.
#[derive(Clone, Debug, PartialEq)]
pub struct Piece {
    /// `segment << 1 | reversed`.
    pub dir: u32,
    /// The part driven, from 0 at the direction's start to 1 at its end.
    pub start: f64,
    pub end: f64,
    pub metres: f64,
    pub time_s: f64,
    /// Its points in the leg's.
    pub points: Range<usize>,
}

/// A route from one stop to the next.
#[derive(Clone, Debug, PartialEq)]
pub struct Leg {
    /// What the route minimized, in tenths of a second: the time with the penalty for
    /// entering restricted roads.
    pub weight: u32,
    pub time_s: f64,
    pub metres: f64,
    pub pieces: Vec<Piece>,
    /// The line, from the first stop's snap to the next's.
    pub points: Vec<P>,
    /// [`super::TOLL`] and [`super::FERRY`]: what the leg drives on.
    pub uses: u8,
    /// The avoided flags the leg had to use, because it has no route without them.
    pub broken: u8,
    /// Nodes the searches settled.
    pub settled: usize,
}

/// A route through stops in the order given (phase 3 decision 5).
#[derive(Clone, Debug, PartialEq)]
pub struct Trip {
    /// Per stop; `None` when no road is near enough.
    pub snaps: Vec<Option<Snap>>,
    /// Per pair of stops; `None` when there is no route at all, or a stop has no snap.
    pub legs: Vec<Option<Leg>>,
    pub avoid: u8,
}

impl Trip {
    /// Every stop snapped and every leg routed.
    pub fn complete(&self) -> bool {
        self.snaps.iter().all(Option::is_some) && self.legs.iter().all(Option::is_some)
    }

    pub fn time_s(&self) -> f64 {
        self.legs.iter().flatten().fold(0.0, |t, l| t + l.time_s)
    }

    pub fn metres(&self) -> f64 {
        self.legs.iter().flatten().fold(0.0, |m, l| m + l.metres)
    }

    /// The avoid options some leg had to break.
    pub fn broken(&self) -> u8 {
        self.legs.iter().flatten().fold(0, |b, l| b | l.broken)
    }
}

/// The nearest point of `pts` to `p`: its distance in world units, and the fraction of
/// the line's length before it.
fn nearest(pts: &[P], p: P) -> (f64, f64, P) {
    let q = [f64::from(p[0]), f64::from(p[1])];
    let (mut best, mut at, mut foot) = (f64::INFINITY, 0.0, pts[0]);
    let mut run = 0.0;
    for w in pts.windows(2) {
        let a = [f64::from(w[0][0]), f64::from(w[0][1])];
        let d = [f64::from(w[1][0]) - a[0], f64::from(w[1][1]) - a[1]];
        let len2 = d[0] * d[0] + d[1] * d[1];
        let t = match len2 > 0.0 {
            true => (((q[0] - a[0]) * d[0] + (q[1] - a[1]) * d[1]) / len2).clamp(0.0, 1.0),
            false => 0.0,
        };
        let f = [a[0] + t * d[0], a[1] + t * d[1]];
        let dist = (q[0] - f[0]).hypot(q[1] - f[1]);
        let len = len2.sqrt();
        if dist < best {
            best = dist;
            at = run + t * len;
            foot = [f[0].round() as u32, f[1].round() as u32];
        }
        run += len;
    }
    let along = match run > 0.0 {
        true => at / run,
        false => 0.0,
    };
    (best, along, foot)
}

/// `pts` from fraction `start` to `end` of its length.
fn cut(pts: &[P], start: f64, end: f64) -> Vec<P> {
    let lens: Vec<f64> = pts.windows(2).map(|w| geo::dist(w[0], w[1])).collect();
    let total: f64 = lens.iter().sum();
    if total == 0.0 {
        return vec![pts[0], pts[pts.len() - 1]];
    }
    let point_at = |m: f64| -> P {
        let mut run = 0.0;
        for (i, &l) in lens.iter().enumerate() {
            if run + l >= m && l > 0.0 {
                let t = ((m - run) / l).clamp(0.0, 1.0);
                let (a, b) = (pts[i], pts[i + 1]);
                let lerp = |x: u32, y: u32| {
                    (f64::from(x) + t * (f64::from(y) - f64::from(x))).round() as u32
                };
                return [lerp(a[0], b[0]), lerp(a[1], b[1])];
            }
            run += l;
        }
        pts[pts.len() - 1]
    };
    let (s, e) = (start * total, end * total);
    let mut out = vec![point_at(s)];
    let mut run = 0.0;
    for (i, &l) in lens.iter().enumerate() {
        run += l;
        if run > s && run < e {
            out.push(pts[i + 1]);
        }
    }
    out.push(point_at(e));
    out
}

/// The share of `t` a fraction `f` of it is, rounded.
fn part(t: u32, f: f64) -> u32 {
    (f64::from(t) * f).round() as u32
}

impl Routing {
    /// The nearest segment to the stop that a route can start or end on: its own street's
    /// within [`STREET_M`], else the nearest road, preferring the large component.
    /// Ferries are never snapped to.
    pub fn snap(&self, stop: &Stop) -> io::Result<Option<Snap>> {
        let mpu = geo::metres_per_unit(stop.p);
        let mut seen = HashSet::new();
        let (mut any, mut big, mut street): (Option<Snap>, Option<Snap>, Option<Snap>) =
            (None, None, None);
        for r in SNAP_RADII_M {
            for s in self.segments_near(stop.p, r)? {
                if !seen.insert(s) {
                    continue;
                }
                // Ferries are left out.
                let found = self.with_segment(s, |seg| {
                    (seg.class != FERRY_CLASS).then(|| {
                        let (d, along, point) = nearest(&seg.points, stop.p);
                        Snap {
                            segment: s,
                            along,
                            point,
                            dist_m: d * mpu,
                            on_street: stop.street.is_some() && seg.street == stop.street,
                            small: seg.small,
                        }
                    })
                })?;
                let Some(snap) = found else { continue };
                let keep = |slot: &mut Option<Snap>| {
                    if slot.as_ref().is_none_or(|x| snap.dist_m < x.dist_m) {
                        *slot = Some(snap.clone());
                    }
                };
                if snap.on_street {
                    keep(&mut street);
                }
                if !snap.small {
                    keep(&mut big);
                }
                keep(&mut any);
            }
            // A snap is certain once it is within `r`: every segment that near is found.
            fn within(x: &Option<Snap>, r: f64) -> Option<&Snap> {
                x.as_ref().filter(|x| x.dist_m <= r)
            }
            if let Some(s) = within(&street, r).filter(|s| s.dist_m <= STREET_M) {
                return Ok(Some(s.clone()));
            }
            if stop.street.is_some() && r < STREET_M {
                continue;
            }
            let Some(a) = within(&any, r) else { continue };
            if !a.small {
                return Ok(Some(a.clone()));
            }
            let slack = a.dist_m + SMALL_SLACK_M;
            if let Some(b) = within(&big, r).filter(|b| b.dist_m <= slack) {
                return Ok(Some(b.clone()));
            }
            if r >= slack {
                return Ok(Some(a.clone()));
            }
        }
        Ok(big.or(any))
    }

    /// Where a route from `snap` can start: the far end of each direction of its segment,
    /// with the part of it still to drive.
    fn sources(snap: &Snap, seg: &Segment) -> Vec<Seed> {
        let mut out = vec![(
            seg.forward.head,
            part(seg.forward.time_ds, 1.0 - snap.along),
        )];
        if let Some(d) = &seg.backward {
            out.push((d.head, part(d.time_ds, snap.along)));
        }
        out
    }

    /// Where a route to `snap` can end: the near end of each direction of its segment, or
    /// a copy of it that may leave along it, with the part of it still to drive.
    fn targets(&self, snap: &Snap, seg: &Segment) -> io::Result<(Vec<Seed>, Vec<bool>)> {
        let (mut out, mut reversed) = (Vec::new(), Vec::new());
        let ends = [
            (seg.a, Some(&seg.forward), snap.along, false),
            (seg.b, seg.backward.as_ref(), 1.0 - snap.along, true),
        ];
        for (v, d, f, rev) in ends {
            let Some(d) = d else { continue };
            let w = part(d.time_ds, f);
            let dir = snap.segment << 1 | u32::from(rev);
            out.push((v, w));
            reversed.push(rev);
            for c in self.copies(v)? {
                if c.leaves.binary_search(&dir).is_ok() {
                    out.push((c.node, w));
                    reversed.push(rev);
                }
            }
        }
        Ok((out, reversed))
    }

    /// The route from `from` to `to` on edges without the flags in `avoid`, or `None`.
    pub fn leg_between(&self, from: &Snap, to: &Snap, avoid: u8) -> io::Result<Option<Leg>> {
        let (a, b) = (self.segment(from.segment)?, self.segment(to.segment)?);
        let sources = Self::sources(from, &a);
        let (targets, reversed) = self.targets(to, &b)?;
        let path = self.search(&sources, &targets, avoid)?;
        let mut settled = 0;
        let mut best: Option<(u32, Vec<Span>)> = None;
        if let Some(Path {
            weight,
            legs,
            settled: n,
            source,
            target,
        }) = path
        {
            settled = n;
            // The forward direction's seed is the first.
            let first = match source {
                0 => (from.segment << 1, from.along, 1.0),
                _ => (from.segment << 1 | 1, 1.0 - from.along, 1.0),
            };
            let last = match reversed[target] {
                false => (to.segment << 1, 0.0, to.along),
                true => (to.segment << 1 | 1, 0.0, 1.0 - to.along),
            };
            let mut pieces = vec![first];
            pieces.extend(legs.iter().map(|&l| (l, 0.0, 1.0)));
            pieces.push(last);
            best = Some((weight, pieces));
        }
        // Along the one segment, without the graph.
        if from.segment == to.segment {
            let ways = [
                (Some(&a.forward), from.along, to.along, 0),
                (a.backward.as_ref(), 1.0 - from.along, 1.0 - to.along, 1),
            ];
            for (d, s, e, rev) in ways {
                let Some(d) = d.filter(|_| s <= e) else {
                    continue;
                };
                let w = part(d.time_ds, e - s);
                if best.as_ref().is_none_or(|b| w <= b.0) {
                    best = Some((w, vec![(from.segment << 1 | rev, s, e)]));
                }
            }
        }
        let Some((weight, pieces)) = best else {
            return Ok(None);
        };
        let mut leg = self.leg(weight, &pieces)?;
        leg.settled = settled;
        Ok(Some(leg))
    }

    /// A leg's totals and line from its pieces.
    fn leg(&self, weight: u32, pieces: &[Span]) -> io::Result<Leg> {
        let mut leg = Leg {
            weight,
            time_s: 0.0,
            metres: 0.0,
            pieces: Vec::with_capacity(pieces.len()),
            points: Vec::new(),
            uses: 0,
            broken: 0,
            settled: 0,
        };
        let only = pieces.len() == 1;
        for &(dir, start, end) in pieces {
            // Nothing driven: a stop at the segment's end.
            if end <= start && !only {
                continue;
            }
            let seg = self.segment(dir >> 1)?;
            let reversed = dir & 1 != 0;
            let d = *seg
                .direction(reversed)
                .ok_or_else(|| crate::varint::invalid("a route along a one-way road's back"))?;
            let mut pts = seg.points;
            if reversed {
                pts.reverse();
            }
            let whole = start == 0.0 && end == 1.0;
            let pts = if whole { pts } else { cut(&pts, start, end) };
            let f = (end - start).max(0.0);
            let first = match leg.points.last() == Some(&pts[0]) {
                true => leg.points.len() - 1,
                false => leg.points.len(),
            };
            let skip = usize::from(first < leg.points.len());
            leg.points.extend_from_slice(&pts[skip..]);
            let piece = Piece {
                dir,
                start,
                end,
                metres: seg.metres * f,
                time_s: f64::from(d.time_ds) / 10.0 * f,
                points: first..leg.points.len(),
            };
            leg.time_s += piece.time_s;
            leg.metres += piece.metres;
            if f > 0.0 {
                leg.uses |= d.flags & AVOID;
            }
            leg.pieces.push(piece);
        }
        Ok(leg)
    }

    /// The leg from `from` to `to` avoiding as many of `avoid` as it can: with all of
    /// them if it can, else with the fewest broken (the quicker route of those that break
    /// as few), else `None`.
    pub fn leg_avoiding(&self, from: &Snap, to: &Snap, avoid: u8) -> io::Result<Option<Leg>> {
        let avoid = avoid & AVOID;
        // Subsets of `avoid`, most kept first.
        let mut subsets: Vec<u8> = (0..=avoid).filter(|s| s & !avoid == 0).collect();
        subsets.sort_by_key(|s| Reverse(s.count_ones()));
        let mut settled = 0;
        let mut i = 0;
        while i < subsets.len() {
            let ones = subsets[i].count_ones();
            let mut best: Option<Leg> = None;
            while i < subsets.len() && subsets[i].count_ones() == ones {
                if let Some(l) = self.leg_between(from, to, subsets[i])? {
                    settled += l.settled;
                    if best.as_ref().is_none_or(|b| l.weight < b.weight) {
                        best = Some(l);
                    }
                }
                i += 1;
            }
            if let Some(mut l) = best {
                l.broken = l.uses & avoid;
                l.settled = settled;
                return Ok(Some(l));
            }
        }
        Ok(None)
    }

    /// Routes through `stops` in order, each leg avoiding what it can of `avoid`.
    pub fn trip(&self, stops: &[Stop], avoid: u8) -> io::Result<Trip> {
        let snaps = stops
            .iter()
            .map(|s| self.snap(s))
            .collect::<io::Result<Vec<_>>>()?;
        let mut legs = Vec::new();
        for w in snaps.windows(2) {
            legs.push(match (&w[0], &w[1]) {
                (Some(a), Some(b)) => self.leg_avoiding(a, b, avoid)?,
                _ => None,
            });
        }
        Ok(Trip { snaps, legs, avoid })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_point_and_fraction() {
        let pts = [[100, 100], [200, 100], [200, 200]];
        let (d, along, foot) = nearest(&pts, [250, 150]);
        assert_eq!(d, 50.0);
        assert_eq!(foot, [200, 150]);
        assert!((along - 0.75).abs() < 1e-9);
        let (d, along, _) = nearest(&pts, [90, 100]);
        assert_eq!((d, along), (10.0, 0.0));
    }

    #[test]
    fn cut_keeps_the_inner_points() {
        let pts = [[0, 0], [100, 0], [100, 100]];
        assert_eq!(cut(&pts, 0.25, 0.75), vec![[50, 0], [100, 0], [100, 50]]);
        assert_eq!(cut(&pts, 0.0, 0.5), vec![[0, 0], [100, 0]]);
        assert_eq!(cut(&pts, 0.6, 0.6), vec![[100, 20], [100, 20]]);
    }
}
