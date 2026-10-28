//! Alternative routes (phase 4 step 4.2): via-node alternatives over the hierarchy
//! (Abraham, Delling, Goldberg and Werneck, *Alternative routes in road networks*, 2013,
//! its variant for contraction hierarchies), ranked again with turn costs (decision 3).
//!
//! Both sides of a leg's search run on past the quickest route, until the next node of
//! each is (1 + ε) times its weight away. A node both sides settled is a via node *v*: the
//! route up the hierarchy from the start to it and down from it to the end weighs
//! d(s, v) + d(v, t). Candidates are taken by a score, twice that weight plus what their
//! hierarchy edges share with the routes already taken, then unpacked and kept if
//! admissible:
//!
//! - **limited sharing:** at most γ of its weight is on segments of the quickest route or
//!   an alternative already taken;
//! - **locally optimal** (the T-test): the stretch of the route around *v*, α times the
//!   quickest weight long, is itself a quickest route, checked with one more query;
//! - **bounded stretch:** at most ε slower than the quickest, which the search's limit
//!   ensures.
//!
//! Each route, the quickest one included, is then ranked by its weight plus the cost of
//! its turns ([`Leg::turns`]), and the best is the one picked. The times shown stay the
//! profile's.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::io;

use super::query::{Explored, seed_index, stopped};
use super::trip::spans;
use super::{AVOID, ChEdge, Leg, Names, Road, Routing, Snap, Turns};
use crate::geo::P;

/// How alternatives are found.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AltParams {
    /// How much slower than the quickest an alternative may be (ε).
    pub stretch: f64,
    /// The share of an alternative's weight that may be on routes already taken (γ).
    pub sharing: f64,
    /// How long the stretch around the via node that must be a quickest route is, as a
    /// share of the quickest weight (α).
    pub local: f64,
    /// Alternatives wanted.
    pub max: usize,
    /// Candidates unpacked and tested, at most.
    pub tests: usize,
    /// Stall-on-demand in the searches. Without it they settle more nodes, with exact
    /// distances, and find more candidates: on random pairs over 50 km, an alternative for
    /// 2.5 to 6.6 points more of them, for 20 to 40% more time.
    pub stall: bool,
}

impl Default for AltParams {
    fn default() -> Self {
        AltParams {
            stretch: 0.25,
            sharing: 0.8,
            local: 0.25,
            max: 2,
            tests: 16,
            stall: false,
        }
    }
}

/// One of a leg's routes.
#[derive(Clone, Debug, PartialEq)]
pub struct Alternative {
    pub leg: Leg,
    /// Where it passes its via node; `None` for the quickest.
    pub via: Option<P>,
    /// The share of its weight on the routes before it.
    pub shared: f64,
    pub turns: Turns,
}

impl Alternative {
    /// What it is ranked by: its weight in seconds and its turns' cost.
    pub fn score_s(&self) -> f64 {
        f64::from(self.leg.weight) / 10.0 + self.turns.cost_s
    }
}

/// A leg's quickest route and its alternatives.
#[derive(Clone, Debug, PartialEq)]
pub struct Alternatives {
    /// The quickest first, then the alternatives in the order they were taken.
    pub routes: Vec<Alternative>,
    /// The route ranked best with its turns' cost.
    pub picked: usize,
    /// Via nodes within the limit that were left after the filter on shared hierarchy
    /// edges, and how many of them were unpacked and tested.
    pub candidates: usize,
    pub tested: usize,
    /// Nodes the searches settled, the T-tests' included.
    pub settled: usize,
}

impl Alternatives {
    pub fn picked(&self) -> &Alternative {
        &self.routes[self.picked]
    }
}

/// A route through via node `v`: its hierarchy edges in order, its start and end seed
/// nodes, and how many of the edges lead up to `v`.
struct Packed {
    edges: Vec<(u32, u32, ChEdge)>,
    s: u32,
    t: u32,
    up: usize,
}

fn packed(x: &Explored, v: u32) -> Packed {
    let (mut edges, s) = x.f.trace(v);
    let up = edges.len();
    let (down, t) = x.b.trace(v);
    edges.extend(down.into_iter().rev().map(|(to, from, e)| (from, to, e)));
    Packed { edges, s, t, up }
}

impl Packed {
    /// Its weight on hierarchy edges in `taken`.
    fn shared(&self, taken: &HashSet<(u32, u32)>) -> u32 {
        self.edges
            .iter()
            .filter(|(a, b, _)| taken.contains(&(*a, *b)))
            .map(|(_, _, e)| e.weight)
            .sum()
    }

    fn signature(&self) -> u64 {
        let mut h = DefaultHasher::new();
        for (a, b, _) in &self.edges {
            (a, b).hash(&mut h);
        }
        (self.s, self.t).hash(&mut h);
        h.finish()
    }
}

impl Routing {
    /// The alternatives to `quickest`, the leg [`Self::leg_avoiding`] routes from `from` to
    /// `to`, on the edges it was routed on. `None` once `stop` returns true; it is asked
    /// between candidates and every few nodes settled.
    pub fn alternatives_until(
        &self,
        from: &Snap,
        to: &Snap,
        quickest: &Leg,
        avoid: u8,
        params: &AltParams,
        stop: &dyn Fn() -> bool,
    ) -> io::Result<Option<Alternatives>> {
        match self.alternatives_inner(from, to, quickest, avoid, params, stop) {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => Ok(None),
            r => r.map(Some),
        }
    }

    fn alternatives_inner(
        &self,
        from: &Snap,
        to: &Snap,
        quickest: &Leg,
        avoid: u8,
        params: &AltParams,
        stop: &dyn Fn() -> bool,
    ) -> io::Result<Alternatives> {
        let mut out = Alternatives {
            routes: vec![Alternative {
                leg: quickest.clone(),
                via: None,
                shared: 0.0,
                turns: quickest.turns(),
            }],
            picked: 0,
            candidates: 0,
            tested: 0,
            settled: 0,
        };
        // A leg along one segment may not use the graph; its alternatives are not looked for.
        if from.segment == to.segment || params.max == 0 {
            return Ok(out);
        }
        // The flags the quickest leg had to use are allowed, as they were for it.
        let avoid = avoid & AVOID & !quickest.broken;
        let opt = quickest.weight;
        let bound = (f64::from(opt) * (1.0 + params.stretch)).floor() as u32;
        let (a, b) = (self.segment(from.segment)?, self.segment(to.segment)?);
        let sources = Self::sources(from, &a);
        let (targets, reversed) = self.targets(to, &b)?;
        let x = self.explore(&sources, &targets, avoid, params.stall, &|_| bound, stop)?;
        out.settled = x.settled[0].len() + x.settled[1].len();
        if x.best == u32::MAX {
            return Ok(out);
        }
        let mut edges_taken: HashSet<(u32, u32)> = packed(&x, x.meet)
            .edges
            .iter()
            .map(|&(a, b, _)| (a, b))
            .collect();
        let mut segments_taken: HashSet<u32> = quickest.pieces.iter().map(|p| p.dir >> 1).collect();

        // Via nodes within the limit, each route once, by score.
        let back: HashSet<u32> = x.settled[1].iter().copied().collect();
        let mut seen = HashSet::new();
        let mut candidates = Vec::new();
        for &v in &x.settled[0] {
            let (Some(df), Some(db)) = (x.f.dist(v), back.get(&v).and_then(|_| x.b.dist(v))) else {
                continue;
            };
            let d = df + db;
            if d > bound {
                continue;
            }
            let p = packed(&x, v);
            let shared = p.shared(&edges_taken);
            if f64::from(shared) > params.sharing * f64::from(d) || !seen.insert(p.signature()) {
                continue;
            }
            candidates.push((2 * u64::from(d) + u64::from(shared), v, d));
        }
        candidates.sort_unstable();
        out.candidates = candidates.len();

        for (_, v, d) in candidates {
            if out.routes.len() > params.max || out.tested >= params.tests {
                break;
            }
            if stop() {
                return Err(stopped());
            }
            let p = packed(&x, v);
            if f64::from(p.shared(&edges_taken)) > params.sharing * f64::from(d) {
                continue;
            }
            out.tested += 1;
            // Unpacked: the nodes along it and the base edges between them.
            let (mut nodes, mut base) = (vec![p.s], Vec::new());
            let mut at_v = 0;
            for (i, (a, b, e)) in p.edges.iter().enumerate() {
                if i == p.up {
                    at_v = base.len();
                }
                self.unpack_each(*a, *b, e, &mut |_, to, be| {
                    nodes.push(to);
                    base.push(*be);
                })?;
            }
            if p.up == p.edges.len() {
                at_v = base.len();
            }
            let (source, target) = (seed_index(&sources, p.s), seed_index(&targets, p.t));
            let dirs: Vec<u32> = base.iter().map(|e| e.mid).collect();
            let spans = spans(from, to, source, reversed[target], &dirs);
            let weights: Vec<u32> = std::iter::once(sources[source].1)
                .chain(base.iter().map(|e| e.weight))
                .chain(std::iter::once(targets[target].1))
                .collect();
            debug_assert_eq!(weights.iter().sum::<u32>(), d);
            // A segment driven twice is a loop.
            let mut once = HashSet::new();
            if !spans.iter().all(|s| once.insert(s.0 >> 1)) {
                continue;
            }
            let shared: u32 = spans
                .iter()
                .zip(&weights)
                .filter(|(s, _)| segments_taken.contains(&(s.0 >> 1)))
                .map(|(_, w)| w)
                .sum();
            if f64::from(shared) > params.sharing * f64::from(d) {
                continue;
            }
            let (ok, settled) =
                self.locally_optimal(&nodes, &base, at_v, opt, params, avoid, stop)?;
            out.settled += settled;
            if !ok {
                continue;
            }
            let leg = self.leg(d, &spans, stop)?;
            let via = match at_v.checked_sub(1) {
                Some(k) => Some(self.direction_point(base[k].mid, true)?),
                None => base
                    .first()
                    .map(|e| self.direction_point(e.mid, false))
                    .transpose()?,
            };
            segments_taken.extend(spans.iter().map(|s| s.0 >> 1));
            edges_taken.extend(p.edges.iter().map(|&(a, b, _)| (a, b)));
            out.routes.push(Alternative {
                turns: leg.turns(),
                leg,
                via,
                shared: f64::from(shared) / f64::from(d.max(1)),
            });
        }
        out.picked = (0..out.routes.len())
            .min_by(|&i, &j| out.routes[i].score_s().total_cmp(&out.routes[j].score_s()))
            .unwrap_or(0);
        Ok(out)
    }

    /// The T-test: whether the stretch of a route (`nodes` joined by the base edges
    /// `base`) from [`AltParams::local`] / 2 times `opt` before node `at` to as far after
    /// it is a quickest route. Returns that and the nodes the query settled.
    #[allow(clippy::too_many_arguments)]
    fn locally_optimal(
        &self,
        nodes: &[u32],
        base: &[ChEdge],
        at: usize,
        opt: u32,
        params: &AltParams,
        avoid: u8,
        stop: &dyn Fn() -> bool,
    ) -> io::Result<(bool, usize)> {
        let t = (f64::from(opt) * params.local / 2.0) as u64;
        let mut pre = Vec::with_capacity(base.len() + 1);
        pre.push(0u64);
        for e in base {
            pre.push(pre[pre.len() - 1] + u64::from(e.weight));
        }
        let (mut a, mut b) = (at, at);
        while a > 0 && pre[at] - pre[a] < t {
            a -= 1;
        }
        while b < base.len() && pre[b] - pre[at] < t {
            b += 1;
        }
        if a == b {
            return Ok((true, 0));
        }
        let want = pre[b] - pre[a];
        let p = self.search_until(&[(nodes[a], 0)], &[(nodes[b], 0)], avoid, stop)?;
        Ok(match p {
            Some(p) => (u64::from(p.weight) >= want, p.settled),
            None => (false, 0),
        })
    }

    /// The end (or start) point of a segment direction.
    fn direction_point(&self, dir: u32, end: bool) -> io::Result<P> {
        self.with_segment(dir >> 1, |s| match (dir & 1 != 0) == end {
            true => s.points[0],
            false => s.points[s.points.len() - 1],
        })
    }

    /// The road most of `route` that is on none of `others` runs along, by metres: what
    /// it goes "via". `None` if it has no named road of its own.
    pub fn via_road(
        &self,
        route: &Leg,
        others: &[&Leg],
        names: Option<&dyn Names>,
    ) -> io::Result<Option<Road>> {
        let theirs: HashSet<u32> = others
            .iter()
            .flat_map(|l| l.pieces.iter().map(|p| p.dir >> 1))
            .collect();
        let roads = self.roads(route, names)?;
        let mut metres: HashMap<String, (f64, usize)> = HashMap::new();
        for (i, (p, road)) in route.pieces.iter().zip(&roads).enumerate() {
            if theirs.contains(&(p.dir >> 1)) || road.is_empty() {
                continue;
            }
            metres.entry(road.text()).or_insert((0.0, i)).0 += p.metres;
        }
        Ok(metres
            .into_values()
            .max_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, i)| roads[i].clone()))
    }
}
