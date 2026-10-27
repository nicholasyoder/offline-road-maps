use omp_core::profile::{FERRY, TOLL};

use super::check::{COMBINATIONS, Rng, dijkstra, query};
use super::*;
use crate::graph::Edge;

fn edge(to: u32, time_ds: u32, flags: u8) -> Edge {
    Edge {
        to,
        time_ds,
        seg: 0,
        flags,
    }
}

/// A graph from `(from, to, weight, flags)` edges.
fn graph(nodes: usize, list: &[(u32, u32, u32, u8)]) -> Csr {
    let list = list
        .iter()
        .map(|&(a, b, w, f)| (a, edge(b, w, f)))
        .collect();
    Csr::from_edges(nodes, list)
}

/// Checks every pair and every combination of avoided flags against Dijkstra, and that each
/// route unpacks to a chain of base edges of its weight that avoids the flags.
fn check_all(g: &Csr, p: &Params) -> Ch {
    let ch = contract(g, p);
    let n = g.nodes() as u32;
    let mut dist = vec![u32::MAX; n as usize];
    for s in 0..n {
        for t in 0..n {
            for avoid in COMBINATIONS {
                let want = dijkstra(g, s, t, avoid, &mut dist);
                let got = query(&ch, g, s, t, avoid);
                assert_eq!(
                    got.as_ref().map(|r| r.weight),
                    want,
                    "{s} -> {t} avoiding {avoid}"
                );
                let Some(r) = got else { continue };
                let mut at = s;
                let mut sum = 0;
                for &i in &r.edges {
                    assert!((g.first[at as usize]..g.first[at as usize + 1]).contains(&i));
                    let e = &g.edges[i as usize];
                    assert_eq!(e.flags & avoid, 0);
                    sum += e.weight();
                    at = e.to;
                }
                assert_eq!((at, sum), (t, r.weight), "{s} -> {t} avoiding {avoid}");
            }
        }
    }
    ch
}

/// A random road-like graph: a grid with two-way streets of random weights, some one-way,
/// some missing, and a share of toll and ferry edges.
fn random_grid(side: u32, seed: u64) -> Csr {
    let mut rng = Rng::new(seed);
    let mut list = Vec::new();
    let id = |x: u32, y: u32| y * side + x;
    for y in 0..side {
        for x in 0..side {
            for (nx, ny) in [(x + 1, y), (x, y + 1)] {
                if nx >= side || ny >= side || rng.below(10) == 0 {
                    continue;
                }
                let (a, b) = (id(x, y), id(nx, ny));
                let flags = match rng.below(12) {
                    0 => TOLL,
                    1 => FERRY,
                    2 => TOLL | FERRY,
                    _ => 0,
                };
                // Flagged edges are often the fast ones, as toll roads are.
                let w = match flags {
                    0 => 10 + rng.below(90) as u32,
                    _ => 1 + rng.below(30) as u32,
                };
                match rng.below(5) {
                    0 => list.push((a, b, w, flags)),
                    1 => list.push((b, a, w, flags)),
                    _ => {
                        list.push((a, b, w, flags));
                        list.push((b, a, w + rng.below(3) as u32, flags));
                    }
                }
            }
        }
    }
    graph((side * side) as usize, &list)
}

#[test]
fn random_grids_match_dijkstra_with_every_avoid_combination() {
    for seed in 0..6 {
        let ch = check_all(&random_grid(9, seed), &Params::default());
        assert!(ch.stats.rounds > 1);
        assert_eq!(ch.rank.iter().filter(|&&r| r == u32::MAX).count(), 0);
    }
}

#[test]
fn small_witness_limits_and_other_priorities_stay_exact() {
    let p = Params {
        settled: 2,
        edge_difference: 1,
        contracted_neighbours: 3,
        depth: 0,
        ..Params::default()
    };
    check_all(&random_grid(8, 42), &p);
}

#[test]
fn without_flags_avoiding_is_not_possible_but_routes_match() {
    let g = random_grid(8, 7);
    let p = Params {
        flags: false,
        ..Params::default()
    };
    let ch = contract(&g, &p);
    let mut dist = vec![u32::MAX; g.nodes()];
    for s in 0..g.nodes() as u32 {
        for t in 0..g.nodes() as u32 {
            assert_eq!(
                query(&ch, &g, s, t, 0).map(|r| r.weight),
                dijkstra(&g, s, t, 0, &mut dist)
            );
        }
    }
    assert_eq!(ch.stats.flagged_edges, 0);
}

#[test]
fn a_toll_road_beside_a_free_road_keeps_both() {
    // 0 -> 1 -> 2 through the toll road is fast; 0 -> 3 -> 2 is free and slow. Node 1 and
    // node 3 are contracted first: the free path is no witness for the toll shortcut, and
    // the toll path no witness for the free one.
    let g = graph(
        4,
        &[
            (0, 1, 5, TOLL),
            (1, 2, 5, TOLL),
            (0, 3, 20, 0),
            (3, 2, 20, 0),
            // A direct ferry, faster still.
            (0, 2, 4, FERRY),
        ],
    );
    let ch = check_all(&g, &Params::default());
    assert_eq!(query(&ch, &g, 0, 2, 0).unwrap().weight, 4);
    assert_eq!(query(&ch, &g, 0, 2, FERRY).unwrap().weight, 10);
    assert_eq!(query(&ch, &g, 0, 2, AVOID).unwrap().weight, 40);
    assert!(query(&ch, &g, 2, 0, 0).is_none());
}

#[test]
fn parallel_edges_keep_only_the_undominated() {
    let g = graph(
        2,
        &[
            (0, 1, 10, 0),
            (0, 1, 12, 0),    // slower, same flags: dropped
            (0, 1, 8, TOLL),  // faster with a toll: kept
            (0, 1, 9, AVOID), // slower than the toll one, more flags: dropped
            (0, 0, 1, 0),     // a loop: left out
        ],
    );
    let lists = arcs(&g, AVOID);
    let out: Vec<(u32, u8)> = lists[0]
        .iter()
        .map(|a| (a.weight, a.bits & AVOID))
        .collect();
    assert_eq!(out, [(10, 0), (8, TOLL)]);
    check_all(&g, &Params::default());
}

#[test]
fn equal_paths_contracted_in_one_round_keep_a_route() {
    // Two equal routes 0 -> 1 -> 3 and 0 -> 2 -> 3: if 1 and 2 are contracted together, each
    // must not count the other as its witness.
    let g = graph(
        4,
        &[
            (0, 1, 5, 0),
            (1, 3, 5, 0),
            (0, 2, 5, 0),
            (2, 3, 5, 0),
            (1, 0, 5, 0),
            (3, 1, 5, 0),
            (2, 0, 5, 0),
            (3, 2, 5, 0),
        ],
    );
    check_all(&g, &Params::default());
}

#[test]
fn restricted_entry_penalty_is_in_the_weight() {
    let g = graph(
        3,
        &[
            (0, 1, 10, 0),
            (1, 2, 10, crate::graph::ENTRY),
            (0, 2, 50, 0),
        ],
    );
    let ch = check_all(&g, &Params::default());
    assert_eq!(query(&ch, &g, 0, 2, 0).unwrap().weight, 50);
}
