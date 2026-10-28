//! Queries over the hierarchy (phase 3 step 3.4): a bidirectional search with
//! stall-on-demand, from any number of start and end nodes each with a distance already
//! driven, and the unpacking of its hierarchy edges to segment directions.
//!
//! The forward search follows up edges from the starts, the backward one down edges from
//! the ends; each settles the side with the smaller distance next and both stop once that
//! is no less than the best route met. A node is **stalled**, its edges not followed, when
//! an edge into it from a higher node the same side has reached gives it a shorter
//! distance: then the search reached it the wrong way round, and the shortest route to it
//! runs through that higher node.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::hash::{BuildHasherDefault, Hasher};
use std::io;

use super::{AVOID, ChEdge, Path, Routing, SHORTCUT};
use crate::varint::invalid;

/// A node where a search starts or ends, and the weight already driven to reach it (or
/// still to drive from it).
pub type Seed = (u32, u32);

/// A hasher for node IDs: one multiply, enough for keys that are already well spread.
#[derive(Default)]
struct NodeHasher(u64);

impl Hasher for NodeHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 << 8 | u64::from(b)).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        }
    }

    fn write_u32(&mut self, n: u32) {
        self.0 = u64::from(n).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    }
}

/// What a side knows of a node: its distance, the node before it (`u32::MAX` at a seed)
/// and the hierarchy edge between them.
#[derive(Clone, Copy)]
struct Label {
    d: u32,
    from: u32,
    edge: ChEdge,
}

struct Side {
    dist: HashMap<u32, Label, BuildHasherDefault<NodeHasher>>,
    heap: BinaryHeap<Reverse<(u32, u32)>>,
}

impl Side {
    fn new(seeds: &[Seed]) -> Self {
        let mut s = Side {
            dist: HashMap::default(),
            heap: BinaryHeap::new(),
        };
        let none = ChEdge {
            other: u32::MAX,
            weight: 0,
            flags: 0,
            mid: 0,
        };
        for &(v, d) in seeds {
            if s.dist.get(&v).is_none_or(|l| d < l.d) {
                s.dist.insert(
                    v,
                    Label {
                        d,
                        from: u32::MAX,
                        edge: none,
                    },
                );
                s.heap.push(Reverse((d, v)));
            }
        }
        s
    }

    fn min(&self) -> u32 {
        self.heap.peek().map_or(u32::MAX, |Reverse((d, _))| *d)
    }

    fn dist(&self, v: u32) -> Option<u32> {
        self.dist.get(&v).map(|l| l.d)
    }

    /// The hierarchy edges from the seed to `v`, seed first, and the seed node.
    fn trace(&self, mut v: u32) -> (Vec<(u32, u32, ChEdge)>, u32) {
        let mut out = Vec::new();
        loop {
            let l = self.dist[&v];
            if l.from == u32::MAX {
                out.reverse();
                return (out, v);
            }
            out.push((l.from, v, l.edge));
            v = l.from;
        }
    }
}

/// The index of the seed at node `v` that a search started from: the one with the least
/// weight, as [`Side::new`] kept.
fn seed_index(seeds: &[Seed], v: u32) -> usize {
    (0..seeds.len())
        .filter(|&i| seeds[i].0 == v)
        .min_by_key(|&i| seeds[i].1)
        .expect("a seed at the route's end")
}

impl Routing {
    /// The shortest route from node `s` to node `t` on edges without the flags in
    /// `avoid`, unpacked to segment directions; `None` if there is none.
    pub fn shortest(&self, s: u32, t: u32, avoid: u8) -> io::Result<Option<Path>> {
        self.search(&[(s, 0)], &[(t, 0)], avoid)
    }

    /// The shortest route from any of `sources` to any of `targets`, counting each seed's
    /// weight, on edges without the flags in `avoid`.
    pub fn search(
        &self,
        sources: &[Seed],
        targets: &[Seed],
        avoid: u8,
    ) -> io::Result<Option<Path>> {
        let (mut f, mut b) = (Side::new(sources), Side::new(targets));
        let (mut best, mut meet, mut settled) = (u32::MAX, u32::MAX, 0);
        loop {
            let forward = f.min() <= b.min();
            let (side, other) = match forward {
                true => (&mut f, &b),
                false => (&mut b, &f),
            };
            // The other side's next is no nearer, so neither can improve on `best`.
            if side.min() >= best {
                break;
            }
            let Some(Reverse((d, u))) = side.heap.pop() else {
                break;
            };
            if side.dist[&u].d < d {
                continue;
            }
            settled += 1;
            if let Some(o) = other.dist(u)
                && d + o < best
            {
                (best, meet) = (d + o, u);
            }
            let node = self.node(u)?;
            let (out, into) = match forward {
                true => (node.up(), node.down()),
                false => (node.down(), node.up()),
            };
            let stalled = into.iter().any(|e| {
                e.flags & avoid == 0 && side.dist(e.other).is_some_and(|w| w + e.weight < d)
            });
            if stalled {
                continue;
            }
            for e in out {
                if e.flags & avoid != 0 {
                    continue;
                }
                let nd = d + e.weight;
                let better = side.dist.get(&e.other).is_none_or(|l| nd < l.d);
                if better {
                    side.dist.insert(
                        e.other,
                        Label {
                            d: nd,
                            from: u,
                            edge: *e,
                        },
                    );
                    side.heap.push(Reverse((nd, e.other)));
                }
            }
        }
        if best == u32::MAX {
            return Ok(None);
        }
        // Up from the start to the meeting node, then down to the end.
        let (mut steps, s) = f.trace(meet);
        let (down, t) = b.trace(meet);
        steps.extend(down.into_iter().rev().map(|(to, from, e)| (from, to, e)));
        let mut legs = Vec::new();
        for (from, to, e) in steps {
            self.unpack(from, to, &e, &mut legs)?;
        }
        Ok(Some(Path {
            weight: best,
            legs,
            settled,
            source: seed_index(sources, s),
            target: seed_index(targets, t),
        }))
    }

    /// Appends the segment directions of `e`, a hierarchy edge from `a` to `b`.
    pub fn unpack(&self, a: u32, b: u32, e: &ChEdge, out: &mut Vec<u32>) -> io::Result<()> {
        let m = e.mid;
        if e.flags & SHORTCUT == 0 {
            out.push(m);
            return Ok(());
        }
        // The middle node is below both ends: `a` to it is one of its down edges, it to `b`
        // one of its up edges, with this weight and flags between them.
        let node = self.node(m)?;
        for d in node.down().iter().filter(|d| d.other == a) {
            for u in node.up().iter().filter(|u| u.other == b) {
                if d.weight + u.weight == e.weight && (d.flags | u.flags) & AVOID == e.flags & AVOID
                {
                    self.unpack(a, m, d, out)?;
                    return self.unpack(m, b, u, out);
                }
            }
        }
        Err(invalid(format!(
            "shortcut {a} -> {b} through {m} has no halves"
        )))
    }
}
