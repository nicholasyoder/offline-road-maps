//! Turn restrictions, applied by splitting nodes (phase 3 decision 2).
//!
//! Each restriction becomes forbidden paths of graph nodes: `no_left_turn` from way F
//! through node v onto way T forbids `[u, v, x]`, where u is F's node next to v and x is
//! T's; a via-way restriction forbids the whole path along the via ways; `only_*` forbids
//! every other way out of the last via node. The paths go into a trie, read as an
//! Aho–Corasick automaton over node IDs: a state is the longest suffix of the nodes walked
//! so far that starts a forbidden path. The new graph has one node per state: the plain
//! nodes, whose state is just themselves, and one copy of v per approach that a restriction
//! names (`[u, v]`), with only the edges out of v that the approach allows. Via-node and
//! via-way restrictions, and restrictions that overlap, all come out of the same rule.
//!
//! A U-turn penalty (phase 4 step 4.4) uses the same automaton. A U-turn is the node path
//! `[u, v, u]`; it needs a state that knows u, so each two-way road `[u, v]` into a junction
//! v (three or more roads) goes into the trie as an approach that bans nothing, and the edge
//! from that copy back to u carries the penalty. Where every road into v is two-way, one of
//! them, u0, gets no copy: arriving from u0 reaches the plain node, which then knows it came
//! from u0. Dead ends and nodes with two roads keep their free U-turn.

use std::collections::{HashMap, HashSet};

use omp_core::profile::Restriction;
use serde::{Deserialize, Serialize};

use super::{Csr, Edge, RestrictionIn, Via};

#[derive(Default, Serialize, Deserialize)]
pub struct RestrictionStats {
    /// Restrictions for cars with usable members.
    pub for_cars: u64,
    pub applied: u64,
    pub via_ways: u64,
    /// A member way or via node is not in the graph: cut at the extract's edge, or not a
    /// road a car may use.
    pub missing_members: u64,
    /// The members do not meet end to end.
    pub broken: u64,
    pub forbidden_paths: u64,
    /// Copies of nodes, one per restricted approach (and per U-turn approach).
    pub nodes_added: u64,
    /// With a U-turn penalty: the junctions, the approach copies made for it (some
    /// restricted approaches already had one), and the edges that carry it.
    #[serde(default)]
    pub uturn_junctions: u64,
    #[serde(default)]
    pub uturn_copies: u64,
    #[serde(default)]
    pub uturn_edges: u64,
}

/// The nodes next to `v` along `seq`, where `v` is one of its ends.
fn next_to(seq: &[u32], v: u32) -> Vec<u32> {
    let mut out = Vec::new();
    if seq.len() >= 2 {
        if seq[0] == v {
            out.push(seq[1]);
        }
        if seq[seq.len() - 1] == v {
            out.push(seq[seq.len() - 2]);
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// The paths a restriction starts with, up to its last via node: `[u, v]` for a via node,
/// `[u, s, …, t]` along the via ways.
fn prefixes(r: &RestrictionIn, seqs: &HashMap<i64, Vec<u32>>, v: Option<u32>) -> Vec<Vec<u32>> {
    let mut out = Vec::new();
    for f in &r.from {
        let seq = &seqs[f];
        match &r.via {
            Via::Node(_) => {
                let v = v.expect("via node");
                out.extend(next_to(seq, v).into_iter().map(|u| vec![u, v]));
            }
            Via::Ways(vias) => {
                for (s, u) in [
                    (seq[0], seq.get(1)),
                    (seq[seq.len() - 1], seq.iter().nth_back(1)),
                ] {
                    let Some(&u) = u else { continue };
                    let mut path = vec![u, s];
                    let mut ok = true;
                    for via in vias {
                        let vs = &seqs[via];
                        let cur = *path.last().unwrap();
                        if vs[0] == cur {
                            path.extend(&vs[1..]);
                        } else if vs[vs.len() - 1] == cur {
                            path.extend(vs[..vs.len() - 1].iter().rev());
                        } else {
                            ok = false;
                            break;
                        }
                    }
                    if ok {
                        out.push(path);
                    }
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// The forbidden node paths of the restrictions.
fn forbidden(
    g: &Csr,
    seqs: &HashMap<i64, Vec<u32>>,
    via_rank: impl Fn(i64) -> Option<u32>,
    list: &[RestrictionIn],
    st: &mut RestrictionStats,
) -> Vec<Vec<u32>> {
    let mut paths = Vec::new();
    for r in list {
        st.for_cars += 1;
        let v = match r.via {
            Via::Node(id) => via_rank(id),
            Via::Ways(_) => None,
        };
        let via_ways = match &r.via {
            Via::Node(_) => &[][..],
            Via::Ways(w) => &w[..],
        };
        let missing = r
            .from
            .iter()
            .chain(via_ways)
            .chain(&r.to)
            .any(|w| !seqs.contains_key(w))
            || matches!(r.via, Via::Node(_)) && v.is_none();
        if missing {
            st.missing_members += 1;
            continue;
        }
        let mut found = Vec::new();
        let mut usable = false;
        for prefix in prefixes(r, seqs, v) {
            let t = *prefix.last().unwrap();
            let mut allowed: Vec<u32> = r.to.iter().flat_map(|w| next_to(&seqs[w], t)).collect();
            allowed.sort_unstable();
            allowed.dedup();
            if allowed.is_empty() {
                continue;
            }
            usable = true;
            // `only_*` forbids every other node out of t (an edge to an allowed node on
            // another way, which a node path cannot tell apart, stays).
            let last = match r.kind {
                Restriction::No => allowed,
                Restriction::Only => {
                    let mut others: Vec<u32> = g
                        .out(t)
                        .iter()
                        .map(|e| e.to)
                        .filter(|y| allowed.binary_search(y).is_err())
                        .collect();
                    others.sort_unstable();
                    others.dedup();
                    others
                }
            };
            found.extend(last.into_iter().map(|x| [&prefix[..], &[x]].concat()));
        }
        if !usable {
            st.broken += 1;
            continue;
        }
        st.applied += 1;
        st.via_ways += u64::from(matches!(r.via, Via::Ways(_)));
        paths.extend(found);
    }
    paths.sort();
    paths.dedup();
    st.forbidden_paths = paths.len() as u64;
    paths
}

/// Where a U-turn costs the penalty: per node, whether it is a junction, and the one road a
/// plain junction node is reached from (`u32::MAX` for none), and the approaches that need
/// a copy.
#[derive(Default)]
struct UTurns {
    junction: Vec<bool>,
    plain_prev: Vec<u32>,
    approaches: Vec<[u32; 2]>,
}

fn uturns(g: &Csr, paths: &[Vec<u32>], st: &mut RestrictionStats) -> UTurns {
    let n = g.nodes();
    // The nodes with an edge into each node, by a counting sort.
    let mut first = vec![0u32; n + 1];
    for e in &g.edges {
        first[e.to as usize + 1] += 1;
    }
    for i in 0..n {
        first[i + 1] += first[i];
    }
    let mut next = first.clone();
    let mut from = vec![0u32; g.edges.len()];
    for v in 0..n as u32 {
        for e in g.out(v) {
            let slot = &mut next[e.to as usize];
            from[*slot as usize] = v;
            *slot += 1;
        }
    }
    drop(next);
    // Approaches a restriction already copies.
    let copied: HashSet<(u32, u32)> = paths.iter().map(|p| (p[0], p[1])).collect();
    let mut u = UTurns {
        junction: vec![false; n],
        plain_prev: vec![u32::MAX; n],
        approaches: Vec::new(),
    };
    let (mut ins, mut outs) = (Vec::new(), Vec::new());
    for v in 0..n as u32 {
        ins.clear();
        ins.extend_from_slice(&from[first[v as usize] as usize..first[v as usize + 1] as usize]);
        ins.sort_unstable();
        ins.dedup();
        outs.clear();
        outs.extend(g.out(v).iter().map(|e| e.to));
        outs.sort_unstable();
        outs.dedup();
        let roads = ins.len()
            + outs
                .iter()
                .filter(|x| ins.binary_search(x).is_err())
                .count();
        if roads < 3 {
            continue;
        }
        u.junction[v as usize] = true;
        st.uturn_junctions += 1;
        let uncopied = |a: &&u32| !copied.contains(&(**a, v));
        let two: Vec<u32> = ins
            .iter()
            .filter(|a| outs.binary_search(a).is_ok())
            .filter(uncopied)
            .copied()
            .collect();
        let one_way = ins
            .iter()
            .filter(|a| outs.binary_search(a).is_err())
            .any(|a| uncopied(&a));
        let rest = match (one_way, two.split_first()) {
            (false, Some((&u0, rest))) => {
                u.plain_prev[v as usize] = u0;
                rest
            }
            _ => &two[..],
        };
        u.approaches.extend(rest.iter().map(|&a| [a, v]));
    }
    u
}

/// The automaton over node IDs. States `0..n` are the plain nodes; the rest are trie nodes of
/// depth two or more.
struct Trie {
    n: u32,
    child: HashMap<(u32, u32), u32>,
    /// Plain nodes that start a forbidden path or an approach.
    starts: Vec<bool>,
    /// Per state from `n`: its graph node, the graph node before it, whether a forbidden
    /// path ends there or at a suffix of it, and its failure state.
    node: Vec<u32>,
    prev: Vec<u32>,
    banned: Vec<bool>,
    fail: Vec<u32>,
}

impl Trie {
    /// The trie of the forbidden `paths` and of the `approaches`, which ban nothing.
    fn new(n: u32, paths: &[Vec<u32>], approaches: &[[u32; 2]]) -> Trie {
        let mut t = Trie {
            n,
            child: HashMap::new(),
            starts: vec![false; n as usize],
            node: Vec::new(),
            prev: Vec::new(),
            banned: Vec::new(),
            fail: Vec::new(),
        };
        let mut depth = Vec::new();
        let all = paths
            .iter()
            .map(|p| (&p[..], true))
            .chain(approaches.iter().map(|a| (&a[..], false)));
        for (p, ban) in all {
            let mut s = p[0];
            t.starts[s as usize] = true;
            for (d, &y) in p.iter().enumerate().skip(1) {
                s = *t.child.entry((s, y)).or_insert_with(|| {
                    t.node.push(y);
                    t.prev.push(p[d - 1]);
                    t.banned.push(false);
                    depth.push(d);
                    n + t.node.len() as u32 - 1
                });
            }
            if ban {
                t.banned[(s - n) as usize] = true;
            }
        }
        // Failure links in order of depth, so a state's failure is done before it is needed.
        let mut parent = vec![0; t.node.len()];
        for (&(p, _), &c) in &t.child {
            parent[(c - n) as usize] = p;
        }
        let mut order: Vec<u32> = (0..t.node.len() as u32).collect();
        order.sort_by_key(|&i| depth[i as usize]);
        t.fail = vec![0; t.node.len()];
        for i in order {
            let p = parent[i as usize];
            let y = t.node[i as usize];
            let f = match p < n {
                true => y,
                false => t.goto(t.fail[(p - n) as usize], y),
            };
            t.fail[i as usize] = f;
            if f >= n && t.banned[(f - n) as usize] {
                t.banned[i as usize] = true;
            }
        }
        t
    }

    fn node_of(&self, s: u32) -> u32 {
        match s < self.n {
            true => s,
            false => self.node[(s - self.n) as usize],
        }
    }

    /// The state after walking from `s` to node `y`.
    fn goto(&self, mut s: u32, y: u32) -> u32 {
        loop {
            if s < self.n && !self.starts[s as usize] {
                return y;
            }
            if let Some(&c) = self.child.get(&(s, y)) {
                return c;
            }
            if s < self.n {
                return y;
            }
            s = self.fail[(s - self.n) as usize];
        }
    }

    fn banned(&self, s: u32) -> bool {
        s >= self.n && self.banned[(s - self.n) as usize]
    }

    /// The graph node walked before state `s`'s, if the state knows it.
    fn prev(&self, s: u32, u: &UTurns) -> Option<u32> {
        match s < self.n {
            true => u
                .plain_prev
                .get(s as usize)
                .copied()
                .filter(|&p| p != u32::MAX),
            false => Some(self.prev[(s - self.n) as usize]),
        }
    }
}

/// The graph with the restrictions applied, and a U-turn penalty of `uturn_ds` tenths of a
/// second (0: none), the node each added copy is of, and the forbidden node paths.
pub fn apply(
    g: Csr,
    seqs: &HashMap<i64, Vec<u32>>,
    via_rank: impl Fn(i64) -> Option<u32>,
    list: &[RestrictionIn],
    uturn_ds: u16,
    st: &mut RestrictionStats,
) -> (Csr, Vec<u32>, Vec<Vec<u32>>) {
    let paths = forbidden(&g, seqs, via_rank, list, st);
    let mut u = match uturn_ds {
        0 => UTurns::default(),
        _ => uturns(&g, &paths, st),
    };
    if paths.is_empty() && u.approaches.is_empty() {
        return (g, Vec::new(), paths);
    }
    let n = g.nodes() as u32;
    // Each approach is a new state, reached from its plain first node.
    let approaches = std::mem::take(&mut u.approaches);
    st.uturn_copies = approaches.len() as u64;
    let trie = Trie::new(n, &paths, &approaches);
    drop(approaches);

    // Copies reached from a plain node, then from each other.
    let mut id: Vec<u32> = vec![u32::MAX; trie.node.len()];
    let mut copy_of = Vec::new();
    let mut queue: Vec<u32> = Vec::new();
    let mut reach = |t: u32, id: &mut Vec<u32>, queue: &mut Vec<u32>| {
        if t >= n && !trie.banned(t) && id[(t - n) as usize] == u32::MAX {
            id[(t - n) as usize] = n + copy_of.len() as u32;
            copy_of.push(trie.node_of(t));
            queue.push(t);
        }
    };
    for s in (0..n).filter(|&s| trie.starts[s as usize]) {
        for e in g.out(s) {
            reach(trie.goto(s, e.to), &mut id, &mut queue);
        }
    }
    let mut i = 0;
    while i < queue.len() {
        let s = queue[i];
        for e in g.out(trie.node_of(s)) {
            reach(trie.goto(s, e.to), &mut id, &mut queue);
        }
        i += 1;
    }
    st.nodes_added = copy_of.len() as u64;

    let states = (0..n).chain(queue.iter().copied());
    let mut first = Vec::with_capacity(n as usize + queue.len() + 1);
    let mut edges: Vec<Edge> = Vec::with_capacity(g.edges.len());
    first.push(0);
    for s in states {
        let v = trie.node_of(s);
        let back = match u.junction.get(v as usize) {
            Some(true) => trie.prev(s, &u),
            _ => None,
        };
        for e in g.out(v) {
            let t = trie.goto(s, e.to);
            if trie.banned(t) {
                continue;
            }
            let to = match t < n {
                true => t,
                false => id[(t - n) as usize],
            };
            let uturn_ds = match back == Some(e.to) {
                true => uturn_ds,
                false => 0,
            };
            st.uturn_edges += u64::from(uturn_ds > 0);
            edges.push(Edge { to, uturn_ds, ..*e });
        }
        first.push(edges.len() as u32);
    }
    (Csr { first, edges }, copy_of, paths)
}
