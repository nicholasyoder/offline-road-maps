//! The quickest order of a trip's stops (phase 4 step 4.1, decision 1), over the travel
//! times between them ([`super::Routing::matrix_until`]). Pure: no pack.
//!
//! The order is found as a cycle. A round trip is the cycle through the stops, from the
//! first. Otherwise one dummy stop joins the last stop back to the first: its arcs cost
//! nothing to an open end, and [`FORBIDDEN`] to anything but a kept one. Up to [`EXACT`]
//! stops to reorder, Held–Karp finds the best cycle. Above that, each of [`STARTS`] starts
//! inserts the stops in a random order, each where it adds least, then Or-opt and 2-opt
//! moves improve it (a reversed run is costed again both ways, as the times are
//! asymmetric), and the best order is kicked with double bridges and improved again, for
//! [`ROUNDS_PER_STOP`] rounds a stop or until a time budget ([`budget`]) runs out. The
//! moves look only at each stop's [`NEIGHBOURS`] nearest, so a pass is near n² at most.
//! The random numbers come from a fixed seed: the same times give the same order, unless
//! the budget runs out first (only for trips of a few hundred stops).

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// No route between two stops.
pub const NONE: u32 = u32::MAX;
/// The most stops to reorder (those not kept at an end) that are ordered exactly.
pub const EXACT: usize = 12;
/// A leg with no route: dearer than any trip, so it is used only when it must be.
const NO_ROUTE: i64 = 1 << 36;
/// An arc that would move a kept end: dearer than all others together.
const FORBIDDEN: i64 = 1 << 52;
const STARTS: usize = 8;
const ROUNDS_PER_STOP: usize = 20;
const NEIGHBOURS: usize = 10;
/// The longest run of stops an Or-opt move takes.
const RUN: usize = 3;
/// The longest run a kick moves.
const KICK: usize = 30;
const SEED: u64 = 0x005e_ed0f_570e;

/// Travel weights between every pair of stops, in tenths of a second as a leg's
/// [`super::Leg::weight`]: row `i`, column `j` is from stop `i` to stop `j`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Matrix {
    n: usize,
    w: Vec<u32>,
}

impl Matrix {
    /// `n` stops, with no route between any two yet, and nothing from a stop to itself.
    pub fn new(n: usize) -> Self {
        let mut w = vec![NONE; n * n];
        for i in 0..n {
            w[i * n + i] = 0;
        }
        Matrix { n, w }
    }

    pub fn len(&self) -> usize {
        self.n
    }

    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    pub fn get(&self, i: usize, j: usize) -> u32 {
        self.w[i * self.n + j]
    }

    pub fn set(&mut self, i: usize, j: usize, w: u32) {
        self.w[i * self.n + j] = w;
    }

    /// The weight of driving through the stops in `order`, back to the first for a round
    /// trip; `None` if a leg has no route.
    pub fn total(&self, order: &[usize], round_trip: bool) -> Option<u64> {
        let back = order.first().filter(|_| round_trip && order.len() > 1);
        let legs = order.windows(2).map(|w| (w[0], w[1]));
        let legs = legs.chain(back.map(|&f| (order[order.len() - 1], f)));
        legs.map(|(i, j)| Some(self.get(i, j)).filter(|&w| w != NONE).map(u64::from))
            .sum()
    }
}

/// Which ends of the trip stay where they are.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ends {
    pub keep_first: bool,
    pub keep_last: bool,
    /// Back to the first stop at the end: the first is kept, and the last is any.
    pub round_trip: bool,
}

impl Ends {
    /// The stops of `n` that are reordered.
    pub fn free(&self, n: usize) -> usize {
        match self.round_trip {
            true => n.saturating_sub(1),
            false => n.saturating_sub(usize::from(self.keep_first) + usize::from(self.keep_last)),
        }
    }
}

/// How long the heuristic may improve the order of `n` stops: 2 s, or 10 ms a stop, up to
/// 10 s. Trips of up to about a hundred stops finish their rounds well before.
pub fn budget(n: usize) -> Duration {
    Duration::from_millis((n as u64 * 10).clamp(2_000, 10_000))
}

/// The quickest order of the stops of `m`: a permutation of `0..m.len()`, starting with 0
/// if the first is kept or the trip is a round trip, and ending with the last if it is
/// kept. `None` once `stop` returns true.
pub fn order(m: &Matrix, ends: Ends, stop: &dyn Fn() -> bool) -> Option<Vec<usize>> {
    let n = m.len();
    if n < 2 {
        return Some((0..n).collect());
    }
    let arcs = Arcs::new(m, ends);
    let tour = match ends.free(n) <= EXACT {
        true => held_karp(&arcs),
        false => heuristic(&arcs, Instant::now() + budget(n), stop)?,
    };
    Some(arcs.order(&tour))
}

/// The best order from `count` random starts, each improved by Or-opt and 2-opt until it
/// cannot be, without the kicks: what criterion 1 measures [`order`] against.
pub fn restarts(m: &Matrix, ends: Ends, count: usize, seed: u64) -> Vec<usize> {
    let n = m.len();
    if n < 2 {
        return (0..n).collect();
    }
    let arcs = Arcs::new(m, ends);
    let near = Near::new(&arcs);
    let mut rng = Rng::new(seed);
    let mut best: Option<Tour> = None;
    for _ in 0..count {
        let mut t = Tour::new(&arcs, insertion(&arcs, &mut rng));
        t.improve(&arcs, &near, None, None, &|| false);
        if best.as_ref().is_none_or(|b| t.cost < b.cost) {
            best = Some(t);
        }
    }
    arcs.order(&best.unwrap().t)
}

/// A small, fast generator (SplitMix64), so orders are the same on every machine.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
}

/// The cycle's arcs: the stops `0..n`, and the dummy `n` unless the trip is a round trip.
struct Arcs<'a> {
    m: &'a Matrix,
    ends: Ends,
    /// The cycle's nodes.
    size: usize,
    /// The node every tour starts from: the dummy, or the first stop of a round trip.
    anchor: usize,
}

impl<'a> Arcs<'a> {
    fn new(m: &'a Matrix, ends: Ends) -> Self {
        let n = m.len();
        let ends = match ends.round_trip {
            true => Ends {
                keep_first: true,
                keep_last: false,
                round_trip: true,
            },
            false => ends,
        };
        let dummy = usize::from(!ends.round_trip);
        Arcs {
            m,
            ends,
            size: n + dummy,
            anchor: if ends.round_trip { 0 } else { n },
        }
    }

    fn get(&self, i: usize, j: usize) -> i64 {
        let n = self.m.len();
        if self.ends.round_trip {
            return Self::weight(self.m.get(i, j));
        }
        if i == n {
            return match self.ends.keep_first && j != 0 {
                true => FORBIDDEN,
                false => 0,
            };
        }
        if j == n {
            return match self.ends.keep_last && i != n - 1 {
                true => FORBIDDEN,
                false => 0,
            };
        }
        Self::weight(self.m.get(i, j))
    }

    fn weight(w: u32) -> i64 {
        match w {
            NONE => NO_ROUTE,
            w => i64::from(w),
        }
    }

    /// The stops of a tour from the anchor, in order, without the dummy.
    fn order(&self, tour: &[usize]) -> Vec<usize> {
        match self.ends.round_trip {
            true => tour.to_vec(),
            false => tour[1..].to_vec(),
        }
    }

    /// The nodes a tour may reorder, from the anchor's next: the kept ends first, so the
    /// insertion puts them in their places before the rest.
    fn movable(&self) -> Vec<usize> {
        let n = self.m.len();
        let mut out: Vec<usize> = (0..self.size).filter(|&v| v != self.anchor).collect();
        if !self.ends.round_trip {
            let mut kept = Vec::new();
            if self.ends.keep_first {
                kept.push(0);
            }
            if self.ends.keep_last {
                kept.push(n - 1);
            }
            out.retain(|v| !kept.contains(v));
            kept.extend(out);
            out = kept;
        }
        out
    }
}

/// The best cycle by dynamic programming over the subsets of nodes after the anchor.
fn held_karp(arcs: &Arcs) -> Vec<usize> {
    let a = arcs.anchor;
    let others: Vec<usize> = (0..arcs.size).filter(|&v| v != a).collect();
    let k = others.len();
    let full = 1usize << k;
    let mut cost = vec![i64::MAX; full * k];
    let mut from = vec![u8::MAX; full * k];
    for (j, &v) in others.iter().enumerate() {
        cost[(1 << j) * k + j] = arcs.get(a, v);
    }
    for mask in 1..full {
        for j in (0..k).filter(|j| mask & 1 << j != 0) {
            let c = cost[mask * k + j];
            if c == i64::MAX {
                continue;
            }
            for x in (0..k).filter(|x| mask & 1 << x == 0) {
                let next = (mask | 1 << x) * k + x;
                let w = c + arcs.get(others[j], others[x]);
                if w < cost[next] {
                    cost[next] = w;
                    from[next] = j as u8;
                }
            }
        }
    }
    let last = (0..k)
        .min_by_key(|&j| cost[(full - 1) * k + j] + arcs.get(others[j], a))
        .expect("at least one node after the anchor");
    let mut tour = Vec::with_capacity(k + 1);
    let (mut mask, mut j) = (full - 1, last);
    loop {
        tour.push(others[j]);
        let f = from[mask * k + j];
        mask &= !(1 << j);
        if f == u8::MAX {
            break;
        }
        j = f as usize;
    }
    tour.push(a);
    tour.reverse();
    tour
}

/// Each node's nearest by the arcs out of it, and by the arcs into it.
struct Near {
    out: Vec<Vec<usize>>,
    into: Vec<Vec<usize>>,
}

impl Near {
    fn new(arcs: &Arcs) -> Self {
        let size = arcs.size;
        let nearest = |u: usize, w: &dyn Fn(usize) -> i64| -> Vec<usize> {
            let mut v: Vec<(i64, usize)> =
                (0..size).filter(|&x| x != u).map(|x| (w(x), x)).collect();
            let k = NEIGHBOURS.min(v.len());
            if k < v.len() {
                v.select_nth_unstable(k);
                v.truncate(k);
            }
            v.sort_unstable();
            v.into_iter().map(|(_, x)| x).collect()
        };
        Near {
            out: (0..size).map(|u| nearest(u, &|x| arcs.get(u, x))).collect(),
            into: (0..size).map(|u| nearest(u, &|x| arcs.get(x, u))).collect(),
        }
    }
}

/// A start: the movable nodes in a random order, each put where it adds least.
fn insertion(arcs: &Arcs, rng: &mut Rng) -> Vec<usize> {
    let mut nodes = arcs.movable();
    let kept = match arcs.ends.round_trip {
        true => 0,
        false => usize::from(arcs.ends.keep_first) + usize::from(arcs.ends.keep_last),
    };
    // Shuffle all but the kept ends, which go in first.
    for i in (kept + 1..nodes.len()).rev() {
        let j = kept + rng.below(i - kept + 1);
        nodes.swap(i, j);
    }
    let mut tour = vec![arcs.anchor];
    for v in nodes {
        let len = tour.len();
        let (mut best, mut at) = (i64::MAX, len);
        for p in 0..len {
            let (x, y) = (tour[p], tour[(p + 1) % len]);
            let add = arcs.get(x, v) + arcs.get(v, y) - if len > 1 { arcs.get(x, y) } else { 0 };
            if add < best {
                (best, at) = (add, p + 1);
            }
        }
        tour.insert(at, v);
    }
    tour
}

/// A cycle from the anchor, with each node's place and the weights along it both ways.
#[derive(Clone)]
struct Tour {
    t: Vec<usize>,
    pos: Vec<usize>,
    /// `fwd[i]`: the weight from `t[0]` to `t[i]` along the tour; `bwd[i]`: the same run
    /// driven the other way.
    fwd: Vec<i64>,
    bwd: Vec<i64>,
    cost: i64,
}

impl Tour {
    fn new(arcs: &Arcs, t: Vec<usize>) -> Self {
        let mut tour = Tour {
            pos: vec![0; t.len()],
            fwd: vec![0; t.len()],
            bwd: vec![0; t.len()],
            t,
            cost: 0,
        };
        tour.update(arcs);
        tour
    }

    fn update(&mut self, arcs: &Arcs) {
        let t = &self.t;
        let n = t.len();
        for (i, &v) in t.iter().enumerate() {
            self.pos[v] = i;
        }
        for i in 1..n {
            self.fwd[i] = self.fwd[i - 1] + arcs.get(t[i - 1], t[i]);
            self.bwd[i] = self.bwd[i - 1] + arcs.get(t[i], t[i - 1]);
        }
        self.cost = self.fwd[n - 1] + arcs.get(t[n - 1], t[0]);
    }

    /// Or-opt and 2-opt moves from the nodes in `dirty` (all of them if `None`), each
    /// move queueing the nodes whose arcs it changed, until none finds one, the deadline
    /// passes or `stop` returns true (`None` then).
    fn improve(
        &mut self,
        arcs: &Arcs,
        near: &Near,
        dirty: Option<&[usize]>,
        deadline: Option<Instant>,
        stop: &dyn Fn() -> bool,
    ) -> Option<()> {
        let n = self.t.len();
        if n < 4 {
            return Some(());
        }
        let mut queued = vec![false; n];
        let mut queue = VecDeque::new();
        let all: Vec<usize>;
        let dirty = match dirty {
            Some(d) => d,
            None => {
                all = self.t.clone();
                &all
            }
        };
        for &v in dirty {
            if !std::mem::replace(&mut queued[v], true) {
                queue.push_back(v);
            }
        }
        let mut tried = 0usize;
        while let Some(v) = queue.pop_front() {
            queued[v] = false;
            tried += 1;
            if tried.is_multiple_of(64) {
                if stop() {
                    return None;
                }
                if deadline.is_some_and(|d| Instant::now() >= d) {
                    return Some(());
                }
            }
            let touched = self
                .or_opt(arcs, near, v)
                .or_else(|| self.two_opt(arcs, near, v));
            for u in touched.into_iter().flatten() {
                if !std::mem::replace(&mut queued[u], true) {
                    queue.push_back(u);
                }
            }
        }
        Some(())
    }

    /// Moves the run of up to [`RUN`] nodes from `v` next to a near node, keeping its
    /// direction. Returns the nodes whose arcs changed.
    fn or_opt(&mut self, arcs: &Arcs, near: &Near, v: usize) -> Option<[usize; 6]> {
        let n = self.t.len();
        let i = self.pos[v];
        if i == 0 {
            return None;
        }
        for len in 1..=RUN {
            let e_i = i + len - 1;
            if e_i >= n {
                break;
            }
            let t = &self.t;
            let (s, e, p, q) = (t[i], t[e_i], t[i - 1], t[(e_i + 1) % n]);
            let gain = arcs.get(p, s) + arcs.get(e, q) - arcs.get(p, q);
            if gain <= 0 {
                continue;
            }
            let inside = |k: usize| (i..=e_i).contains(&k);
            let mut best: Option<(i64, usize, usize)> = None;
            let mut try_at = |x: usize, y: usize| {
                let d = arcs.get(x, s) + arcs.get(e, y) - arcs.get(x, y) - gain;
                if d < 0 && best.is_none_or(|b| d < b.0) {
                    best = Some((d, x, y));
                }
            };
            for &x in &near.into[s] {
                let xi = self.pos[x];
                if !inside(xi) && xi != i - 1 {
                    try_at(x, t[(xi + 1) % n]);
                }
            }
            for &y in &near.out[e] {
                let yi = self.pos[y];
                if !inside(yi) && yi != (e_i + 1) % n {
                    try_at(t[(yi + n - 1) % n], y);
                }
            }
            if let Some((_, x, y)) = best {
                let run: Vec<usize> = self.t.drain(i..=e_i).collect();
                let at = self.t.iter().position(|&v| v == x).unwrap() + 1;
                self.t.splice(at..at, run);
                self.update(arcs);
                return Some([p, s, e, q, x, y]);
            }
        }
        None
    }

    /// Reverses a run that starts or ends at `v`, if its new ends join near nodes.
    /// Returns the nodes whose arcs changed.
    fn two_opt(&mut self, arcs: &Arcs, near: &Near, v: usize) -> Option<[usize; 6]> {
        let n = self.t.len();
        let i = self.pos[v];
        if i == 0 || i == n - 1 {
            return None;
        }
        let mut best: Option<(i64, usize, usize)> = None;
        let mut try_run = |tour: &Tour, i: usize, j: usize| {
            let t = &tour.t;
            let (p, q) = (t[i - 1], t[(j + 1) % n]);
            let d = arcs.get(p, t[j]) + arcs.get(t[i], q) + (tour.bwd[j] - tour.bwd[i])
                - arcs.get(p, t[i])
                - arcs.get(t[j], q)
                - (tour.fwd[j] - tour.fwd[i]);
            if d < 0 && best.is_none_or(|b| d < b.0) {
                best = Some((d, i, j));
            }
        };
        // From `v`, the run's new first node is one near `t[i - 1]`; to `v`, its new
        // last is one near `t[i + 1]`.
        for &c in &near.out[self.t[i - 1]] {
            let j = self.pos[c];
            if j > i {
                try_run(self, i, j);
            }
        }
        for &d in &near.into[self.t[i + 1]] {
            let k = self.pos[d];
            if k >= 1 && k < i {
                try_run(self, k, i);
            }
        }
        let (_, a, b) = best?;
        let t = &self.t;
        let touched = [t[a - 1], t[a], t[b], t[(b + 1) % n], v, v];
        self.t[a..=b].reverse();
        self.update(arcs);
        Some(touched)
    }

    /// Two short runs next to each other swapped: a move Or-opt and 2-opt cannot undo in
    /// one step, that keeps each run's direction. Returns the nodes whose arcs changed.
    fn kicked(&self, arcs: &Arcs, rng: &mut Rng) -> (Tour, [usize; 6]) {
        let n = self.t.len();
        let most = KICK.min((n - 1) / 3);
        let (a, b, c) = loop {
            let a = 1 + rng.below(n - 1);
            let b = a + 1 + rng.below(most);
            let c = b + 1 + rng.below(most);
            if c < n {
                break (a, b, c);
            }
        };
        let t = &self.t;
        let touched = [t[a - 1], t[a], t[b - 1], t[b], t[c - 1], t[c]];
        let mut out = Vec::with_capacity(n);
        out.extend_from_slice(&t[..a]);
        out.extend_from_slice(&t[b..c]);
        out.extend_from_slice(&t[a..b]);
        out.extend_from_slice(&t[c..]);
        (Tour::new(arcs, out), touched)
    }
}

/// The starts, improved, then the best kicked and improved again.
fn heuristic(arcs: &Arcs, deadline: Instant, stop: &dyn Fn() -> bool) -> Option<Vec<usize>> {
    let near = Near::new(arcs);
    let mut rng = Rng::new(SEED);
    let mut best: Option<Tour> = None;
    for k in 0..STARTS {
        // Always one start, whatever the budget.
        if k > 0 && Instant::now() >= deadline {
            break;
        }
        let mut t = Tour::new(arcs, insertion(arcs, &mut rng));
        t.improve(arcs, &near, None, Some(deadline), stop)?;
        if best.as_ref().is_none_or(|b| t.cost < b.cost) {
            best = Some(t);
        }
    }
    let mut best = best.unwrap();
    if arcs.size >= 8 {
        for round in 0..ROUNDS_PER_STOP * arcs.size {
            if round.is_multiple_of(16) && Instant::now() >= deadline {
                break;
            }
            if round.is_multiple_of(16) && stop() {
                return None;
            }
            let (mut t, touched) = best.kicked(arcs, &mut rng);
            t.improve(arcs, &near, Some(&touched), Some(deadline), stop)?;
            if t.cost < best.cost {
                best = t;
            }
        }
    }
    Some(best.t)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Times between random points with noise, some one way only dearer: like roads.
    fn roads(n: usize, seed: u64) -> Matrix {
        let mut rng = Rng::new(seed);
        let pts: Vec<(f64, f64)> = (0..n)
            .map(|_| (rng.below(10_000) as f64, rng.below(10_000) as f64))
            .collect();
        let mut m = Matrix::new(n);
        for i in 0..n {
            for j in (0..n).filter(|&j| j != i) {
                let d = (pts[i].0 - pts[j].0).hypot(pts[i].1 - pts[j].1);
                let extra = rng.below(300) as f64 + if rng.below(5) == 0 { 500.0 } else { 0.0 };
                m.set(i, j, (d * 1.3 + extra) as u32);
            }
        }
        m
    }

    /// Times with no pattern at all.
    fn random(n: usize, seed: u64) -> Matrix {
        let mut rng = Rng::new(seed);
        let mut m = Matrix::new(n);
        for i in 0..n {
            for j in (0..n).filter(|&j| j != i) {
                m.set(i, j, 1 + rng.below(1000) as u32);
            }
        }
        m
    }

    fn ends_all() -> Vec<Ends> {
        let mut out = Vec::new();
        for keep_first in [false, true] {
            for keep_last in [false, true] {
                out.push(Ends {
                    keep_first,
                    keep_last,
                    round_trip: false,
                });
            }
        }
        out.push(Ends {
            round_trip: true,
            ..Ends::default()
        });
        out
    }

    fn valid(order: &[usize], n: usize, ends: Ends) {
        let mut sorted = order.to_vec();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..n).collect::<Vec<_>>());
        if ends.keep_first || ends.round_trip {
            assert_eq!(order[0], 0);
        }
        if ends.keep_last && !ends.round_trip {
            assert_eq!(order[n - 1], n - 1);
        }
    }

    /// Every permutation's weight, keeping the ends asked.
    fn brute(m: &Matrix, ends: Ends) -> u64 {
        fn perms(rest: &mut Vec<usize>, k: usize, f: &mut dyn FnMut(&[usize])) {
            if k == rest.len() {
                return f(rest);
            }
            for i in k..rest.len() {
                rest.swap(k, i);
                perms(rest, k + 1, f);
                rest.swap(k, i);
            }
        }
        let n = m.len();
        let mut all: Vec<usize> = (0..n).collect();
        let mut best = u64::MAX;
        perms(&mut all, 0, &mut |p| {
            let first = !(ends.keep_first || ends.round_trip) || p[0] == 0;
            let last = !ends.keep_last || ends.round_trip || p[n - 1] == n - 1;
            if first && last {
                best = best.min(m.total(p, ends.round_trip).unwrap());
            }
        });
        best
    }

    #[test]
    fn exact_matches_brute_force() {
        for n in 1..=9 {
            for seed in 0..4 {
                let m = match seed % 2 {
                    0 => random(n, seed),
                    _ => roads(n, seed),
                };
                for ends in ends_all() {
                    let o = order(&m, ends, &|| false).unwrap();
                    valid(&o, n, ends);
                    assert_eq!(
                        m.total(&o, ends.round_trip).unwrap(),
                        brute(&m, ends),
                        "{n} stops, {ends:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn heuristic_near_exact_up_to_twelve() {
        let (mut sum_h, mut sum_x) = (0u64, 0u64);
        for n in 10..=13 {
            for seed in 0..3 {
                let m = roads(n, 100 + seed);
                for ends in ends_all() {
                    let arcs = Arcs::new(&m, ends);
                    let x = arcs.order(&held_karp(&arcs));
                    let far = Instant::now() + Duration::from_secs(60);
                    let h = arcs.order(&heuristic(&arcs, far, &|| false).unwrap());
                    valid(&x, n, ends);
                    valid(&h, n, ends);
                    let (x, h) = (m.total(&x, ends.round_trip), m.total(&h, ends.round_trip));
                    let (x, h) = (x.unwrap(), h.unwrap());
                    assert!(h >= x);
                    sum_h += h;
                    sum_x += x;
                }
            }
        }
        // Within 0.5% in all.
        assert!(sum_h * 1000 <= sum_x * 1005, "{sum_h} against {sum_x}");
    }

    #[test]
    fn heuristic_against_random_restarts() {
        let mut ratios = Vec::new();
        for (n, seed) in [(13, 1), (20, 2), (25, 3), (40, 4), (60, 5)] {
            let m = roads(n, seed);
            for ends in ends_all() {
                let o = order(&m, ends, &|| false).unwrap();
                valid(&o, n, ends);
                let r = restarts(&m, ends, 100, 9);
                valid(&r, n, ends);
                let (o, r) = (m.total(&o, ends.round_trip), m.total(&r, ends.round_trip));
                ratios.push(o.unwrap() as f64 / r.unwrap() as f64);
            }
        }
        let mean = ratios.iter().sum::<f64>() / ratios.len() as f64;
        assert!(mean < 1.02, "{mean} {ratios:?}");
    }

    #[test]
    fn same_times_same_order() {
        let m = roads(40, 11);
        let ends = Ends::default();
        assert_eq!(order(&m, ends, &|| false), order(&m, ends, &|| false));
    }

    #[test]
    fn missing_routes_and_stopping() {
        // Stop 2 cannot be reached from 0 directly: the order goes round.
        let mut m = random(5, 3);
        m.set(0, 2, NONE);
        let ends = Ends {
            keep_first: true,
            ..Ends::default()
        };
        let o = order(&m, ends, &|| false).unwrap();
        assert_ne!(o[1], 2);
        assert!(m.total(&o, false).is_some());
        assert_eq!(m.total(&[0, 2], false), None);
        // Stopped at once: no order for a large trip.
        assert_eq!(order(&roads(30, 1), ends, &|| true), None);
        assert_eq!(order(&Matrix::new(1), ends, &|| true), Some(vec![0]));
    }
}
