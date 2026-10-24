//! Street index for attaching addresses: named OSM ways bucketed by name key and grid cell,
//! and "streets" (connected ways sharing a key) with a linear offset along each.

use std::collections::HashMap;

use omp_core::normalize::{is_direction, loose_key, street_key};
use rayon::prelude::*;

use super::osm::StreetWay;
use crate::measure::geom::{self, P};

/// Grid cells of 2^17 world units: about 940 m at 40° N and 560 m at 62° N, so the 3 × 3
/// cells around a point cover every way within 500 m of it.
const CELL_SHIFT: u32 = 17;

pub fn cell(p: P) -> (i32, i32) {
    ((p[0] >> CELL_SHIFT) as i32, (p[1] >> CELL_SHIFT) as i32)
}

/// Interned street keys. Every key also has a loose key (possibly itself) and a direction
/// signature: its direction words, so "N Wren" and "S Wren" stay apart in the dedup even
/// when both attach to a street called "Wren".
#[derive(Default)]
pub struct Keys {
    map: HashMap<String, u32>,
    pub list: Vec<String>,
    pub loose: Vec<u32>,
    pub dirs: Vec<u32>,
    dir_ids: HashMap<String, u32>,
}

impl Keys {
    pub fn intern(&mut self, key: String) -> u32 {
        if let Some(&id) = self.map.get(&key) {
            return id;
        }
        let id = self.list.len() as u32;
        let loose = loose_key(&key);
        let dirs: Vec<&str> = key.split(' ').filter(|w| is_direction(w)).collect();
        let n = self.dir_ids.len() as u32;
        let dir_id = *self.dir_ids.entry(dirs.join(" ")).or_insert(n);
        self.dirs.push(dir_id);
        self.map.insert(key.clone(), id);
        self.list.push(key);
        self.loose.push(id);
        if loose != self.list[id as usize] {
            let l = self.intern(loose);
            self.loose[id as usize] = l;
        }
        id
    }

    /// Keys for many names at once: normalized in parallel, interned in order.
    pub fn intern_names(&mut self, names: &[String]) -> Vec<u32> {
        let keys: Vec<String> = names.par_iter().map(|n| street_key(n)).collect();
        keys.into_iter().map(|k| self.intern(k)).collect()
    }
}

pub struct Way {
    pub id: i64,
    pub name: String,
    pub coords: Vec<P>,
    /// Metres from the start to each vertex.
    pub cum: Vec<f64>,
    /// Strict keys of every name and ref.
    pub keys: Vec<u32>,
}

/// A run of ways that share a key and touch: one street record in the address store.
pub struct Comp {
    pub key: u32,
    pub ways: Vec<u32>,
    pub length_m: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct Hit {
    pub way: u32,
    pub dist_m: f64,
    pub along_m: f64,
    pub left: bool,
}

pub struct Streets {
    pub ways: Vec<Way>,
    strict: HashMap<(u32, i32, i32), Vec<u32>>,
    loose: HashMap<(u32, i32, i32), Vec<u32>>,
    any: HashMap<(i32, i32), Vec<u32>>,
    /// Sorted by the Morton code of their first point, so nearby streets are stored together.
    pub comps: Vec<Comp>,
    /// `(way, key)` → (component, offset of the way's start along the component).
    entry: HashMap<(u32, u32), (u32, f64)>,
}

/// Interleaves the bits of x and y (a Z-order curve).
pub fn morton(p: P) -> u64 {
    let spread = |v: u32| {
        let mut v = u64::from(v);
        v = (v | (v << 16)) & 0x0000_ffff_0000_ffff;
        v = (v | (v << 8)) & 0x00ff_00ff_00ff_00ff;
        v = (v | (v << 4)) & 0x0f0f_0f0f_0f0f_0f0f;
        v = (v | (v << 2)) & 0x3333_3333_3333_3333;
        (v | (v << 1)) & 0x5555_5555_5555_5555
    };
    spread(p[0]) | (spread(p[1]) << 1)
}

fn find(parent: &mut [u32], mut i: u32) -> u32 {
    while parent[i as usize] != i {
        parent[i as usize] = parent[parent[i as usize] as usize];
        i = parent[i as usize];
    }
    i
}

impl Streets {
    pub fn build(raw: Vec<StreetWay>, keys: &mut Keys) -> Self {
        let mut ways = Vec::with_capacity(raw.len());
        let mut way_refs = Vec::with_capacity(raw.len());
        for s in raw {
            let ks = keys.intern_names(&s.names);
            let mut ks_sorted = ks.clone();
            ks_sorted.sort_unstable();
            ks_sorted.dedup();
            let mpu = geom::metres_per_unit(s.coords[0]);
            let mut cum = vec![0.0];
            for w in s.coords.windows(2) {
                let d = (f64::from(w[1][0]) - f64::from(w[0][0]))
                    .hypot(f64::from(w[1][1]) - f64::from(w[0][1]));
                cum.push(cum[cum.len() - 1] + d * mpu);
            }
            ways.push(Way {
                id: s.id,
                name: s.names[0].clone(),
                coords: s.coords,
                cum,
                keys: ks_sorted,
            });
            way_refs.push(s.refs);
        }

        let mut strict: HashMap<(u32, i32, i32), Vec<u32>> = HashMap::new();
        let mut loose: HashMap<(u32, i32, i32), Vec<u32>> = HashMap::new();
        let mut any: HashMap<(i32, i32), Vec<u32>> = HashMap::new();
        let mut cells = Vec::new();
        for (wi, w) in ways.iter().enumerate() {
            cells.clear();
            for s in w.coords.windows(2) {
                let (a, b) = (cell(s[0]), cell(s[1]));
                for x in a.0.min(b.0)..=a.0.max(b.0) {
                    for y in a.1.min(b.1)..=a.1.max(b.1) {
                        cells.push((x, y));
                    }
                }
            }
            cells.sort_unstable();
            cells.dedup();
            let mut lk: Vec<u32> = w.keys.iter().map(|&k| keys.loose[k as usize]).collect();
            lk.sort_unstable();
            lk.dedup();
            for &(x, y) in &cells {
                any.entry((x, y)).or_default().push(wi as u32);
                for &k in &w.keys {
                    strict.entry((k, x, y)).or_default().push(wi as u32);
                }
                for &k in &lk {
                    loose.entry((k, x, y)).or_default().push(wi as u32);
                }
            }
        }

        // Components: (way, key) entries for strict and loose keys, joined when two ways
        // with the same key share a node.
        let mut entries: Vec<(u32, u32)> = Vec::new();
        for (wi, w) in ways.iter().enumerate() {
            let mut ks: Vec<u32> = w
                .keys
                .iter()
                .flat_map(|&k| [k, keys.loose[k as usize]])
                .collect();
            ks.sort_unstable();
            ks.dedup();
            entries.extend(ks.into_iter().map(|k| (wi as u32, k)));
        }
        let mut parent: Vec<u32> = (0..entries.len() as u32).collect();
        let mut by_node: HashMap<(u32, i64), u32> = HashMap::new();
        for (ei, &(w, k)) in entries.iter().enumerate() {
            for &n in &way_refs[w as usize] {
                match by_node.get(&(k, n)) {
                    Some(&other) => {
                        let (a, b) = (find(&mut parent, ei as u32), find(&mut parent, other));
                        if a != b {
                            parent[a as usize] = b;
                        }
                    }
                    None => {
                        by_node.insert((k, n), ei as u32);
                    }
                }
            }
        }
        drop(by_node);
        let mut groups: HashMap<u32, Vec<u32>> = HashMap::new();
        for ei in 0..entries.len() as u32 {
            groups.entry(find(&mut parent, ei)).or_default().push(ei);
        }
        let mut comps: Vec<(u64, Comp)> = groups
            .into_values()
            .map(|es| {
                let key = entries[es[0] as usize].1;
                let mut ws: Vec<u32> = es.iter().map(|&e| entries[e as usize].0).collect();
                ws.sort_unstable_by_key(|&w| ways[w as usize].id);
                let start = ways[ws[0] as usize].coords[0];
                let length_m = ws
                    .iter()
                    .map(|&w| *ways[w as usize].cum.last().unwrap())
                    .sum();
                (
                    morton(start),
                    Comp {
                        key,
                        ways: ws,
                        length_m,
                    },
                )
            })
            .collect();
        comps.sort_unstable_by_key(|(m, c)| (*m, c.key, ways[c.ways[0] as usize].id));
        let comps: Vec<Comp> = comps.into_iter().map(|(_, c)| c).collect();
        let mut entry = HashMap::new();
        for (ci, c) in comps.iter().enumerate() {
            let mut base = 0.0;
            for &w in &c.ways {
                entry.insert((w, c.key), (ci as u32, base));
                base += ways[w as usize].cum.last().unwrap();
            }
        }
        Self {
            ways,
            strict,
            loose,
            any,
            comps,
            entry,
        }
    }

    /// Distance, offset along the way and side of `p` relative to way `w`.
    pub fn project(&self, w: u32, p: P) -> Hit {
        let way = &self.ways[w as usize];
        let mpu = geom::metres_per_unit(p);
        let (px, py) = (f64::from(p[0]), f64::from(p[1]));
        let mut best = (f64::INFINITY, 0.0, false);
        for (i, s) in way.coords.windows(2).enumerate() {
            let (ax, ay) = (f64::from(s[0][0]), f64::from(s[0][1]));
            let (dx, dy) = (f64::from(s[1][0]) - ax, f64::from(s[1][1]) - ay);
            let len2 = dx * dx + dy * dy;
            let t = if len2 > 0.0 {
                (((px - ax) * dx + (py - ay) * dy) / len2).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let (qx, qy) = (ax + t * dx, ay + t * dy);
            let d2 = (px - qx).powi(2) + (py - qy).powi(2);
            if d2 < best.0 {
                let along = way.cum[i] + t * (way.cum[i + 1] - way.cum[i]);
                // y grows downward, so a negative cross product is the left side.
                let left = dx * (py - ay) - dy * (px - ax) < 0.0;
                best = (d2, along, left);
            }
        }
        Hit {
            way: w,
            dist_m: best.0.sqrt() * mpu,
            along_m: best.1,
            left: best.2,
        }
    }

    /// The nearest way with this key (strict, or the loose key's index) within the 3 × 3
    /// cells around `p`.
    pub fn nearest(&self, key: u32, loose: bool, p: P) -> Option<Hit> {
        let index = if loose { &self.loose } else { &self.strict };
        let (cx, cy) = cell(p);
        let mut best: Option<Hit> = None;
        for x in cx - 1..=cx + 1 {
            for y in cy - 1..=cy + 1 {
                for &w in index.get(&(key, x, y)).into_iter().flatten() {
                    let h = self.project(w, p);
                    if best.is_none_or(|b| h.dist_m < b.dist_m) {
                        best = Some(h);
                    }
                }
            }
        }
        best
    }

    /// Component and offset along it for a hit found under `key`.
    pub fn locate(&self, hit: &Hit, key: u32) -> Option<(u32, f64)> {
        self.entry
            .get(&(hit.way, key))
            .map(|&(c, base)| (c, base + hit.along_m))
    }

    /// Distinct way names within `radius_m` of `p`, nearest first (for samples).
    pub fn names_near(&self, p: P, radius_m: f64) -> Vec<(String, f64)> {
        let (cx, cy) = cell(p);
        let mut found: Vec<(String, f64)> = Vec::new();
        for x in cx - 1..=cx + 1 {
            for y in cy - 1..=cy + 1 {
                for &w in self.any.get(&(x, y)).into_iter().flatten() {
                    let h = self.project(w, p);
                    if h.dist_m <= radius_m {
                        found.push((self.ways[w as usize].name.clone(), h.dist_m));
                    }
                }
            }
        }
        found.sort_by(|a, b| a.1.total_cmp(&b.1));
        let mut seen = std::collections::HashSet::new();
        found.retain(|(n, _)| seen.insert(n.clone()));
        found
    }
}
